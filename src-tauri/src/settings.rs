use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::RwLock,
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::vad::VadConfig;

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
    pub microphone_device_id: Option<String>,
    pub microphone_gain: f32,
    pub output_directory: Option<PathBuf>,
    pub segment_minutes: u32,
    pub source: AudioSourceMode,
    pub system_device_id: Option<String>,
    pub system_gain: f32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            format: AudioFormat::Flac,
            microphone_device_id: None,
            microphone_gain: 1.0,
            output_directory: None,
            segment_minutes: 60,
            source: AudioSourceMode::Mixed,
            system_device_id: None,
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

impl Default for TranscriptionSettings {
    fn default() -> Self {
        Self {
            language: "auto".to_owned(),
            model_id: "qwen3-asr-0.6b-int8".to_owned(),
            threads: default_transcription_threads(),
            unload_after_idle_minutes: 10,
            vad: VadConfig::default(),
        }
    }
}

const fn default_transcription_threads() -> u16 {
    if cfg!(target_os = "macos") { 6 } else { 2 }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    pub audio: AudioSettings,
    pub desktop: DesktopSettings,
    pub transcription: TranscriptionSettings,
}

impl AppSettings {
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
        if self.transcription.model_id.trim().is_empty() {
            return Err(SettingsError::Validation(
                "transcription modelId cannot be empty".to_owned(),
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
    current: RwLock<AppSettings>,
    path: PathBuf,
}

impl SettingsStore {
    pub fn open(path: PathBuf) -> Result<Self, SettingsError> {
        let current = load_settings(&path)?;
        current.validate()?;
        Ok(Self {
            current: RwLock::new(current),
            path,
        })
    }

    pub fn snapshot(&self) -> AppSettings {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn save(&self, settings: AppSettings) -> Result<AppSettings, SettingsError> {
        settings.validate()?;
        save_settings_atomic(&self.path, &settings)?;
        *self
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = settings.clone();
        Ok(settings)
    }
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_mixed_audio_with_transcription() {
        let settings = AppSettings::default();

        assert_eq!(settings.audio.source, AudioSourceMode::Mixed);
        #[cfg(target_os = "macos")]
        assert_eq!(settings.transcription.threads, 6);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(settings.transcription.threads, 2);
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
        let mut settings = store.snapshot();
        settings.audio.segment_minutes = 30;
        settings.audio.source = AudioSourceMode::System;

        store.save(settings.clone()).unwrap();
        let reopened = SettingsStore::open(path).unwrap();

        assert_eq!(reopened.snapshot(), settings);
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
}
