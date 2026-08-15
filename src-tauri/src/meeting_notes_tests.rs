use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    time::Instant,
};

use chrono::DateTime;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::{
    meeting_notes_document::{
        CheckpointUnitState, InputQualityKind, MeetingContextContent, save_meeting_context,
    },
    meeting_notes_pipeline::{CLEAN_SYSTEM_PROMPT, input_fingerprint},
    openai_compatible::{
        ChatError, ChatMessage, ChatRequest, ChatRole, ReqwestChatClient,
        test_support::RecordedChatRequest,
    },
    settings::{
        AppSettings, AppSettingsWithoutSecrets, AudioFormat, AudioSourceMode,
        MeetingNotesSettingsWithoutSecrets, SecretUpdate, SettingsSecretUpdates, SettingsStore,
        TranslationSettingsWithoutSecrets,
    },
    storage::{ArchiveStatus, SessionManifest},
    transcription::{
        TranscriptDocument, TranscriptDocumentStatus, TranscriptSegment, TranscriptSegmentStatus,
    },
    translation::TranslationService,
};

const API_KEY: &str = "sk-meeting-notes-marker";
const GATE_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_ID: &str = "20260815T101010.000Z-abcdef";
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

struct Setup {
    budget: u32,
    clean_padding: usize,
    endpoint: String,
    request_timeout_seconds: u32,
    summary_padding: usize,
    texts: Vec<String>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            budget: 8_000,
            clean_padding: 0,
            endpoint: "http://127.0.0.1:8000/v1".to_owned(),
            request_timeout_seconds: 180,
            summary_padding: 0,
            texts: vec!["a".repeat(3_000), "b".repeat(3_000)],
        }
    }
}

struct Fixture {
    catalog: Arc<SessionCatalog>,
    directory: TempDir,
    global: Arc<GlobalContextStore>,
    lifecycle: Arc<SessionLifecycle>,
    provider: Arc<FakeProvider>,
    session_dir: PathBuf,
    settings: SettingsStore,
    sleeps: Arc<Mutex<Vec<Duration>>>,
}

/// Answers every request from the request itself, so a plan change never
/// invalidates a hand written script. Faults are injected by call index.
#[derive(Default)]
struct FakeProvider {
    clean_padding: usize,
    faults: Mutex<VecDeque<Option<ChatError>>>,
    gate: Mutex<Option<(usize, crossbeam_channel::Receiver<()>)>>,
    requests: Mutex<Vec<RecordedChatRequest>>,
    summary_padding: usize,
}

impl FakeProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn failing(faults: Vec<Option<ChatError>>) -> Arc<Self> {
        Arc::new(Self {
            faults: Mutex::new(faults.into()),
            ..Self::default()
        })
    }

    fn padded(clean_padding: usize, summary_padding: usize) -> Arc<Self> {
        Arc::new(Self {
            clean_padding,
            summary_padding,
            ..Self::default()
        })
    }

    fn hold(&self, at: usize) -> crossbeam_channel::Sender<()> {
        let (release, receiver) = crossbeam_channel::bounded(1);
        *self.gate.lock().unwrap() = Some((at, receiver));
        release
    }

    fn requests(&self) -> Vec<RecordedChatRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The `(segmentId, partIndex)` key list of every clean request, in call
    /// order, so a per chunk request count needs no scripted bookkeeping.
    fn clean_keys(&self) -> Vec<Vec<(u64, u64)>> {
        self.requests()
            .iter()
            .filter_map(|request| {
                let user: Value = serde_json::from_str(&request.messages[1].1).ok()?;
                let parts = user.get("parts")?.as_array()?;
                Some(
                    parts
                        .iter()
                        .map(|part| {
                            (
                                part["segmentId"].as_u64().unwrap_or_default(),
                                part["partIndex"].as_u64().unwrap_or_default(),
                            )
                        })
                        .collect(),
                )
            })
            .collect()
    }
}

impl ChatCompletionPort for FakeProvider {
    fn complete(&self, request: ChatRequest<'_>) -> Result<String, ChatError> {
        let index = {
            let mut requests = self.requests.lock().unwrap();
            requests.push(RecordedChatRequest {
                api_key: request
                    .api_key
                    .map(|key| key.trimmed().to_owned())
                    .filter(|key| !key.is_empty()),
                endpoint: request.endpoint.to_owned(),
                messages: request
                    .messages
                    .iter()
                    .map(|message| (role_of(message), message.content.to_owned()))
                    .collect(),
                model: request.model.to_owned(),
                timeout: request.timeout,
            });
            requests.len() - 1
        };
        let gated = self
            .gate
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(at, _)| *at == index)
            .map(|(_, receiver)| receiver.clone());
        if let Some(receiver) = gated {
            let _ = receiver.recv_timeout(GATE_TIMEOUT);
        }
        if let Some(fault) = self.faults.lock().unwrap().pop_front().flatten() {
            return Err(fault);
        }
        Ok(answer(
            request.messages[1].content,
            self.clean_padding,
            self.summary_padding,
        ))
    }
}

fn role_of(message: &ChatMessage<'_>) -> String {
    if message.role == ChatRole::System {
        "system".to_owned()
    } else {
        "user".to_owned()
    }
}

fn answer(user: &str, clean_padding: usize, summary_padding: usize) -> String {
    let value: Value = serde_json::from_str(user).expect("the user message is always JSON");
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        let parts = parts
            .iter()
            .map(|part| {
                json!({
                    "partIndex": part["partIndex"],
                    "segmentId": part["segmentId"],
                    "text": format!(
                        "clean {}.{}{}",
                        part["segmentId"],
                        part["partIndex"],
                        "x".repeat(clean_padding)
                    ),
                })
            })
            .collect::<Vec<_>>();
        return json!({ "parts": parts }).to_string();
    }
    let ids = match value.get("segments").and_then(Value::as_array) {
        Some(segments) => segments
            .iter()
            .filter_map(|segment| segment["segmentId"].as_u64())
            .collect::<Vec<_>>(),
        None => value["candidates"]
            .as_array()
            .expect("a summary request carries segments or candidates")
            .iter()
            .flat_map(|candidate| {
                candidate["keyPoints"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .filter_map(|point| point["sourceSegmentIds"][0].as_u64())
            .collect(),
    };
    json!({
        "actionItems": [],
        "background": [],
        "decisions": [],
        "keyPoints": ids
            .iter()
            .map(|id| json!({ "sourceSegmentIds": [id], "text": format!("point {id}") }))
            .collect::<Vec<_>>(),
        "openQuestions": [],
        "overview": {
            "sourceSegmentIds": [ids.first().copied().unwrap_or_default()],
            "text": format!("overview{}", "y".repeat(summary_padding)),
        },
        "title": "release sync",
    })
    .to_string()
}

fn setup(options: Setup) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("recordings");
    let session_dir = root.join("session");
    fs::create_dir_all(&session_dir).unwrap();
    write_json(&session_dir.join("session.json"), &manifest());
    write_json(
        &session_dir.join("transcript.json"),
        &transcript(&options.texts),
    );
    let settings = SettingsStore::open(directory.path().join("settings.json")).unwrap();
    let defaults = AppSettings::default();
    settings
        .save(
            AppSettingsWithoutSecrets {
                audio: defaults.audio,
                desktop: defaults.desktop,
                meeting_notes: MeetingNotesSettingsWithoutSecrets {
                    endpoint: options.endpoint.clone(),
                    max_input_characters: options.budget,
                    model: "local-model".to_owned(),
                    request_timeout_seconds: options.request_timeout_seconds,
                },
                transcription: defaults.transcription,
                translation: TranslationSettingsWithoutSecrets {
                    enabled: false,
                    endpoint: defaults.translation.endpoint,
                    model: defaults.translation.model,
                    target_language: defaults.translation.target_language,
                },
            },
            SettingsSecretUpdates {
                meeting_notes_api_key: SecretUpdate::Set {
                    value: API_KEY.to_owned(),
                },
                ..SettingsSecretUpdates::default()
            },
        )
        .unwrap();
    let catalog = Arc::new(
        SessionCatalog::open(directory.path().join(crate::session_catalog::DOCUMENT_NAME)).unwrap(),
    );
    catalog.register_root(&root).unwrap();
    Fixture {
        catalog,
        global: Arc::new(
            GlobalContextStore::open(directory.path().join("global-context.json")).unwrap(),
        ),
        lifecycle: SessionLifecycle::new(),
        provider: FakeProvider::padded(options.clean_padding, options.summary_padding),
        session_dir: fs::canonicalize(&session_dir).unwrap(),
        settings,
        sleeps: Arc::new(Mutex::new(Vec::new())),
        directory,
    }
}

impl Fixture {
    fn service(&self) -> MeetingNotesService {
        self.service_with(self.provider.clone(), Arc::new(|_| {}))
    }

    fn service_with(
        &self,
        provider: Arc<FakeProvider>,
        observer: MeetingNotesObserver,
    ) -> MeetingNotesService {
        self.service_using(self.lifecycle.clone(), provider, observer)
    }

    /// A fresh lifecycle stands in for a restarted process: the tombstones are
    /// gone and only the `.deleting` markers on disk are left.
    fn service_using(
        &self,
        lifecycle: Arc<SessionLifecycle>,
        provider: Arc<FakeProvider>,
        observer: MeetingNotesObserver,
    ) -> MeetingNotesService {
        let sleeps = self.sleeps.clone();
        MeetingNotesService::with_chat_client(
            self.catalog.clone(),
            self.global.clone(),
            lifecycle,
            self.settings.credentials(),
            observer,
            provider,
            Arc::new(move |delay| sleeps.lock().unwrap().push(delay)),
        )
        .unwrap()
    }

    fn root(&self) -> PathBuf {
        self.directory.path().join("recordings")
    }

    fn translation(&self, lifecycle: Arc<SessionLifecycle>) -> TranslationService {
        TranslationService::with_chat_client(
            Arc::new(|_| {}),
            self.settings.credentials(),
            Arc::new(crate::openai_compatible::test_support::ScriptedChatClient::new(Vec::new())),
            lifecycle,
        )
        .unwrap()
    }

    fn generate(
        &self,
        service: &MeetingNotesService,
        mode: GenerateMode,
    ) -> Result<SessionAnalysisView, MeetingNotesError> {
        self.generate_with(service, mode, true)
    }

    fn generate_with(
        &self,
        service: &MeetingNotesService,
        mode: GenerateMode,
        accept_partial: bool,
    ) -> Result<SessionAnalysisView, MeetingNotesError> {
        let meeting_revision = self.document().meeting_context.revision;
        service.generate(GenerateRequest {
            accept_partial,
            expected_global_context_revision: self.global.document().revision,
            expected_meeting_context_revision: meeting_revision,
            mode,
            output_language: OutputLanguage::EnUs,
            session_id: SESSION_ID.to_owned(),
        })
    }

    fn document(&self) -> MeetingNotesDocument {
        let path = self.session_dir.join(DOCUMENT_NAME);
        if path.exists() {
            load_document(&path).unwrap()
        } else {
            new_document(SESSION_ID)
        }
    }

    fn run(&self) -> MeetingNotesRun {
        self.document().current_run.expect("the run was persisted")
    }

    fn store(&self, document: &MeetingNotesDocument) {
        save_document(&self.session_dir.join(DOCUMENT_NAME), document).unwrap();
    }

    fn sleeps(&self) -> Vec<Duration> {
        self.sleeps.lock().unwrap().clone()
    }

    fn wait_for(&self, state: AnalysisState) -> MeetingNotesRun {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            let run = self.document().current_run;
            if let Some(run) = run.filter(|run| run.state == state) {
                return run;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {state:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_requests(&self, provider: &FakeProvider, count: usize) {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        while provider.count() < count {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {count} requests"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

fn manifest() -> SessionManifest {
    SessionManifest {
        audio_format: AudioFormat::Flac,
        audio_source: AudioSourceMode::Microphone,
        completed_at: Some(DateTime::UNIX_EPOCH),
        last_error: None,
        sample_rate: 16_000,
        schema_version: 1,
        segment_minutes: 30,
        segments: Vec::new(),
        session_id: SESSION_ID.to_owned(),
        started_at: DateTime::UNIX_EPOCH,
        status: ArchiveStatus::Completed,
    }
}

fn transcript(texts: &[String]) -> TranscriptDocument {
    TranscriptDocument {
        forced_language: "auto".to_owned(),
        model_id: "qwen3-asr".to_owned(),
        schema_version: 1,
        segments: texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let id = index as u32 + 1;
                TranscriptSegment {
                    attempts: 1,
                    audio_file: format!("segments/{id:04}.flac"),
                    detected_language: "English".to_owned(),
                    end_ms: u64::from(id) * 1_000 + 900,
                    error: None,
                    id,
                    peak_probability: 0.9,
                    start_ms: u64::from(id) * 1_000,
                    status: TranscriptSegmentStatus::Complete,
                    text: text.clone(),
                }
            })
            .collect(),
        session_id: SESSION_ID.to_owned(),
        status: TranscriptDocumentStatus::Complete,
        threads: 4,
        translation: Default::default(),
        unload_after_idle_minutes: 10,
        updated_at: DateTime::UNIX_EPOCH,
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn sha256_of(path: &Path) -> String {
    sha256(&String::from_utf8_lossy(&fs::read(path).unwrap()))
}

fn write_manifest(fixture: &Fixture, status: ArchiveStatus) {
    write_json(
        &fixture.session_dir.join("session.json"),
        &SessionManifest {
            status,
            ..manifest()
        },
    );
}

fn write_transcript(fixture: &Fixture, document: TranscriptDocument) {
    write_json(&fixture.session_dir.join("transcript.json"), &document);
}

#[test]
fn a_session_that_is_still_recording_is_refused_before_any_request() {
    let fixture = setup(Setup::default());
    write_manifest(&fixture, ArchiveStatus::Recording);
    let service = fixture.service();

    let error = fixture
        .generate(&service, GenerateMode::Ensure)
        .unwrap_err();

    assert_eq!(error.code(), MeetingNotesErrorCode::SessionStillRecording);
    assert_eq!(fixture.provider.count(), 0);
    assert!(fixture.document().current_run.is_none());
}

#[test]
fn a_transcript_that_is_still_running_is_refused_before_any_request() {
    let fixture = setup(Setup::default());
    let mut document = transcript(&Setup::default().texts);
    document.status = TranscriptDocumentStatus::Processing;
    document.segments[1].status = TranscriptSegmentStatus::Processing;
    document.segments[1].text = String::new();
    write_transcript(&fixture, document);
    let service = fixture.service();

    let error = fixture
        .generate(&service, GenerateMode::Ensure)
        .unwrap_err();

    assert_eq!(error.code(), MeetingNotesErrorCode::TranscriptNotReady);
    assert_eq!(fixture.provider.count(), 0);
    assert!(fixture.document().current_run.is_none());
}

#[test]
fn a_complete_transcript_needs_no_partial_confirmation() {
    let fixture = setup(Setup::default());
    let service = fixture.service();

    fixture
        .generate_with(&service, GenerateMode::Ensure, false)
        .unwrap();
    fixture.wait_for(AnalysisState::Complete);

    let result = fixture
        .document()
        .last_successful_result
        .expect("the result was persisted");
    assert_eq!(result.input_quality.kind, InputQualityKind::Complete);
    assert_eq!(result.input_quality.completed_segment_count, 2);
    assert_eq!(result.input_quality.failed_segment_count, 0);
}

#[test]
fn a_partial_transcript_needs_confirmation_and_stays_marked_partial() {
    let fixture = setup(Setup::default());
    let mut document = transcript(&Setup::default().texts);
    document.status = TranscriptDocumentStatus::Partial;
    document.segments[1].attempts = 3;
    document.segments[1].error = Some("asr failed".to_owned());
    document.segments[1].status = TranscriptSegmentStatus::Failed;
    document.segments[1].text = String::new();
    write_transcript(&fixture, document);
    let service = fixture.service();

    let error = fixture
        .generate_with(&service, GenerateMode::Ensure, false)
        .unwrap_err();

    assert_eq!(
        error.code(),
        MeetingNotesErrorCode::PartialConfirmationRequired
    );
    assert_eq!(fixture.provider.count(), 0);

    fixture
        .generate_with(&service, GenerateMode::Ensure, true)
        .unwrap();
    fixture.wait_for(AnalysisState::Complete);

    let result = fixture
        .document()
        .last_successful_result
        .expect("the result was persisted");
    assert_eq!(result.input_quality.kind, InputQualityKind::Partial);
    assert_eq!(result.input_quality.completed_segment_count, 1);
    assert_eq!(result.input_quality.failed_segment_count, 1);
    assert_eq!(result.cleaned.segments.len(), 1);
}

#[test]
fn cleans_and_summarizes_a_session_end_to_end() {
    let fixture = setup(Setup::default());
    let observed: Arc<Mutex<Vec<(AnalysisState, u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = observed.clone();
    let path = fixture.session_dir.join(DOCUMENT_NAME);
    let service = fixture.service_with(
        fixture.provider.clone(),
        Arc::new(move |event: MeetingNotesStatusEvent| {
            let stored = load_document(&path).unwrap();
            sink.lock().unwrap().push((
                event.state,
                event.document_revision,
                stored.document_revision,
            ));
        }),
    );

    let queued = fixture.generate(&service, GenerateMode::Ensure).unwrap();
    let run = fixture.wait_for(AnalysisState::Complete);

    let document = fixture.document();
    let result = document
        .last_successful_result
        .expect("a result was stored");
    assert_eq!(queued.current_run.unwrap().state, AnalysisState::Queued);
    assert_eq!(run.progress.total_chunks, 1);
    assert_eq!(run.progress.completed_chunks, 1);
    assert!(run.clean_checkpoint.units.len() >= 2);
    assert!(
        run.clean_checkpoint
            .units
            .iter()
            .all(|unit| unit.state == CheckpointUnitState::Committed)
    );
    assert_eq!(run.summary_checkpoint.mode, SummaryMode::Direct);
    assert_eq!(result.input_fingerprint, run.input_fingerprint);
    assert_eq!(result.run_fingerprint, run.run_fingerprint);
    assert_eq!(result.cleaned.segments.len(), 2);
    assert_eq!(result.cleaned.segments[0].text, "clean 1.0");
    assert_eq!(result.cleaned.segments[0].start_ms, 1_000);
    assert_eq!(result.summary.title, "release sync");
    assert_eq!(result.output_language, OutputLanguage::EnUs);

    let requests = fixture.provider.requests();
    assert_eq!(requests.len(), run.clean_checkpoint.units.len() + 1);
    assert_eq!(requests[0].messages[0].0, "system");
    assert_eq!(requests[0].messages[0].1, CLEAN_SYSTEM_PROMPT);
    assert_eq!(requests[0].api_key.as_deref(), Some(API_KEY));
    assert_eq!(requests[0].model, "local-model");
    let user: Value = serde_json::from_str(&requests[0].messages[1].1).unwrap();
    assert!(user.get("globalContext").is_some());
    assert!(user.get("meetingContext").is_some());
    assert_eq!(user["outputLanguage"], "en-US");

    // The file is written before the event is published, so the last event can
    // still be in flight once the state file reads complete.
    let deadline = Instant::now() + WAIT_TIMEOUT;
    while observed.lock().unwrap().last().map(|(state, _, _)| *state)
        != Some(AnalysisState::Complete)
    {
        assert!(Instant::now() < deadline, "timed out waiting for the event");
        thread::sleep(Duration::from_millis(5));
    }

    let observed = observed.lock().unwrap().clone();
    assert!(
        observed
            .iter()
            .all(|(_, emitted, stored)| stored >= emitted),
        "the state file must never trail the event"
    );
    let states = observed
        .iter()
        .map(|(state, _, _)| *state)
        .collect::<Vec<_>>();
    assert_eq!(states.first(), Some(&AnalysisState::Queued));
    assert_eq!(states.last(), Some(&AnalysisState::Complete));
    assert!(states.contains(&AnalysisState::Cleaning));
    assert!(states.contains(&AnalysisState::Summarizing));
}

#[test]
fn retries_only_the_failing_chunk_with_the_scripted_backoff() {
    let fixture = setup(Setup::default());
    let provider = FakeProvider::failing(vec![
        None,
        Some(ChatError::Timeout),
        Some(ChatError::Timeout),
    ]);
    let service = fixture.service_with(provider.clone(), Arc::new(|_| {}));

    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for(AnalysisState::Complete);

    let keys = provider.clean_keys();
    let first = keys.first().cloned().unwrap();
    let second = keys
        .iter()
        .find(|entry| **entry != first)
        .cloned()
        .expect("a second clean chunk was requested");
    assert_eq!(keys.iter().filter(|entry| **entry == first).count(), 1);
    assert_eq!(keys.iter().filter(|entry| **entry == second).count(), 3);
    assert_eq!(
        fixture.sleeps(),
        vec![Duration::from_secs(1), Duration::from_secs(5)]
    );
}

#[test]
fn a_failed_summary_keeps_the_clean_checkpoints_and_the_previous_result() {
    let fixture = setup(Setup::default());
    let service = fixture.service();
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for(AnalysisState::Complete);
    let previous = fixture.document().last_successful_result.unwrap();
    drop(service);

    let clean_chunks = fixture.run().clean_checkpoint.units.len();
    let mut faults = std::iter::repeat_with(|| None)
        .take(clean_chunks)
        .collect::<Vec<_>>();
    faults.push(Some(ChatError::Unauthorized));
    let provider = FakeProvider::failing(faults);
    let service = fixture.service_with(provider, Arc::new(|_| {}));
    fixture
        .generate(&service, GenerateMode::Regenerate)
        .unwrap();
    let run = fixture.wait_for(AnalysisState::Failed);

    let error = run.error.expect("the failure was recorded");
    assert_eq!(error.code, MeetingNotesErrorCode::ProviderUnauthorized);
    assert_eq!(error.stage, Some(RunStage::Summarizing));
    assert!(!error.retryable);
    assert_eq!(run.stage, Some(RunStage::Summarizing));
    assert!(
        run.clean_checkpoint
            .units
            .iter()
            .all(|unit| unit.state == CheckpointUnitState::Committed)
    );
    assert_eq!(fixture.document().last_successful_result, Some(previous));
}

#[test]
fn recovery_resumes_at_the_first_uncommitted_chunk() {
    let fixture = setup(Setup::default());
    let provider = FakeProvider::failing(vec![None, Some(ChatError::Unauthorized)]);
    let service = fixture.service_with(provider.clone(), Arc::new(|_| {}));
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for(AnalysisState::Failed);
    drop(service);

    // The app exited between the committed first chunk and the second answer.
    let mut document = fixture.document();
    let run = document.current_run.as_mut().unwrap();
    run.error = None;
    run.state = AnalysisState::Cleaning;
    run.clean_checkpoint.units[1].state = CheckpointUnitState::Processing;
    fixture.store(&document);
    let committed = fixture.run().clean_checkpoint.units[0]
        .output
        .clone()
        .unwrap();

    let resumed = FakeProvider::new();
    let service = fixture.service_with(resumed.clone(), Arc::new(|_| {}));
    assert_eq!(service.recover_catalog().unwrap(), 1);
    fixture.wait_for(AnalysisState::Complete);

    let keys = resumed.clean_keys();
    assert_eq!(keys.len(), 1, "the committed chunk was not requested again");
    assert_eq!(
        fixture.run().clean_checkpoint.units[0].output,
        Some(committed)
    );
}

#[test]
fn a_repeated_ensure_reuses_the_running_job_and_the_stored_result() {
    let fixture = setup(Setup::default());
    let release = fixture.provider.hold(0);
    let service = fixture.service();

    let first = fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for_requests(&fixture.provider, 1);
    let second = fixture.generate(&service, GenerateMode::Ensure).unwrap();
    release.send(()).unwrap();
    let run = fixture.wait_for(AnalysisState::Complete);

    let job_id = first.current_run.unwrap().job_id;
    assert_eq!(second.current_run.unwrap().job_id, job_id);
    assert_eq!(run.job_id, job_id);
    assert_eq!(
        fixture.provider.count(),
        run.clean_checkpoint.units.len() + 1
    );

    let before = fixture.provider.count();
    let reused = fixture.generate(&service, GenerateMode::Ensure).unwrap();
    assert_eq!(fixture.provider.count(), before);
    assert_eq!(reused.freshness, AnalysisFreshness::Fresh);
    assert_eq!(reused.current_run.unwrap().state, AnalysisState::Complete);
}

#[test]
fn a_changed_meeting_context_keeps_the_old_result_and_marks_it_stale() {
    let fixture = setup(Setup::default());
    let service = fixture.service();
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for(AnalysisState::Complete);
    let previous = fixture.document().last_successful_result.unwrap();

    save_meeting_context(
        &fixture.session_dir,
        SESSION_ID,
        0,
        &MeetingContextContent {
            purpose: "confirm the release risks".to_owned(),
            ..MeetingContextContent::default()
        },
    )
    .unwrap();

    let stale = service.load(SESSION_ID, OutputLanguage::EnUs).unwrap();
    assert_eq!(stale.freshness, AnalysisFreshness::Stale);
    assert_eq!(
        stale.stale_reasons,
        vec![StaleReason::MeetingContextChanged]
    );
    assert_eq!(stale.last_successful_result, Some(previous.clone()));

    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for(AnalysisState::Complete);

    let refreshed = service.load(SESSION_ID, OutputLanguage::EnUs).unwrap();
    let result = refreshed.last_successful_result.unwrap();
    assert_eq!(refreshed.freshness, AnalysisFreshness::Fresh);
    assert_ne!(result.input_fingerprint, previous.input_fingerprint);
    assert_eq!(
        result.input_snapshot.context.meeting.purpose,
        "confirm the release risks"
    );
}

#[test]
fn a_provider_error_body_never_reaches_the_document_or_the_payload() {
    let echo = echo_server();
    let fixture = setup(Setup {
        endpoint: format!("http://{}/v1", echo.address),
        ..Setup::default()
    });
    let service = MeetingNotesService::with_chat_client(
        fixture.catalog.clone(),
        fixture.global.clone(),
        fixture.lifecycle.clone(),
        fixture.settings.credentials(),
        Arc::new(|_| {}),
        Arc::new(ReqwestChatClient::new().unwrap()),
        Arc::new(|_| {}),
    )
    .unwrap();

    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    let run = fixture.wait_for(AnalysisState::Failed);

    let error = run.error.expect("the failure was recorded");
    assert_eq!(error.code, MeetingNotesErrorCode::RetryExhausted);
    assert_eq!(
        error.cause_code,
        Some(MeetingNotesErrorCode::ProviderUnavailable)
    );
    assert_eq!(error.http_status, Some(500));
    assert!(error.retryable);
    let stored = fs::read_to_string(fixture.session_dir.join(DOCUMENT_NAME)).unwrap();
    assert!(!stored.contains(API_KEY));
    assert!(!stored.contains("echoed-request-body"));
    let payload = MeetingNotesError::new(error.code).payload();
    assert!(payload.params.is_empty());
    assert!(!format!("{payload:?}").contains("echoed-request-body"));
    assert!(!format!("{:?}", ChatError::Unavailable(500.try_into().unwrap())).contains("body"));
}

#[test]
fn a_cancelled_run_discards_the_pending_answer_and_resumes_on_retry() {
    let fixture = setup(Setup::default());
    let service = fixture.service();
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for(AnalysisState::Complete);
    let previous = fixture.document().last_successful_result.unwrap();
    drop(service);

    let provider = FakeProvider::new();
    let release = provider.hold(1);
    let service = fixture.service_with(provider.clone(), Arc::new(|_| {}));
    let queued = fixture
        .generate(&service, GenerateMode::Regenerate)
        .unwrap();
    let job = queued.current_run.unwrap();
    fixture.wait_for_requests(&provider, 2);

    let cancelling = service
        .cancel(CancelRequest {
            expected_run_fingerprint: job.run_fingerprint.clone(),
            job_id: job.job_id.clone(),
            output_language: OutputLanguage::EnUs,
            session_id: SESSION_ID.to_owned(),
        })
        .unwrap();
    assert!(cancelling.current_run.unwrap().cancellation_requested);
    release.send(()).unwrap();
    let cancelled = fixture.wait_for(AnalysisState::Cancelled);

    assert_eq!(
        cancelled.clean_checkpoint.units[0].state,
        CheckpointUnitState::Committed
    );
    assert_eq!(
        cancelled.clean_checkpoint.units[1].state,
        CheckpointUnitState::Pending
    );
    assert_eq!(fixture.document().last_successful_result, Some(previous));

    service
        .retry(RetryRequest {
            expected_run_fingerprint: job.run_fingerprint,
            job_id: job.job_id.clone(),
            output_language: OutputLanguage::EnUs,
            session_id: SESSION_ID.to_owned(),
        })
        .unwrap();
    let completed = fixture.wait_for(AnalysisState::Complete);
    assert_eq!(completed.job_id, job.job_id);
    assert_eq!(
        provider
            .clean_keys()
            .iter()
            .filter(|keys| !keys.is_empty())
            .count(),
        3,
        "only the uncommitted chunk was requested again"
    );
}

#[test]
fn the_snapshot_timeout_reaches_the_chat_port() {
    let fixture = setup(Setup {
        request_timeout_seconds: 300,
        ..Setup::default()
    });
    let service = fixture.service();

    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for(AnalysisState::Complete);

    assert!(
        fixture
            .provider
            .requests()
            .iter()
            .all(|request| request.timeout == Duration::from_secs(300))
    );
}

#[test]
fn a_long_transcript_reduces_over_several_levels() {
    let fixture = setup(Setup {
        clean_padding: 2_000,
        summary_padding: 1_500,
        texts: (0..8).map(|index| format!("segment {index} ")).collect(),
        ..Setup::default()
    });
    let service = fixture.service();

    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    let run = fixture.wait_for(AnalysisState::Complete);

    assert_eq!(run.summary_checkpoint.mode, SummaryMode::MapReduce);
    assert!(
        run.summary_checkpoint.reduce_level >= 2,
        "expected at least two reduce levels, saw {}",
        run.summary_checkpoint.reduce_level
    );
    let summary = fixture.document().last_successful_result.unwrap().summary;
    assert_eq!(summary.key_points.len(), 8);
    assert!(
        summary
            .key_points
            .iter()
            .all(|point| point.source_segment_ids.len() == 1)
    );
}

#[test]
fn a_reduce_level_that_cannot_shrink_fails_the_run() {
    let fixture = setup(Setup {
        clean_padding: 2_000,
        summary_padding: 3_000,
        texts: (0..8).map(|index| format!("segment {index} ")).collect(),
        ..Setup::default()
    });
    let service = fixture.service();

    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    let run = fixture.wait_for(AnalysisState::Failed);

    let error = run.error.expect("the failure was recorded");
    assert_eq!(
        error.code,
        MeetingNotesErrorCode::SummaryReduceDidNotConverge
    );
    assert_eq!(error.stage, Some(RunStage::Summarizing));
}

#[test]
fn recovery_quarantines_an_unparsable_document() {
    let fixture = setup(Setup::default());
    fs::write(fixture.session_dir.join(DOCUMENT_NAME), b"{not json").unwrap();
    let service = fixture.service();

    assert_eq!(service.recover_catalog().unwrap(), 0);

    let error = service
        .load(SESSION_ID, OutputLanguage::EnUs)
        .expect_err("a quarantined document is reported");
    assert_eq!(error.code(), MeetingNotesErrorCode::AnalysisDocumentCorrupt);
    assert!(!fixture.session_dir.join(DOCUMENT_NAME).exists());
    assert!(fs::read_dir(&fixture.session_dir).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("analysis.json.corrupt-")
    }));
    assert!(fixture.session_dir.join("transcript.json").exists());

    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for(AnalysisState::Complete);
    assert!(
        service
            .load(SESSION_ID, OutputLanguage::EnUs)
            .unwrap()
            .last_successful_result
            .is_some(),
        "generating rebuilds the quarantined document"
    );
}

#[test]
fn recovery_drops_a_checkpoint_that_lost_its_hash() {
    let fixture = setup(Setup::default());
    let service = fixture.service();
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for(AnalysisState::Complete);
    let previous = fixture.document().last_successful_result.unwrap();
    drop(service);

    let mut document = fixture.document();
    let run = document.current_run.as_mut().unwrap();
    run.state = AnalysisState::Cleaning;
    run.clean_checkpoint.units[0].output_sha256 = Some("0".repeat(64));
    fixture.store(&document);

    // The resumed run stops on its first request, so the reset is observable.
    let provider = FakeProvider::failing(vec![Some(ChatError::Forbidden)]);
    let service = fixture.service_with(provider.clone(), Arc::new(|_| {}));
    assert_eq!(service.recover_catalog().unwrap(), 1);
    let run = fixture.wait_for(AnalysisState::Failed);

    assert_eq!(provider.clean_keys().len(), 1);
    assert_eq!(run.clean_checkpoint.units[0].output, None);
    assert_eq!(
        run.clean_checkpoint.units[1].state,
        CheckpointUnitState::Committed
    );
    assert_eq!(fixture.document().last_successful_result, Some(previous));
}

#[test]
fn recovery_fails_a_run_whose_provider_changed() {
    let fixture = setup(Setup::default());
    let provider = FakeProvider::failing(vec![Some(ChatError::Unauthorized)]);
    let service = fixture.service_with(provider.clone(), Arc::new(|_| {}));
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for(AnalysisState::Failed);
    let mut document = fixture.document();
    document.current_run.as_mut().unwrap().state = AnalysisState::Queued;
    fixture.store(&document);
    drop(service);

    let defaults = AppSettings::default();
    fixture
        .settings
        .save(
            AppSettingsWithoutSecrets {
                audio: defaults.audio,
                desktop: defaults.desktop,
                meeting_notes: MeetingNotesSettingsWithoutSecrets {
                    endpoint: "https://moved.example/v1".to_owned(),
                    max_input_characters: 8_000,
                    model: "local-model".to_owned(),
                    request_timeout_seconds: 180,
                },
                transcription: defaults.transcription,
                translation: TranslationSettingsWithoutSecrets {
                    enabled: false,
                    endpoint: defaults.translation.endpoint,
                    model: defaults.translation.model,
                    target_language: defaults.translation.target_language,
                },
            },
            SettingsSecretUpdates::default(),
        )
        .unwrap();

    let resumed = FakeProvider::new();
    let service = fixture.service_with(resumed.clone(), Arc::new(|_| {}));
    assert_eq!(service.recover_catalog().unwrap(), 0);

    let run = fixture.run();
    assert_eq!(run.state, AnalysisState::Failed);
    assert_eq!(
        run.error.unwrap().code,
        MeetingNotesErrorCode::ProviderChanged
    );
    assert_eq!(resumed.count(), 0);
}

#[test]
fn a_failing_run_never_touches_the_recording_files() {
    let fixture = setup(Setup::default());
    let manifest_path = fixture.session_dir.join("session.json");
    let transcript_path = fixture.session_dir.join("transcript.json");
    let before = (sha256_of(&manifest_path), sha256_of(&transcript_path));
    let provider = FakeProvider::failing(vec![Some(ChatError::Forbidden)]);
    let service = fixture.service_with(provider, Arc::new(|_| {}));

    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    let run = fixture.wait_for(AnalysisState::Failed);

    assert_eq!(
        run.error.unwrap().code,
        MeetingNotesErrorCode::ProviderForbidden
    );
    assert_eq!(
        before,
        (sha256_of(&manifest_path), sha256_of(&transcript_path))
    );
}

#[test]
fn a_regenerate_during_a_run_is_rejected() {
    let fixture = setup(Setup::default());
    let release = fixture.provider.hold(0);
    let service = fixture.service();
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for_requests(&fixture.provider, 1);

    let error = fixture
        .generate(&service, GenerateMode::Regenerate)
        .expect_err("a running job blocks a regenerate");

    assert_eq!(error.code(), MeetingNotesErrorCode::AnalysisBusy);
    release.send(()).unwrap();
    fixture.wait_for(AnalysisState::Complete);
}

#[test]
fn a_stale_context_revision_is_rejected() {
    let fixture = setup(Setup::default());
    let service = fixture.service();

    let error = service
        .generate(GenerateRequest {
            accept_partial: true,
            expected_global_context_revision: 7,
            expected_meeting_context_revision: 0,
            mode: GenerateMode::Ensure,
            output_language: OutputLanguage::EnUs,
            session_id: SESSION_ID.to_owned(),
        })
        .expect_err("the frontend confirmed another revision");

    assert_eq!(error.code(), MeetingNotesErrorCode::ContextRevisionConflict);
    assert_eq!(fixture.provider.count(), 0);
}

#[test]
fn a_regenerate_keeps_the_input_fingerprint_and_mints_a_new_run() {
    let fixture = setup(Setup::default());
    let service = fixture.service();
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    let first = fixture.wait_for(AnalysisState::Complete);

    fixture
        .generate(&service, GenerateMode::Regenerate)
        .unwrap();
    let second = fixture.wait_for(AnalysisState::Complete);

    assert_eq!(second.input_fingerprint, first.input_fingerprint);
    assert_ne!(second.run_fingerprint, first.run_fingerprint);
    assert_ne!(second.job_id, first.job_id);
    assert_eq!(second.regeneration_nonce, first.regeneration_nonce + 1);
    let result = fixture.document().last_successful_result.unwrap();
    assert_eq!(result.run_fingerprint, second.run_fingerprint);
    assert_eq!(
        result.input_fingerprint,
        input_fingerprint(&second.input_snapshot)
    );
}

/// AC-27: the provider answers a run whose session was deleted mid-request. The
/// commit is refused, the directory stays gone and no result file reappears.
#[test]
fn a_response_that_arrives_after_a_delete_is_dropped() {
    let fixture = setup(Setup::default());
    let release = fixture.provider.hold(0);
    let service = fixture.service();
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for_requests(&fixture.provider, 1);

    fixture
        .lifecycle
        .begin_delete(&fixture.session_dir)
        .unwrap();
    service.forget(SESSION_ID).unwrap();
    fixture
        .lifecycle
        .finish_delete(&fixture.session_dir)
        .unwrap();
    release.send(()).unwrap();

    // The worker acknowledges the shutdown, so it neither hung nor died on the
    // response it could no longer commit.
    let stopping = Instant::now();
    service.shutdown();
    assert!(stopping.elapsed() < SHUTDOWN_TIMEOUT);
    assert_eq!(fixture.provider.count(), 1);
    assert!(!fixture.session_dir.exists());
    assert!(!fixture.session_dir.join(DOCUMENT_NAME).exists());
}

/// The marker alone stops a commit: the directory is still there, so only the
/// tombstone check inside the commit gate can refuse this write.
#[test]
fn a_marked_session_refuses_a_checkpoint_while_its_directory_still_exists() {
    let fixture = setup(Setup::default());
    let release = fixture.provider.hold(0);
    let service = fixture.service();
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for_requests(&fixture.provider, 1);

    fixture
        .lifecycle
        .begin_delete(&fixture.session_dir)
        .unwrap();
    release.send(()).unwrap();
    service.shutdown();

    let run = fixture.run();
    assert!(fixture.session_dir.exists());
    assert_eq!(fixture.provider.count(), 1);
    assert_ne!(run.state, AnalysisState::Complete);
    assert!(
        run.clean_checkpoint
            .units
            .iter()
            .all(|unit| unit.state != CheckpointUnitState::Committed)
    );
    assert_eq!(fixture.document().last_successful_result, None);
}

#[test]
fn a_session_being_deleted_refuses_reads_and_new_runs() {
    let fixture = setup(Setup::default());
    let service = fixture.service();
    fixture
        .lifecycle
        .begin_delete(&fixture.session_dir)
        .unwrap();

    let load = service
        .load(SESSION_ID, OutputLanguage::EnUs)
        .expect_err("a deleted session has no view");
    let generated = fixture
        .generate(&service, GenerateMode::Ensure)
        .expect_err("a deleted session cannot start a run");

    assert_eq!(load.code(), MeetingNotesErrorCode::SessionDeleted);
    assert_eq!(generated.code(), MeetingNotesErrorCode::SessionDeleted);
    assert_eq!(fixture.provider.count(), 0);
}

/// AC-44: replays one crash point of a delete on a restarted process. The
/// `.deleting` sweep runs first, so neither recovery scan can revive the
/// session or recreate its directory.
fn restart_after_delete_crash(forget: bool, remove: bool) {
    let fixture = setup(Setup::default());
    let release = fixture.provider.hold(0);
    let service = fixture.service();
    fixture.generate(&service, GenerateMode::Ensure).unwrap();
    fixture.wait_for_requests(&fixture.provider, 1);
    fixture
        .lifecycle
        .begin_delete(&fixture.session_dir)
        .unwrap();
    if forget {
        service.forget(SESSION_ID).unwrap();
    }
    if remove {
        fixture
            .lifecycle
            .finish_delete(&fixture.session_dir)
            .unwrap();
    }
    release.send(()).unwrap();
    drop(service);

    let restarted = SessionLifecycle::new();
    let completed = restarted.recover_roots(&fixture.catalog.roots()).unwrap();
    let translation = fixture.translation(restarted.clone());
    let notes = fixture.service_using(restarted, FakeProvider::new(), Arc::new(|_| {}));

    assert_eq!(completed, usize::from(!remove));
    assert_eq!(translation.recover_root(&fixture.root()).unwrap(), 0);
    assert_eq!(notes.recover_catalog().unwrap(), 0);
    assert!(!fixture.session_dir.exists());
    assert!(!fixture.session_dir.join(DOCUMENT_NAME).exists());
}

#[test]
fn a_delete_that_crashed_after_the_marker_is_finished_on_the_next_start() {
    restart_after_delete_crash(false, false);
}

#[test]
fn a_delete_that_crashed_after_the_forget_is_finished_on_the_next_start() {
    restart_after_delete_crash(true, false);
}

#[test]
fn a_delete_that_crashed_after_the_directory_removal_stays_deleted() {
    restart_after_delete_crash(true, true);
}

struct EchoServer {
    address: SocketAddr,
}

/// Answers every attempt with a 500 that echoes the request it received, so the
/// test can prove none of it survives into the document or the payload.
fn echo_server() -> EchoServer {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else {
                break;
            };
            let request = read_request(&mut stream);
            let body = json!({ "error": "echoed-request-body", "request": request }).to_string();
            let response = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    EchoServer { address }
}

fn read_request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4_096];
    while let Ok(count) = stream.read(&mut buffer) {
        if count == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..count]);
        let Some(header_end) = request.windows(4).position(|item| item == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let length = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if request.len() >= header_end + 4 + length {
            break;
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}
