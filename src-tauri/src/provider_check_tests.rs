use serde_json::Value;

use crate::{
    openai_compatible::{
        ChatError, ReqwestChatClient,
        test_support::{ScriptedChatClient, json_response, serve_once},
    },
    settings::SecretUpdate,
};

use super::*;

const COMPLETION_BODY: &str = r#"{"choices":[{"message":{"content":"OK"}}]}"#;

fn store() -> (tempfile::TempDir, SettingsStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = SettingsStore::open(directory.path().join("settings.json")).unwrap();
    (directory, store)
}

/// Mirrors the frontend payload: the view minus every field the private
/// settings own.
fn without_secrets(settings: &AppSettings) -> AppSettingsWithoutSecrets {
    let mut value = serde_json::to_value(settings.view()).unwrap();
    for provider in ["meetingNotes", "translation"] {
        let object = value
            .get_mut(provider)
            .and_then(Value::as_object_mut)
            .unwrap();
        object.remove("apiKeyConfigured");
        object.remove("verified");
    }
    serde_json::from_value(value).unwrap()
}

fn request(
    settings: &AppSettings,
    provider: ProviderKind,
    secrets: SettingsSecretUpdates,
) -> TestProviderRequest {
    TestProviderRequest {
        provider,
        secrets,
        settings: without_secrets(settings),
    }
}

#[test]
fn verifies_and_persists_the_tested_provider() {
    let (_directory, store) = store();
    let chat = ScriptedChatClient::new(vec![Ok("OK".to_owned())]);
    let mut settings = AppSettings::default();
    settings.meeting_notes.model = "notes-model".to_owned();

    let view = test_provider(
        &store,
        &chat,
        request(
            &settings,
            ProviderKind::MeetingNotes,
            SettingsSecretUpdates::default(),
        ),
    )
    .unwrap();

    assert!(view.meeting_notes.provider.verified);
    assert!(!view.translation.provider.verified);
    assert_eq!(store.view(), view);
    let recorded = chat.requests();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].model, "notes-model");
    assert_eq!(
        recorded[0].messages,
        vec![("user".to_owned(), PROBE_MESSAGE.to_owned())]
    );
    assert_eq!(recorded[0].timeout, Duration::from_secs(180));
    assert!(recorded[0].api_key.is_none());
}

#[test]
fn keeps_the_stored_settings_when_the_provider_rejects_the_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let store = SettingsStore::open(path.clone()).unwrap();
    let chat = ScriptedChatClient::new(vec![Err(ChatError::Unauthorized)]);
    let mut settings = AppSettings::default();
    settings.translation.model = "unverified-model".to_owned();

    let error = test_provider(
        &store,
        &chat,
        request(
            &settings,
            ProviderKind::Translation,
            SettingsSecretUpdates::default(),
        ),
    )
    .unwrap_err();

    assert_eq!(error.code(), MeetingNotesErrorCode::ProviderUnauthorized);
    assert_eq!(
        error.payload().message_key,
        "meetingNotesErrorProviderUnauthorized"
    );
    let reopened = SettingsStore::open(path).unwrap();
    assert_eq!(reopened.snapshot().translation.model, "Hy-MT2-1.8B");
    assert!(!reopened.view().translation.provider.verified);
    assert!(!store.view().translation.provider.verified);
}

#[test]
fn refuses_an_unconfigured_meeting_notes_model_without_calling_the_provider() {
    let (_directory, store) = store();
    let chat = ScriptedChatClient::new(vec![Ok("OK".to_owned())]);

    let error = test_provider(
        &store,
        &chat,
        request(
            &AppSettings::default(),
            ProviderKind::MeetingNotes,
            SettingsSecretUpdates::default(),
        ),
    )
    .unwrap_err();

    assert_eq!(
        error.code(),
        MeetingNotesErrorCode::MeetingNotesNotConfigured
    );
    assert!(chat.requests().is_empty());
}

#[test]
fn reports_an_unusable_endpoint_as_a_structured_error() {
    let (_directory, store) = store();
    let chat = ScriptedChatClient::new(vec![Ok("OK".to_owned())]);
    let mut settings = AppSettings::default();
    settings.translation.endpoint = "localhost:8000/v1".to_owned();

    let error = test_provider(
        &store,
        &chat,
        request(
            &settings,
            ProviderKind::Translation,
            SettingsSecretUpdates::default(),
        ),
    )
    .unwrap_err();

    assert_eq!(error.code(), MeetingNotesErrorCode::InvalidEndpoint);
    assert!(chat.requests().is_empty());
}

#[test]
fn reports_a_key_bound_to_a_plaintext_remote_endpoint_as_insecure() {
    let (_directory, store) = store();
    let chat = ScriptedChatClient::new(vec![Ok("OK".to_owned())]);
    let mut settings = AppSettings::default();
    settings.translation.endpoint = "http://provider.example/v1".to_owned();

    let error = test_provider(
        &store,
        &chat,
        request(
            &settings,
            ProviderKind::Translation,
            SettingsSecretUpdates {
                translation_api_key: SecretUpdate::Set {
                    value: "secret".to_owned(),
                },
                ..SettingsSecretUpdates::default()
            },
        ),
    )
    .unwrap_err();

    assert_eq!(error.code(), MeetingNotesErrorCode::InsecureEndpoint);
    assert!(chat.requests().is_empty());
}

#[test]
fn sends_a_freshly_typed_key_and_stores_it_only_after_the_provider_answered() {
    let (_directory, store) = store();
    let server = serve_once(json_response("200 OK", COMPLETION_BODY));
    let chat = ReqwestChatClient::new().unwrap();
    let mut settings = AppSettings::default();
    settings.translation.endpoint = format!("http://{}/v1", server.address);

    let view = test_provider(
        &store,
        &chat,
        request(
            &settings,
            ProviderKind::Translation,
            SettingsSecretUpdates {
                translation_api_key: SecretUpdate::Set {
                    value: "fresh-key".to_owned(),
                },
                ..SettingsSecretUpdates::default()
            },
        ),
    )
    .unwrap();

    let sent = server.handle.join().unwrap();
    assert!(sent.contains("authorization: Bearer fresh-key\r\n"));
    assert!(view.translation.provider.api_key_configured);
    assert!(view.translation.provider.verified);
    assert_eq!(
        store
            .credentials()
            .translation_key(&settings.translation.endpoint)
            .unwrap()
            .trimmed(),
        "fresh-key"
    );
}
