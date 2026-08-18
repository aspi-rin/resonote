use std::{
    collections::{HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    meeting_notes_document::{
        AnalysisFreshness, AnalysisState, CheckpointStage, CheckpointUnit, CheckpointUnitState,
        CleanCheckpoint, CleanPartResult, DOCUMENT_NAME, GlobalContextDocument, GlobalContextStore,
        InputSnapshot, MeetingNotesDocument, MeetingNotesError, MeetingNotesErrorCode,
        MeetingNotesRun, MeetingNotesRunView, OutputLanguage, ProviderSnapshot, RunProgress,
        RunStage, SessionAnalysisView, StaleReason, SummaryCheckpoint, SummaryMode, load_document,
        new_document, save_document,
    },
    meeting_notes_pipeline::{
        CleanChunk, ContextInputs, TranscriptSource, build_context_snapshot, build_input_snapshot,
        build_provider_snapshot, derive_freshness, evaluate_readiness, input_fingerprint,
        plan_clean_chunks, run_fingerprint, select_source_snapshot, sha256,
    },
    openai_compatible::{ChatCompletionPort, ReqwestChatClient},
    session_catalog::SessionCatalog,
    session_lifecycle::SessionLifecycle,
    settings::{ProviderAuthMode, ProviderCredentials, SecretString},
    storage::SessionManifest,
    transcription::TranscriptDocument,
};

#[path = "meeting_notes_worker.rs"]
mod worker;

const MANIFEST_NAME: &str = "session.json";
const MAX_ATTEMPTS: u32 = 3;
const MAX_SCAN_DEPTH: usize = 5;
const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(5)];
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const TRANSCRIPT_NAME: &str = "transcript.json";
const WORKER_POLL_INTERVAL: Duration = Duration::from_secs(1);

pub type MeetingNotesObserver = Arc<dyn Fn(MeetingNotesStatusEvent) + Send + Sync + 'static>;
/// Injected so tests exhaust the retry policy without waiting in real time.
pub type RetrySleeper = Arc<dyn Fn(Duration) + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GenerateMode {
    Ensure,
    Regenerate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateRequest {
    pub accept_partial: bool,
    pub expected_global_context_revision: u64,
    pub expected_meeting_context_revision: u64,
    pub mode: GenerateMode,
    pub output_language: OutputLanguage,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryRequest {
    pub expected_run_fingerprint: String,
    pub job_id: String,
    pub output_language: OutputLanguage,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelRequest {
    pub expected_run_fingerprint: String,
    pub job_id: String,
    pub output_language: OutputLanguage,
    pub session_id: String,
}

/// Task metadata only: cleaned text, summaries, context and credentials are read
/// through the explicit commands instead of being broadcast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingNotesStatusEvent {
    pub completed_chunks: u32,
    pub document_revision: u64,
    pub error_code: Option<MeetingNotesErrorCode>,
    pub generation: u64,
    pub job_id: String,
    pub retryable: bool,
    pub session_id: String,
    pub stage: Option<RunStage>,
    pub state: AnalysisState,
    pub total_chunks: u32,
    pub updated_at: chrono::DateTime<Utc>,
}

/// Credential and identity of one queued execution. The bearer key lives here
/// only, never in `analysis.json`, and is dropped when the run ends.
#[derive(Clone)]
struct JobTicket {
    api_key: Option<SecretString>,
    generation: u64,
    job_id: String,
    run_fingerprint: String,
    session_dir: PathBuf,
    session_id: String,
}

struct Prepared {
    event: Option<MeetingNotesStatusEvent>,
    ticket: Option<JobTicket>,
    view: SessionAnalysisView,
}

pub struct MeetingNotesService {
    ack: crossbeam_channel::Receiver<()>,
    inner: Arc<MeetingNotesInner>,
    wake: crossbeam_channel::Sender<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct MeetingNotesInner {
    catalog: Arc<SessionCatalog>,
    chat: Arc<dyn ChatCompletionPort>,
    corrupt: Mutex<HashSet<PathBuf>>,
    credentials: ProviderCredentials,
    global_context: Arc<GlobalContextStore>,
    lifecycle: Arc<SessionLifecycle>,
    observer: MeetingNotesObserver,
    queue: Mutex<VecDeque<JobTicket>>,
    sleeper: RetrySleeper,
    stop: AtomicBool,
}

impl MeetingNotesService {
    pub fn new(
        catalog: Arc<SessionCatalog>,
        global_context: Arc<GlobalContextStore>,
        lifecycle: Arc<SessionLifecycle>,
        credentials: ProviderCredentials,
        observer: MeetingNotesObserver,
    ) -> Result<Self, MeetingNotesError> {
        let chat = Arc::new(
            ReqwestChatClient::new().map_err(|error| MeetingNotesError::io().with_source(error))?,
        );
        Self::with_chat_client(
            catalog,
            global_context,
            lifecycle,
            credentials,
            observer,
            chat,
            Arc::new(thread::sleep),
        )
    }

    pub fn with_chat_client(
        catalog: Arc<SessionCatalog>,
        global_context: Arc<GlobalContextStore>,
        lifecycle: Arc<SessionLifecycle>,
        credentials: ProviderCredentials,
        observer: MeetingNotesObserver,
        chat: Arc<dyn ChatCompletionPort>,
        sleeper: RetrySleeper,
    ) -> Result<Self, MeetingNotesError> {
        let inner = Arc::new(MeetingNotesInner {
            catalog,
            chat,
            corrupt: Mutex::new(HashSet::new()),
            credentials,
            global_context,
            lifecycle,
            observer,
            queue: Mutex::new(VecDeque::new()),
            sleeper,
            stop: AtomicBool::new(false),
        });
        let (wake, receiver) = crossbeam_channel::bounded(1);
        let (acknowledge, ack) = crossbeam_channel::bounded(1);
        let worker_inner = inner.clone();
        let worker = thread::Builder::new()
            .name("resonote-meeting-notes".to_owned())
            .spawn(move || worker::worker_loop(worker_inner, receiver, acknowledge))?;
        Ok(Self {
            ack,
            inner,
            wake,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn generate(
        &self,
        request: GenerateRequest,
    ) -> Result<SessionAnalysisView, MeetingNotesError> {
        let session_dir = self.inner.resolve(&request.session_id)?;
        let lock = self.inner.session_lock(&session_dir);
        let prepared = {
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            self.inner.prepare_run(&session_dir, &request)?
        };
        Ok(self.dispatch(prepared))
    }

    pub fn retry(&self, request: RetryRequest) -> Result<SessionAnalysisView, MeetingNotesError> {
        let session_dir = self.inner.resolve(&request.session_id)?;
        let lock = self.inner.session_lock(&session_dir);
        let prepared = {
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            self.inner.prepare_retry(&session_dir, &request)?
        };
        Ok(self.dispatch(prepared))
    }

    pub fn cancel(&self, request: CancelRequest) -> Result<SessionAnalysisView, MeetingNotesError> {
        let session_dir = self.inner.resolve(&request.session_id)?;
        let lock = self.inner.session_lock(&session_dir);
        let prepared = {
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            self.inner.prepare_cancel(&session_dir, &request)?
        };
        Ok(self.dispatch(prepared))
    }

    pub fn load(
        &self,
        session_id: &str,
        output_language: OutputLanguage,
    ) -> Result<SessionAnalysisView, MeetingNotesError> {
        let session_dir = self.inner.resolve(session_id)?;
        let lock = self.inner.session_lock(&session_dir);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let document = self.inner.read_document(&session_dir, session_id)?;
        Ok(self.inner.view(&session_dir, &document, output_language))
    }

    /// Drops the queued work and the in-memory credentials of one session. It is
    /// called between the `.deleting` marker and the directory removal: the
    /// tombstone the marker installed is what cancels a run that is already
    /// waiting for the provider, so nothing here waits for that response.
    pub fn forget(&self, session_id: &str) -> Result<(), MeetingNotesError> {
        let dropped = {
            let mut queue = self
                .inner
                .queue
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let before = queue.len();
            queue.retain(|ticket| ticket.session_id != session_id);
            before - queue.len()
        };
        if dropped > 0 {
            tracing::info!(dropped, session_id, "dropped queued meeting notes jobs");
        }
        Ok(())
    }

    pub fn recover_catalog(&self) -> Result<usize, MeetingNotesError> {
        let mut recovered = 0;
        for root in self.inner.catalog.roots() {
            recovered += self.recover_root(&root)?;
        }
        if recovered > 0 {
            let _ = self.wake.try_send(());
        }
        Ok(recovered)
    }

    /// Sets the stop flag and waits for the worker acknowledgement at most two
    /// seconds; an in-flight request is left to the process exit.
    pub fn shutdown(&self) {
        let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return;
        };
        self.inner.stop.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        if self.ack.recv_timeout(SHUTDOWN_TIMEOUT).is_ok() {
            let _ = worker.join();
        } else {
            tracing::warn!("meeting notes worker did not stop within the shutdown budget");
        }
    }

    fn recover_root(&self, root: &Path) -> Result<usize, MeetingNotesError> {
        if !root.is_dir() {
            return Ok(0);
        }
        let mut paths = Vec::new();
        collect_documents(root, 0, &mut paths)?;
        let mut recovered = 0;
        let mut events = Vec::new();
        for path in paths {
            let Some(session_dir) = path.parent().map(Path::to_path_buf) else {
                continue;
            };
            if self.inner.lifecycle.is_deleting(&session_dir) {
                continue;
            }
            let lock = self.inner.session_lock(&session_dir);
            let outcome = {
                let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                self.inner.recover_document(&session_dir, &path)
            };
            match outcome {
                Ok(Some(prepared)) => {
                    if let Some(ticket) = prepared.ticket {
                        self.inner.enqueue(ticket);
                        recovered += 1;
                    }
                    events.extend(prepared.event);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(?error, "meeting notes recovery skipped a session");
                }
            }
        }
        for event in events {
            self.inner.publish(event);
        }
        Ok(recovered)
    }

    fn dispatch(&self, prepared: Prepared) -> SessionAnalysisView {
        if let Some(ticket) = prepared.ticket {
            self.inner.enqueue(ticket);
            let _ = self.wake.try_send(());
        }
        if let Some(event) = prepared.event {
            self.inner.publish(event);
        }
        prepared.view
    }
}

impl Drop for MeetingNotesService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl MeetingNotesInner {
    /// The shared lifecycle lock is this service's document lock too, so one
    /// mutex per session orders every commit against the delete.
    fn session_lock(&self, session_dir: &Path) -> Arc<Mutex<()>> {
        self.lifecycle.session_lock(session_dir)
    }

    fn resolve(&self, session_id: &str) -> Result<PathBuf, MeetingNotesError> {
        let session_dir = self.catalog.resolve(session_id)?;
        if self.lifecycle.is_deleting(&session_dir) {
            return Err(MeetingNotesError::new(
                MeetingNotesErrorCode::SessionDeleted,
            ));
        }
        Ok(session_dir)
    }

    fn enqueue(&self, ticket: JobTicket) {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(ticket);
    }

    fn next_job(&self) -> Option<JobTicket> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
    }

    fn publish(&self, event: MeetingNotesStatusEvent) {
        (self.observer)(event);
    }

    fn prepare_run(
        &self,
        session_dir: &Path,
        request: &GenerateRequest,
    ) -> Result<Prepared, MeetingNotesError> {
        // Generating rebuilds the derived document, so a quarantined predecessor
        // stops blocking the session; a file that is still unparsable fails below.
        self.clear_corrupt(session_dir);
        let mut document = self.read_document(session_dir, &request.session_id)?;
        let global = self.global_context.document();
        if global.revision != request.expected_global_context_revision
            || document.meeting_context.revision != request.expected_meeting_context_revision
        {
            return Err(MeetingNotesError::new(
                MeetingNotesErrorCode::ContextRevisionConflict,
            ));
        }
        let snapshot = self.build_inputs(
            session_dir,
            &document,
            &global,
            request.accept_partial,
            request.output_language,
        )?;
        let fingerprint = input_fingerprint(&snapshot);
        let reusable = match document.current_run.as_ref() {
            Some(run) if is_live(run.state) => {
                if request.mode != GenerateMode::Ensure || run.input_fingerprint != fingerprint {
                    return Err(MeetingNotesError::new(MeetingNotesErrorCode::AnalysisBusy));
                }
                true
            }
            _ => {
                request.mode == GenerateMode::Ensure
                    && document
                        .last_successful_result
                        .as_ref()
                        .is_some_and(|result| result.input_fingerprint == fingerprint)
            }
        };
        if reusable {
            return Ok(Prepared {
                event: None,
                ticket: None,
                view: view_of(&document, &snapshot),
            });
        }
        let api_key = self.resolve_key(&snapshot.provider)?;
        let chunks = plan_clean_chunks(&snapshot)?;
        let previous = document
            .current_run
            .as_ref()
            .filter(|run| run.input_fingerprint == fingerprint);
        let regeneration_nonce = previous.map_or(0, |run| run.regeneration_nonce + 1);
        let generation = document
            .current_run
            .as_ref()
            .map_or(1, |run| run.generation + 1);
        let signature = run_fingerprint(&fingerprint, regeneration_nonce);
        let job_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        document.current_run = Some(MeetingNotesRun {
            accept_partial: request.accept_partial,
            cancellation_requested: false,
            clean_checkpoint: CleanCheckpoint {
                units: chunks.iter().map(clean_unit).collect(),
            },
            created_at: now,
            error: None,
            generation,
            input_fingerprint: fingerprint,
            input_snapshot: snapshot.clone(),
            job_id: job_id.clone(),
            progress: RunProgress {
                completed_chunks: 0,
                total_chunks: chunks.len() as u32,
            },
            regeneration_nonce,
            run_fingerprint: signature.clone(),
            stage: None,
            state: AnalysisState::Queued,
            summary_checkpoint: empty_summary_checkpoint(),
            updated_at: now,
        });
        document.document_revision += 1;
        document.updated_at = now;
        save_document(&session_dir.join(DOCUMENT_NAME), &document)?;
        self.clear_corrupt(session_dir);
        Ok(Prepared {
            event: status_event(&request.session_id, &document),
            ticket: Some(JobTicket {
                api_key,
                generation,
                job_id,
                run_fingerprint: signature,
                session_dir: session_dir.to_path_buf(),
                session_id: request.session_id.clone(),
            }),
            view: view_of(&document, &snapshot),
        })
    }

    fn prepare_retry(
        &self,
        session_dir: &Path,
        request: &RetryRequest,
    ) -> Result<Prepared, MeetingNotesError> {
        let mut document = self.read_document(session_dir, &request.session_id)?;
        let run = document
            .current_run
            .as_mut()
            .filter(|run| {
                run.job_id == request.job_id
                    && run.run_fingerprint == request.expected_run_fingerprint
                    && matches!(run.state, AnalysisState::Cancelled | AnalysisState::Failed)
            })
            .ok_or_else(|| MeetingNotesError::new(MeetingNotesErrorCode::AnalysisBusy))?;
        let api_key = self.ensure_provider_unchanged(&run.input_snapshot.provider)?;
        reset_pending_units(&mut run.clean_checkpoint.units);
        reset_pending_units(&mut run.summary_checkpoint.units);
        run.cancellation_requested = false;
        run.error = None;
        run.generation += 1;
        run.state = AnalysisState::Queued;
        let ticket = JobTicket {
            api_key,
            generation: run.generation,
            job_id: run.job_id.clone(),
            run_fingerprint: run.run_fingerprint.clone(),
            session_dir: session_dir.to_path_buf(),
            session_id: request.session_id.clone(),
        };
        let now = Utc::now();
        run.updated_at = now;
        document.document_revision += 1;
        document.updated_at = now;
        save_document(&session_dir.join(DOCUMENT_NAME), &document)?;
        Ok(Prepared {
            event: status_event(&request.session_id, &document),
            ticket: Some(ticket),
            view: self.view(session_dir, &document, request.output_language),
        })
    }

    fn prepare_cancel(
        &self,
        session_dir: &Path,
        request: &CancelRequest,
    ) -> Result<Prepared, MeetingNotesError> {
        let mut document = self.read_document(session_dir, &request.session_id)?;
        let run = document
            .current_run
            .as_mut()
            .filter(|run| {
                run.job_id == request.job_id
                    && run.run_fingerprint == request.expected_run_fingerprint
            })
            .ok_or_else(|| MeetingNotesError::new(MeetingNotesErrorCode::AnalysisBusy))?;
        if !is_live(run.state) || run.cancellation_requested {
            return Ok(Prepared {
                event: None,
                ticket: None,
                view: self.view(session_dir, &document, request.output_language),
            });
        }
        let now = Utc::now();
        run.cancellation_requested = true;
        run.updated_at = now;
        document.document_revision += 1;
        document.updated_at = now;
        save_document(&session_dir.join(DOCUMENT_NAME), &document)?;
        Ok(Prepared {
            event: status_event(&request.session_id, &document),
            ticket: None,
            view: self.view(session_dir, &document, request.output_language),
        })
    }

    /// Quarantines an unparsable document, restarts `queued` runs and rewinds
    /// unfinished ones to their first uncommitted unit.
    fn recover_document(
        &self,
        session_dir: &Path,
        path: &Path,
    ) -> Result<Option<Prepared>, MeetingNotesError> {
        let Ok(mut document) = load_document(path) else {
            let quarantined = path.with_file_name(format!(
                "{DOCUMENT_NAME}.corrupt-{}",
                Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
            ));
            fs::rename(path, &quarantined)?;
            self.mark_corrupt(session_dir);
            tracing::warn!(?quarantined, "quarantined a corrupt meeting notes document");
            return Ok(None);
        };
        let session_id = document.session_id.clone();
        let Some(run) = document
            .current_run
            .as_mut()
            .filter(|run| is_live(run.state))
        else {
            return Ok(None);
        };
        let api_key = match self.ensure_provider_unchanged(&run.input_snapshot.provider) {
            Ok(api_key) => api_key,
            Err(error) => {
                run.error = Some(worker::run_error(error.code(), None, None, run.stage));
                run.state = AnalysisState::Failed;
                run.updated_at = Utc::now();
                document.document_revision += 1;
                document.updated_at = Utc::now();
                save_document(path, &document)?;
                return Ok(Some(Prepared {
                    event: status_event(&session_id, &document),
                    ticket: None,
                    view: self.view(session_dir, &document, OutputLanguage::EnUs),
                }));
            }
        };
        let mut checkpoint_corrupt = rewind_units(&mut run.clean_checkpoint.units);
        checkpoint_corrupt |= rewind_units(&mut run.summary_checkpoint.units);
        if checkpoint_corrupt {
            run.error = Some(worker::run_error(
                MeetingNotesErrorCode::AnalysisCheckpointCorrupt,
                None,
                None,
                run.stage,
            ));
        }
        let ticket = JobTicket {
            api_key,
            generation: run.generation,
            job_id: run.job_id.clone(),
            run_fingerprint: run.run_fingerprint.clone(),
            session_dir: session_dir.to_path_buf(),
            session_id: session_id.clone(),
        };
        let now = Utc::now();
        run.updated_at = now;
        document.document_revision += 1;
        document.updated_at = now;
        save_document(path, &document)?;
        Ok(Some(Prepared {
            event: status_event(&session_id, &document),
            ticket: Some(ticket),
            view: self.view(session_dir, &document, OutputLanguage::EnUs),
        }))
    }

    fn read_document(
        &self,
        session_dir: &Path,
        session_id: &str,
    ) -> Result<MeetingNotesDocument, MeetingNotesError> {
        if self
            .corrupt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(session_dir)
        {
            return Err(MeetingNotesError::new(
                MeetingNotesErrorCode::AnalysisDocumentCorrupt,
            ));
        }
        let path = session_dir.join(DOCUMENT_NAME);
        if !path.exists() {
            return Ok(new_document(session_id));
        }
        let document = load_document(&path).inspect_err(|_| self.mark_corrupt(session_dir))?;
        if document.session_id != session_id {
            self.mark_corrupt(session_dir);
            return Err(MeetingNotesError::new(
                MeetingNotesErrorCode::AnalysisDocumentCorrupt,
            ));
        }
        Ok(document)
    }

    fn mark_corrupt(&self, session_dir: &Path) {
        self.corrupt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_dir.to_path_buf());
    }

    fn clear_corrupt(&self, session_dir: &Path) {
        self.corrupt
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_dir);
    }

    fn view(
        &self,
        session_dir: &Path,
        document: &MeetingNotesDocument,
        output_language: OutputLanguage,
    ) -> SessionAnalysisView {
        let global = self.global_context.document();
        match self.build_inputs(session_dir, document, &global, true, output_language) {
            Ok(snapshot) => view_of(document, &snapshot),
            Err(error) => stale_view_of(document, error.code()),
        }
    }

    /// The freshly normalized inputs behind both the run snapshot and the
    /// freshness derivation, so neither ever reads a persisted verdict.
    fn build_inputs(
        &self,
        session_dir: &Path,
        document: &MeetingNotesDocument,
        global: &GlobalContextDocument,
        accept_partial: bool,
        output_language: OutputLanguage,
    ) -> Result<InputSnapshot, MeetingNotesError> {
        let manifest = read_manifest(session_dir)?;
        let transcript = read_transcript(session_dir);
        let decision = evaluate_readiness(manifest.status, transcript.as_source(), accept_partial)?;
        let TranscriptFile::Present(document_transcript) = &transcript else {
            return Err(MeetingNotesError::new(
                MeetingNotesErrorCode::NoTranscriptContent,
            ));
        };
        let source = select_source_snapshot(&manifest, document_transcript, decision)?;
        let context = build_context_snapshot(ContextInputs {
            captured_at: Utc::now(),
            global: &global.content,
            global_revision: global.revision,
            meeting: &document.meeting_context.content,
            meeting_revision: document.meeting_context.revision,
        })?;
        let provider = build_provider_snapshot(&self.credentials.meeting_notes())?;
        Ok(build_input_snapshot(
            context,
            output_language,
            provider,
            source,
        ))
    }

    fn ensure_provider_unchanged(
        &self,
        snapshot: &ProviderSnapshot,
    ) -> Result<Option<SecretString>, MeetingNotesError> {
        let current = build_provider_snapshot(&self.credentials.meeting_notes())?;
        if current.endpoint != snapshot.endpoint || current.model != snapshot.model {
            return Err(MeetingNotesError::new(
                MeetingNotesErrorCode::ProviderChanged,
            ));
        }
        self.resolve_key(snapshot)
    }

    fn resolve_key(
        &self,
        snapshot: &ProviderSnapshot,
    ) -> Result<Option<SecretString>, MeetingNotesError> {
        match snapshot.auth_mode {
            ProviderAuthMode::None => Ok(None),
            ProviderAuthMode::Bearer => self
                .credentials
                .meeting_notes_key(&snapshot.endpoint)
                .map(Some)
                .ok_or_else(|| MeetingNotesError::new(MeetingNotesErrorCode::ProviderChanged)),
        }
    }
}

enum TranscriptFile {
    Corrupt,
    Missing,
    Present(TranscriptDocument),
}

impl TranscriptFile {
    fn as_source(&self) -> TranscriptSource<'_> {
        match self {
            Self::Corrupt => TranscriptSource::Corrupt,
            Self::Missing => TranscriptSource::Missing,
            Self::Present(document) => TranscriptSource::Present(document),
        }
    }
}

fn read_manifest(session_dir: &Path) -> Result<SessionManifest, MeetingNotesError> {
    let bytes = fs::read(session_dir.join(MANIFEST_NAME))
        .map_err(|_| MeetingNotesError::new(MeetingNotesErrorCode::SessionNotFound))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        MeetingNotesError::new(MeetingNotesErrorCode::SessionNotFound).with_source(error)
    })
}

fn read_transcript(session_dir: &Path) -> TranscriptFile {
    let path = session_dir.join(TRANSCRIPT_NAME);
    if !path.exists() {
        return TranscriptFile::Missing;
    }
    match fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<TranscriptDocument>(&bytes).ok())
    {
        Some(document) => TranscriptFile::Present(document),
        None => TranscriptFile::Corrupt,
    }
}

fn view_of(document: &MeetingNotesDocument, current: &InputSnapshot) -> SessionAnalysisView {
    let freshness = derive_freshness(document.last_successful_result.as_ref(), current);
    SessionAnalysisView {
        current_run: document.current_run.as_ref().map(run_view),
        document_revision: document.document_revision,
        freshness: freshness.freshness,
        last_successful_result: document.last_successful_result.clone(),
        meeting_context: document.meeting_context.clone(),
        session_id: document.session_id.clone(),
        stale_reasons: freshness.stale_reasons,
    }
}

/// Reading the current inputs failed, so a stored result cannot be reproduced:
/// it stays readable and the blocked input is reported as the stale reason.
fn stale_view_of(
    document: &MeetingNotesDocument,
    code: MeetingNotesErrorCode,
) -> SessionAnalysisView {
    let stale_reasons = match document.last_successful_result {
        None => Vec::new(),
        Some(_) => vec![match code {
            MeetingNotesErrorCode::MeetingNotesNotConfigured
            | MeetingNotesErrorCode::InsecureEndpoint
            | MeetingNotesErrorCode::InvalidEndpoint => StaleReason::ProviderChanged,
            MeetingNotesErrorCode::NoTranscriptContent
            | MeetingNotesErrorCode::SessionNotFound
            | MeetingNotesErrorCode::SessionStillRecording
            | MeetingNotesErrorCode::TranscriptDocumentCorrupt
            | MeetingNotesErrorCode::TranscriptInvalid
            | MeetingNotesErrorCode::TranscriptNotReady => StaleReason::TranscriptChanged,
            _ => StaleReason::PipelineChanged,
        }],
    };
    SessionAnalysisView {
        current_run: document.current_run.as_ref().map(run_view),
        document_revision: document.document_revision,
        freshness: if stale_reasons.is_empty() {
            AnalysisFreshness::None
        } else {
            AnalysisFreshness::Stale
        },
        last_successful_result: document.last_successful_result.clone(),
        meeting_context: document.meeting_context.clone(),
        session_id: document.session_id.clone(),
        stale_reasons,
    }
}

fn run_view(run: &MeetingNotesRun) -> MeetingNotesRunView {
    MeetingNotesRunView {
        cancellation_requested: run.cancellation_requested,
        created_at: run.created_at,
        error: run.error.clone(),
        generation: run.generation,
        input_fingerprint: run.input_fingerprint.clone(),
        job_id: run.job_id.clone(),
        progress: run.progress,
        run_fingerprint: run.run_fingerprint.clone(),
        stage: run.stage,
        state: run.state,
        updated_at: run.updated_at,
    }
}

fn status_event(
    session_id: &str,
    document: &MeetingNotesDocument,
) -> Option<MeetingNotesStatusEvent> {
    let run = document.current_run.as_ref()?;
    Some(MeetingNotesStatusEvent {
        completed_chunks: run.progress.completed_chunks,
        document_revision: document.document_revision,
        error_code: run.error.as_ref().map(|error| error.code),
        generation: run.generation,
        job_id: run.job_id.clone(),
        retryable: run.error.as_ref().is_some_and(|error| error.retryable),
        session_id: session_id.to_owned(),
        stage: run.stage,
        state: run.state,
        total_chunks: run.progress.total_chunks,
        updated_at: run.updated_at,
    })
}

fn is_live(state: AnalysisState) -> bool {
    matches!(
        state,
        AnalysisState::Queued | AnalysisState::Cleaning | AnalysisState::Summarizing
    )
}

fn clean_unit(chunk: &CleanChunk) -> CheckpointUnit<CleanPartResult> {
    CheckpointUnit {
        attempts: 0,
        error: None,
        input_sha256: unit_hash(&chunk.parts),
        next_retry_at: None,
        output: None,
        output_sha256: None,
        source_keys: chunk
            .parts
            .iter()
            .map(|part| format!("{}:{}", part.segment_id, part.part_index))
            .collect(),
        stage: CheckpointStage::Clean,
        state: CheckpointUnitState::Pending,
        unit_id: format!("clean-{}", chunk.index),
    }
}

fn empty_summary_checkpoint() -> SummaryCheckpoint {
    SummaryCheckpoint {
        candidates: Vec::new(),
        mode: SummaryMode::Direct,
        reduce_level: 0,
        units: Vec::new(),
    }
}

fn unit_hash<T: Serialize>(value: &T) -> String {
    sha256(&serde_json::to_string(value).unwrap_or_default())
}

/// A user retry gives every unfinished unit a fresh attempt budget; committed
/// units are never requested again.
fn reset_pending_units<T>(units: &mut [CheckpointUnit<T>]) {
    for unit in units
        .iter_mut()
        .filter(|unit| unit.state != CheckpointUnitState::Committed)
    {
        unit.attempts = 0;
        unit.error = None;
        unit.state = CheckpointUnitState::Pending;
    }
}

/// Returns true when a committed unit failed its own hash and had to be dropped.
fn rewind_units<T: Serialize>(units: &mut [CheckpointUnit<T>]) -> bool {
    let mut corrupt = false;
    for unit in units.iter_mut() {
        match unit.state {
            CheckpointUnitState::Processing => unit.state = CheckpointUnitState::Pending,
            CheckpointUnitState::Committed if !worker::output_is_intact(unit) => {
                corrupt = true;
                unit.output = None;
                unit.output_sha256 = None;
                unit.state = CheckpointUnitState::Pending;
            }
            _ => {}
        }
    }
    corrupt
}

fn collect_documents(
    directory: &Path,
    depth: usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    if depth > MAX_SCAN_DEPTH {
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

#[cfg(test)]
#[path = "meeting_notes_tests.rs"]
mod tests;
