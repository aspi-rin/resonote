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
mod model_package;
pub mod models;
pub mod recording;
pub mod settings;
pub mod storage;
pub mod transcription;
pub mod vad;

use capture::AudioDeviceDescriptor;
use history::{HistoryEntry, HistoryService};
use models::{ModelDownloadStatus, ModelManager};
use recording::{RecordingService, RecordingStatus};
use settings::{AppSettings, SettingsStore};
use transcription::{TranscriptionService, TranscriptionStatus};

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
fn list_audio_devices() -> Result<Vec<AudioDeviceDescriptor>, String> {
    capture::list_audio_devices().map_err(|error| error.to_string())
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
fn get_settings(state: tauri::State<'_, SettingsStore>) -> AppSettings {
    state.snapshot()
}

#[tauri::command]
fn save_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, SettingsStore>,
    tray: tauri::State<'_, Arc<desktop::TrayController>>,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    desktop::sync_autostart(&app, settings.desktop.launch_at_login)?;
    let saved = state.save(settings).map_err(|error| error.to_string())?;
    tray.update_language(saved.desktop.language);
    Ok(saved)
}

#[tauri::command]
fn get_recording_status(state: tauri::State<'_, RecordingService>) -> RecordingStatus {
    state.status()
}

#[tauri::command]
fn start_recording(
    recording: tauri::State<'_, RecordingService>,
    settings: tauri::State<'_, SettingsStore>,
) -> Result<RecordingStatus, String> {
    let settings = settings.snapshot();
    recording
        .start(settings.audio, settings.transcription)
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
fn cancel_model_install(state: tauri::State<'_, Arc<ModelManager>>) {
    state.cancel_install();
}

#[tauri::command]
fn get_transcription_status(
    state: tauri::State<'_, Arc<TranscriptionService>>,
) -> TranscriptionStatus {
    state.status()
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
            app.manage(settings_store);
            let recordings_root = app.path().app_local_data_dir()?.join("recordings");
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
            let transcription_event_app = app.handle().clone();
            let transcription_observer = Arc::new(move |status: TranscriptionStatus| {
                if let Err(error) = transcription_event_app.emit("transcription-status", status) {
                    tracing::debug!(?error, "transcription status event had no listener");
                }
            });
            let transcription = Arc::new(TranscriptionService::with_observer(
                model_manager.clone(),
                transcription_observer,
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
            ));
            app.manage(HistoryService::with_transcription(
                recordings_root,
                transcription.clone(),
            ));
            app.manage(tray);
            app.manage(transcription);
            app.manage(model_manager);
            if startup_settings.desktop.start_recording_on_launch {
                let recording = app.state::<RecordingService>();
                if let Err(error) =
                    recording.start(startup_settings.audio, startup_settings.transcription)
                {
                    tracing::error!(?error, "failed to start launch recording");
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            cancel_model_install,
            delete_recording,
            get_settings,
            get_model_status,
            get_recording_status,
            get_transcription_status,
            install_model,
            list_audio_devices,
            list_recording_history,
            open_recording_directory,
            save_settings,
            start_recording,
            stop_recording
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Resonote");
}
