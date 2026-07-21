use std::{
    collections::{HashMap, HashSet},
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
use reqwest::{StatusCode, Url, blocking::Client};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::settings::TranslationSettings;

pub const DOCUMENT_NAME: &str = "translation.json";
const MAX_ATTEMPTS: u32 = 3;
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
    client: Client,
    document_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    known_sessions: Mutex<HashSet<PathBuf>>,
    observer: TranslationObserver,
    stop: AtomicBool,
}

impl TranslationService {
    pub fn new(observer: TranslationObserver) -> Result<Self, TranslationError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(60))
            .build()?;
        let inner = Arc::new(TranslationInner {
            client,
            document_locks: Mutex::new(HashMap::new()),
            known_sessions: Mutex::new(HashSet::new()),
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
        settings: &TranslationSettings,
    ) -> Result<Option<TranslationSegment>, TranslationError> {
        if !settings.enabled || source_text.trim().is_empty() {
            return Ok(None);
        }
        let lock = self.inner.document_lock(session_dir);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = session_dir.join(DOCUMENT_NAME);
        let mut document = if path.exists() {
            let mut document = load_document(&path)?;
            document.endpoint = settings.endpoint.trim().to_owned();
            document.model = settings.model.trim().to_owned();
            document.target_language = settings.target_language.trim().to_owned();
            document
        } else {
            TranslationDocument {
                endpoint: settings.endpoint.trim().to_owned(),
                model: settings.model.trim().to_owned(),
                schema_version: 1,
                segments: Vec::new(),
                session_id: session_id.to_owned(),
                status: TranslationDocumentStatus::Pending,
                target_language: settings.target_language.trim().to_owned(),
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
        self.document_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(session_dir.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
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
    if !path.exists() {
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
        &inner.client,
        &document.endpoint,
        &document.model,
        &document.target_language,
        &processing.source_text,
    );

    let (session_id, resolved, retryable) = {
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
    client: &Client,
    endpoint: &str,
    model: &str,
    target_language: &str,
    source_text: &str,
) -> Result<String, TranslationError> {
    let url = chat_completions_url(endpoint)?;
    let body = serde_json::json!({
        "model": model,
        "temperature": 0,
        "messages": [
            {
                "role": "system",
                "content": format!(
                    "Translate the user's text into {target_language}. Preserve meaning, names, numbers, and formatting. Return only the translation."
                )
            },
            { "role": "user", "content": source_text }
        ]
    });
    let response = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()?;
    let status = response.status();
    let response_body = response.text()?;
    if !status.is_success() {
        return Err(TranslationError::HttpStatus(
            status,
            response_body.chars().take(500).collect(),
        ));
    }
    parse_translation_response(&response_body)
}

fn chat_completions_url(endpoint: &str) -> Result<Url, TranslationError> {
    let mut url = Url::parse(endpoint.trim()).map_err(|_| TranslationError::InvalidEndpoint)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(TranslationError::InvalidEndpoint);
    }
    let base_path = url.path().trim_end_matches('/');
    let path = if base_path.ends_with("/chat/completions") {
        base_path.to_owned()
    } else if base_path.is_empty() {
        "/chat/completions".to_owned()
    } else {
        format!("{base_path}/chat/completions")
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn parse_translation_response(body: &str) -> Result<String, TranslationError> {
    #[derive(Deserialize)]
    struct Response {
        choices: Vec<Choice>,
    }
    #[derive(Deserialize)]
    struct Choice {
        message: Message,
    }
    #[derive(Deserialize)]
    struct Message {
        content: String,
    }

    let parsed: Response = serde_json::from_str(body)?;
    let text = parsed
        .choices
        .first()
        .map(|choice| choice.message.content.trim())
        .filter(|text| !text.is_empty())
        .ok_or(TranslationError::EmptyResponse)?;
    Ok(text.to_owned())
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

fn save_document(path: &Path, document: &TranslationDocument) -> Result<(), TranslationError> {
    let parent = path.parent().ok_or(TranslationError::InvalidDocumentPath)?;
    fs::create_dir_all(parent)?;
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
    #[error("translation endpoint returned an empty response")]
    EmptyResponse,
    #[error("translation endpoint returned HTTP {0}: {1}")]
    HttpStatus(StatusCode, String),
    #[error("translation document path is invalid")]
    InvalidDocumentPath,
    #[error("translation endpoint must be a valid http or https URL")]
    InvalidEndpoint,
    #[error("failed to access translation: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse translation: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to persist translation: {0}")]
    Persist(#[from] tempfile::PersistError),
    #[error("translation request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("translation segment {0} is missing")]
    SegmentMissing(u32),
    #[error("translation belongs to a different recording session")]
    SessionMismatch,
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    use super::*;

    #[test]
    fn builds_chat_completions_url_from_base_or_full_endpoint() {
        assert_eq!(
            chat_completions_url("http://127.0.0.1:8000/v1")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:8000/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://localhost:8000/v1/chat/completions/")
                .unwrap()
                .as_str(),
            "http://localhost:8000/v1/chat/completions"
        );
    }

    #[test]
    fn parses_openai_chat_completion_content() {
        let body = r#"{"choices":[{"message":{"content":"  Hello world.  "}}]}"#;

        assert_eq!(parse_translation_response(body).unwrap(), "Hello world.");
    }

    #[test]
    fn rejects_empty_chat_completion_content() {
        let body = r#"{"choices":[{"message":{"content":"  "}}]}"#;

        assert!(matches!(
            parse_translation_response(body),
            Err(TranslationError::EmptyResponse)
        ));
    }

    #[test]
    fn queued_translations_use_the_latest_runtime_language() {
        let directory = tempfile::tempdir().unwrap();
        let service = TranslationService::new(Arc::new(|_| {})).unwrap();
        let session = directory.path().join("session");
        fs::create_dir_all(&session).unwrap();
        let initial = TranslationSettings {
            endpoint: "http://127.0.0.1:9/v1".to_owned(),
            target_language: "Chinese".to_owned(),
            ..TranslationSettings::default()
        };
        service
            .enqueue(&session, "session-1", 1, "hello", &initial)
            .unwrap();
        let updated = TranslationSettings {
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
    }

    #[test]
    fn sends_openai_compatible_translation_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 2_048];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                let Some(header_end) = request.windows(4).position(|item| item == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1"));
            assert!(request.contains("\"model\":\"Hy-MT2-1.8B\""));
            assert!(request.contains("\"content\":\"你好\""));

            let body = r#"{"choices":[{"message":{"content":"Hello"}}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let client = Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();

        let translated = translate_text(
            &client,
            &format!("http://{address}/v1"),
            "Hy-MT2-1.8B",
            "English",
            "你好",
        )
        .unwrap();

        assert_eq!(translated, "Hello");
        server.join().unwrap();
    }
}
