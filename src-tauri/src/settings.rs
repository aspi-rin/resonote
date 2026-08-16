use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::{
    models::{DEFAULT_MODEL_ID, is_supported_model},
    openai_compatible::{allows_bearer_auth, chat_completions_url},
    vad::VadConfig,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppLanguage {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en-US")]
    EnUs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AudioSourceMode {
    Microphone,
    System,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    Flac,
    Wav,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSettings {
    pub hide_window_on_close: bool,
    pub language: AppLanguage,
    pub launch_at_login: bool,
    pub start_recording_on_launch: bool,
    pub theme: ThemeMode,
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            hide_window_on_close: true,
            language: AppLanguage::System,
            launch_at_login: false,
            start_recording_on_launch: false,
            theme: ThemeMode::System,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSettings {
    pub format: AudioFormat,
    pub microphone_gain: f32,
    pub output_directory: Option<PathBuf>,
    pub segment_minutes: u32,
    pub source: AudioSourceMode,
    pub system_gain: f32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            format: AudioFormat::Flac,
            microphone_gain: 1.0,
            output_directory: None,
            segment_minutes: 60,
            source: AudioSourceMode::Mixed,
            system_gain: 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranscriptionSettings {
    pub language: String,
    pub model_id: String,
    pub threads: u16,
    pub unload_after_idle_minutes: u32,
    pub vad: VadConfig,
}

/// Debug always renders `[REDACTED]`; only the private stored settings file
/// ever serializes the value.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn trimmed(&self) -> &str {
        self.0.trim()
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderAuthMode {
    #[default]
    None,
    Bearer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranslationSettings {
    pub api_key: SecretString,
    pub api_key_endpoint: String,
    pub enabled: bool,
    pub endpoint: String,
    pub model: String,
    pub target_language: String,
    pub verified_fingerprint: String,
}

impl Default for TranslationSettings {
    fn default() -> Self {
        Self {
            api_key: SecretString::default(),
            api_key_endpoint: String::new(),
            enabled: true,
            endpoint: "http://127.0.0.1:8000/v1".to_owned(),
            model: "Hy-MT2-1.8B".to_owned(),
            target_language: "Chinese".to_owned(),
            verified_fingerprint: String::new(),
        }
    }
}

impl TranslationSettings {
    pub fn snapshot(&self) -> TranslationSnapshot {
        TranslationSnapshot {
            auth_mode: if self.credential_configured() {
                ProviderAuthMode::Bearer
            } else {
                ProviderAuthMode::None
            },
            enabled: self.enabled,
            endpoint: self.endpoint.trim().to_owned(),
            model: self.model.trim().to_owned(),
            target_language: self.target_language.trim().to_owned(),
        }
    }

    pub fn credential_configured(&self) -> bool {
        credential_configured(&self.api_key, &self.api_key_endpoint, &self.endpoint)
    }

    /// The stored key only counts while it is still bound to this endpoint, so
    /// the fingerprint follows the same rule as `apiKeyConfigured`.
    pub fn bound_key(&self) -> Option<&SecretString> {
        self.credential_configured().then_some(&self.api_key)
    }

    fn fingerprint(&self) -> String {
        provider_fingerprint(&self.endpoint, &self.model, self.bound_key())
    }

    fn verified(&self) -> bool {
        !self.verified_fingerprint.is_empty() && self.verified_fingerprint == self.fingerprint()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MeetingNotesSettings {
    pub api_key: SecretString,
    pub api_key_endpoint: String,
    pub endpoint: String,
    pub max_input_characters: u32,
    pub model: String,
    pub request_timeout_seconds: u32,
    pub verified_fingerprint: String,
}

impl Default for MeetingNotesSettings {
    fn default() -> Self {
        Self {
            api_key: SecretString::default(),
            api_key_endpoint: String::new(),
            endpoint: "http://127.0.0.1:8000/v1".to_owned(),
            max_input_characters: 48_000,
            model: String::new(),
            request_timeout_seconds: 180,
            verified_fingerprint: String::new(),
        }
    }
}

impl MeetingNotesSettings {
    pub fn credential_configured(&self) -> bool {
        credential_configured(&self.api_key, &self.api_key_endpoint, &self.endpoint)
    }

    pub fn bound_key(&self) -> Option<&SecretString> {
        self.credential_configured().then_some(&self.api_key)
    }

    fn fingerprint(&self) -> String {
        provider_fingerprint(&self.endpoint, &self.model, self.bound_key())
    }

    fn verified(&self) -> bool {
        !self.verified_fingerprint.is_empty() && self.verified_fingerprint == self.fingerprint()
    }
}

/// Secret-free projection of the translation settings, frozen into session
/// documents so a recording keeps using the provider it started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSnapshot {
    #[serde(default)]
    pub auth_mode: ProviderAuthMode,
    pub enabled: bool,
    pub endpoint: String,
    pub model: String,
    pub target_language: String,
}

impl Default for TranslationSnapshot {
    fn default() -> Self {
        TranslationSettings::default().snapshot()
    }
}

impl Default for TranscriptionSettings {
    fn default() -> Self {
        Self {
            language: "auto".to_owned(),
            model_id: DEFAULT_MODEL_ID.to_owned(),
            threads: default_transcription_threads(),
            unload_after_idle_minutes: 10,
            vad: VadConfig::default(),
        }
    }
}

const fn default_transcription_threads() -> u16 {
    if cfg!(target_os = "macos") { 6 } else { 2 }
}

/// Private stored settings: the only place API keys live.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub audio: AudioSettings,
    pub desktop: DesktopSettings,
    pub meeting_notes: MeetingNotesSettings,
    pub transcription: TranscriptionSettings,
    pub translation: TranslationSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettingsView {
    pub api_key_configured: bool,
    pub endpoint: String,
    pub model: String,
    pub verified: bool,
}

/// Names the provider a connection test verifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    MeetingNotes,
    Translation,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSettingsView {
    pub enabled: bool,
    #[serde(flatten)]
    pub provider: ProviderSettingsView,
    pub target_language: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingNotesSettingsView {
    pub max_input_characters: u32,
    #[serde(flatten)]
    pub provider: ProviderSettingsView,
    pub request_timeout_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettingsView {
    pub audio: AudioSettings,
    pub desktop: DesktopSettings,
    pub meeting_notes: MeetingNotesSettingsView,
    pub transcription: TranscriptionSettings,
    pub translation: TranslationSettingsView,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSettingsWithoutSecrets {
    pub enabled: bool,
    pub endpoint: String,
    pub model: String,
    pub target_language: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingNotesSettingsWithoutSecrets {
    pub endpoint: String,
    pub max_input_characters: u32,
    pub model: String,
    pub request_timeout_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettingsWithoutSecrets {
    pub audio: AudioSettings,
    pub desktop: DesktopSettings,
    pub meeting_notes: MeetingNotesSettingsWithoutSecrets,
    pub transcription: TranscriptionSettings,
    pub translation: TranslationSettingsWithoutSecrets,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum SecretUpdate {
    #[default]
    Keep,
    Set {
        value: String,
    },
    Clear,
}

impl fmt::Debug for SecretUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Keep => "Keep",
            Self::Set { .. } => "Set([REDACTED])",
            Self::Clear => "Clear",
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SettingsSecretUpdates {
    pub meeting_notes_api_key: SecretUpdate,
    pub translation_api_key: SecretUpdate,
}

impl AppSettings {
    pub fn view(&self) -> AppSettingsView {
        AppSettingsView {
            audio: self.audio.clone(),
            desktop: self.desktop.clone(),
            meeting_notes: MeetingNotesSettingsView {
                max_input_characters: self.meeting_notes.max_input_characters,
                provider: ProviderSettingsView {
                    api_key_configured: self.meeting_notes.credential_configured(),
                    endpoint: self.meeting_notes.endpoint.clone(),
                    model: self.meeting_notes.model.clone(),
                    verified: self.meeting_notes.verified(),
                },
                request_timeout_seconds: self.meeting_notes.request_timeout_seconds,
            },
            transcription: self.transcription.clone(),
            translation: TranslationSettingsView {
                enabled: self.translation.enabled,
                provider: ProviderSettingsView {
                    api_key_configured: self.translation.credential_configured(),
                    endpoint: self.translation.endpoint.clone(),
                    model: self.translation.model.clone(),
                    verified: self.translation.verified(),
                },
                target_language: self.translation.target_language.clone(),
            },
        }
    }

    fn merge(
        mut self,
        incoming: AppSettingsWithoutSecrets,
        secrets: SettingsSecretUpdates,
    ) -> Self {
        self.audio = incoming.audio;
        self.desktop = incoming.desktop;
        self.transcription = incoming.transcription;
        self.meeting_notes.endpoint = incoming.meeting_notes.endpoint.trim().to_owned();
        self.meeting_notes.max_input_characters = incoming.meeting_notes.max_input_characters;
        self.meeting_notes.model = incoming.meeting_notes.model.trim().to_owned();
        self.meeting_notes.request_timeout_seconds = incoming.meeting_notes.request_timeout_seconds;
        self.translation.enabled = incoming.translation.enabled;
        self.translation.endpoint = incoming.translation.endpoint.trim().to_owned();
        self.translation.model = incoming.translation.model.trim().to_owned();
        self.translation.target_language = incoming.translation.target_language.trim().to_owned();
        apply_secret(
            &mut self.meeting_notes.api_key,
            &mut self.meeting_notes.api_key_endpoint,
            secrets.meeting_notes_api_key,
            &self.meeting_notes.endpoint,
        );
        apply_secret(
            &mut self.translation.api_key,
            &mut self.translation.api_key_endpoint,
            secrets.translation_api_key,
            &self.translation.endpoint,
        );
        self
    }

    pub fn validate(&self) -> Result<(), SettingsError> {
        if !(1..=24 * 60).contains(&self.audio.segment_minutes) {
            return Err(SettingsError::Validation(
                "segmentMinutes must be between 1 and 1440".to_owned(),
            ));
        }
        for (name, value) in [
            ("microphoneGain", self.audio.microphone_gain),
            ("systemGain", self.audio.system_gain),
        ] {
            if !value.is_finite() || !(0.0..=4.0).contains(&value) {
                return Err(SettingsError::Validation(format!(
                    "{name} must be a finite value between 0 and 4"
                )));
            }
        }
        if !(1..=16).contains(&self.transcription.threads) {
            return Err(SettingsError::Validation(
                "transcription threads must be between 1 and 16".to_owned(),
            ));
        }
        if !is_supported_model(&self.transcription.model_id) {
            return Err(SettingsError::Validation(
                "transcription modelId is not supported".to_owned(),
            ));
        }
        validate_provider(
            "translation",
            &self.translation.endpoint,
            &self.translation.api_key,
            &self.translation.api_key_endpoint,
        )?;
        if self.translation.model.trim().is_empty() {
            return Err(SettingsError::Validation(
                "translation model cannot be empty".to_owned(),
            ));
        }
        if self.translation.target_language.trim().is_empty() {
            return Err(SettingsError::Validation(
                "translation targetLanguage cannot be empty".to_owned(),
            ));
        }
        validate_provider(
            "meetingNotes",
            &self.meeting_notes.endpoint,
            &self.meeting_notes.api_key,
            &self.meeting_notes.api_key_endpoint,
        )?;
        if !(8_000..=200_000).contains(&self.meeting_notes.max_input_characters) {
            return Err(SettingsError::Validation(
                "meetingNotes maxInputCharacters must be between 8000 and 200000".to_owned(),
            ));
        }
        if !(30..=900).contains(&self.meeting_notes.request_timeout_seconds) {
            return Err(SettingsError::Validation(
                "meetingNotes requestTimeoutSeconds must be between 30 and 900".to_owned(),
            ));
        }
        self.transcription
            .vad
            .validate()
            .map_err(|error| SettingsError::Validation(error.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("invalid settings: {0} endpoint must use https or a loopback host to store an API key")]
    InsecureEndpoint(String),
    #[error(
        "invalid settings: {0} endpoint must be a valid http or https URL without embedded credentials"
    )]
    InvalidEndpoint(String),
    #[error("failed to access settings: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse settings: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid settings: {0}")]
    Validation(String),
    #[error("failed to persist temporary settings file: {0}")]
    Persist(#[from] tempfile::PersistError),
}

pub struct SettingsStore {
    current: Arc<RwLock<AppSettings>>,
    path: PathBuf,
}

impl SettingsStore {
    pub fn open(path: PathBuf) -> Result<Self, SettingsError> {
        let mut current = load_settings(&path)?;
        if !is_supported_model(&current.transcription.model_id) {
            current.transcription.model_id = DEFAULT_MODEL_ID.to_owned();
            save_settings_atomic(&path, &current)?;
        }
        current.validate()?;
        Ok(Self {
            current: Arc::new(RwLock::new(current)),
            path,
        })
    }

    pub fn snapshot(&self) -> AppSettings {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn view(&self) -> AppSettingsView {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .view()
    }

    pub fn credentials(&self) -> ProviderCredentials {
        ProviderCredentials {
            settings: self.current.clone(),
        }
    }

    pub fn save(
        &self,
        settings: AppSettingsWithoutSecrets,
        secrets: SettingsSecretUpdates,
    ) -> Result<AppSettingsView, SettingsError> {
        let merged = self.snapshot().merge(settings, secrets);
        merged.validate()?;
        self.commit(merged)
    }

    /// The candidate configuration a connection test runs against: validated,
    /// but only persisted once the provider answered.
    pub fn merge_for_test(
        &self,
        settings: AppSettingsWithoutSecrets,
        secrets: SettingsSecretUpdates,
    ) -> Result<AppSettings, SettingsError> {
        let merged = self.snapshot().merge(settings, secrets);
        merged.validate()?;
        Ok(merged)
    }

    pub fn save_verified(
        &self,
        mut merged: AppSettings,
        provider: ProviderKind,
    ) -> Result<AppSettingsView, SettingsError> {
        match provider {
            ProviderKind::MeetingNotes => {
                merged.meeting_notes.verified_fingerprint = merged.meeting_notes.fingerprint();
            }
            ProviderKind::Translation => {
                merged.translation.verified_fingerprint = merged.translation.fingerprint();
            }
        }
        merged.validate()?;
        self.commit(merged)
    }

    fn commit(&self, merged: AppSettings) -> Result<AppSettingsView, SettingsError> {
        save_settings_atomic(&self.path, &merged)?;
        let view = merged.view();
        *self
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = merged;
        Ok(view)
    }
}

/// Read-only handle on the stored credentials, resolved per request so a key
/// rotated for the same endpoint reaches queued work.
#[derive(Clone, Default)]
pub struct ProviderCredentials {
    settings: Arc<RwLock<AppSettings>>,
}

impl ProviderCredentials {
    /// Read under the same lock as the credentials, so a job freezes a
    /// consistent provider and key pair.
    pub fn meeting_notes(&self) -> MeetingNotesSettings {
        self.settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .meeting_notes
            .clone()
    }

    pub fn meeting_notes_key(&self, endpoint: &str) -> Option<SecretString> {
        let settings = self
            .settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        credential_configured(
            &settings.meeting_notes.api_key,
            &settings.meeting_notes.api_key_endpoint,
            endpoint,
        )
        .then(|| settings.meeting_notes.api_key.clone())
    }

    pub fn translation_key(&self, endpoint: &str) -> Option<SecretString> {
        let settings = self
            .settings
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        credential_configured(
            &settings.translation.api_key,
            &settings.translation.api_key_endpoint,
            endpoint,
        )
        .then(|| settings.translation.api_key.clone())
    }
}

fn apply_secret(
    key: &mut SecretString,
    binding: &mut String,
    update: SecretUpdate,
    endpoint: &str,
) {
    match update {
        SecretUpdate::Keep => {}
        SecretUpdate::Clear => {
            *key = SecretString::default();
            binding.clear();
        }
        SecretUpdate::Set { value } => {
            if value.trim().is_empty() {
                *key = SecretString::default();
                binding.clear();
            } else {
                *key = SecretString::new(value);
                *binding = normalized_endpoint(endpoint).unwrap_or_default();
            }
        }
    }
}

fn credential_configured(key: &SecretString, binding: &str, endpoint: &str) -> bool {
    !key.trimmed().is_empty()
        && !binding.is_empty()
        && normalized_endpoint(endpoint).is_some_and(|normalized| normalized == binding)
}

fn normalized_endpoint(endpoint: &str) -> Option<String> {
    chat_completions_url(endpoint)
        .ok()
        .map(|url| url.to_string())
}

/// A key bound to another endpoint is never sent here, so only a credential
/// bound to this endpoint blocks a plaintext remote host.
fn validate_provider(
    name: &str,
    endpoint: &str,
    api_key: &SecretString,
    api_key_endpoint: &str,
) -> Result<(), SettingsError> {
    let url = chat_completions_url(endpoint)
        .map_err(|_| SettingsError::InvalidEndpoint(name.to_owned()))?;
    if credential_configured(api_key, api_key_endpoint, endpoint) && !allows_bearer_auth(&url) {
        return Err(SettingsError::InsecureEndpoint(name.to_owned()));
    }
    Ok(())
}

/// Identifies the exact provider configuration a connection test verified:
/// endpoint, model and the key that would be sent with it. The key is hashed
/// before it reaches the fingerprint, and an unusable endpoint fingerprints its
/// raw form so it can never match a stored value.
pub fn provider_fingerprint(
    endpoint: &str,
    model: &str,
    bound_key: Option<&SecretString>,
) -> String {
    let endpoint = normalized_endpoint(endpoint).unwrap_or_else(|| endpoint.trim().to_owned());
    let key = bound_key
        .map(|key| sha256_hex(key.trimmed().as_bytes()))
        .unwrap_or_default();
    sha256_hex(format!("{endpoint}\n{}\n{key}", model.trim()).as_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn load_settings(path: &Path) -> Result<AppSettings, SettingsError> {
    if !path.exists() {
        return Ok(AppSettings::default());
    }
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn save_settings_atomic(path: &Path, settings: &AppSettings) -> Result<(), SettingsError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, settings)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    restrict_to_current_user(path)?;
    Ok(())
}

/// Windows inherits the current-user ACL of the app config directory.
#[cfg(unix)]
fn restrict_to_current_user(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_to_current_user(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn without_secrets(settings: &AppSettings) -> AppSettingsWithoutSecrets {
        AppSettingsWithoutSecrets {
            audio: settings.audio.clone(),
            desktop: settings.desktop.clone(),
            meeting_notes: MeetingNotesSettingsWithoutSecrets {
                endpoint: settings.meeting_notes.endpoint.clone(),
                max_input_characters: settings.meeting_notes.max_input_characters,
                model: settings.meeting_notes.model.clone(),
                request_timeout_seconds: settings.meeting_notes.request_timeout_seconds,
            },
            transcription: settings.transcription.clone(),
            translation: TranslationSettingsWithoutSecrets {
                enabled: settings.translation.enabled,
                endpoint: settings.translation.endpoint.clone(),
                model: settings.translation.model.clone(),
                target_language: settings.translation.target_language.clone(),
            },
        }
    }

    fn set_key(value: &str) -> SettingsSecretUpdates {
        SettingsSecretUpdates {
            translation_api_key: SecretUpdate::Set {
                value: value.to_owned(),
            },
            ..SettingsSecretUpdates::default()
        }
    }

    #[test]
    fn defaults_to_mixed_audio_with_transcription() {
        let settings = AppSettings::default();

        assert_eq!(settings.audio.source, AudioSourceMode::Mixed);
        #[cfg(target_os = "macos")]
        assert_eq!(settings.transcription.threads, 6);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(settings.transcription.threads, 2);
        assert!(settings.translation.enabled);
        assert_eq!(settings.translation.endpoint, "http://127.0.0.1:8000/v1");
        assert_eq!(settings.translation.model, "Hy-MT2-1.8B");
        assert_eq!(settings.translation.target_language, "Chinese");
        assert_eq!(settings.meeting_notes.endpoint, "http://127.0.0.1:8000/v1");
        assert!(settings.meeting_notes.model.is_empty());
        assert_eq!(settings.meeting_notes.max_input_characters, 48_000);
        assert_eq!(settings.meeting_notes.request_timeout_seconds, 180);
        settings.validate().unwrap();
    }

    #[test]
    fn rejects_unsafe_gain_and_thread_values() {
        let mut settings = AppSettings::default();
        settings.audio.system_gain = f32::NAN;
        assert!(settings.validate().is_err());

        settings.audio.system_gain = 1.0;
        settings.transcription.threads = 0;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn persists_and_reloads_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let store = SettingsStore::open(path.clone()).unwrap();
        let mut incoming = without_secrets(&store.snapshot());
        incoming.audio.segment_minutes = 30;
        incoming.audio.source = AudioSourceMode::System;
        incoming.meeting_notes.model = "  notes-model  ".to_owned();
        incoming.translation.endpoint = " http://127.0.0.1:9000/v1 ".to_owned();

        let saved = store
            .save(incoming, SettingsSecretUpdates::default())
            .unwrap();
        let reopened = SettingsStore::open(path).unwrap();

        assert_eq!(saved.audio.segment_minutes, 30);
        assert_eq!(saved.meeting_notes.provider.model, "notes-model");
        assert_eq!(
            saved.translation.provider.endpoint,
            "http://127.0.0.1:9000/v1"
        );
        assert_eq!(reopened.view(), saved);
    }

    #[test]
    fn adds_default_vad_settings_to_older_configuration_files() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        value
            .get_mut("transcription")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("vad");

        let settings: AppSettings = serde_json::from_value(value).unwrap();

        assert_eq!(settings.transcription.vad, VadConfig::default());
        settings.validate().unwrap();
    }

    #[test]
    fn replaces_the_removed_parakeet_model_in_existing_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let mut settings = AppSettings::default();
        settings.transcription.model_id = "parakeet-tdt-ctc-0.6b-ja-int8".to_owned();
        save_settings_atomic(&path, &settings).unwrap();

        let store = SettingsStore::open(path).unwrap();

        assert_eq!(
            store.snapshot().transcription.model_id,
            TranscriptionSettings::default().model_id
        );
    }

    #[test]
    fn adds_default_translation_settings_to_older_configuration_files() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        value.as_object_mut().unwrap().remove("translation");

        let settings: AppSettings = serde_json::from_value(value).unwrap();

        assert_eq!(settings.translation, TranslationSettings::default());
        settings.validate().unwrap();
    }

    #[test]
    fn rejects_invalid_translation_settings() {
        let mut settings = AppSettings::default();
        settings.translation.endpoint = "localhost:8000/v1".to_owned();
        assert!(settings.validate().is_err());

        settings.translation.endpoint = "http://127.0.0.1:8000/v1".to_owned();
        settings.translation.model.clear();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn loads_older_settings_without_secrets_or_meeting_notes() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        let root = value.as_object_mut().unwrap();
        root.remove("meetingNotes");
        let translation = root
            .get_mut("translation")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap();
        translation.remove("apiKey");
        translation.remove("apiKeyEndpoint");
        translation.insert("enabled".to_owned(), false.into());
        translation.insert("endpoint".to_owned(), "https://provider.example/v1".into());
        translation.insert("model".to_owned(), "legacy-model".into());
        translation.insert("targetLanguage".to_owned(), "Japanese".into());

        let settings: AppSettings = serde_json::from_value(value).unwrap();

        assert!(!settings.translation.enabled);
        assert_eq!(settings.translation.endpoint, "https://provider.example/v1");
        assert_eq!(settings.translation.model, "legacy-model");
        assert_eq!(settings.translation.target_language, "Japanese");
        assert_eq!(settings.translation.api_key, SecretString::default());
        assert!(settings.translation.api_key_endpoint.is_empty());
        assert_eq!(settings.meeting_notes, MeetingNotesSettings::default());
        settings.validate().unwrap();
        let view = serde_json::to_value(settings.view()).unwrap();
        assert_eq!(view["translation"]["apiKeyConfigured"], false);
        assert_eq!(view["meetingNotes"]["apiKeyConfigured"], false);
        assert_eq!(view["meetingNotes"]["maxInputCharacters"], 48_000);
        assert!(view["translation"].get("apiKey").is_none());
    }

    #[test]
    fn accepts_the_frontend_save_payload() {
        let mut value = serde_json::to_value(AppSettings::default().view()).unwrap();
        for provider in ["meetingNotes", "translation"] {
            value
                .get_mut(provider)
                .and_then(serde_json::Value::as_object_mut)
                .unwrap()
                .remove("apiKeyConfigured");
        }
        let updates: SettingsSecretUpdates = serde_json::from_str(
            r#"{"meetingNotesApiKey":{"action":"clear"},"translationApiKey":{"action":"set","value":"secret"}}"#,
        )
        .unwrap();

        let incoming: AppSettingsWithoutSecrets = serde_json::from_value(value).unwrap();

        assert_eq!(incoming.translation.model, "Hy-MT2-1.8B");
        assert_eq!(incoming.meeting_notes.max_input_characters, 48_000);
        assert!(matches!(updates.meeting_notes_api_key, SecretUpdate::Clear));
        assert!(matches!(
            updates.translation_api_key,
            SecretUpdate::Set { .. }
        ));
        assert!(matches!(
            serde_json::from_str::<SecretUpdate>(r#"{"action":"keep"}"#).unwrap(),
            SecretUpdate::Keep
        ));
    }

    #[test]
    fn settings_view_reports_a_configured_key_without_exposing_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let store = SettingsStore::open(path.clone()).unwrap();

        let view = store
            .save(
                without_secrets(&AppSettings::default()),
                set_key("top-secret-key"),
            )
            .unwrap();

        assert!(view.translation.provider.api_key_configured);
        assert!(!view.meeting_notes.provider.api_key_configured);
        let serialized = serde_json::to_string(&view).unwrap();
        assert!(!serialized.contains("top-secret-key"));
        assert!(serialized.contains("\"apiKeyConfigured\":true"));
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("top-secret-key")
        );
    }

    #[test]
    fn keeps_an_existing_key_bound_to_its_previous_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(directory.path().join("settings.json")).unwrap();
        let credentials = store.credentials();
        let mut incoming = without_secrets(&AppSettings::default());
        incoming.translation.endpoint = "https://first.example/v1".to_owned();
        let view = store
            .save(incoming.clone(), set_key("  first-key  "))
            .unwrap();
        assert!(view.translation.provider.api_key_configured);
        assert_eq!(
            credentials
                .translation_key("https://first.example/v1")
                .unwrap()
                .trimmed(),
            "first-key"
        );

        incoming.translation.endpoint = "https://second.example/v1".to_owned();
        let view = store
            .save(incoming.clone(), SettingsSecretUpdates::default())
            .unwrap();

        assert!(!view.translation.provider.api_key_configured);
        assert!(
            credentials
                .translation_key("https://second.example/v1")
                .is_none()
        );
        assert_eq!(
            store.snapshot().translation.snapshot().auth_mode,
            ProviderAuthMode::None
        );

        let view = store.save(incoming, set_key("second-key")).unwrap();

        assert!(view.translation.provider.api_key_configured);
        assert_eq!(
            credentials
                .translation_key("https://second.example/v1")
                .unwrap()
                .trimmed(),
            "second-key"
        );
        assert!(
            credentials
                .translation_key("https://first.example/v1")
                .is_none()
        );
    }

    #[test]
    fn clearing_a_secret_drops_the_key_and_its_binding() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(directory.path().join("settings.json")).unwrap();
        let incoming = without_secrets(&AppSettings::default());
        store.save(incoming.clone(), set_key("secret")).unwrap();

        let view = store
            .save(
                incoming,
                SettingsSecretUpdates {
                    translation_api_key: SecretUpdate::Clear,
                    ..SettingsSecretUpdates::default()
                },
            )
            .unwrap();

        assert!(!view.translation.provider.api_key_configured);
        let stored = store.snapshot();
        assert_eq!(stored.translation.api_key, SecretString::default());
        assert!(stored.translation.api_key_endpoint.is_empty());
    }

    #[test]
    fn redacts_secrets_in_debug_output_and_snapshots() {
        let mut settings = AppSettings::default();
        settings.translation.api_key = SecretString::new("top-secret-key".to_owned());
        settings.translation.api_key_endpoint =
            normalized_endpoint(&settings.translation.endpoint).unwrap();

        let snapshot = settings.translation.snapshot();

        assert_eq!(snapshot.auth_mode, ProviderAuthMode::Bearer);
        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains("top-secret-key"));
        assert!(serialized.contains("\"authMode\":\"bearer\""));
        assert_eq!(format!("{:?}", settings.translation.api_key), "[REDACTED]");
        assert!(!format!("{settings:?}").contains("top-secret-key"));
        assert!(
            !format!(
                "{:?}",
                SecretUpdate::Set {
                    value: "top-secret-key".to_owned()
                }
            )
            .contains("top-secret-key")
        );
    }

    #[test]
    fn rejects_endpoints_with_userinfo() {
        let mut settings = AppSettings::default();
        settings.translation.endpoint = "http://user:password@example.com/v1".to_owned();
        assert!(settings.validate().is_err());

        let mut settings = AppSettings::default();
        settings.meeting_notes.endpoint = "http://user@example.com/v1".to_owned();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn refuses_to_bind_a_key_to_a_remote_plaintext_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(directory.path().join("settings.json")).unwrap();
        let mut incoming = without_secrets(&AppSettings::default());
        incoming.translation.endpoint = "http://provider.example/v1".to_owned();

        store
            .save(incoming.clone(), SettingsSecretUpdates::default())
            .unwrap();
        assert!(store.save(incoming.clone(), set_key("secret")).is_err());

        incoming.translation.endpoint = "https://provider.example/v1".to_owned();
        store.save(incoming.clone(), set_key("secret")).unwrap();
        incoming.translation.endpoint = "http://provider.example/v1".to_owned();
        let view = store
            .save(incoming, SettingsSecretUpdates::default())
            .unwrap();

        assert!(!view.translation.provider.api_key_configured);
    }

    #[test]
    fn accepts_an_empty_meeting_notes_model_and_range_checks_advanced_values() {
        let mut settings = AppSettings::default();
        assert!(settings.meeting_notes.model.is_empty());
        settings.validate().unwrap();

        for characters in [7_999, 200_001] {
            settings.meeting_notes.max_input_characters = characters;
            assert!(settings.validate().is_err());
        }
        settings.meeting_notes.max_input_characters = 8_000;
        for seconds in [29, 901] {
            settings.meeting_notes.request_timeout_seconds = seconds;
            assert!(settings.validate().is_err());
        }
        settings.meeting_notes.request_timeout_seconds = 900;
        settings.validate().unwrap();
    }

    #[test]
    fn fingerprints_only_the_fields_a_connection_test_exercises() {
        let mut settings = AppSettings::default();
        let baseline = settings.translation.fingerprint();

        settings.translation.target_language = "Japanese".to_owned();
        settings.translation.enabled = false;
        settings.meeting_notes.max_input_characters = 60_000;
        assert_eq!(settings.translation.fingerprint(), baseline);

        settings.translation.model = "other-model".to_owned();
        assert_ne!(settings.translation.fingerprint(), baseline);

        let mut settings = AppSettings::default();
        settings.translation.endpoint = "https://provider.example/v1".to_owned();
        assert_ne!(settings.translation.fingerprint(), baseline);

        let mut settings = AppSettings::default();
        settings.translation.api_key = SecretString::new("top-secret-key".to_owned());
        settings.translation.api_key_endpoint =
            normalized_endpoint(&settings.translation.endpoint).unwrap();
        let with_key = settings.translation.fingerprint();
        assert_ne!(with_key, baseline);
        assert!(!with_key.contains("top-secret-key"));
        settings.translation.api_key = SecretString::new("rotated-key".to_owned());
        assert_ne!(settings.translation.fingerprint(), with_key);
    }

    #[test]
    fn treats_a_trailing_slash_and_surrounding_space_as_the_same_endpoint() {
        assert_eq!(
            provider_fingerprint(" http://127.0.0.1:8000/v1/ ", " model ", None),
            provider_fingerprint("http://127.0.0.1:8000/v1", "model", None)
        );
        assert_ne!(
            provider_fingerprint("not a url", "model", None),
            provider_fingerprint("http://127.0.0.1:8000/v1", "model", None)
        );
    }

    #[test]
    fn reports_older_configuration_files_as_unverified() {
        let mut value = serde_json::to_value(AppSettings::default()).unwrap();
        for provider in ["meetingNotes", "translation"] {
            value
                .get_mut(provider)
                .and_then(serde_json::Value::as_object_mut)
                .unwrap()
                .remove("verifiedFingerprint");
        }

        let settings: AppSettings = serde_json::from_value(value).unwrap();

        assert!(settings.translation.verified_fingerprint.is_empty());
        assert!(settings.meeting_notes.verified_fingerprint.is_empty());
        let view = settings.view();
        assert!(!view.translation.provider.verified);
        assert!(!view.meeting_notes.provider.verified);
    }

    #[test]
    fn saving_preserves_a_verified_provider_until_its_endpoint_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let store = SettingsStore::open(path.clone()).unwrap();
        let incoming = without_secrets(&AppSettings::default());

        let view = store
            .save_verified(
                store
                    .merge_for_test(incoming.clone(), SettingsSecretUpdates::default())
                    .unwrap(),
                ProviderKind::Translation,
            )
            .unwrap();
        assert!(view.translation.provider.verified);
        assert!(!view.meeting_notes.provider.verified);
        assert_eq!(SettingsStore::open(path).unwrap().view(), view);

        let mut moved = incoming.clone();
        moved.translation.endpoint = "https://provider.example/v1".to_owned();
        let view = store.save(moved, SettingsSecretUpdates::default()).unwrap();
        assert!(!view.translation.provider.verified);

        let view = store
            .save(incoming, SettingsSecretUpdates::default())
            .unwrap();
        assert!(view.translation.provider.verified);
        assert!(!store.snapshot().translation.verified_fingerprint.is_empty());
    }

    #[test]
    fn storing_a_key_invalidates_a_verified_provider() {
        let directory = tempfile::tempdir().unwrap();
        let store = SettingsStore::open(directory.path().join("settings.json")).unwrap();
        let incoming = without_secrets(&AppSettings::default());
        store
            .save_verified(
                store
                    .merge_for_test(incoming.clone(), SettingsSecretUpdates::default())
                    .unwrap(),
                ProviderKind::Translation,
            )
            .unwrap();

        let view = store.save(incoming, set_key("late-key")).unwrap();

        assert!(view.translation.provider.api_key_configured);
        assert!(!view.translation.provider.verified);
    }

    #[cfg(unix)]
    #[test]
    fn restricts_the_settings_file_to_the_current_user() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let store = SettingsStore::open(path.clone()).unwrap();

        store
            .save(
                without_secrets(&AppSettings::default()),
                set_key("top-secret-key"),
            )
            .unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
