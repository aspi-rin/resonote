#[cfg(not(any(target_os = "windows", target_os = "macos")))]
compile_error!("Resonote currently supports only Windows and macOS");

use serde::Serialize;
use std::sync::Arc;

use tauri::{Emitter, Manager};

pub mod asr;
pub mod audio;
pub mod capture;
pub mod desktop;
pub mod history;
pub mod meeting_notes;
pub mod meeting_notes_document;
pub mod meeting_notes_pipeline;
mod model_package;
pub mod models;
pub mod openai_compatible;
pub mod recording;
pub mod session_catalog;
pub mod session_lifecycle;
pub mod settings;
pub mod storage;
pub mod transcription;
pub mod translation;
pub mod vad;

use history::{HistoryEntry, HistoryService, HistoryWorkers};
use meeting_notes::{
    CancelRequest, GenerateRequest, MeetingNotesService, MeetingNotesStatusEvent, RetryRequest,
};
use meeting_notes_document::{
    GlobalContextContent, GlobalContextDocument, GlobalContextStore, MeetingContextContent,
    MeetingNotesError, MeetingNotesErrorPayload, OutputLanguage, SessionAnalysisView,
    VersionedMeetingContext,
};
use models::{ModelCatalogEntry, ModelDownloadStatus, ModelManager};
use recording::{RecordingService, RecordingStatus};
use session_catalog::SessionCatalog;
use session_lifecycle::SessionLifecycle;
use settings::{
    AppSettingsView, AppSettingsWithoutSecrets, AudioSourceMode, SettingsSecretUpdates,
    SettingsStore,
};
use transcription::{
    TranscriptDocument, TranscriptSegmentUpdate, TranscriptionService, TranscriptionStatus,
};
use translation::{TranslationDocument, TranslationSegmentUpdate, TranslationService};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    name: &'static str,
    version: &'static str,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: "Resonote",
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[tauri::command]
fn list_recording_history(
    state: tauri::State<'_, HistoryService>,
    limit: usize,
) -> Result<Vec<HistoryEntry>, String> {
    state.list(limit).map_err(|error| error.to_string())
}

#[tauri::command]
fn open_recording_directory(
    state: tauri::State<'_, HistoryService>,
    session_id: String,
) -> Result<(), String> {
    state
        .open_session_directory(&session_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn delete_recording(
    state: tauri::State<'_, HistoryService>,
    session_id: String,
) -> Result<(), String> {
    state
        .delete_session(&session_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, SettingsStore>) -> AppSettingsView {
    state.view()
}

#[tauri::command]
fn save_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, SettingsStore>,
    tray: tauri::State<'_, Arc<desktop::TrayController>>,
    settings: AppSettingsWithoutSecrets,
    secrets: SettingsSecretUpdates,
) -> Result<AppSettingsView, String> {
    desktop::sync_autostart(&app, settings.desktop.launch_at_login)?;
    let saved = state
        .save(settings, secrets)
        .map_err(|error| error.to_string())?;
    tray.update_language(saved.desktop.language);
    Ok(saved)
}

#[tauri::command]
fn get_global_context(state: tauri::State<'_, Arc<GlobalContextStore>>) -> GlobalContextDocument {
    state.document()
}

#[tauri::command]
fn save_global_context(
    state: tauri::State<'_, Arc<GlobalContextStore>>,
    expected_global_context_revision: u64,
    content: GlobalContextContent,
) -> Result<GlobalContextDocument, MeetingNotesErrorPayload> {
    state
        .save(expected_global_context_revision, &content)
        .map_err(report_meeting_notes_error)
}

#[tauri::command]
fn save_session_context(
    state: tauri::State<'_, Arc<SessionCatalog>>,
    session_id: String,
    expected_meeting_context_revision: u64,
    content: MeetingContextContent,
) -> Result<VersionedMeetingContext, MeetingNotesErrorPayload> {
    let session_dir = state
        .resolve(&session_id)
        .map_err(|error| report_meeting_notes_error(error.into()))?;
    meeting_notes_document::save_meeting_context(
        &session_dir,
        &session_id,
        expected_meeting_context_revision,
        &content,
    )
    .map_err(report_meeting_notes_error)
}

/// The payload is code-only, so the cause is logged here instead of returned.
fn report_meeting_notes_error(error: MeetingNotesError) -> MeetingNotesErrorPayload {
    tracing::warn!(?error, "meeting notes command failed");
    error.payload()
}

#[tauri::command]
fn get_session_analysis(
    state: tauri::State<'_, Arc<MeetingNotesService>>,
    session_id: String,
    output_language: OutputLanguage,
) -> Result<SessionAnalysisView, MeetingNotesErrorPayload> {
    state
        .load(&session_id, output_language)
        .map_err(report_meeting_notes_error)
}

#[tauri::command]
async fn generate_session_analysis(
    app: tauri::AppHandle,
    request: GenerateRequest,
) -> Result<SessionAnalysisView, MeetingNotesErrorPayload> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Arc<MeetingNotesService>>().generate(request)
    })
    .await
    .map_err(|error| report_meeting_notes_error(MeetingNotesError::io().with_source(error)))?
    .map_err(report_meeting_notes_error)
}

#[tauri::command]
async fn retry_session_analysis(
    app: tauri::AppHandle,
    request: RetryRequest,
) -> Result<SessionAnalysisView, MeetingNotesErrorPayload> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Arc<MeetingNotesService>>().retry(request)
    })
    .await
    .map_err(|error| report_meeting_notes_error(MeetingNotesError::io().with_source(error)))?
    .map_err(report_meeting_notes_error)
}

#[tauri::command]
async fn cancel_session_analysis(
    app: tauri::AppHandle,
    request: CancelRequest,
) -> Result<SessionAnalysisView, MeetingNotesErrorPayload> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Arc<MeetingNotesService>>().cancel(request)
    })
    .await
    .map_err(|error| report_meeting_notes_error(MeetingNotesError::io().with_source(error)))?
    .map_err(report_meeting_notes_error)
}

#[tauri::command]
fn get_recording_status(state: tauri::State<'_, RecordingService>) -> RecordingStatus {
    state.status()
}

#[tauri::command]
fn list_transcription_models() -> Vec<ModelCatalogEntry> {
    models::model_catalog()
}

#[tauri::command]
fn start_recording(
    recording: tauri::State<'_, RecordingService>,
    settings: tauri::State<'_, SettingsStore>,
) -> Result<RecordingStatus, String> {
    let settings = settings.snapshot();
    recording
        .start(settings.audio, settings.transcription, settings.translation)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn stop_recording(app: tauri::AppHandle) -> Result<RecordingStatus, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<RecordingService>().stop())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn set_recording_source(
    app: tauri::AppHandle,
    source: AudioSourceMode,
) -> Result<RecordingStatus, String> {
    tauri::async_runtime::spawn_blocking(move || app.state::<RecordingService>().set_source(source))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn set_recording_languages(
    app: tauri::AppHandle,
    recognition_language: String,
    translation_enabled: bool,
    translation_target_language: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<RecordingService>().set_languages(
            recognition_language,
            translation_enabled,
            translation_target_language,
        )
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_model_status(
    state: tauri::State<'_, Arc<ModelManager>>,
    model_id: String,
) -> Result<ModelDownloadStatus, String> {
    state.status(&model_id).map_err(|error| error.to_string())
}

#[tauri::command]
async fn install_model(
    state: tauri::State<'_, Arc<ModelManager>>,
    model_id: String,
) -> Result<ModelDownloadStatus, String> {
    let manager = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.install(&model_id))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn cancel_model_install(state: tauri::State<'_, Arc<ModelManager>>) -> Result<(), String> {
    let manager = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.cancel_install_and_wait())
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_transcription_status(
    state: tauri::State<'_, Arc<TranscriptionService>>,
) -> TranscriptionStatus {
    state.status()
}

#[tauri::command]
fn get_session_transcript(
    state: tauri::State<'_, HistoryService>,
    session_id: String,
) -> Result<Option<TranscriptDocument>, String> {
    state
        .session_transcript(&session_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_session_translation(
    state: tauri::State<'_, HistoryService>,
    session_id: String,
) -> Result<Option<TranslationDocument>, String> {
    state
        .session_translation(&session_id)
        .map_err(|error| error.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "resonote=info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            desktop::show_main_window(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(desktop::handle_window_event)
        .setup(|app| {
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let settings_store = SettingsStore::open(settings_path)?;
            let startup_settings = settings_store.snapshot();
            if let Err(error) =
                desktop::sync_autostart(app.handle(), startup_settings.desktop.launch_at_login)
            {
                tracing::warn!(%error, "failed to synchronize launch-at-login state");
            }
            let tray = desktop::TrayController::create(app, startup_settings.desktop.language)?;
            let credentials = settings_store.credentials();
            app.manage(settings_store);
            let config_dir = app.path().app_config_dir()?;
            let global_context = Arc::new(GlobalContextStore::open(
                config_dir.join("global-context.json"),
            )?);
            let catalog = Arc::new(SessionCatalog::open(
                config_dir.join(session_catalog::DOCUMENT_NAME),
            )?);
            let recordings_root = app.path().app_local_data_dir()?.join("recordings");
            for root in std::iter::once(&recordings_root)
                .chain(startup_settings.audio.output_directory.as_ref())
            {
                if let Err(error) = catalog.register_root(root) {
                    tracing::warn!(?error, "failed to register a recording root");
                }
            }
            // Half deleted sessions go first: no recovery scan below may revive
            // one of them or hand it back to a worker queue.
            let lifecycle = SessionLifecycle::new();
            match lifecycle.recover_roots(&catalog.roots()) {
                Ok(count) if count > 0 => {
                    tracing::info!(deleted_sessions = count, "finished interrupted deletions");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(?error, "failed to finish interrupted deletions"),
            }
            for manifest in storage::recover_interrupted_sessions(&recordings_root)? {
                tracing::warn!(
                    session_id = manifest.session_id,
                    "recovered an interrupted recording session"
                );
            }
            let model_event_app = app.handle().clone();
            let model_observer = Arc::new(move |status: ModelDownloadStatus| {
                if let Err(error) = model_event_app.emit("model-download-status", status) {
                    tracing::debug!(?error, "model status event had no listener");
                }
            });
            let model_manager = Arc::new(ModelManager::with_observer(
                app.path().app_local_data_dir()?.join("models"),
                model_observer,
            )?);
            let translation_event_app = app.handle().clone();
            let translation_observer = Arc::new(move |update: TranslationSegmentUpdate| {
                if let Err(error) = translation_event_app.emit("translation-segment", update) {
                    tracing::debug!(?error, "translation segment event had no listener");
                }
            });
            let translation = Arc::new(TranslationService::with_credentials(
                translation_observer,
                credentials.clone(),
                lifecycle.clone(),
            )?);
            let recovered_translations = translation.recover_root(&recordings_root)?;
            if recovered_translations > 0 {
                tracing::info!(
                    recovered_translations,
                    "recovered pending translation sessions"
                );
            }
            let transcription_event_app = app.handle().clone();
            let transcription_observer = Arc::new(move |status: TranscriptionStatus| {
                if let Err(error) = transcription_event_app.emit("transcription-status", status) {
                    tracing::debug!(?error, "transcription status event had no listener");
                }
            });
            let segment_event_app = app.handle().clone();
            let segment_observer = Arc::new(move |update: TranscriptSegmentUpdate| {
                if let Err(error) = segment_event_app.emit("transcript-segment", update) {
                    tracing::debug!(?error, "transcript segment event had no listener");
                }
            });
            let completion_translation = translation.clone();
            let completion_observer: transcription::CompletionObserver =
                Arc::new(move |session_dir, session_id, settings, segment| {
                    if let Err(error) = completion_translation.enqueue(
                        &session_dir,
                        &session_id,
                        segment.id,
                        &segment.text,
                        &settings,
                    ) {
                        tracing::warn!(?error, "failed to queue transcript translation");
                    }
                });
            let transcription = Arc::new(TranscriptionService::with_completion_observer(
                model_manager.clone(),
                transcription_observer,
                segment_observer,
                completion_observer,
                lifecycle.clone(),
            ));
            let recovered_transcripts = transcription.recover_root(&recordings_root)?;
            if recovered_transcripts > 0 {
                tracing::info!(
                    recovered_transcripts,
                    "recovered pending transcription sessions"
                );
            }
            let recording_event_app = app.handle().clone();
            let recording_tray = tray.clone();
            let recording_observer = Arc::new(move |status: RecordingStatus| {
                recording_tray.update_recording(&status);
                if let Err(error) = recording_event_app.emit("recording-status", status) {
                    tracing::debug!(?error, "recording status event had no listener");
                }
            });
            app.manage(RecordingService::with_services(
                recordings_root.clone(),
                recording_observer,
                Some(transcription.clone()),
                Some(model_manager.clone()),
                Some(catalog.clone()),
            ));
            let meeting_notes_event_app = app.handle().clone();
            let meeting_notes_observer = Arc::new(move |event: MeetingNotesStatusEvent| {
                if let Err(error) = meeting_notes_event_app.emit("meeting-notes-status", event) {
                    tracing::debug!(?error, "meeting notes status event had no listener");
                }
            });
            let meeting_notes = Arc::new(MeetingNotesService::new(
                catalog.clone(),
                global_context.clone(),
                lifecycle.clone(),
                credentials,
                meeting_notes_observer,
            )?);
            let recovered_analyses = meeting_notes.recover_catalog()?;
            if recovered_analyses > 0 {
                tracing::info!(recovered_analyses, "recovered pending meeting notes runs");
            }
            app.manage(HistoryService::with_workers(
                recordings_root,
                catalog.clone(),
                lifecycle,
                HistoryWorkers {
                    meeting_notes: meeting_notes.clone(),
                    transcription: transcription.clone(),
                    translation: translation.clone(),
                },
            ));
            app.manage(catalog);
            app.manage(global_context);
            app.manage(meeting_notes);
            app.manage(tray);
            app.manage(transcription);
            app.manage(translation);
            app.manage(model_manager);
            if startup_settings.desktop.start_recording_on_launch {
                let recording = app.state::<RecordingService>();
                if let Err(error) = recording.start(
                    startup_settings.audio,
                    startup_settings.transcription,
                    startup_settings.translation,
                ) {
                    tracing::error!(?error, "failed to start launch recording");
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            cancel_model_install,
            cancel_session_analysis,
            delete_recording,
            generate_session_analysis,
            get_global_context,
            get_settings,
            get_model_status,
            get_recording_status,
            get_session_analysis,
            get_session_transcript,
            get_session_translation,
            get_transcription_status,
            install_model,
            list_recording_history,
            list_transcription_models,
            open_recording_directory,
            retry_session_analysis,
            save_global_context,
            save_session_context,
            save_settings,
            set_recording_languages,
            set_recording_source,
            start_recording,
            stop_recording
        ])
        .build(tauri::generate_context!())
        .expect("failed to run Resonote")
        .run(desktop::handle_run_event);
}
