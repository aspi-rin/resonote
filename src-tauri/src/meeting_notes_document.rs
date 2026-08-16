use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::RwLock,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    openai_compatible::ChatError, session_catalog::CatalogError, settings::ProviderAuthMode,
};

pub const DOCUMENT_NAME: &str = "analysis.json";
pub const GLOBAL_CONTEXT_CHARACTER_LIMIT: usize = 20_000;
pub const MEETING_CONTEXT_CHARACTER_LIMIT: usize = 20_000;
pub const MERGED_CONTEXT_CHARACTER_LIMIT: usize = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ContextOrigin {
    Manual,
    DocumentDraft,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextOriginMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    pub origin: ContextOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContextIdentity {
    pub aliases: Vec<String>,
    pub canonical_name: String,
    pub common_asr_errors: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContextEntity {
    pub aliases: Vec<String>,
    pub canonical_name: String,
    pub common_asr_errors: Vec<String>,
    pub description: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ContextOriginMetadata>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GlossaryEntry {
    pub aliases: Vec<String>,
    pub common_asr_errors: Vec<String>,
    pub id: String,
    pub meaning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ContextOriginMetadata>,
    pub term: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ParticipantContext {
    #[serde(flatten)]
    pub entity: ContextEntity,
    pub role: String,
    pub speaker_label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GlobalContextContent {
    pub free_text: String,
    pub glossary: Vec<GlossaryEntry>,
    pub identity: ContextIdentity,
    pub knowledge_background: String,
    pub preferred_languages: Vec<String>,
    pub recurring_organizations: Vec<ContextEntity>,
    pub recurring_people: Vec<ContextEntity>,
    pub recurring_products_and_projects: Vec<ContextEntity>,
    pub roles_and_affiliations: Vec<String>,
    pub timezone: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MeetingContextContent {
    pub agenda: Vec<String>,
    pub background: String,
    pub constraints: Vec<String>,
    pub date: String,
    pub free_text: String,
    pub glossary: Vec<GlossaryEntry>,
    pub key_numbers: Vec<String>,
    pub participants: Vec<ParticipantContext>,
    pub prior_facts_and_decisions: Vec<String>,
    pub purpose: String,
    pub questions_to_discuss: Vec<String>,
    pub source_notes: Vec<String>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalContextDocument {
    pub content: GlobalContextContent,
    pub revision: u64,
    pub schema_version: u32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionedMeetingContext {
    pub content: MeetingContextContent,
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisState {
    Draft,
    Queued,
    Cleaning,
    Summarizing,
    Complete,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalysisFreshness {
    None,
    Fresh,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StaleReason {
    GlobalContextChanged,
    MeetingContextChanged,
    TranscriptChanged,
    ProviderChanged,
    OutputLanguageChanged,
    PipelineChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputLanguage {
    #[serde(rename = "zh-CN")]
    ZhCn,
    #[serde(rename = "en-US")]
    EnUs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceSessionStatus {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceTranscriptStatus {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSegmentSnapshot {
    pub end_ms: u64,
    pub id: u32,
    pub start_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSnapshot {
    pub complete_segment_ids: Vec<u32>,
    pub completed_segment_count: usize,
    pub failed_segment_count: usize,
    pub failed_segment_ids: Vec<u32>,
    pub selected_segments: Vec<SourceSegmentSnapshot>,
    pub session_status: SourceSessionStatus,
    pub skipped_empty_segment_ids: Vec<u32>,
    pub skipped_segment_count: usize,
    pub transcript_sha256: String,
    pub transcript_status: SourceTranscriptStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSnapshot {
    pub captured_at: DateTime<Utc>,
    pub global: GlobalContextContent,
    pub global_revision: u64,
    pub meeting: MeetingContextContent,
    pub meeting_revision: u64,
    pub merge_policy_version: String,
    pub rendered_sha256: String,
    pub schema_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSnapshot {
    pub auth_mode: ProviderAuthMode,
    pub endpoint: String,
    pub max_input_characters: u32,
    pub model: String,
    pub request_timeout_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSnapshot {
    pub chunker_version: String,
    pub context: ContextSnapshot,
    pub output_language: OutputLanguage,
    pub pipeline_version: String,
    pub prompt_version: String,
    pub provider: ProviderSnapshot,
    pub source: SourceSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunStage {
    Cleaning,
    Summarizing,
}

/// Carries only locally generated codes and an HTTP status: never provider
/// bodies, context, transcript text or credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunError {
    pub cause_code: Option<MeetingNotesErrorCode>,
    pub code: MeetingNotesErrorCode,
    pub http_status: Option<u16>,
    pub message_key: String,
    pub retryable: bool,
    pub stage: Option<RunStage>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProgress {
    pub completed_chunks: u32,
    pub total_chunks: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckpointUnitState {
    Pending,
    Processing,
    Committed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CheckpointStage {
    Clean,
    SummaryDirect,
    SummaryMap,
    SummaryReduce,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointUnit<T> {
    pub attempts: u32,
    pub error: Option<RunError>,
    pub input_sha256: String,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub output: Option<T>,
    pub output_sha256: Option<String>,
    pub source_keys: Vec<String>,
    pub stage: CheckpointStage,
    pub state: CheckpointUnitState,
    pub unit_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanCheckpoint {
    pub units: Vec<CheckpointUnit<CleanPartResult>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SummaryMode {
    Direct,
    MapReduce,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryCheckpoint {
    pub candidates: Vec<MeetingSummary>,
    pub mode: SummaryMode,
    pub reduce_level: u32,
    pub units: Vec<CheckpointUnit<MeetingSummary>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingNotesRun {
    pub accept_partial: bool,
    pub cancellation_requested: bool,
    pub clean_checkpoint: CleanCheckpoint,
    pub created_at: DateTime<Utc>,
    pub error: Option<RunError>,
    pub generation: u64,
    pub input_fingerprint: String,
    pub input_snapshot: InputSnapshot,
    pub job_id: String,
    pub progress: RunProgress,
    pub regeneration_nonce: u64,
    pub run_fingerprint: String,
    pub stage: Option<RunStage>,
    pub state: AnalysisState,
    pub summary_checkpoint: SummaryCheckpoint,
    pub updated_at: DateTime<Utc>,
}

/// Internal shape of `analysis.json`. Only `SessionAnalysisView` crosses the
/// Tauri boundary, so checkpoints never reach the WebView.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingNotesDocument {
    pub current_run: Option<MeetingNotesRun>,
    pub document_revision: u64,
    pub last_successful_result: Option<AnalysisResult>,
    pub meeting_context: VersionedMeetingContext,
    pub schema_version: u32,
    pub session_id: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingNotesRunView {
    pub cancellation_requested: bool,
    pub created_at: DateTime<Utc>,
    pub error: Option<RunError>,
    pub generation: u64,
    pub input_fingerprint: String,
    pub job_id: String,
    pub progress: RunProgress,
    pub run_fingerprint: String,
    pub stage: Option<RunStage>,
    pub state: AnalysisState,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAnalysisView {
    pub current_run: Option<MeetingNotesRunView>,
    pub document_revision: u64,
    pub freshness: AnalysisFreshness,
    pub last_successful_result: Option<AnalysisResult>,
    pub meeting_context: VersionedMeetingContext,
    pub session_id: String,
    pub stale_reasons: Vec<StaleReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanedPart {
    pub part_index: u32,
    pub segment_id: u32,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanPartResult {
    pub parts: Vec<CleanedPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanedSegment {
    pub end_ms: u64,
    pub segment_id: u32,
    pub start_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanResult {
    pub segments: Vec<CleanedSegment>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcedStatement {
    pub source_segment_ids: Vec<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundStatement {
    pub context_paths: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionItem {
    pub due_date: Option<String>,
    pub owner: Option<String>,
    pub source_segment_ids: Vec<u32>,
    pub task: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingSummary {
    pub action_items: Vec<ActionItem>,
    pub background: Vec<BackgroundStatement>,
    pub decisions: Vec<SourcedStatement>,
    pub key_points: Vec<SourcedStatement>,
    pub open_questions: Vec<SourcedStatement>,
    pub overview: SourcedStatement,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputQualityKind {
    Complete,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputQuality {
    pub completed_segment_count: usize,
    pub failed_segment_count: usize,
    pub kind: InputQualityKind,
    pub skipped_segment_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisResult {
    pub cleaned: CleanResult,
    pub generated_at: DateTime<Utc>,
    pub input_fingerprint: String,
    pub input_quality: InputQuality,
    pub input_snapshot: InputSnapshot,
    pub output_language: OutputLanguage,
    pub run_fingerprint: String,
    pub summary: MeetingSummary,
}

pub fn empty_global_context_document() -> GlobalContextDocument {
    GlobalContextDocument {
        content: GlobalContextContent::default(),
        revision: 0,
        schema_version: 1,
        updated_at: DateTime::UNIX_EPOCH,
    }
}

pub fn empty_meeting_context() -> VersionedMeetingContext {
    VersionedMeetingContext {
        content: MeetingContextContent::default(),
        revision: 0,
        updated_at: DateTime::UNIX_EPOCH,
    }
}

pub fn new_document(session_id: &str) -> MeetingNotesDocument {
    MeetingNotesDocument {
        current_run: None,
        document_revision: 0,
        last_successful_result: None,
        meeting_context: empty_meeting_context(),
        schema_version: 1,
        session_id: session_id.to_owned(),
        updated_at: Utc::now(),
    }
}

/// Canonical form used by both saving and the run fingerprint: trims every
/// string, drops blank list entries, keeps business order and gives every entry
/// a unique id.
pub fn normalize_global(content: &GlobalContextContent) -> GlobalContextContent {
    GlobalContextContent {
        free_text: content.free_text.trim().to_owned(),
        glossary: normalize_glossary(&content.glossary),
        identity: ContextIdentity {
            aliases: normalize_list(&content.identity.aliases),
            canonical_name: content.identity.canonical_name.trim().to_owned(),
            common_asr_errors: normalize_list(&content.identity.common_asr_errors),
        },
        knowledge_background: content.knowledge_background.trim().to_owned(),
        preferred_languages: normalize_list(&content.preferred_languages),
        recurring_organizations: normalize_entities(&content.recurring_organizations),
        recurring_people: normalize_entities(&content.recurring_people),
        recurring_products_and_projects: normalize_entities(
            &content.recurring_products_and_projects,
        ),
        roles_and_affiliations: normalize_list(&content.roles_and_affiliations),
        timezone: content.timezone.trim().to_owned(),
    }
}

pub fn normalize_meeting(content: &MeetingContextContent) -> MeetingContextContent {
    MeetingContextContent {
        agenda: normalize_list(&content.agenda),
        background: content.background.trim().to_owned(),
        constraints: normalize_list(&content.constraints),
        date: content.date.trim().to_owned(),
        free_text: content.free_text.trim().to_owned(),
        glossary: normalize_glossary(&content.glossary),
        key_numbers: normalize_list(&content.key_numbers),
        participants: normalize_participants(&content.participants),
        prior_facts_and_decisions: normalize_list(&content.prior_facts_and_decisions),
        purpose: content.purpose.trim().to_owned(),
        questions_to_discuss: normalize_list(&content.questions_to_discuss),
        source_notes: normalize_list(&content.source_notes),
        title: content.title.trim().to_owned(),
    }
}

pub fn render_global(content: &GlobalContextContent) -> String {
    render(content)
}

pub fn render_meeting(content: &MeetingContextContent) -> String {
    render(content)
}

/// Renders the effective context in the order the prompt uses: global first,
/// meeting second.
pub fn render_merged(global: &GlobalContextContent, meeting: &MeetingContextContent) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct MergedContext<'a> {
        global_context: &'a GlobalContextContent,
        meeting_context: &'a MeetingContextContent,
    }

    render(&MergedContext {
        global_context: global,
        meeting_context: meeting,
    })
}

pub fn rendered_character_count(rendered: &str) -> usize {
    rendered.chars().count()
}

pub fn ensure_within_limit(rendered: &str, limit: usize) -> Result<(), MeetingNotesError> {
    let characters = rendered_character_count(rendered);
    if characters > limit {
        return Err(MeetingNotesError::context_too_large(characters, limit));
    }
    Ok(())
}

pub fn load_document(path: &Path) -> Result<MeetingNotesDocument, MeetingNotesError> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        MeetingNotesError::new(MeetingNotesErrorCode::AnalysisDocumentCorrupt).with_source(error)
    })
}

/// Never recreates the session directory: a deleted session must stay deleted
/// even when a late write arrives.
pub fn save_document(
    path: &Path,
    document: &MeetingNotesDocument,
) -> Result<(), MeetingNotesError> {
    let parent = path
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or_else(|| MeetingNotesError::new(MeetingNotesErrorCode::SessionDeleted))?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, document)
        .map_err(|error| MeetingNotesError::io().with_source(error))?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

/// Compare-and-swap on the meeting context revision. Identical normalized
/// content leaves the document, its revision and every existing result alone.
pub fn save_meeting_context(
    session_dir: &Path,
    session_id: &str,
    expected_revision: u64,
    content: &MeetingContextContent,
) -> Result<VersionedMeetingContext, MeetingNotesError> {
    let path = session_dir.join(DOCUMENT_NAME);
    let mut document = if path.exists() {
        load_document(&path)?
    } else {
        new_document(session_id)
    };
    if document.session_id != session_id {
        return Err(MeetingNotesError::new(
            MeetingNotesErrorCode::AnalysisDocumentCorrupt,
        ));
    }
    if document.meeting_context.revision != expected_revision {
        return Err(MeetingNotesError::new(
            MeetingNotesErrorCode::ContextRevisionConflict,
        ));
    }
    let normalized = normalize_meeting(content);
    ensure_within_limit(
        &render_meeting(&normalized),
        MEETING_CONTEXT_CHARACTER_LIMIT,
    )?;
    if normalized == document.meeting_context.content {
        return Ok(document.meeting_context);
    }
    let now = Utc::now();
    document.document_revision += 1;
    document.meeting_context = VersionedMeetingContext {
        content: normalized,
        revision: expected_revision + 1,
        updated_at: now,
    };
    document.updated_at = now;
    save_document(&path, &document)?;
    Ok(document.meeting_context)
}

/// In-memory view of `global-context.json`, saved atomically like the settings.
pub struct GlobalContextStore {
    current: RwLock<GlobalContextDocument>,
    path: PathBuf,
}

impl GlobalContextStore {
    pub fn open(path: PathBuf) -> Result<Self, MeetingNotesError> {
        let current = if path.exists() {
            let bytes = fs::read(&path)?;
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(?error, "starting from an empty global context");
                empty_global_context_document()
            })
        } else {
            empty_global_context_document()
        };
        Ok(Self {
            current: RwLock::new(current),
            path,
        })
    }

    pub fn document(&self) -> GlobalContextDocument {
        self.current
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn save(
        &self,
        expected_revision: u64,
        content: &GlobalContextContent,
    ) -> Result<GlobalContextDocument, MeetingNotesError> {
        let mut current = self
            .current
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.revision != expected_revision {
            return Err(MeetingNotesError::new(
                MeetingNotesErrorCode::ContextRevisionConflict,
            ));
        }
        let normalized = normalize_global(content);
        ensure_within_limit(&render_global(&normalized), GLOBAL_CONTEXT_CHARACTER_LIMIT)?;
        if normalized == current.content {
            return Ok(current.clone());
        }
        let updated = GlobalContextDocument {
            content: normalized,
            revision: expected_revision + 1,
            schema_version: 1,
            updated_at: Utc::now(),
        };
        save_global_document(&self.path, &updated)?;
        *current = updated.clone();
        Ok(updated)
    }
}

fn save_global_document(
    path: &Path,
    document: &GlobalContextDocument,
) -> Result<(), MeetingNotesError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, document)
        .map_err(|error| MeetingNotesError::io().with_source(error))?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

fn render<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("context content is always serializable")
}

fn normalize_list(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalize_entities(entries: &[ContextEntity]) -> Vec<ContextEntity> {
    let mut seen = HashSet::new();
    entries
        .iter()
        .map(normalize_entity)
        .filter(|entry| !entity_is_empty(entry))
        .map(|mut entry| {
            entry.id = stable_id(&entry.id, &mut seen);
            entry
        })
        .collect()
}

fn normalize_entity(entry: &ContextEntity) -> ContextEntity {
    ContextEntity {
        aliases: normalize_list(&entry.aliases),
        canonical_name: entry.canonical_name.trim().to_owned(),
        common_asr_errors: normalize_list(&entry.common_asr_errors),
        description: entry.description.trim().to_owned(),
        id: entry.id.trim().to_owned(),
        origin: entry.origin.as_ref().map(normalize_origin),
    }
}

fn normalize_origin(origin: &ContextOriginMetadata) -> ContextOriginMetadata {
    ContextOriginMetadata {
        locator: normalize_optional(origin.locator.as_deref()),
        origin: origin.origin,
        source_id: normalize_optional(origin.source_id.as_deref()),
    }
}

fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn entity_is_empty(entry: &ContextEntity) -> bool {
    entry.aliases.is_empty()
        && entry.canonical_name.is_empty()
        && entry.common_asr_errors.is_empty()
        && entry.description.is_empty()
        && entry.origin.is_none()
}

fn normalize_glossary(entries: &[GlossaryEntry]) -> Vec<GlossaryEntry> {
    let mut seen = HashSet::new();
    entries
        .iter()
        .map(|entry| GlossaryEntry {
            aliases: normalize_list(&entry.aliases),
            common_asr_errors: normalize_list(&entry.common_asr_errors),
            id: entry.id.trim().to_owned(),
            meaning: entry.meaning.trim().to_owned(),
            origin: entry.origin.as_ref().map(normalize_origin),
            term: entry.term.trim().to_owned(),
        })
        .filter(|entry| {
            !(entry.aliases.is_empty()
                && entry.common_asr_errors.is_empty()
                && entry.meaning.is_empty()
                && entry.origin.is_none()
                && entry.term.is_empty())
        })
        .map(|mut entry| {
            entry.id = stable_id(&entry.id, &mut seen);
            entry
        })
        .collect()
}

fn normalize_participants(entries: &[ParticipantContext]) -> Vec<ParticipantContext> {
    let mut seen = HashSet::new();
    entries
        .iter()
        .map(|entry| ParticipantContext {
            entity: normalize_entity(&entry.entity),
            role: entry.role.trim().to_owned(),
            speaker_label: normalize_optional(entry.speaker_label.as_deref()),
        })
        .filter(|entry| {
            !(entity_is_empty(&entry.entity)
                && entry.role.is_empty()
                && entry.speaker_label.is_none())
        })
        .map(|mut entry| {
            entry.entity.id = stable_id(&entry.entity.id, &mut seen);
            entry
        })
        .collect()
}

/// Keeps an existing id when it is unique inside its list and mints a v4 UUID
/// otherwise, so P1 can keep pointing at the same entry.
fn stable_id(id: &str, seen: &mut HashSet<String>) -> String {
    let id = if id.is_empty() || seen.contains(id) {
        Uuid::new_v4().to_string()
    } else {
        id.to_owned()
    };
    seen.insert(id.clone());
    id
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MeetingNotesErrorCode {
    MeetingNotesNotConfigured,
    InvalidEndpoint,
    InsecureEndpoint,
    ResponseTooLarge,
    SessionNotFound,
    SessionIdConflict,
    SessionDeleted,
    SessionStillRecording,
    TranscriptNotReady,
    TranscriptDocumentCorrupt,
    TranscriptInvalid,
    NoTranscriptContent,
    PartialConfirmationRequired,
    ContextRevisionConflict,
    ContextTooLarge,
    ProviderChanged,
    ProviderUnauthorized,
    ProviderForbidden,
    ProviderRateLimited,
    ProviderTimeout,
    ProviderUnavailable,
    ProviderResponseInvalid,
    AnalysisBusy,
    CleanOutputInvalid,
    SummaryOutputInvalid,
    SummaryReduceDidNotConverge,
    SummaryCandidateTooLarge,
    RetryExhausted,
    AnalysisCheckpointCorrupt,
    AnalysisDocumentCorrupt,
    IoError,
}

impl MeetingNotesErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MeetingNotesNotConfigured => "MEETING_NOTES_NOT_CONFIGURED",
            Self::InvalidEndpoint => "INVALID_ENDPOINT",
            Self::InsecureEndpoint => "INSECURE_ENDPOINT",
            Self::ResponseTooLarge => "RESPONSE_TOO_LARGE",
            Self::SessionNotFound => "SESSION_NOT_FOUND",
            Self::SessionIdConflict => "SESSION_ID_CONFLICT",
            Self::SessionDeleted => "SESSION_DELETED",
            Self::SessionStillRecording => "SESSION_STILL_RECORDING",
            Self::TranscriptNotReady => "TRANSCRIPT_NOT_READY",
            Self::TranscriptDocumentCorrupt => "TRANSCRIPT_DOCUMENT_CORRUPT",
            Self::TranscriptInvalid => "TRANSCRIPT_INVALID",
            Self::NoTranscriptContent => "NO_TRANSCRIPT_CONTENT",
            Self::PartialConfirmationRequired => "PARTIAL_CONFIRMATION_REQUIRED",
            Self::ContextRevisionConflict => "CONTEXT_REVISION_CONFLICT",
            Self::ContextTooLarge => "CONTEXT_TOO_LARGE",
            Self::ProviderChanged => "PROVIDER_CHANGED",
            Self::ProviderUnauthorized => "PROVIDER_UNAUTHORIZED",
            Self::ProviderForbidden => "PROVIDER_FORBIDDEN",
            Self::ProviderRateLimited => "PROVIDER_RATE_LIMITED",
            Self::ProviderTimeout => "PROVIDER_TIMEOUT",
            Self::ProviderUnavailable => "PROVIDER_UNAVAILABLE",
            Self::ProviderResponseInvalid => "PROVIDER_RESPONSE_INVALID",
            Self::AnalysisBusy => "ANALYSIS_BUSY",
            Self::CleanOutputInvalid => "CLEAN_OUTPUT_INVALID",
            Self::SummaryOutputInvalid => "SUMMARY_OUTPUT_INVALID",
            Self::SummaryReduceDidNotConverge => "SUMMARY_REDUCE_DID_NOT_CONVERGE",
            Self::SummaryCandidateTooLarge => "SUMMARY_CANDIDATE_TOO_LARGE",
            Self::RetryExhausted => "RETRY_EXHAUSTED",
            Self::AnalysisCheckpointCorrupt => "ANALYSIS_CHECKPOINT_CORRUPT",
            Self::AnalysisDocumentCorrupt => "ANALYSIS_DOCUMENT_CORRUPT",
            Self::IoError => "IO_ERROR",
        }
    }

    /// Flat camelCase key so the frontend localizes without parsing the code.
    pub fn message_key(self) -> String {
        let mut key = String::from("meetingNotesError");
        for word in self.as_str().split('_') {
            let mut characters = word.chars();
            if let Some(first) = characters.next() {
                key.push(first);
                key.extend(characters.map(|character| character.to_ascii_lowercase()));
            }
        }
        key
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::IoError
                | Self::ProviderRateLimited
                | Self::ProviderResponseInvalid
                | Self::ProviderTimeout
                | Self::ProviderUnavailable
        )
    }
}

impl std::fmt::Display for MeetingNotesErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Shared by the notes worker and the settings connection test so both report a
/// provider failure under the same code.
pub fn chat_error_code(error: &ChatError) -> MeetingNotesErrorCode {
    match error {
        ChatError::Forbidden => MeetingNotesErrorCode::ProviderForbidden,
        ChatError::InsecureEndpoint => MeetingNotesErrorCode::InsecureEndpoint,
        ChatError::InvalidEndpoint { .. } | ChatError::UnexpectedStatus(_) => {
            MeetingNotesErrorCode::InvalidEndpoint
        }
        ChatError::ProviderResponseInvalid => MeetingNotesErrorCode::ProviderResponseInvalid,
        ChatError::RateLimited(_) => MeetingNotesErrorCode::ProviderRateLimited,
        ChatError::RequestFailed | ChatError::Unavailable(_) => {
            MeetingNotesErrorCode::ProviderUnavailable
        }
        ChatError::ResponseTooLarge => MeetingNotesErrorCode::ResponseTooLarge,
        ChatError::Timeout => MeetingNotesErrorCode::ProviderTimeout,
        ChatError::Unauthorized => MeetingNotesErrorCode::ProviderUnauthorized,
    }
}

/// IPC error payload. `params` is numeric by construction, so no context,
/// transcript or key material can reach the WebView through it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingNotesErrorPayload {
    pub code: MeetingNotesErrorCode,
    pub message_key: String,
    pub params: BTreeMap<String, u64>,
    pub retryable: bool,
}

#[derive(Debug, Error)]
#[error("meeting notes operation failed: {code}")]
pub struct MeetingNotesError {
    code: MeetingNotesErrorCode,
    params: BTreeMap<String, u64>,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl MeetingNotesError {
    pub fn new(code: MeetingNotesErrorCode) -> Self {
        Self {
            code,
            params: BTreeMap::new(),
            source: None,
        }
    }

    pub fn io() -> Self {
        Self::new(MeetingNotesErrorCode::IoError)
    }

    pub fn context_too_large(characters: usize, limit: usize) -> Self {
        let mut error = Self::new(MeetingNotesErrorCode::ContextTooLarge);
        error
            .params
            .insert("characters".to_owned(), characters as u64);
        error.params.insert("limit".to_owned(), limit as u64);
        error
    }

    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    pub fn code(&self) -> MeetingNotesErrorCode {
        self.code
    }

    pub fn payload(&self) -> MeetingNotesErrorPayload {
        MeetingNotesErrorPayload {
            code: self.code,
            message_key: self.code.message_key(),
            params: self.params.clone(),
            retryable: self.code.retryable(),
        }
    }
}

impl From<std::io::Error> for MeetingNotesError {
    fn from(error: std::io::Error) -> Self {
        Self::io().with_source(error)
    }
}

impl From<tempfile::PersistError> for MeetingNotesError {
    fn from(error: tempfile::PersistError) -> Self {
        Self::io().with_source(error)
    }
}

impl From<CatalogError> for MeetingNotesError {
    fn from(error: CatalogError) -> Self {
        let code = match error {
            CatalogError::InvalidSessionId | CatalogError::SessionNotFound(_) => {
                MeetingNotesErrorCode::SessionNotFound
            }
            CatalogError::SessionIdConflict(_) => MeetingNotesErrorCode::SessionIdConflict,
            _ => MeetingNotesErrorCode::IoError,
        };
        Self::new(code).with_source(error)
    }
}

#[cfg(test)]
#[path = "meeting_notes_document_tests.rs"]
mod tests;
