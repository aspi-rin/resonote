use std::time::Duration;

use serde::Deserialize;

use crate::{
    meeting_notes_document::{MeetingNotesError, MeetingNotesErrorCode, chat_error_code},
    openai_compatible::{ChatCompletionPort, ChatMessage, ChatRequest, ChatRole},
    settings::{
        AppSettings, AppSettingsView, AppSettingsWithoutSecrets, ProviderKind, SecretString,
        SettingsError, SettingsSecretUpdates, SettingsStore,
    },
};

/// One real completion, short enough to stay cheap on any provider.
const PROBE_MESSAGE: &str = "Reply with the word OK.";
const TRANSLATION_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestProviderRequest {
    pub provider: ProviderKind,
    pub secrets: SettingsSecretUpdates,
    pub settings: AppSettingsWithoutSecrets,
}

/// Never borrows the request: the probe runs against the merged configuration,
/// so the settings that were verified are exactly the settings that get saved.
struct ProviderProbe {
    api_key: Option<SecretString>,
    endpoint: String,
    model: String,
    timeout: Duration,
}

pub fn test_provider(
    store: &SettingsStore,
    chat: &dyn ChatCompletionPort,
    request: TestProviderRequest,
) -> Result<AppSettingsView, MeetingNotesError> {
    let provider = request.provider;
    let merged = store
        .merge_for_test(request.settings, request.secrets)
        .map_err(settings_error)?;
    let probe = probe_for(&merged, provider)?;
    chat.complete(ChatRequest {
        api_key: probe.api_key.as_ref(),
        endpoint: &probe.endpoint,
        messages: vec![ChatMessage {
            content: PROBE_MESSAGE,
            role: ChatRole::User,
        }],
        model: &probe.model,
        timeout: probe.timeout,
    })
    .map_err(|error| MeetingNotesError::new(chat_error_code(&error)).with_source(error))?;
    store
        .save_verified(merged, provider)
        .map_err(settings_error)
}

fn probe_for(
    settings: &AppSettings,
    provider: ProviderKind,
) -> Result<ProviderProbe, MeetingNotesError> {
    match provider {
        ProviderKind::MeetingNotes => {
            let notes = &settings.meeting_notes;
            if notes.model.trim().is_empty() {
                return Err(MeetingNotesError::new(
                    MeetingNotesErrorCode::MeetingNotesNotConfigured,
                ));
            }
            Ok(ProviderProbe {
                api_key: notes.bound_key().cloned(),
                endpoint: notes.endpoint.clone(),
                model: notes.model.trim().to_owned(),
                timeout: Duration::from_secs(u64::from(notes.request_timeout_seconds)),
            })
        }
        ProviderKind::Translation => Ok(ProviderProbe {
            api_key: settings.translation.bound_key().cloned(),
            endpoint: settings.translation.endpoint.clone(),
            model: settings.translation.model.trim().to_owned(),
            timeout: TRANSLATION_TIMEOUT,
        }),
    }
}

fn settings_error(error: SettingsError) -> MeetingNotesError {
    let code = match error {
        SettingsError::InsecureEndpoint(_) => MeetingNotesErrorCode::InsecureEndpoint,
        SettingsError::InvalidEndpoint(_) | SettingsError::Validation(_) => {
            MeetingNotesErrorCode::InvalidEndpoint
        }
        _ => MeetingNotesErrorCode::IoError,
    };
    MeetingNotesError::new(code).with_source(error)
}

#[cfg(test)]
#[path = "provider_check_tests.rs"]
mod tests;
