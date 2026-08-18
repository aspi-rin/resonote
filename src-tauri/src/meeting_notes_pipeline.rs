use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    meeting_notes_document::{
        AnalysisFreshness, AnalysisResult, ContextSnapshot, GlobalContextContent, InputQualityKind,
        InputSnapshot, MERGED_CONTEXT_CHARACTER_LIMIT, MeetingContextContent, MeetingNotesError,
        MeetingNotesErrorCode, OutputLanguage, ProviderSnapshot, SourceSegmentSnapshot,
        SourceSessionStatus, SourceSnapshot, SourceTranscriptStatus, StaleReason,
        ensure_within_limit, normalize_global, normalize_meeting, render_merged,
    },
    openai_compatible::{allows_bearer_auth, chat_completions_url},
    settings::{MeetingNotesSettings, ProviderAuthMode},
    storage::{ArchiveStatus, SessionManifest},
    transcription::{
        TranscriptDocument, TranscriptDocumentStatus, TranscriptSegment, TranscriptSegmentStatus,
    },
};

#[path = "meeting_notes_chunking.rs"]
mod chunking;
#[path = "meeting_notes_parsing.rs"]
mod parsing;
#[path = "meeting_notes_prompts.rs"]
mod prompts;
#[path = "meeting_notes_reduce.rs"]
mod reduce;

pub use chunking::{
    CleanChunk, MAX_CLEAN_OUTPUT_CHARACTERS, MAX_OUTPUT_TOKENS, MINIMUM_BODY_CHARACTERS,
    ReduceGroup, SummaryChunk, SummaryPlan, chat_messages, measure_request, plan_clean_chunks,
    plan_reduce_groups, plan_summary,
};
pub use parsing::{
    json_payload, parse_clean_response, parse_summary_response, reassemble_clean_result,
    resolve_context_pointer,
};
pub use prompts::{
    CLEAN_PROMPT_VERSION, CLEAN_SYSTEM_PROMPT, SUMMARY_MAP_PROMPT_VERSION, SUMMARY_PROMPT_VERSION,
    SUMMARY_REDUCE_PROMPT_VERSION, SummaryScope, clean_user_message, reduce_user_message,
    summary_prompt_version, summary_system_prompt, summary_user_message,
};
pub use reduce::{
    MAX_REDUCE_DEPTH, ensure_reduce_converged, merge_summary_candidates, serialized_characters,
};

pub const ANALYSIS_SCHEMA_VERSION: u32 = 1;
/// Bumped to "2" when clean chunking gained an expected-output budget: a run
/// planned by the old chunker must be replanned rather than resumed.
pub const CHUNKER_VERSION: &str = "2";
pub const CONTEXT_SCHEMA_VERSION: u32 = 1;
pub const MERGE_POLICY_VERSION: &str = "1";
pub const PIPELINE_VERSION: &str = "1";
/// Aggregate of every prompt constant in `prompts`: bump it whenever one of them
/// changes so existing results turn stale.
pub const PROMPT_VERSION: &str = "1";

#[derive(Debug, Clone, Copy)]
pub enum TranscriptSource<'a> {
    Corrupt,
    Missing,
    Present(&'a TranscriptDocument),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessDecision {
    pub quality: InputQualityKind,
    pub session_status: SourceSessionStatus,
    pub transcript_status: SourceTranscriptStatus,
}

#[derive(Debug, Clone, Copy)]
pub struct ContextInputs<'a> {
    pub captured_at: DateTime<Utc>,
    pub global: &'a GlobalContextContent,
    pub global_revision: u64,
    pub meeting: &'a MeetingContextContent,
    pub meeting_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshnessView {
    pub freshness: AnalysisFreshness,
    pub stale_reasons: Vec<StaleReason>,
}

/// Both files decide together: `transcript.json` can read `complete` while the
/// recording is still running, so the session status always wins.
pub fn evaluate_readiness(
    session_status: ArchiveStatus,
    transcript: TranscriptSource<'_>,
    accept_partial: bool,
) -> Result<ReadinessDecision, MeetingNotesError> {
    let session_status = match session_status {
        ArchiveStatus::Recording => {
            return Err(failed(MeetingNotesErrorCode::SessionStillRecording));
        }
        ArchiveStatus::Completed => SourceSessionStatus::Completed,
        ArchiveStatus::Interrupted => SourceSessionStatus::Interrupted,
        ArchiveStatus::Failed => SourceSessionStatus::Failed,
    };
    let document = match transcript {
        TranscriptSource::Corrupt => {
            return Err(failed(MeetingNotesErrorCode::TranscriptDocumentCorrupt));
        }
        TranscriptSource::Missing => {
            return Err(failed(MeetingNotesErrorCode::NoTranscriptContent));
        }
        TranscriptSource::Present(document) => document,
    };
    let transcript_status = match document.status {
        TranscriptDocumentStatus::Pending | TranscriptDocumentStatus::Processing => {
            return Err(failed(MeetingNotesErrorCode::TranscriptNotReady));
        }
        TranscriptDocumentStatus::Complete => SourceTranscriptStatus::Complete,
        TranscriptDocumentStatus::Partial => SourceTranscriptStatus::Partial,
    };
    if !document.segments.iter().any(is_usable) {
        return Err(failed(MeetingNotesErrorCode::NoTranscriptContent));
    }
    let quality = if session_status == SourceSessionStatus::Completed
        && transcript_status == SourceTranscriptStatus::Complete
    {
        InputQualityKind::Complete
    } else {
        InputQualityKind::Partial
    };
    if quality == InputQualityKind::Partial && !accept_partial {
        return Err(failed(MeetingNotesErrorCode::PartialConfirmationRequired));
    }
    Ok(ReadinessDecision {
        quality,
        session_status,
        transcript_status,
    })
}

pub fn select_source_snapshot(
    manifest: &SessionManifest,
    transcript: &TranscriptDocument,
    decision: ReadinessDecision,
) -> Result<SourceSnapshot, MeetingNotesError> {
    validate_transcript(manifest, transcript)?;
    let mut selected = Vec::new();
    let mut failed_segment_ids = Vec::new();
    let mut skipped_empty_segment_ids = Vec::new();
    for segment in &transcript.segments {
        match segment.status {
            TranscriptSegmentStatus::Complete if is_usable(segment) => {
                selected.push(SourceSegmentSnapshot {
                    end_ms: segment.end_ms,
                    id: segment.id,
                    start_ms: segment.start_ms,
                    text: segment.text.trim().to_owned(),
                });
            }
            TranscriptSegmentStatus::Complete => skipped_empty_segment_ids.push(segment.id),
            _ => failed_segment_ids.push(segment.id),
        }
    }
    selected.sort_by_key(|segment| segment.id);
    failed_segment_ids.sort_unstable();
    skipped_empty_segment_ids.sort_unstable();
    Ok(SourceSnapshot {
        complete_segment_ids: selected.iter().map(|segment| segment.id).collect(),
        completed_segment_count: selected.len(),
        failed_segment_count: failed_segment_ids.len(),
        failed_segment_ids,
        selected_segments: selected,
        session_status: decision.session_status,
        skipped_segment_count: skipped_empty_segment_ids.len(),
        skipped_empty_segment_ids,
        transcript_sha256: transcript_sha256(transcript),
        transcript_status: decision.transcript_status,
    })
}

/// Hashes only the canonical business fields, so a retry that rewrites
/// `updatedAt`, `attempts` or `error` leaves an existing result fresh.
pub fn transcript_sha256(transcript: &TranscriptDocument) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CanonicalSegment<'a> {
        end_ms: u64,
        id: u32,
        start_ms: u64,
        status: TranscriptSegmentStatus,
        text: &'a str,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CanonicalTranscript<'a> {
        segments: Vec<CanonicalSegment<'a>>,
        session_id: &'a str,
        status: TranscriptDocumentStatus,
    }

    sha256(&canonical_json(&CanonicalTranscript {
        segments: transcript
            .segments
            .iter()
            .map(|segment| CanonicalSegment {
                end_ms: segment.end_ms,
                id: segment.id,
                start_ms: segment.start_ms,
                status: segment.status,
                text: &segment.text,
            })
            .collect(),
        session_id: &transcript.session_id,
        status: transcript.status,
    }))
}

pub fn build_context_snapshot(
    inputs: ContextInputs<'_>,
) -> Result<ContextSnapshot, MeetingNotesError> {
    let global = normalize_global(inputs.global);
    let meeting = normalize_meeting(inputs.meeting);
    let rendered = render_merged(&global, &meeting);
    ensure_within_limit(&rendered, MERGED_CONTEXT_CHARACTER_LIMIT)?;
    Ok(ContextSnapshot {
        captured_at: inputs.captured_at,
        global,
        global_revision: inputs.global_revision,
        meeting,
        meeting_revision: inputs.meeting_revision,
        merge_policy_version: MERGE_POLICY_VERSION.to_owned(),
        rendered_sha256: sha256(&rendered),
        schema_version: CONTEXT_SCHEMA_VERSION,
    })
}

pub fn build_provider_snapshot(
    settings: &MeetingNotesSettings,
) -> Result<ProviderSnapshot, MeetingNotesError> {
    let model = settings.model.trim();
    if settings.endpoint.trim().is_empty() || model.is_empty() {
        return Err(failed(MeetingNotesErrorCode::MeetingNotesNotConfigured));
    }
    let endpoint = chat_completions_url(&settings.endpoint)
        .map_err(|_| failed(MeetingNotesErrorCode::InvalidEndpoint))?;
    let auth_mode = if settings.credential_configured() {
        ProviderAuthMode::Bearer
    } else {
        ProviderAuthMode::None
    };
    if auth_mode == ProviderAuthMode::Bearer && !allows_bearer_auth(&endpoint) {
        return Err(failed(MeetingNotesErrorCode::InsecureEndpoint));
    }
    Ok(ProviderSnapshot {
        auth_mode,
        endpoint: endpoint.to_string(),
        max_input_characters: settings.max_input_characters,
        model: model.to_owned(),
        request_timeout_seconds: settings.request_timeout_seconds,
    })
}

pub fn build_input_snapshot(
    context: ContextSnapshot,
    output_language: OutputLanguage,
    provider: ProviderSnapshot,
    source: SourceSnapshot,
) -> InputSnapshot {
    InputSnapshot {
        chunker_version: CHUNKER_VERSION.to_owned(),
        context,
        output_language,
        pipeline_version: PIPELINE_VERSION.to_owned(),
        prompt_version: PROMPT_VERSION.to_owned(),
        provider,
        source,
    }
}

/// Canonical JSON with a fixed key order. It carries no API key, no timestamp,
/// no revision, no attempt count and no regeneration nonce.
pub fn input_fingerprint(snapshot: &InputSnapshot) -> String {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FingerprintContext<'a> {
        global: &'a GlobalContextContent,
        meeting: &'a MeetingContextContent,
        rendered_sha256: &'a str,
        schema_version: u32,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FingerprintProvider<'a> {
        auth_mode: ProviderAuthMode,
        endpoint: &'a str,
        max_input_characters: u32,
        model: &'a str,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FingerprintVersions<'a> {
        chunker: &'a str,
        merge_policy: &'a str,
        pipeline: &'a str,
        prompt: &'a str,
        schema: u32,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FingerprintInput<'a> {
        context: FingerprintContext<'a>,
        output_language: OutputLanguage,
        provider: FingerprintProvider<'a>,
        source: &'a SourceSnapshot,
        versions: FingerprintVersions<'a>,
    }

    sha256(&canonical_json(&FingerprintInput {
        context: FingerprintContext {
            global: &snapshot.context.global,
            meeting: &snapshot.context.meeting,
            rendered_sha256: &snapshot.context.rendered_sha256,
            schema_version: snapshot.context.schema_version,
        },
        output_language: snapshot.output_language,
        provider: FingerprintProvider {
            auth_mode: snapshot.provider.auth_mode,
            endpoint: &snapshot.provider.endpoint,
            max_input_characters: snapshot.provider.max_input_characters,
            model: &snapshot.provider.model,
        },
        source: &snapshot.source,
        versions: FingerprintVersions {
            chunker: &snapshot.chunker_version,
            merge_policy: &snapshot.context.merge_policy_version,
            pipeline: &snapshot.pipeline_version,
            prompt: &snapshot.prompt_version,
            schema: ANALYSIS_SCHEMA_VERSION,
        },
    }))
}

pub fn run_fingerprint(input_fingerprint: &str, regeneration_nonce: u64) -> String {
    sha256(&format!("{input_fingerprint}:{regeneration_nonce}"))
}

/// Derived at read time from the current normalized inputs and never persisted.
pub fn derive_freshness(result: Option<&AnalysisResult>, current: &InputSnapshot) -> FreshnessView {
    let Some(result) = result else {
        return FreshnessView {
            freshness: AnalysisFreshness::None,
            stale_reasons: Vec::new(),
        };
    };
    let stored = &result.input_snapshot;
    let mut stale_reasons = Vec::new();
    if stored.context.global != current.context.global {
        stale_reasons.push(StaleReason::GlobalContextChanged);
    }
    if stored.context.meeting != current.context.meeting {
        stale_reasons.push(StaleReason::MeetingContextChanged);
    }
    if stored.source != current.source {
        stale_reasons.push(StaleReason::TranscriptChanged);
    }
    if provider_identity(&stored.provider) != provider_identity(&current.provider) {
        stale_reasons.push(StaleReason::ProviderChanged);
    }
    if stored.output_language != current.output_language {
        stale_reasons.push(StaleReason::OutputLanguageChanged);
    }
    if pipeline_identity(stored) != pipeline_identity(current) {
        stale_reasons.push(StaleReason::PipelineChanged);
    }
    if stale_reasons.is_empty() && result.input_fingerprint != input_fingerprint(current) {
        stale_reasons.push(StaleReason::PipelineChanged);
    }
    FreshnessView {
        freshness: if stale_reasons.is_empty() {
            AnalysisFreshness::Fresh
        } else {
            AnalysisFreshness::Stale
        },
        stale_reasons,
    }
}

/// The backend accepts the two task languages only; anything else is rejected
/// before it can be frozen into a snapshot.
pub fn parse_output_language(value: &str) -> Option<OutputLanguage> {
    match value.trim() {
        "zh-CN" => Some(OutputLanguage::ZhCn),
        "en-US" => Some(OutputLanguage::EnUs),
        _ => None,
    }
}

pub(crate) fn failed(code: MeetingNotesErrorCode) -> MeetingNotesError {
    MeetingNotesError::new(code)
}

pub(crate) fn sha256(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn canonical_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("canonical input is always serializable")
}

fn is_usable(segment: &TranscriptSegment) -> bool {
    segment.status == TranscriptSegmentStatus::Complete && !segment.text.trim().is_empty()
}

fn validate_transcript(
    manifest: &SessionManifest,
    transcript: &TranscriptDocument,
) -> Result<(), MeetingNotesError> {
    if transcript.session_id != manifest.session_id {
        return Err(failed(MeetingNotesErrorCode::TranscriptInvalid));
    }
    let mut seen = HashSet::new();
    for segment in &transcript.segments {
        let unfinished = matches!(
            segment.status,
            TranscriptSegmentStatus::Pending | TranscriptSegmentStatus::Processing
        );
        if unfinished || segment.start_ms > segment.end_ms || !seen.insert(segment.id) {
            return Err(failed(MeetingNotesErrorCode::TranscriptInvalid));
        }
    }
    Ok(())
}

fn provider_identity(provider: &ProviderSnapshot) -> (ProviderAuthMode, &str, u32, &str) {
    (
        provider.auth_mode,
        &provider.endpoint,
        provider.max_input_characters,
        &provider.model,
    )
}

fn pipeline_identity(snapshot: &InputSnapshot) -> (&str, &str, &str, &str) {
    (
        &snapshot.chunker_version,
        &snapshot.context.merge_policy_version,
        &snapshot.pipeline_version,
        &snapshot.prompt_version,
    )
}

#[cfg(test)]
#[path = "meeting_notes_pipeline_tests.rs"]
mod tests;
