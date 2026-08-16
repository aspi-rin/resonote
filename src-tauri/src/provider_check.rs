use std::time::Duration;

use serde::Deserialize;

use crate::{
    meeting_notes_document::{MeetingNotesError, MeetingNotesErrorCode, chat_error_code},
    openai_compatible::{
        ChatCompletionPort, ChatError, ChatMessage, ChatRequest, ChatRole, ModelListPort,
        ModelListRequest,
    },
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

/// Never borrows the request: both calls run against the merged configuration,
/// so the settings that were verified are exactly the settings that get saved.
struct ProviderCredentials {
    api_key: Option<SecretString>,
    endpoint: String,
}

struct ProviderProbe {
    credentials: ProviderCredentials,
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
        api_key: probe.credentials.api_key.as_ref(),
        endpoint: &probe.credentials.endpoint,
        messages: vec![ChatMessage {
            content: PROBE_MESSAGE,
            role: ChatRole::User,
        }],
        model: &probe.model,
        timeout: probe.timeout,
    })
    .map_err(chat_failure)?;
    store
        .save_verified(merged, provider)
        .map_err(settings_error)
}

/// Discovery, not verification: nothing is persisted and no model is required,
/// because the list is exactly what a user without a model needs.
pub fn list_provider_models(
    store: &SettingsStore,
    lister: &dyn ModelListPort,
    request: TestProviderRequest,
) -> Result<Vec<String>, MeetingNotesError> {
    let provider = request.provider;
    let merged = store
        .merge_for_test(request.settings, request.secrets)
        .map_err(settings_error)?;
    let credentials = credentials_for(&merged, provider);
    lister
        .list_models(ModelListRequest {
            api_key: credentials.api_key.as_ref(),
            endpoint: &credentials.endpoint,
        })
        .map_err(chat_failure)
}

fn probe_for(
    settings: &AppSettings,
    provider: ProviderKind,
) -> Result<ProviderProbe, MeetingNotesError> {
    let credentials = credentials_for(settings, provider);
    match provider {
        ProviderKind::MeetingNotes => {
            let notes = &settings.meeting_notes;
            if notes.model.trim().is_empty() {
                return Err(MeetingNotesError::new(
                    MeetingNotesErrorCode::MeetingNotesNotConfigured,
                ));
            }
            Ok(ProviderProbe {
                credentials,
                model: notes.model.trim().to_owned(),
                timeout: Duration::from_secs(u64::from(notes.request_timeout_seconds)),
            })
        }
        ProviderKind::Translation => Ok(ProviderProbe {
            credentials,
            model: settings.translation.model.trim().to_owned(),
            timeout: TRANSLATION_TIMEOUT,
        }),
    }
}

fn credentials_for(settings: &AppSettings, provider: ProviderKind) -> ProviderCredentials {
    match provider {
        ProviderKind::MeetingNotes => ProviderCredentials {
            api_key: settings.meeting_notes.bound_key().cloned(),
            endpoint: settings.meeting_notes.endpoint.clone(),
        },
        ProviderKind::Translation => ProviderCredentials {
            api_key: settings.translation.bound_key().cloned(),
            endpoint: settings.translation.endpoint.clone(),
        },
    }
}

fn chat_failure(error: ChatError) -> MeetingNotesError {
    MeetingNotesError::new(chat_error_code(&error)).with_source(error)
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
