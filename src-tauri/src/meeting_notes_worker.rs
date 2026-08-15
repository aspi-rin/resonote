use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use chrono::Utc;
use serde::Serialize;

use crate::{
    meeting_notes_document::{
        AnalysisResult, AnalysisState, CheckpointStage, CheckpointUnit, CheckpointUnitState,
        CleanResult, CleanedPart, CleanedSegment, DOCUMENT_NAME, InputQuality, InputQualityKind,
        InputSnapshot, MeetingNotesDocument, MeetingNotesError, MeetingNotesErrorCode,
        MeetingNotesRun, MeetingSummary, RunError, RunProgress, RunStage, SourceSessionStatus,
        SourceTranscriptStatus, SummaryMode, load_document, save_document,
    },
    meeting_notes_pipeline::{
        CLEAN_SYSTEM_PROMPT, SummaryPlan, SummaryScope, chat_messages, clean_user_message,
        ensure_reduce_converged, merge_summary_candidates, parse_clean_response,
        parse_summary_response, plan_clean_chunks, plan_reduce_groups, plan_summary,
        reassemble_clean_result, reduce_user_message, summary_system_prompt, summary_user_message,
    },
    openai_compatible::{ChatError, ChatRequest},
};

use super::{
    JobTicket, MAX_ATTEMPTS, MeetingNotesInner, RETRY_DELAYS, WORKER_POLL_INTERVAL, status_event,
    unit_hash,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitSlot {
    Clean(usize),
    Summary(usize),
}

/// `Terminal` still runs every identity check but ignores the cancellation flag,
/// so a cancelled or failed run can persist its own end state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitKind {
    Live,
    Terminal,
}

enum CommitGate {
    Cancelled(Box<MeetingNotesDocument>),
    Dropped,
    Open(Box<MeetingNotesDocument>),
}

enum RunOutcome {
    Cancelled,
    Dropped,
    Failed,
}

struct UnitRequest<'a> {
    slot: UnitSlot,
    snapshot: &'a InputSnapshot,
    stage: RunStage,
    system: &'a str,
    user: &'a str,
}

struct UnitSpec {
    input_sha256: String,
    source_keys: Vec<String>,
    stage: CheckpointStage,
    unit_id: String,
}

pub(super) fn worker_loop(
    inner: Arc<MeetingNotesInner>,
    receiver: crossbeam_channel::Receiver<()>,
    acknowledge: crossbeam_channel::Sender<()>,
) {
    while !inner.stop.load(Ordering::Acquire) {
        let _ = receiver.recv_timeout(WORKER_POLL_INTERVAL);
        while !inner.stop.load(Ordering::Acquire) {
            let Some(ticket) = inner.next_job() else {
                break;
            };
            // A panic inside one job must never take the recording and
            // transcription services down with it.
            if catch_unwind(AssertUnwindSafe(|| run_job(&inner, &ticket))).is_err() {
                tracing::error!(
                    job_id = %ticket.job_id,
                    session_id = %ticket.session_id,
                    "meeting notes job panicked"
                );
                let _ = fail_run(
                    &inner,
                    &ticket,
                    run_error(MeetingNotesErrorCode::IoError, None, None, None),
                );
            }
        }
    }
    let _ = acknowledge.try_send(());
}

fn run_job(inner: &MeetingNotesInner, ticket: &JobTicket) {
    match execute(inner, ticket) {
        Ok(()) | Err(RunOutcome::Failed) => {}
        Err(RunOutcome::Cancelled) => finish_cancelled(inner, ticket),
        Err(RunOutcome::Dropped) => tracing::info!(
            job_id = %ticket.job_id,
            session_id = %ticket.session_id,
            "dropped a stale meeting notes run"
        ),
    }
}

fn execute(inner: &MeetingNotesInner, ticket: &JobTicket) -> Result<(), RunOutcome> {
    let document = open(inner, ticket)?;
    let run = current_run(&document)?;
    let snapshot = run.input_snapshot.clone();
    commit(inner, ticket, CommitKind::Live, |document| {
        let Some(run) = document.current_run.as_mut() else {
            return;
        };
        run.error = None;
        run.progress = RunProgress {
            completed_chunks: committed_count(&run.clean_checkpoint.units),
            total_chunks: run.clean_checkpoint.units.len() as u32,
        };
        run.stage = Some(RunStage::Cleaning);
        run.state = AnalysisState::Cleaning;
    })?;
    let cleaned = run_clean_stage(inner, ticket, &snapshot)?;
    commit(inner, ticket, CommitKind::Live, |document| {
        let Some(run) = document.current_run.as_mut() else {
            return;
        };
        run.stage = Some(RunStage::Summarizing);
        run.state = AnalysisState::Summarizing;
    })?;
    let summary = run_summary_stage(inner, ticket, &snapshot, &cleaned)?;
    let result = build_result(&snapshot, run, cleaned, summary);
    commit(inner, ticket, CommitKind::Live, move |document| {
        document.last_successful_result = Some(result);
        let Some(run) = document.current_run.as_mut() else {
            return;
        };
        run.error = None;
        run.stage = None;
        run.state = AnalysisState::Complete;
    })
}

fn run_clean_stage(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    snapshot: &InputSnapshot,
) -> Result<CleanResult, RunOutcome> {
    let chunks = plan_clean_chunks(snapshot)
        .map_err(|error| fail_stage(inner, ticket, &error, RunStage::Cleaning))?;
    for chunk in &chunks {
        let index = chunk.index as usize;
        if clean_unit_committed(inner, ticket, index)? {
            continue;
        }
        let user = clean_user_message(snapshot, &chunk.parts);
        let requested = chunk.parts.clone();
        let output = request_with_retry(
            inner,
            ticket,
            UnitRequest {
                slot: UnitSlot::Clean(index),
                snapshot,
                stage: RunStage::Cleaning,
                system: CLEAN_SYSTEM_PROMPT,
                user: &user,
            },
            &|body| parse_clean_response(body, &requested),
        )?;
        let hash = unit_hash(&output);
        commit(inner, ticket, CommitKind::Live, move |document| {
            let Some(run) = document.current_run.as_mut() else {
                return;
            };
            finish_unit(run.clean_checkpoint.units.get_mut(index), output, hash);
            run.progress.completed_chunks = committed_count(&run.clean_checkpoint.units);
        })?;
    }
    let parts = collect_clean_parts(inner, ticket)?;
    reassemble_clean_result(snapshot, &parts)
        .map_err(|error| fail_stage(inner, ticket, &error, RunStage::Cleaning))
}

fn run_summary_stage(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    snapshot: &InputSnapshot,
    cleaned: &CleanResult,
) -> Result<MeetingSummary, RunOutcome> {
    let document = open(inner, ticket)?;
    let checkpoint = &current_run(&document)?.summary_checkpoint;
    // A stored reduce level holds that level's own input candidates, so the loop
    // has to redo it instead of stepping past it.
    let resumed = checkpoint.reduce_level > 0 && !checkpoint.units.is_empty();
    let mut level = if resumed {
        checkpoint.reduce_level - 1
    } else {
        0
    };
    let mut candidates = if resumed {
        checkpoint.candidates.clone()
    } else {
        run_first_pass(inner, ticket, snapshot, cleaned)?
    };
    while candidates.len() > 1 {
        level += 1;
        candidates = run_reduce_level(inner, ticket, snapshot, cleaned, level, candidates)?;
    }
    candidates.into_iter().next().ok_or_else(|| {
        fail_run(
            inner,
            ticket,
            run_error(
                MeetingNotesErrorCode::SummaryOutputInvalid,
                None,
                None,
                Some(RunStage::Summarizing),
            ),
        )
    })
}

/// Direct summary or the map level of a map/reduce run.
fn run_first_pass(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    snapshot: &InputSnapshot,
    cleaned: &CleanResult,
) -> Result<Vec<MeetingSummary>, RunOutcome> {
    let plan = plan_summary(snapshot, cleaned)
        .map_err(|error| fail_stage(inner, ticket, &error, RunStage::Summarizing))?;
    let slices: Vec<Vec<CleanedSegment>> = match &plan {
        SummaryPlan::Direct => vec![cleaned.segments.clone()],
        SummaryPlan::MapReduce(chunks) => {
            chunks.iter().map(|chunk| chunk.segments.clone()).collect()
        }
    };
    let (mode, stage, prefix) = match plan {
        SummaryPlan::Direct => (
            SummaryMode::Direct,
            CheckpointStage::SummaryDirect,
            "direct",
        ),
        SummaryPlan::MapReduce(_) => (SummaryMode::MapReduce, CheckpointStage::SummaryMap, "map"),
    };
    let specs = slices
        .iter()
        .enumerate()
        .map(|(index, segments)| UnitSpec {
            input_sha256: unit_hash(segments),
            source_keys: segments
                .iter()
                .map(|segment| segment.segment_id.to_string())
                .collect(),
            stage,
            unit_id: format!("summary-{prefix}-{index}"),
        })
        .collect::<Vec<_>>();
    prepare_summary_units(inner, ticket, mode, 0, &[], &specs)?;
    let scope = if mode == SummaryMode::Direct {
        SummaryScope::Direct
    } else {
        SummaryScope::Map
    };
    let system = summary_system_prompt(scope);
    let mut outputs = Vec::with_capacity(slices.len());
    for (index, segments) in slices.iter().enumerate() {
        if let Some(output) = committed_summary_output(inner, ticket, index)? {
            outputs.push(output);
            continue;
        }
        let user = summary_user_message(snapshot, segments);
        let parsed = request_with_retry(
            inner,
            ticket,
            UnitRequest {
                slot: UnitSlot::Summary(index),
                snapshot,
                stage: RunStage::Summarizing,
                system: &system,
                user: &user,
            },
            &|body| parse_summary_response(body, cleaned, &snapshot.context),
        )?;
        commit_summary_unit(inner, ticket, index, parsed.clone())?;
        outputs.push(parsed);
    }
    Ok(outputs)
}

fn run_reduce_level(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    snapshot: &InputSnapshot,
    cleaned: &CleanResult,
    level: u32,
    inputs: Vec<MeetingSummary>,
) -> Result<Vec<MeetingSummary>, RunOutcome> {
    let groups = plan_reduce_groups(snapshot, &inputs)
        .map_err(|error| fail_stage(inner, ticket, &error, RunStage::Summarizing))?;
    let mut offset = 0;
    let specs = groups
        .iter()
        .map(|group| {
            let keys = (offset..offset + group.candidates.len())
                .map(|position| position.to_string())
                .collect();
            offset += group.candidates.len();
            UnitSpec {
                input_sha256: unit_hash(&group.candidates),
                source_keys: keys,
                stage: CheckpointStage::SummaryReduce,
                unit_id: format!("summary-reduce-{level}-{}", group.index),
            }
        })
        .collect::<Vec<_>>();
    prepare_summary_units(
        inner,
        ticket,
        SummaryMode::MapReduce,
        level,
        &inputs,
        &specs,
    )?;
    let system = summary_system_prompt(SummaryScope::Reduce);
    let mut outputs = Vec::with_capacity(groups.len());
    for (index, group) in groups.iter().enumerate() {
        if let Some(output) = committed_summary_output(inner, ticket, index)? {
            outputs.push(output);
            continue;
        }
        // A group of one has nothing to merge remotely: carrying it forward keeps
        // its source ids and saves a request.
        let merged = if group.candidates.len() == 1 {
            merge_summary_candidates(&group.candidates)
        } else {
            let user = reduce_user_message(snapshot, &group.candidates);
            let parsed = request_with_retry(
                inner,
                ticket,
                UnitRequest {
                    slot: UnitSlot::Summary(index),
                    snapshot,
                    stage: RunStage::Summarizing,
                    system: &system,
                    user: &user,
                },
                &|body| parse_summary_response(body, cleaned, &snapshot.context),
            )?;
            merge_summary_candidates(std::slice::from_ref(&parsed))
        };
        commit_summary_unit(inner, ticket, index, merged.clone())?;
        outputs.push(merged);
    }
    ensure_reduce_converged(&inputs, &outputs, level)
        .map_err(|error| fail_stage(inner, ticket, &error, RunStage::Summarizing))?;
    Ok(outputs)
}

fn request_with_retry<T>(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    request: UnitRequest<'_>,
    parse: &dyn Fn(&str) -> Result<T, MeetingNotesError>,
) -> Result<T, RunOutcome> {
    let provider = &request.snapshot.provider;
    let timeout = Duration::from_secs(u64::from(provider.request_timeout_seconds));
    let mut cause = (MeetingNotesErrorCode::ProviderResponseInvalid, None);
    for attempt in 1..=MAX_ATTEMPTS {
        if let Some(delay) = attempt
            .checked_sub(2)
            .and_then(|index| RETRY_DELAYS.get(index as usize))
        {
            (inner.sleeper)(*delay);
        }
        let slot = request.slot;
        commit(inner, ticket, CommitKind::Live, move |document| {
            let Some(run) = document.current_run.as_mut() else {
                return;
            };
            match slot {
                UnitSlot::Clean(index) => {
                    start_unit(run.clean_checkpoint.units.get_mut(index), attempt)
                }
                UnitSlot::Summary(index) => {
                    start_unit(run.summary_checkpoint.units.get_mut(index), attempt)
                }
            }
        })?;
        let answer = inner.chat.complete(ChatRequest {
            api_key: ticket.api_key.as_ref(),
            endpoint: &provider.endpoint,
            messages: chat_messages(request.system, request.user),
            model: &provider.model,
            timeout,
        });
        // A cancelled or deleted run drops the response it was already waiting for.
        match locked_guard(inner, ticket) {
            CommitGate::Cancelled(_) => return Err(RunOutcome::Cancelled),
            CommitGate::Dropped => return Err(RunOutcome::Dropped),
            CommitGate::Open(_) => {}
        }
        match answer {
            Ok(body) => match parse(&body) {
                Ok(value) => return Ok(value),
                Err(error) => cause = (error.code(), None),
            },
            Err(error) => {
                let code = chat_error_code(&error);
                let status = error.http_status().map(|status| status.as_u16());
                if !attempt_retryable(code) {
                    return Err(fail_unit(
                        inner,
                        ticket,
                        request.slot,
                        run_error(code, None, status, Some(request.stage)),
                    ));
                }
                cause = (code, status);
            }
        }
    }
    Err(fail_unit(
        inner,
        ticket,
        request.slot,
        run_error(
            MeetingNotesErrorCode::RetryExhausted,
            Some(cause.0),
            cause.1,
            Some(request.stage),
        ),
    ))
}

/// The single gate in front of every checkpoint and result write. Callers hold
/// the session lifecycle lock, so the tombstone, the `.deleting` marker, the
/// directory and the run identity are all read in the same critical section the
/// delete takes: a delete can never slip between the check and the write.
fn commit_guard(inner: &MeetingNotesInner, ticket: &JobTicket) -> CommitGate {
    if inner.lifecycle.is_deleting(&ticket.session_dir) || !ticket.session_dir.is_dir() {
        return CommitGate::Dropped;
    }
    let Ok(document) = load_document(&ticket.session_dir.join(DOCUMENT_NAME)) else {
        return CommitGate::Dropped;
    };
    let stale = document.session_id != ticket.session_id
        || document.current_run.as_ref().is_none_or(|run| {
            run.generation != ticket.generation
                || run.job_id != ticket.job_id
                || run.run_fingerprint != ticket.run_fingerprint
        });
    if stale {
        return CommitGate::Dropped;
    }
    let cancelled = document
        .current_run
        .as_ref()
        .is_some_and(|run| run.cancellation_requested);
    if cancelled {
        return CommitGate::Cancelled(Box::new(document));
    }
    CommitGate::Open(Box::new(document))
}

fn locked_guard(inner: &MeetingNotesInner, ticket: &JobTicket) -> CommitGate {
    let lock = inner.session_lock(&ticket.session_dir);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    commit_guard(inner, ticket)
}

fn commit<F>(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    kind: CommitKind,
    mutate: F,
) -> Result<(), RunOutcome>
where
    F: FnOnce(&mut MeetingNotesDocument),
{
    let lock = inner.session_lock(&ticket.session_dir);
    let event = {
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut document = match (commit_guard(inner, ticket), kind) {
            (CommitGate::Dropped, _) => return Err(RunOutcome::Dropped),
            (CommitGate::Cancelled(_), CommitKind::Live) => return Err(RunOutcome::Cancelled),
            (CommitGate::Cancelled(document), CommitKind::Terminal)
            | (CommitGate::Open(document), _) => document,
        };
        mutate(&mut document);
        let now = Utc::now();
        if let Some(run) = document.current_run.as_mut() {
            run.updated_at = now;
        }
        document.document_revision += 1;
        document.updated_at = now;
        if let Err(error) = save_document(&ticket.session_dir.join(DOCUMENT_NAME), &document) {
            tracing::warn!(
                code = %error.code(),
                job_id = %ticket.job_id,
                session_id = %ticket.session_id,
                "failed to persist a meeting notes checkpoint"
            );
            return Err(RunOutcome::Dropped);
        }
        status_event(&ticket.session_id, &document)
    };
    if let Some(event) = event {
        inner.publish(event);
    }
    Ok(())
}

fn open(inner: &MeetingNotesInner, ticket: &JobTicket) -> Result<MeetingNotesDocument, RunOutcome> {
    match locked_guard(inner, ticket) {
        CommitGate::Cancelled(_) => Err(RunOutcome::Cancelled),
        CommitGate::Dropped => Err(RunOutcome::Dropped),
        CommitGate::Open(document) => Ok(*document),
    }
}

fn current_run(document: &MeetingNotesDocument) -> Result<&MeetingNotesRun, RunOutcome> {
    document.current_run.as_ref().ok_or(RunOutcome::Dropped)
}

fn prepare_summary_units(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    mode: SummaryMode,
    reduce_level: u32,
    candidates: &[MeetingSummary],
    specs: &[UnitSpec],
) -> Result<(), RunOutcome> {
    let planned = specs
        .iter()
        .map(|spec| CheckpointUnit {
            attempts: 0,
            error: None,
            input_sha256: spec.input_sha256.clone(),
            next_retry_at: None,
            output: None,
            output_sha256: None,
            source_keys: spec.source_keys.clone(),
            stage: spec.stage,
            state: CheckpointUnitState::Pending,
            unit_id: spec.unit_id.clone(),
        })
        .collect::<Vec<_>>();
    let candidates = candidates.to_vec();
    commit(inner, ticket, CommitKind::Live, move |document| {
        let Some(run) = document.current_run.as_mut() else {
            return;
        };
        let checkpoint = &mut run.summary_checkpoint;
        let resumable = checkpoint.mode == mode
            && checkpoint.reduce_level == reduce_level
            && checkpoint.units.len() == planned.len()
            && checkpoint
                .units
                .iter()
                .zip(&planned)
                .all(|(stored, unit)| stored.input_sha256 == unit.input_sha256);
        if !resumable {
            checkpoint.candidates = candidates;
            checkpoint.mode = mode;
            checkpoint.reduce_level = reduce_level;
            checkpoint.units = planned;
        }
        run.progress = RunProgress {
            completed_chunks: committed_count(&checkpoint.units),
            total_chunks: checkpoint.units.len() as u32,
        };
    })
}

fn commit_summary_unit(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    index: usize,
    output: MeetingSummary,
) -> Result<(), RunOutcome> {
    let hash = unit_hash(&output);
    commit(inner, ticket, CommitKind::Live, move |document| {
        let Some(run) = document.current_run.as_mut() else {
            return;
        };
        finish_unit(run.summary_checkpoint.units.get_mut(index), output, hash);
        run.progress.completed_chunks = committed_count(&run.summary_checkpoint.units);
    })
}

fn clean_unit_committed(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    index: usize,
) -> Result<bool, RunOutcome> {
    let document = open(inner, ticket)?;
    Ok(current_run(&document)?
        .clean_checkpoint
        .units
        .get(index)
        .is_some_and(|unit| unit.state == CheckpointUnitState::Committed && output_is_intact(unit)))
}

fn committed_summary_output(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    index: usize,
) -> Result<Option<MeetingSummary>, RunOutcome> {
    let document = open(inner, ticket)?;
    Ok(current_run(&document)?
        .summary_checkpoint
        .units
        .get(index)
        .filter(|unit| unit.state == CheckpointUnitState::Committed && output_is_intact(unit))
        .and_then(|unit| unit.output.clone()))
}

fn collect_clean_parts(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
) -> Result<Vec<CleanedPart>, RunOutcome> {
    let document = open(inner, ticket)?;
    Ok(current_run(&document)?
        .clean_checkpoint
        .units
        .iter()
        .filter(|unit| unit.state == CheckpointUnitState::Committed)
        .filter_map(|unit| unit.output.as_ref())
        .flat_map(|output| output.parts.clone())
        .collect())
}

fn build_result(
    snapshot: &InputSnapshot,
    run: &MeetingNotesRun,
    cleaned: CleanResult,
    summary: MeetingSummary,
) -> AnalysisResult {
    let source = &snapshot.source;
    AnalysisResult {
        cleaned,
        generated_at: Utc::now(),
        input_fingerprint: run.input_fingerprint.clone(),
        input_quality: InputQuality {
            completed_segment_count: source.completed_segment_count,
            failed_segment_count: source.failed_segment_count,
            kind: if source.session_status == SourceSessionStatus::Completed
                && source.transcript_status == SourceTranscriptStatus::Complete
            {
                InputQualityKind::Complete
            } else {
                InputQualityKind::Partial
            },
            skipped_segment_count: source.skipped_segment_count,
        },
        input_snapshot: snapshot.clone(),
        output_language: snapshot.output_language,
        run_fingerprint: run.run_fingerprint.clone(),
        summary,
    }
}

fn finish_cancelled(inner: &MeetingNotesInner, ticket: &JobTicket) {
    let _ = commit(inner, ticket, CommitKind::Terminal, |document| {
        let Some(run) = document.current_run.as_mut() else {
            return;
        };
        rewind_processing(&mut run.clean_checkpoint.units);
        rewind_processing(&mut run.summary_checkpoint.units);
        run.state = AnalysisState::Cancelled;
    });
}

fn fail_stage(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    error: &MeetingNotesError,
    stage: RunStage,
) -> RunOutcome {
    fail_run(
        inner,
        ticket,
        run_error(error.code(), None, None, Some(stage)),
    )
}

fn fail_unit(
    inner: &MeetingNotesInner,
    ticket: &JobTicket,
    slot: UnitSlot,
    error: RunError,
) -> RunOutcome {
    let unit_error = error.clone();
    let outcome = commit(inner, ticket, CommitKind::Terminal, move |document| {
        let Some(run) = document.current_run.as_mut() else {
            return;
        };
        match slot {
            UnitSlot::Clean(index) => {
                stop_unit(run.clean_checkpoint.units.get_mut(index), &unit_error)
            }
            UnitSlot::Summary(index) => {
                stop_unit(run.summary_checkpoint.units.get_mut(index), &unit_error)
            }
        }
        run.error = Some(unit_error);
        run.state = AnalysisState::Failed;
    });
    report_failure(ticket, &error);
    outcome.err().unwrap_or(RunOutcome::Failed)
}

fn fail_run(inner: &MeetingNotesInner, ticket: &JobTicket, error: RunError) -> RunOutcome {
    let run_failure = error.clone();
    let outcome = commit(inner, ticket, CommitKind::Terminal, move |document| {
        let Some(run) = document.current_run.as_mut() else {
            return;
        };
        run.error = Some(run_failure);
        run.state = AnalysisState::Failed;
    });
    report_failure(ticket, &error);
    outcome.err().unwrap_or(RunOutcome::Failed)
}

/// Category, status, job and session only: no request body, provider body,
/// context or transcript ever reaches the log.
fn report_failure(ticket: &JobTicket, error: &RunError) {
    tracing::warn!(
        cause_code = ?error.cause_code.map(|code| code.as_str()),
        code = %error.code,
        http_status = ?error.http_status,
        job_id = %ticket.job_id,
        session_id = %ticket.session_id,
        "meeting notes run failed"
    );
}

pub(super) fn run_error(
    code: MeetingNotesErrorCode,
    cause_code: Option<MeetingNotesErrorCode>,
    http_status: Option<u16>,
    stage: Option<RunStage>,
) -> RunError {
    RunError {
        cause_code,
        code,
        http_status,
        message_key: code.message_key(),
        retryable: attempt_retryable(cause_code.unwrap_or(code)),
        stage,
    }
}

pub(super) fn output_is_intact<T: Serialize>(unit: &CheckpointUnit<T>) -> bool {
    unit.output
        .as_ref()
        .is_some_and(|output| unit.output_sha256.as_deref() == Some(unit_hash(output).as_str()))
}

/// Attempt level retryability: network trouble, throttling, timeouts and output
/// that failed validation are worth another attempt.
fn attempt_retryable(code: MeetingNotesErrorCode) -> bool {
    matches!(
        code,
        MeetingNotesErrorCode::CleanOutputInvalid
            | MeetingNotesErrorCode::IoError
            | MeetingNotesErrorCode::ProviderRateLimited
            | MeetingNotesErrorCode::ProviderResponseInvalid
            | MeetingNotesErrorCode::ProviderTimeout
            | MeetingNotesErrorCode::ProviderUnavailable
            | MeetingNotesErrorCode::SummaryOutputInvalid
    )
}

fn chat_error_code(error: &ChatError) -> MeetingNotesErrorCode {
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

fn committed_count<T>(units: &[CheckpointUnit<T>]) -> u32 {
    units
        .iter()
        .filter(|unit| unit.state == CheckpointUnitState::Committed)
        .count() as u32
}

fn start_unit<T>(unit: Option<&mut CheckpointUnit<T>>, attempt: u32) {
    if let Some(unit) = unit {
        unit.attempts = attempt;
        unit.error = None;
        unit.state = CheckpointUnitState::Processing;
    }
}

fn finish_unit<T>(unit: Option<&mut CheckpointUnit<T>>, output: T, hash: String) {
    if let Some(unit) = unit {
        unit.error = None;
        unit.output = Some(output);
        unit.output_sha256 = Some(hash);
        unit.state = CheckpointUnitState::Committed;
    }
}

fn stop_unit<T>(unit: Option<&mut CheckpointUnit<T>>, error: &RunError) {
    if let Some(unit) = unit {
        unit.error = Some(error.clone());
        unit.state = CheckpointUnitState::Failed;
    }
}

fn rewind_processing<T>(units: &mut [CheckpointUnit<T>]) {
    for unit in units
        .iter_mut()
        .filter(|unit| unit.state == CheckpointUnitState::Processing)
    {
        unit.state = CheckpointUnitState::Pending;
    }
}
