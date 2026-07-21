use std::{
    path::PathBuf,
    sync::{Arc, Mutex, RwLock},
    thread::{self, JoinHandle},
    time::Duration,
};

use crossbeam_channel::Sender;
use serde::Serialize;
use thiserror::Error;

use crate::{
    audio::{AudioBlockError, ResampleStreamError},
    capture::{CaptureError, CaptureEvent, CaptureSession, CaptureSource, start_capture},
    models::{ModelError, ModelManager},
    settings::{AudioSettings, AudioSourceMode, TranscriptionSettings, TranslationSettings},
    storage::{RecordingArchive, SessionManifest, StorageError, recover_interrupted_sessions},
    transcription::TranscriptionService,
    vad::VadError,
};

const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(5);
const STATUS_INTERVAL: Duration = Duration::from_millis(100);
const MAX_BUFFERED_SECONDS: usize = 10;
const MIX_CHUNK_SAMPLES: usize = 1_600;

pub type StatusObserver = Arc<dyn Fn(RecordingStatus) + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordingPhase {
    Idle,
    Starting,
    Recording,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub audio_source: AudioSourceMode,
    pub captured_samples: u64,
    pub elapsed_ms: u64,
    pub error: Option<String>,
    pub microphone_db: f32,
    pub microphone_device: Option<String>,
    pub microphone_waveform: Vec<f32>,
    pub phase: RecordingPhase,
    pub segment_count: usize,
    pub session_directory: Option<PathBuf>,
    pub session_id: Option<String>,
    pub system_db: f32,
    pub system_device: Option<String>,
    pub system_waveform: Vec<f32>,
    pub transcription_error: Option<String>,
    pub vad_probability: f32,
}

impl Default for RecordingStatus {
    fn default() -> Self {
        Self {
            audio_source: AudioSourceMode::Mixed,
            captured_samples: 0,
            elapsed_ms: 0,
            error: None,
            microphone_db: -80.0,
            microphone_device: None,
            microphone_waveform: vec![0.0; 48],
            phase: RecordingPhase::Idle,
            segment_count: 0,
            session_directory: None,
            session_id: None,
            system_db: -80.0,
            system_device: None,
            system_waveform: vec![0.0; 48],
            transcription_error: None,
            vad_probability: 0.0,
        }
    }
}

pub struct RecordingService {
    active: Mutex<Option<ActiveRecording>>,
    default_root: PathBuf,
    models: Option<Arc<ModelManager>>,
    observer: StatusObserver,
    status: Arc<RwLock<RecordingStatus>>,
    transcription: Option<Arc<TranscriptionService>>,
}

impl RecordingService {
    pub fn new(default_root: PathBuf) -> Self {
        Self::with_observer(default_root, Arc::new(|_| {}))
    }

    pub fn with_observer(default_root: PathBuf, observer: StatusObserver) -> Self {
        Self::with_services(default_root, observer, None, None)
    }

    pub fn with_services(
        default_root: PathBuf,
        observer: StatusObserver,
        transcription: Option<Arc<TranscriptionService>>,
        models: Option<Arc<ModelManager>>,
    ) -> Self {
        Self {
            active: Mutex::new(None),
            default_root,
            models,
            observer,
            status: Arc::new(RwLock::new(RecordingStatus::default())),
            transcription,
        }
    }

    pub fn start(
        &self,
        settings: AudioSettings,
        transcription_settings: TranscriptionSettings,
        translation_settings: TranslationSettings,
    ) -> Result<RecordingStatus, RecordingError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reap_finished(&mut active);
        if active.is_some() {
            return Err(RecordingError::AlreadyRecording);
        }
        let vad_model = if self.transcription.is_some() {
            let manager = self
                .models
                .as_ref()
                .ok_or(RecordingError::MissingModelManager)?;
            Some(
                manager
                    .installed_model(&transcription_settings.model_id)?
                    .vad_model,
            )
        } else {
            None
        };

        let output_root = settings
            .output_directory
            .clone()
            .unwrap_or_else(|| self.default_root.clone());
        recover_interrupted_sessions(&output_root)?;
        let archive = RecordingArchive::create(&output_root, &settings)?;
        self.replace_status(RecordingStatus {
            audio_source: settings.source,
            phase: RecordingPhase::Starting,
            ..RecordingStatus::default()
        });
        let (capture_sender, capture_receiver) = crossbeam_channel::unbounded();
        let mut sessions = Vec::new();
        let capture_result = ensure_sources(settings.source, &capture_sender, &mut sessions);
        if let Err(error) = capture_result {
            drop(sessions);
            let _ = archive.fail(error.to_string());
            self.fail_status(error.to_string());
            return Err(error);
        }

        let microphone_device = sessions
            .iter()
            .find(|session| session.source == CaptureSource::Microphone)
            .map(|session| session.device_name.clone());
        let system_device = sessions
            .iter()
            .find(|session| session.source == CaptureSource::System)
            .map(|session| session.device_name.clone());
        let source = Arc::new(RwLock::new(settings.source));
        let session_directory = archive.session_dir().to_path_buf();
        let session_id = archive.manifest().session_id.clone();
        let recording_status = RecordingStatus {
            audio_source: settings.source,
            microphone_device,
            phase: RecordingPhase::Recording,
            session_directory: Some(session_directory),
            session_id: Some(session_id),
            system_device,
            ..RecordingStatus::default()
        };
        self.replace_status(recording_status.clone());
        let (stop_sender, stop_receiver) = crossbeam_channel::bounded(1);
        let status = self.status.clone();
        let observer = self.observer.clone();
        let transcription = self.transcription.clone();
        let context = RecordingWorkerContext {
            observer,
            settings: settings.clone(),
            source: source.clone(),
            status,
            transcription,
            transcription_settings,
            translation_settings,
            vad_model,
        };
        let worker = thread::Builder::new()
            .name("resonote-recording".to_owned())
            .spawn(move || recording_worker(archive, capture_receiver, stop_receiver, context))
            .map_err(|error| {
                self.fail_status(error.to_string());
                if let Err(recovery_error) = recover_interrupted_sessions(&output_root) {
                    tracing::error!(
                        ?recovery_error,
                        "failed to recover recording after thread error"
                    );
                }
                RecordingError::Thread(error)
            })?;
        *active = Some(ActiveRecording {
            capture_sender,
            sessions,
            source,
            stop_sender,
            worker,
        });
        Ok(recording_status)
    }

    pub fn set_source(&self, source: AudioSourceMode) -> Result<RecordingStatus, RecordingError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reap_finished(&mut active);
        let Some(recording) = active.as_mut() else {
            return Err(RecordingError::NotRecording);
        };
        ensure_sources(source, &recording.capture_sender, &mut recording.sessions)?;
        *recording
            .source
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = source;
        let microphone_device = recording
            .sessions
            .iter()
            .find(|session| session.source == CaptureSource::Microphone)
            .map(|session| session.device_name.clone());
        let system_device = recording
            .sessions
            .iter()
            .find(|session| session.source == CaptureSource::System)
            .map(|session| session.device_name.clone());
        drop(active);
        self.mutate_status(|status| {
            status.audio_source = source;
            status.microphone_device = microphone_device;
            status.system_device = system_device;
        });
        Ok(self.status())
    }

    pub fn stop(&self) -> Result<RecordingStatus, RecordingError> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reap_finished(&mut active);
        let Some(active_recording) = active.take() else {
            return Err(RecordingError::NotRecording);
        };
        drop(active);
        self.mutate_status(|status| status.phase = RecordingPhase::Stopping);
        drop(active_recording.sessions);
        let _ = active_recording.stop_sender.send(());
        let result = active_recording
            .worker
            .join()
            .map_err(|_| RecordingError::WorkerPanic)?;
        result?;
        Ok(self.status())
    }

    pub fn status(&self) -> RecordingStatus {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reap_finished(&mut active);
        self.status
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn replace_status(&self, status: RecordingStatus) {
        *self
            .status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = status.clone();
        (self.observer)(status);
    }

    fn mutate_status(&self, mutate: impl FnOnce(&mut RecordingStatus)) {
        let updated = {
            let mut status = self
                .status
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            mutate(&mut status);
            status.clone()
        };
        (self.observer)(updated);
    }

    fn fail_status(&self, error: String) {
        self.mutate_status(|status| {
            status.error = Some(error);
            status.phase = RecordingPhase::Failed;
        });
    }
}

struct ActiveRecording {
    capture_sender: Sender<CaptureEvent>,
    sessions: Vec<CaptureSession>,
    source: Arc<RwLock<AudioSourceMode>>,
    stop_sender: Sender<()>,
    worker: JoinHandle<Result<SessionManifest, RecordingError>>,
}

fn reap_finished(active: &mut Option<ActiveRecording>) {
    if active
        .as_ref()
        .is_some_and(|recording| recording.worker.is_finished())
    {
        let finished = active.take().expect("active recording exists");
        drop(finished.sessions);
        if finished.worker.join().is_err() {
            tracing::error!("recording worker panicked");
        }
    }
}

fn ensure_sources(
    mode: AudioSourceMode,
    sender: &Sender<CaptureEvent>,
    sessions: &mut Vec<CaptureSession>,
) -> Result<(), RecordingError> {
    for source in [CaptureSource::Microphone, CaptureSource::System] {
        if source_is_enabled(mode, source)
            && !sessions.iter().any(|session| session.source == source)
        {
            sessions.push(start_capture(source, sender.clone())?);
        }
    }
    Ok(())
}

fn source_is_enabled(mode: AudioSourceMode, source: CaptureSource) -> bool {
    matches!(
        (mode, source),
        (AudioSourceMode::Microphone, CaptureSource::Microphone)
            | (AudioSourceMode::System, CaptureSource::System)
            | (AudioSourceMode::Mixed, _)
    )
}

#[path = "recording_worker.rs"]
mod worker;

use worker::{RecordingWorkerContext, recording_worker};

#[derive(Debug, Error)]
pub enum RecordingError {
    #[error("a recording is already active")]
    AlreadyRecording,
    #[error("audio buffer overflowed while waiting for {0:?}")]
    AudioBufferOverflow(CaptureSource),
    #[error(transparent)]
    AudioBlock(#[from] AudioBlockError),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(
        "audio capture sequence gap for {capture_source:?}: expected {expected}, received {actual}"
    )]
    CaptureSequence {
        actual: u64,
        capture_source: CaptureSource,
        expected: u64,
    },
    #[error("{capture_source:?} capture stream failed: {message}")]
    CaptureStream {
        capture_source: CaptureSource,
        message: String,
    },
    #[error("recording service has no model manager for sherpa-onnx VAD")]
    MissingModelManager,
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("there is no active recording")]
    NotRecording,
    #[error(transparent)]
    Resample(#[from] ResampleStreamError),
    #[error(
        "{source_kind:?} sample rate changed while recording: expected {expected}, received {actual}"
    )]
    SampleRateChanged {
        actual: u32,
        expected: u32,
        source_kind: CaptureSource,
    },
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("failed to start recording worker: {0}")]
    Thread(std::io::Error),
    #[error(transparent)]
    Vad(#[from] VadError),
    #[error("recording worker panicked")]
    WorkerPanic,
}

#[cfg(test)]
#[path = "recording_tests.rs"]
mod tests;
