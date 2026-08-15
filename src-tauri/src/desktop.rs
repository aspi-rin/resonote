use std::{
    sync::{Arc, RwLock},
    thread,
    time::{Duration, Instant},
};

use tauri::{
    App, AppHandle, Manager, Wry,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_autostart::ManagerExt;

use crate::{
    meeting_notes::MeetingNotesService,
    recording::{RecordingPhase, RecordingService, RecordingStatus},
    settings::{AppLanguage, SettingsStore},
};

const MENU_QUIT: &str = "quit";
const MENU_SHOW: &str = "show";
const MENU_TOGGLE_RECORDING: &str = "toggle-recording";

pub struct TrayController {
    _icon: TrayIcon<Wry>,
    language: RwLock<AppLanguage>,
    quit_item: MenuItem<Wry>,
    show_item: MenuItem<Wry>,
    toggle_item: MenuItem<Wry>,
}

impl TrayController {
    pub fn create(app: &App, language: AppLanguage) -> tauri::Result<Arc<Self>> {
        let labels = TrayLabels::for_language(language);
        let show_item = MenuItem::with_id(app, MENU_SHOW, labels.show, true, None::<&str>)?;
        let toggle_item = MenuItem::with_id(
            app,
            MENU_TOGGLE_RECORDING,
            labels.start_recording,
            true,
            None::<&str>,
        )?;
        let separator = PredefinedMenuItem::separator(app)?;
        let quit_item = MenuItem::with_id(app, MENU_QUIT, labels.quit, true, None::<&str>)?;
        let menu = Menu::with_items(app, &[&show_item, &toggle_item, &separator, &quit_item])?;
        let mut builder = TrayIconBuilder::with_id("resonote-main")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .tooltip("Resonote")
            .on_menu_event(handle_menu_event)
            .on_tray_icon_event(|tray, event| {
                if matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                ) {
                    show_main_window(tray.app_handle());
                }
            });
        if let Some(icon) = app.default_window_icon().cloned() {
            builder = builder.icon(icon);
        }
        let icon = builder.build(app)?;
        Ok(Arc::new(Self {
            _icon: icon,
            language: RwLock::new(language),
            quit_item,
            show_item,
            toggle_item,
        }))
    }

    pub fn update_language(&self, language: AppLanguage) {
        *self
            .language
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = language;
        let labels = TrayLabels::for_language(language);
        let _ = self.show_item.set_text(labels.show);
        let _ = self.quit_item.set_text(labels.quit);
    }

    pub fn update_recording(&self, status: &RecordingStatus) {
        let language = *self
            .language
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let labels = TrayLabels::for_language(language);
        let (text, enabled) = match status.phase {
            RecordingPhase::Idle | RecordingPhase::Failed => (labels.start_recording, true),
            RecordingPhase::Recording => (labels.stop_recording, true),
            RecordingPhase::Starting => (labels.starting, false),
            RecordingPhase::Stopping => (labels.stopping, false),
        };
        let _ = self.toggle_item.set_text(text);
        let _ = self.toggle_item.set_enabled(enabled);
    }
}

pub fn handle_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if window.label() != "main" {
        return;
    }
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        let settings = window.state::<SettingsStore>().snapshot();
        if settings.desktop.hide_window_on_close {
            api.prevent_close();
            if let Err(error) = window.hide() {
                tracing::warn!(?error, "failed to hide main window");
            }
        }
    }
}

pub fn handle_run_event(app: &AppHandle, event: tauri::RunEvent) {
    // macOS delivers Dock icon clicks as Reopen, not as a window event, so the
    // hidden main window has to be restored here.
    #[cfg(target_os = "macos")]
    if let tauri::RunEvent::Reopen { .. } = event {
        show_main_window(app);
    }
    if let tauri::RunEvent::Exit = event {
        if let Some(service) = app.try_state::<Arc<MeetingNotesService>>() {
            service.shutdown();
        }
    }
}

pub fn sync_autostart(app: &AppHandle, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    let currently_enabled = manager.is_enabled().map_err(|error| error.to_string())?;
    if enabled != currently_enabled {
        if enabled {
            manager.enable().map_err(|error| error.to_string())?;
        } else {
            manager.disable().map_err(|error| error.to_string())?;
        }
    }
    manager.is_enabled().map_err(|error| error.to_string())
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        MENU_SHOW => show_main_window(app),
        MENU_TOGGLE_RECORDING => toggle_recording(app),
        MENU_QUIT => quit_after_stop(app),
        _ => {}
    }
}

pub fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.unminimize();
    if let Err(error) = window.show().and_then(|()| window.set_focus()) {
        tracing::warn!(?error, "failed to show main window");
    }
}

fn toggle_recording(app: &AppHandle) {
    let recording = app.state::<RecordingService>();
    match recording.status().phase {
        RecordingPhase::Idle | RecordingPhase::Failed => {
            let settings = app.state::<SettingsStore>().snapshot();
            if let Err(error) = recording
                .start(settings.audio, settings.transcription, settings.translation)
                .map(|_| ())
            {
                tracing::error!(?error, "tray recording action failed");
            }
        }
        RecordingPhase::Starting | RecordingPhase::Recording => stop_in_background(app),
        RecordingPhase::Stopping => {}
    }
}

fn stop_in_background(app: &AppHandle) {
    let app = app.clone();
    if let Err(error) = thread::Builder::new()
        .name("resonote-stop".to_owned())
        .spawn(move || {
            if let Err(error) = app.state::<RecordingService>().stop() {
                tracing::error!(?error, "tray recording action failed");
            }
        })
    {
        tracing::error!(?error, "failed to start recording stop task");
    }
}

fn quit_after_stop(app: &AppHandle) {
    let background_app = app.clone();
    if let Err(error) = thread::Builder::new()
        .name("resonote-quit".to_owned())
        .spawn(move || {
            stop_before_exit(&background_app);
            background_app.exit(0);
        })
    {
        tracing::error!(?error, "failed to start graceful exit task");
        app.exit(1);
    }
}

fn stop_before_exit(app: &AppHandle) {
    const STOP_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
    let recording = app.state::<RecordingService>();
    match recording.status().phase {
        RecordingPhase::Starting | RecordingPhase::Recording => {
            if let Err(error) = recording.stop() {
                tracing::error!(?error, "failed to stop recording before exit");
            }
        }
        RecordingPhase::Stopping => {
            let started = Instant::now();
            while recording.status().phase == RecordingPhase::Stopping
                && started.elapsed() < STOP_WAIT_TIMEOUT
            {
                thread::sleep(Duration::from_millis(25));
            }
            if recording.status().phase == RecordingPhase::Stopping {
                tracing::error!("timed out waiting for recording to stop before exit");
            }
        }
        RecordingPhase::Idle | RecordingPhase::Failed => {}
    }
}

struct TrayLabels {
    quit: &'static str,
    show: &'static str,
    start_recording: &'static str,
    starting: &'static str,
    stop_recording: &'static str,
    stopping: &'static str,
}

impl TrayLabels {
    fn for_language(language: AppLanguage) -> Self {
        if language == AppLanguage::ZhCn {
            Self {
                quit: "退出 Resonote",
                show: "显示主窗口",
                start_recording: "开始录音",
                starting: "正在启动…",
                stop_recording: "停止并保存",
                stopping: "正在停止…",
            }
        } else {
            Self {
                quit: "Quit Resonote",
                show: "Show Resonote",
                start_recording: "Start Recording",
                starting: "Starting…",
                stop_recording: "Stop and Save",
                stopping: "Stopping…",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localizes_tray_actions_for_explicit_chinese() {
        let chinese = TrayLabels::for_language(AppLanguage::ZhCn);
        let english = TrayLabels::for_language(AppLanguage::EnUs);

        assert_eq!(chinese.start_recording, "开始录音");
        assert_eq!(chinese.stop_recording, "停止并保存");
        assert_eq!(english.start_recording, "Start Recording");
    }
}
