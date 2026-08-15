use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::{
    openai_compatible::{
        ChatCompletionPort, ChatError, ChatMessage, ChatRequest, ChatRole, ReqwestChatClient,
    },
    session_lifecycle::SessionLifecycle,
    settings::{ProviderAuthMode, ProviderCredentials, TranslationSnapshot},
};

pub const DOCUMENT_NAME: &str = "translation.json";
const MAX_ATTEMPTS: u32 = 3;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub type TranslationObserver = Arc<dyn Fn(TranslationSegmentUpdate) + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TranslationDocumentStatus {
    Pending,
    Processing,
    Complete,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TranslationSegmentStatus {
    Pending,
    Processing,
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSegment {
    pub attempts: u32,
    pub error: Option<String>,
    pub segment_id: u32,
    pub source_text: String,
    pub status: TranslationSegmentStatus,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationSegmentUpdate {
    pub segment: TranslationSegment,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationDocument {
    #[serde(default)]
    pub auth_mode: ProviderAuthMode,
    pub endpoint: String,
    pub model: String,
    pub schema_version: u32,
    pub segments: Vec<TranslationSegment>,
    pub session_id: String,
    pub status: TranslationDocumentStatus,
    pub target_language: String,
    pub updated_at: DateTime<Utc>,
}

pub struct TranslationService {
    inner: Arc<TranslationInner>,
    wake: crossbeam_channel::Sender<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct TranslationInner {
    chat: Arc<dyn ChatCompletionPort>,
    credentials: ProviderCredentials,
    known_sessions: Mutex<HashSet<PathBuf>>,
    lifecycle: Arc<SessionLifecycle>,
    observer: TranslationObserver,
    stop: AtomicBool,
}

impl TranslationService {
    pub fn new(observer: TranslationObserver) -> Result<Self, TranslationError> {
        Self::with_credentials(
            observer,
            ProviderCredentials::default(),
            SessionLifecycle::new(),
        )
    }

    pub fn with_credentials(
        observer: TranslationObserver,
        credentials: ProviderCredentials,
        lifecycle: Arc<SessionLifecycle>,
    ) -> Result<Self, TranslationError> {
        Self::with_chat_client(
            observer,
            credentials,
            Arc::new(ReqwestChatClient::new()?),
            lifecycle,
        )
    }

    pub fn with_chat_client(
        observer: TranslationObserver,
        credentials: ProviderCredentials,
        chat: Arc<dyn ChatCompletionPort>,
        lifecycle: Arc<SessionLifecycle>,
    ) -> Result<Self, TranslationError> {
        let inner = Arc::new(TranslationInner {
            chat,
            credentials,
            known_sessions: Mutex::new(HashSet::new()),
            lifecycle,
            observer,
            stop: AtomicBool::new(false),
        });
        let (wake, receiver) = crossbeam_channel::bounded(1);
        let worker_inner = inner.clone();
        let worker = thread::Builder::new()
            .name("resonote-translation".to_owned())
            .spawn(move || worker_loop(worker_inner, receiver))?;
        Ok(Self {
            inner,
            wake,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn enqueue(
        &self,
        session_dir: &Path,
        session_id: &str,
        segment_id: u32,
        source_text: &str,
        translation: &TranslationSnapshot,
    ) -> Result<Option<TranslationSegment>, TranslationError> {
        if !translation.enabled || source_text.trim().is_empty() {
            return Ok(None);
        }
        let lock = self.inner.document_lock(session_dir);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.inner.lifecycle.is_deleting(session_dir) {
            return Ok(None);
        }
        let path = session_dir.join(DOCUMENT_NAME);
        let mut document = if path.exists() {
            let mut document = load_document(&path)?;
            document.auth_mode = translation.auth_mode;
            document.endpoint = translation.endpoint.clone();
            document.model = translation.model.clone();
            document.target_language = translation.target_language.clone();
            document
        } else {
            TranslationDocument {
                auth_mode: translation.auth_mode,
                endpoint: translation.endpoint.clone(),
                model: translation.model.clone(),
                schema_version: 1,
                segments: Vec::new(),
                session_id: session_id.to_owned(),
                status: TranslationDocumentStatus::Pending,
                target_language: translation.target_language.clone(),
                updated_at: Utc::now(),
            }
        };
        if document.session_id != session_id {
            return Err(TranslationError::SessionMismatch);
        }
        let segment = if let Some(existing) = document
            .segments
            .iter_mut()
            .find(|item| item.segment_id == segment_id)
        {
            if existing.source_text != source_text {
                existing.attempts = 0;
                existing.error = None;
                existing.source_text = source_text.to_owned();
                existing.status = TranslationSegmentStatus::Pending;
                existing.text.clear();
            }
            existing.clone()
        } else {
            let segment = TranslationSegment {
                attempts: 0,
                error: None,
                segment_id,
                source_text: source_text.to_owned(),
                status: TranslationSegmentStatus::Pending,
                text: String::new(),
            };
            document.segments.push(segment.clone());
            document
                .segments
                .sort_unstable_by_key(|item| item.segment_id);
            segment
        };
        document.status = document_status(&document.segments);
        document.updated_at = Utc::now();
        save_document(&path, &document)?;
        drop(_guard);

        self.inner
            .known_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_dir.to_path_buf());
        self.inner.publish(session_id, &segment);
        let _ = self.wake.try_send(());
        Ok(Some(segment))
    }

    /// Stops the worker from picking the session up again. The in-flight request
    /// is left alone: its response is dropped by the commit guard.
    pub fn forget(&self, session_dir: &Path) {
        self.inner
            .known_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_dir);
    }

    pub fn recover_root(&self, root: &Path) -> Result<usize, TranslationError> {
        if !root.exists() {
            return Ok(0);
        }
        let mut paths = Vec::new();
        collect_documents(root, 0, &mut paths)?;
        let mut recovered = 0;
        for path in paths {
            let session_dir = path.parent().ok_or(TranslationError::InvalidDocumentPath)?;
            let lock = self.inner.document_lock(session_dir);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut document = load_document(&path)?;
            let mut changed = false;
            for segment in &mut document.segments {
                if segment.status == TranslationSegmentStatus::Processing {
                    segment.status = TranslationSegmentStatus::Pending;
                    changed = true;
                }
            }
            if document.segments.iter().any(is_retryable) {
                changed |= document.status != TranslationDocumentStatus::Pending;
                document.status = TranslationDocumentStatus::Pending;
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
}

impl Drop for TranslationService {
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

impl TranslationInner {
    fn document_lock(&self, session_dir: &Path) -> Arc<Mutex<()>> {
        self.lifecycle.session_lock(session_dir)
    }

    fn publish(&self, session_id: &str, segment: &TranslationSegment) {
        (self.observer)(TranslationSegmentUpdate {
            segment: segment.clone(),
            session_id: session_id.to_owned(),
        });
    }
}

fn worker_loop(inner: Arc<TranslationInner>, receiver: crossbeam_channel::Receiver<()>) {
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
            match process_session(&inner, &session_dir) {
                Ok(false) => {
                    inner
                        .known_sessions
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&session_dir);
                }
                Ok(true) => {}
                Err(error) => {
                    tracing::warn!(?error, "translation worker skipped an invalid session");
                    inner
                        .known_sessions
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&session_dir);
                }
            }
        }
    }
}

/// Returns true while this session still has retryable work.
fn process_session(inner: &TranslationInner, session_dir: &Path) -> Result<bool, TranslationError> {
    let path = session_dir.join(DOCUMENT_NAME);
    if !path.exists() || inner.lifecycle.is_deleting(session_dir) {
        return Ok(false);
    }
    let lock = inner.document_lock(session_dir);
    let (document, segment_id) = {
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let document = load_document(&path)?;
        let segment_id = document
            .segments
            .iter()
            .find(|item| is_retryable(item))
            .map(|item| item.segment_id);
        (document, segment_id)
    };
    let Some(segment_id) = segment_id else {
        return Ok(false);
    };
    let processing = {
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut latest = load_document(&path)?;
        let item = latest
            .segments
            .iter_mut()
            .find(|item| item.segment_id == segment_id)
            .ok_or(TranslationError::SegmentMissing(segment_id))?;
        item.attempts += 1;
        item.error = None;
        item.status = TranslationSegmentStatus::Processing;
        let snapshot = item.clone();
        latest.status = TranslationDocumentStatus::Processing;
        latest.updated_at = Utc::now();
        save_document(&path, &latest)?;
        snapshot
    };
    inner.publish(&document.session_id, &processing);

    let result = translate_text(
        inner.chat.as_ref(),
        &inner.credentials,
        &document,
        &processing.source_text,
    );

    let (session_id, resolved, retryable) = {
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // The tombstone, the marker and the directory are checked under the same
        // lifecycle lock the delete takes, so a late response is dropped here
        // instead of racing the removal.
        if inner.lifecycle.is_deleting(session_dir) || !session_dir.is_dir() {
            tracing::debug!(segment_id, "dropped a translation for a deleted session");
            return Ok(false);
        }
        let mut latest = load_document(&path)?;
        let item = latest
            .segments
            .iter_mut()
            .find(|item| item.segment_id == segment_id)
            .ok_or(TranslationError::SegmentMissing(segment_id))?;
        match result {
            Ok(text) => {
                item.error = None;
                item.status = TranslationSegmentStatus::Complete;
                item.text = text;
            }
            Err(error) => {
                item.error = Some(error.to_string());
                item.status = TranslationSegmentStatus::Failed;
            }
        }
        let resolved = item.clone();
        let retryable = latest
            .segments
            .iter()
            .filter(|item| is_retryable(item))
            .count();
        latest.status = document_status(&latest.segments);
        latest.updated_at = Utc::now();
        save_document(&path, &latest)?;
        (latest.session_id, resolved, retryable)
    };
    inner.publish(&session_id, &resolved);
    Ok(retryable > 0)
}

fn translate_text(
    chat: &dyn ChatCompletionPort,
    credentials: &ProviderCredentials,
    document: &TranslationDocument,
    source_text: &str,
) -> Result<String, TranslationError> {
    let api_key = match document.auth_mode {
        ProviderAuthMode::None => None,
        ProviderAuthMode::Bearer => Some(
            credentials
                .translation_key(&document.endpoint)
                .ok_or(TranslationError::ProviderChanged)?,
        ),
    };
    let instruction = format!(
        "Translate the user's text into {}. Preserve meaning, names, numbers, and formatting. Return only the translation.",
        document.target_language
    );
    Ok(chat.complete(ChatRequest {
        api_key: api_key.as_ref(),
        endpoint: &document.endpoint,
        messages: vec![
            ChatMessage {
                content: &instruction,
                role: ChatRole::System,
            },
            ChatMessage {
                content: source_text,
                role: ChatRole::User,
            },
        ],
        model: &document.model,
        timeout: REQUEST_TIMEOUT,
    })?)
}

fn is_retryable(segment: &TranslationSegment) -> bool {
    matches!(
        segment.status,
        TranslationSegmentStatus::Pending | TranslationSegmentStatus::Processing
    ) || (segment.status == TranslationSegmentStatus::Failed && segment.attempts < MAX_ATTEMPTS)
}

fn document_status(segments: &[TranslationSegment]) -> TranslationDocumentStatus {
    if segments.iter().any(is_retryable) {
        TranslationDocumentStatus::Pending
    } else if segments
        .iter()
        .any(|segment| segment.status == TranslationSegmentStatus::Failed)
    {
        TranslationDocumentStatus::Partial
    } else {
        TranslationDocumentStatus::Complete
    }
}

fn load_document(path: &Path) -> Result<TranslationDocument, TranslationError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

/// Never recreates the session directory: a deleted session must stay deleted
/// even when a late response arrives.
fn save_document(path: &Path, document: &TranslationDocument) -> Result<(), TranslationError> {
    let parent = path.parent().ok_or(TranslationError::InvalidDocumentPath)?;
    if !parent.is_dir() {
        return Err(TranslationError::MissingSessionDirectory);
    }
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, document)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

fn collect_documents(
    directory: &Path,
    depth: usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    if depth > 5 {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_documents(&entry.path(), depth + 1, output)?;
        } else if entry.file_name() == DOCUMENT_NAME {
            output.push(entry.path());
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum TranslationError {
    #[error(transparent)]
    Chat(#[from] ChatError),
    #[error("translation document path is invalid")]
    InvalidDocumentPath,
    #[error("failed to access translation: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse translation: {0}")]
    Json(#[from] serde_json::Error),
    #[error("translation session directory no longer exists")]
    MissingSessionDirectory,
    #[error("failed to persist translation: {0}")]
    Persist(#[from] tempfile::PersistError),
    #[error("translation provider credentials changed")]
    ProviderChanged,
    #[error("translation segment {0} is missing")]
    SegmentMissing(u32),
    #[error("translation belongs to a different recording session")]
    SessionMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        openai_compatible::test_support::ScriptedChatClient,
        settings::{
            AppSettings, AppSettingsWithoutSecrets, MeetingNotesSettingsWithoutSecrets,
            SecretUpdate, SettingsSecretUpdates, SettingsStore, TranslationSettingsWithoutSecrets,
        },
    };

    /// Blocks inside `complete` until the test releases it, so a delete can be
    /// interleaved with a request that is already on the wire.
    struct GatedChatClient {
        release: Mutex<Option<crossbeam_channel::Receiver<()>>>,
        requests: Mutex<usize>,
    }

    impl GatedChatClient {
        fn new() -> (Arc<Self>, crossbeam_channel::Sender<()>) {
            let (sender, receiver) = crossbeam_channel::bounded(1);
            (
                Arc::new(Self {
                    release: Mutex::new(Some(receiver)),
                    requests: Mutex::new(0),
                }),
                sender,
            )
        }

        fn wait_for_request(&self) {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while *self.requests.lock().unwrap() == 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "timed out waiting for the translation request"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl ChatCompletionPort for GatedChatClient {
        fn complete(&self, _request: ChatRequest<'_>) -> Result<String, ChatError> {
            *self.requests.lock().unwrap() += 1;
            let gate = self.release.lock().unwrap().take();
            if let Some(gate) = gate {
                let _ = gate.recv_timeout(Duration::from_secs(5));
            }
            Ok("translated".to_owned())
        }
    }

    /// Marks the session as deleting while its request is on the wire.
    struct MarkingChatClient {
        lifecycle: Arc<SessionLifecycle>,
        session_dir: PathBuf,
    }

    impl ChatCompletionPort for MarkingChatClient {
        fn complete(&self, _request: ChatRequest<'_>) -> Result<String, ChatError> {
            self.lifecycle.begin_delete(&self.session_dir).unwrap();
            Ok("translated".to_owned())
        }
    }

    fn inner_with(
        chat: Arc<dyn ChatCompletionPort>,
        lifecycle: Arc<SessionLifecycle>,
    ) -> TranslationInner {
        TranslationInner {
            chat,
            credentials: ProviderCredentials::default(),
            known_sessions: Mutex::new(HashSet::new()),
            lifecycle,
            observer: Arc::new(|_| {}),
            stop: AtomicBool::new(false),
        }
    }

    fn document(endpoint: &str, auth_mode: ProviderAuthMode) -> TranslationDocument {
        TranslationDocument {
            auth_mode,
            endpoint: endpoint.to_owned(),
            model: "Hy-MT2-1.8B".to_owned(),
            schema_version: 1,
            segments: Vec::new(),
            session_id: "session-1".to_owned(),
            status: TranslationDocumentStatus::Pending,
            target_language: "English".to_owned(),
            updated_at: Utc::now(),
        }
    }

    fn store_with_key(directory: &Path, endpoint: &str, value: &str) -> SettingsStore {
        let store = SettingsStore::open(directory.join("settings.json")).unwrap();
        let defaults = AppSettings::default();
        store
            .save(
                AppSettingsWithoutSecrets {
                    audio: defaults.audio,
                    desktop: defaults.desktop,
                    meeting_notes: MeetingNotesSettingsWithoutSecrets {
                        endpoint: defaults.meeting_notes.endpoint,
                        max_input_characters: defaults.meeting_notes.max_input_characters,
                        model: defaults.meeting_notes.model,
                        request_timeout_seconds: defaults.meeting_notes.request_timeout_seconds,
                    },
                    transcription: defaults.transcription,
                    translation: TranslationSettingsWithoutSecrets {
                        enabled: true,
                        endpoint: endpoint.to_owned(),
                        model: "Hy-MT2-1.8B".to_owned(),
                        target_language: "English".to_owned(),
                    },
                },
                SettingsSecretUpdates {
                    translation_api_key: SecretUpdate::Set {
                        value: value.to_owned(),
                    },
                    ..SettingsSecretUpdates::default()
                },
            )
            .unwrap();
        store
    }

    #[test]
    fn queued_translations_use_the_latest_runtime_language() {
        let directory = tempfile::tempdir().unwrap();
        let service = TranslationService::new(Arc::new(|_| {})).unwrap();
        let session = directory.path().join("session");
        fs::create_dir_all(&session).unwrap();
        let initial = TranslationSnapshot {
            endpoint: "http://127.0.0.1:9/v1".to_owned(),
            target_language: "Chinese".to_owned(),
            ..TranslationSnapshot::default()
        };
        service
            .enqueue(&session, "session-1", 1, "hello", &initial)
            .unwrap();
        let updated = TranslationSnapshot {
            model: "updated-model".to_owned(),
            target_language: "Japanese".to_owned(),
            ..initial
        };

        service
            .enqueue(&session, "session-1", 2, "hello again", &updated)
            .unwrap();

        let document = load_document(&session.join(DOCUMENT_NAME)).unwrap();
        assert_eq!(document.model, "updated-model");
        assert_eq!(document.target_language, "Japanese");
        assert_eq!(document.auth_mode, ProviderAuthMode::None);
    }

    #[test]
    fn sends_the_task_prompt_without_credentials_when_auth_is_disabled() {
        let chat = ScriptedChatClient::new(vec![Ok("Hello".to_owned())]);

        let translated = translate_text(
            &chat,
            &ProviderCredentials::default(),
            &document("http://127.0.0.1:8000/v1", ProviderAuthMode::None),
            "你好",
        )
        .unwrap();

        let requests = chat.requests();
        assert_eq!(translated, "Hello");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].api_key, None);
        assert_eq!(requests[0].model, "Hy-MT2-1.8B");
        assert!(requests[0].messages[0].1.contains("English"));
        assert_eq!(requests[0].messages[1].1, "你好");
    }

    #[test]
    fn resolves_only_the_credential_bound_to_the_task_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_with_key(directory.path(), "https://first.example/v1", "first-key");
        let credentials = store.credentials();
        let chat = ScriptedChatClient::new(vec![Ok("Hello".to_owned())]);

        translate_text(
            &chat,
            &credentials,
            &document("https://first.example/v1", ProviderAuthMode::Bearer),
            "你好",
        )
        .unwrap();

        assert_eq!(chat.requests()[0].api_key.as_deref(), Some("first-key"));
        let error = translate_text(
            &chat,
            &credentials,
            &document("https://second.example/v1", ProviderAuthMode::Bearer),
            "你好",
        )
        .unwrap_err();
        assert!(matches!(error, TranslationError::ProviderChanged));
        assert_eq!(chat.requests().len(), 1);
    }

    #[test]
    fn refuses_to_recreate_a_deleted_session_directory() {
        let directory = tempfile::tempdir().unwrap();
        let session = directory.path().join("session");

        let error = save_document(
            &session.join(DOCUMENT_NAME),
            &document("http://127.0.0.1:8000/v1", ProviderAuthMode::None),
        )
        .unwrap_err();

        assert!(matches!(error, TranslationError::MissingSessionDirectory));
        assert!(!session.exists());
    }

    #[test]
    fn a_response_that_arrives_after_a_delete_cannot_recreate_the_session() {
        let directory = tempfile::tempdir().unwrap();
        let session = directory.path().join("session");
        fs::create_dir_all(&session).unwrap();
        let lifecycle = SessionLifecycle::new();
        let (chat, release) = GatedChatClient::new();
        let service = TranslationService::with_chat_client(
            Arc::new(|_| {}),
            ProviderCredentials::default(),
            chat.clone(),
            lifecycle.clone(),
        )
        .unwrap();
        service
            .enqueue(
                &session,
                "session-1",
                1,
                "hello",
                &TranslationSnapshot {
                    enabled: true,
                    endpoint: "http://127.0.0.1:8000/v1".to_owned(),
                    ..TranslationSnapshot::default()
                },
            )
            .unwrap();
        chat.wait_for_request();

        lifecycle.begin_delete(&session).unwrap();
        service.forget(&session);
        lifecycle.finish_delete(&session).unwrap();
        release.send(()).unwrap();
        drop(service);

        assert!(!session.exists());
        assert!(!session.join(DOCUMENT_NAME).exists());
    }

    /// The marker alone drops the result: the directory is still there, so only
    /// the tombstone check under the lifecycle lock can refuse this write.
    #[test]
    fn a_marked_session_drops_a_late_translation_while_its_directory_still_exists() {
        let directory = tempfile::tempdir().unwrap();
        let session = directory.path().join("session");
        fs::create_dir_all(&session).unwrap();
        let lifecycle = SessionLifecycle::new();
        let inner = inner_with(
            Arc::new(MarkingChatClient {
                lifecycle: lifecycle.clone(),
                session_dir: session.clone(),
            }),
            lifecycle,
        );
        let mut pending = document("http://127.0.0.1:8000/v1", ProviderAuthMode::None);
        pending.segments.push(TranslationSegment {
            attempts: 0,
            error: None,
            segment_id: 1,
            source_text: "你好".to_owned(),
            status: TranslationSegmentStatus::Pending,
            text: String::new(),
        });
        save_document(&session.join(DOCUMENT_NAME), &pending).unwrap();

        assert!(!process_session(&inner, &session).unwrap());

        let stored = load_document(&session.join(DOCUMENT_NAME)).unwrap();
        assert_eq!(
            stored.segments[0].status,
            TranslationSegmentStatus::Processing
        );
        assert!(stored.segments[0].text.is_empty());
    }

    #[test]
    fn a_deleting_session_is_never_enqueued_again() {
        let directory = tempfile::tempdir().unwrap();
        let session = directory.path().join("session");
        fs::create_dir_all(&session).unwrap();
        let lifecycle = SessionLifecycle::new();
        let service = TranslationService::with_chat_client(
            Arc::new(|_| {}),
            ProviderCredentials::default(),
            Arc::new(ScriptedChatClient::new(Vec::new())),
            lifecycle.clone(),
        )
        .unwrap();
        lifecycle.begin_delete(&session).unwrap();

        let queued = service
            .enqueue(
                &session,
                "session-1",
                1,
                "hello",
                &TranslationSnapshot {
                    enabled: true,
                    endpoint: "http://127.0.0.1:8000/v1".to_owned(),
                    ..TranslationSnapshot::default()
                },
            )
            .unwrap();

        assert_eq!(queued, None);
        assert!(!session.join(DOCUMENT_NAME).exists());
    }

    #[test]
    fn records_only_local_error_text_for_a_failed_segment() {
        let directory = tempfile::tempdir().unwrap();
        let session = directory.path().join("session");
        fs::create_dir_all(&session).unwrap();
        let inner = inner_with(
            Arc::new(ScriptedChatClient::new(vec![Err(ChatError::Unauthorized)])),
            SessionLifecycle::new(),
        );
        let mut pending = document("http://127.0.0.1:8000/v1", ProviderAuthMode::None);
        pending.segments.push(TranslationSegment {
            attempts: 0,
            error: None,
            segment_id: 1,
            source_text: "你好".to_owned(),
            status: TranslationSegmentStatus::Pending,
            text: String::new(),
        });
        save_document(&session.join(DOCUMENT_NAME), &pending).unwrap();

        process_session(&inner, &session).unwrap();

        let stored = load_document(&session.join(DOCUMENT_NAME)).unwrap();
        assert_eq!(stored.segments[0].status, TranslationSegmentStatus::Failed);
        assert_eq!(
            stored.segments[0].error.as_deref(),
            Some("chat endpoint rejected the credentials")
        );
    }
}
