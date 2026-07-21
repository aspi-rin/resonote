use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use chrono::Utc;
use thiserror::Error;

use crate::{
    asr::{AsrError, SherpaAsrRecognizer},
    audio::{RecoverableWavWriter, TARGET_SAMPLE_RATE, WavError},
    models::{ModelError, ModelManager},
    settings::TranscriptionSettings,
    vad::SpeechSegment,
};

const DOCUMENT_NAME: &str = "transcript.json";
const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub type TranscriptionObserver = Arc<dyn Fn(TranscriptionStatus) + Send + Sync + 'static>;
pub type SegmentObserver = Arc<dyn Fn(TranscriptSegmentUpdate) + Send + Sync + 'static>;

#[path = "transcription_delete.rs"]
mod deletion;
#[path = "transcript_document.rs"]
mod document;

pub use document::{
    TranscriptDocument, TranscriptDocumentStatus, TranscriptSegment, TranscriptSegmentStatus,
    TranscriptSegmentUpdate, TranscriptionPhase, TranscriptionStatus,
};
use document::{
    collect_documents, is_retryable, load_document, qwen_language, sample_to_ms, save_document,
};

pub struct TranscriptionService {
    inner: Arc<TranscriptionInner>,
    wake: crossbeam_channel::Sender<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct TranscriptionInner {
    document_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    known_sessions: Mutex<HashSet<PathBuf>>,
    manager: Arc<ModelManager>,
    observer: TranscriptionObserver,
    segment_observer: SegmentObserver,
    status: RwLock<TranscriptionStatus>,
    stop: AtomicBool,
}

impl TranscriptionService {
    pub fn new(manager: Arc<ModelManager>) -> Self {
        Self::with_observers(manager, Arc::new(|_| {}), Arc::new(|_| {}))
    }

    pub fn with_observer(manager: Arc<ModelManager>, observer: TranscriptionObserver) -> Self {
        Self::with_observers(manager, observer, Arc::new(|_| {}))
    }

    pub fn with_observers(
        manager: Arc<ModelManager>,
        observer: TranscriptionObserver,
        segment_observer: SegmentObserver,
    ) -> Self {
        let inner = Arc::new(TranscriptionInner {
            document_locks: Mutex::new(HashMap::new()),
            known_sessions: Mutex::new(HashSet::new()),
            manager,
            observer,
            segment_observer,
            status: RwLock::new(TranscriptionStatus::default()),
            stop: AtomicBool::new(false),
        });
        let (wake, receiver) = crossbeam_channel::bounded(1);
        let worker_inner = inner.clone();
        let worker = thread::Builder::new()
            .name("resonote-transcription".to_owned())
            .spawn(move || worker_loop(worker_inner, receiver))
            .expect("failed to start transcription worker");
        Self {
            inner,
            wake,
            worker: Mutex::new(Some(worker)),
        }
    }

    pub fn enqueue(
        &self,
        session_dir: &Path,
        session_id: &str,
        segment: &SpeechSegment,
        settings: &TranscriptionSettings,
    ) -> Result<TranscriptSegment, TranscriptionError> {
        let lock = self.inner.document_lock(session_dir);
        let document_guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = session_dir.join(DOCUMENT_NAME);
        let mut document = if path.exists() {
            load_document(&path)?
        } else {
            TranscriptDocument {
                forced_language: settings.language.clone(),
                model_id: settings.model_id.clone(),
                schema_version: 1,
                segments: Vec::new(),
                session_id: session_id.to_owned(),
                status: TranscriptDocumentStatus::Pending,
                threads: settings.threads,
                unload_after_idle_minutes: settings.unload_after_idle_minutes,
                updated_at: Utc::now(),
            }
        };
        if document.session_id != session_id {
            return Err(TranscriptionError::SessionMismatch);
        }
        let id = document
            .segments
            .iter()
            .map(|item| item.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(TranscriptionError::TooManySegments)?;
        let relative_audio = format!("speech/speech-{id:06}.wav");
        let audio_path = session_dir.join(&relative_audio);
        let mut writer = RecoverableWavWriter::create(audio_path, TARGET_SAMPLE_RATE)?;
        writer.append(&segment.samples)?;
        writer.finalize()?;
        let item = TranscriptSegment {
            attempts: 0,
            audio_file: relative_audio,
            detected_language: String::new(),
            end_ms: sample_to_ms(segment.end_sample),
            error: None,
            id,
            peak_probability: segment.peak_probability,
            start_ms: sample_to_ms(segment.start_sample),
            status: TranscriptSegmentStatus::Pending,
            text: String::new(),
        };
        document.segments.push(item.clone());
        document.status = TranscriptDocumentStatus::Pending;
        document.updated_at = Utc::now();
        save_document(&path, &document)?;
        drop(document_guard);

        self.inner
            .known_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_dir.to_path_buf());
        let _ = self.wake.try_send(());
        self.inner.publish(|status| {
            status.current_session_id = Some(session_id.to_owned());
            status.model_id = Some(settings.model_id.clone());
            status.pending_segments = status.pending_segments.saturating_add(1);
            if status.phase == TranscriptionPhase::Idle {
                status.phase = TranscriptionPhase::WaitingForModel;
            }
        });
        self.inner.publish_segment(session_id, &item);
        Ok(item)
    }

    pub fn recover_root(&self, root: &Path) -> Result<usize, TranscriptionError> {
        if !root.exists() {
            return Ok(0);
        }
        let mut paths = Vec::new();
        collect_documents(root, 0, &mut paths)?;
        let mut recovered = 0;
        for path in paths {
            let session_dir = path
                .parent()
                .ok_or(TranscriptionError::InvalidDocumentPath)?;
            let lock = self.inner.document_lock(session_dir);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut document = load_document(&path)?;
            let mut changed = false;
            for segment in &mut document.segments {
                if segment.status == TranscriptSegmentStatus::Processing {
                    segment.status = TranscriptSegmentStatus::Pending;
                    changed = true;
                }
            }
            if document.segments.iter().any(is_retryable) {
                document.status = TranscriptDocumentStatus::Pending;
                self.inner
                    .known_sessions
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(session_dir.to_path_buf());
                recovered += 1;
            }
            if changed {
                document.updated_at = Utc::now();
                save_document(&path, &document)?;
            }
        }
        if recovered > 0 {
            let _ = self.wake.try_send(());
        }
        Ok(recovered)
    }

    pub fn status(&self) -> TranscriptionStatus {
        self.inner
            .status
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Drop for TranscriptionService {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = worker.join();
        }
    }
}

impl TranscriptionInner {
    fn document_lock(&self, session_dir: &Path) -> Arc<Mutex<()>> {
        self.document_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(session_dir.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn publish(&self, mutate: impl FnOnce(&mut TranscriptionStatus)) {
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

    fn publish_segment(&self, session_id: &str, segment: &TranscriptSegment) {
        (self.segment_observer)(TranscriptSegmentUpdate {
            segment: segment.clone(),
            session_id: session_id.to_owned(),
        });
    }
}

struct CachedRecognizer {
    idle_timeout: Duration,
    last_used: Instant,
    model_id: String,
    recognizer: SherpaAsrRecognizer,
    threads: u16,
}

fn worker_loop(inner: Arc<TranscriptionInner>, receiver: crossbeam_channel::Receiver<()>) {
    let mut cached: Option<CachedRecognizer> = None;
    while !inner.stop.load(Ordering::Acquire) {
        let _ = receiver.recv_timeout(WORKER_POLL_INTERVAL);
        if inner.stop.load(Ordering::Acquire) {
            break;
        }
        let sessions = inner
            .known_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for session_dir in sessions {
            if inner.stop.load(Ordering::Acquire) {
                break;
            }
            match process_session(&inner, &session_dir, &mut cached) {
                Ok(false) => {
                    inner
                        .known_sessions
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&session_dir);
                }
                Ok(true) => {}
                Err(error) => inner.publish(|status| {
                    status.error = Some(error.to_string());
                    status.phase = TranscriptionPhase::Failed;
                }),
            }
        }
        if cached
            .as_ref()
            .is_some_and(|recognizer| recognizer.last_used.elapsed() >= recognizer.idle_timeout)
        {
            cached = None;
            inner.publish(|status| {
                status.model_loaded = false;
                if status.pending_segments == 0 {
                    status.phase = TranscriptionPhase::Idle;
                }
            });
        }
    }
    drop(cached);
}

/// Returns true while this session still has retryable work.
fn process_session(
    inner: &TranscriptionInner,
    session_dir: &Path,
    cached: &mut Option<CachedRecognizer>,
) -> Result<bool, TranscriptionError> {
    let path = session_dir.join(DOCUMENT_NAME);
    if !path.exists() {
        return Ok(false);
    }
    let lock = inner.document_lock(session_dir);
    let (document, next_id) = {
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let document = load_document(&path)?;
        let next_id = document
            .segments
            .iter()
            .find(|item| is_retryable(item))
            .map(|item| item.id);
        (document, next_id)
    };
    let Some(segment_id) = next_id else {
        return Ok(false);
    };

    match inner.manager.installed_model(&document.model_id) {
        Ok(_) => {}
        Err(ModelError::NotInstalled(_)) => {
            inner.publish(|status| {
                status.current_session_id = Some(document.session_id.clone());
                status.model_id = Some(document.model_id.clone());
                status.model_loaded = false;
                status.phase = TranscriptionPhase::WaitingForModel;
            });
            return Ok(true);
        }
        Err(error) => return Err(error.into()),
    }
    ensure_recognizer(inner, &document, cached)?;
    let (audio_path, processing_snapshot) = {
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut latest = load_document(&path)?;
        let item = latest
            .segments
            .iter_mut()
            .find(|item| item.id == segment_id)
            .ok_or(TranscriptionError::SegmentMissing(segment_id))?;
        item.status = TranscriptSegmentStatus::Processing;
        item.attempts += 1;
        item.error = None;
        let audio_path = session_dir.join(&item.audio_file);
        let snapshot = item.clone();
        latest.status = TranscriptDocumentStatus::Processing;
        latest.updated_at = Utc::now();
        save_document(&path, &latest)?;
        (audio_path, snapshot)
    };
    inner.publish_segment(&document.session_id, &processing_snapshot);
    inner.publish(|status| {
        status.current_session_id = Some(document.session_id.clone());
        status.error = None;
        status.model_id = Some(document.model_id.clone());
        status.model_loaded = true;
        status.phase = TranscriptionPhase::Transcribing;
    });
    let forced_language = qwen_language(&document.forced_language);
    let result = cached
        .as_mut()
        .expect("recognizer was initialized")
        .recognizer
        .transcribe_wav(&audio_path, forced_language);
    if let Some(recognizer) = cached.as_mut() {
        recognizer.last_used = Instant::now();
    }

    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut latest = load_document(&path)?;
    let item = latest
        .segments
        .iter_mut()
        .find(|item| item.id == segment_id)
        .ok_or(TranscriptionError::SegmentMissing(segment_id))?;
    match result {
        Ok(transcription) => {
            item.detected_language = transcription.language;
            item.error = None;
            item.status = TranscriptSegmentStatus::Complete;
            item.text = transcription.text;
        }
        Err(error) => {
            item.error = Some(error.to_string());
            item.status = TranscriptSegmentStatus::Failed;
        }
    }
    let resolved_snapshot = item.clone();
    let retryable = latest
        .segments
        .iter()
        .filter(|item| is_retryable(item))
        .count();
    let failed = latest
        .segments
        .iter()
        .any(|item| item.status == TranscriptSegmentStatus::Failed);
    latest.status = if retryable > 0 {
        TranscriptDocumentStatus::Pending
    } else if failed {
        TranscriptDocumentStatus::Partial
    } else {
        TranscriptDocumentStatus::Complete
    };
    latest.updated_at = Utc::now();
    save_document(&path, &latest)?;
    inner.publish_segment(&latest.session_id, &resolved_snapshot);
    inner.publish(|status| {
        status.pending_segments = retryable;
        if retryable == 0 {
            status.phase = TranscriptionPhase::Idle;
        }
    });
    Ok(retryable > 0)
}

fn ensure_recognizer(
    inner: &TranscriptionInner,
    document: &TranscriptDocument,
    cached: &mut Option<CachedRecognizer>,
) -> Result<(), TranscriptionError> {
    let reusable = cached.as_ref().is_some_and(|recognizer| {
        recognizer.model_id == document.model_id && recognizer.threads == document.threads
    });
    if reusable {
        return Ok(());
    }
    *cached = None;
    inner.publish(|status| {
        status.current_session_id = Some(document.session_id.clone());
        status.model_id = Some(document.model_id.clone());
        status.model_loaded = false;
        status.phase = TranscriptionPhase::LoadingModel;
    });
    let recognizer =
        SherpaAsrRecognizer::load(&inner.manager, &document.model_id, document.threads)?;
    *cached = Some(CachedRecognizer {
        idle_timeout: Duration::from_secs(
            u64::from(document.unload_after_idle_minutes).saturating_mul(60),
        ),
        last_used: Instant::now(),
        model_id: document.model_id.clone(),
        recognizer,
        threads: document.threads,
    });
    inner.publish(|status| status.model_loaded = true);
    Ok(())
}

#[derive(Debug, Error)]
pub enum TranscriptionError {
    #[error(transparent)]
    Asr(#[from] AsrError),
    #[error("transcript document path is invalid")]
    InvalidDocumentPath,
    #[error("failed to access transcript: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse transcript: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("failed to persist transcript: {0}")]
    Persist(#[from] tempfile::PersistError),
    #[error("transcript segment {0} is missing")]
    SegmentMissing(u32),
    #[error("transcript belongs to a different recording session")]
    SessionMismatch,
    #[error("recording contains too many transcript segments")]
    TooManySegments,
    #[error(transparent)]
    Wav(#[from] WavError),
}

#[cfg(test)]
#[path = "transcription_tests.rs"]
mod tests;
