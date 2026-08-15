use std::mem::take;

use crate::{
    meeting_notes_document::{
        CleanResult, CleanedPart, CleanedSegment, InputSnapshot, MeetingNotesError,
        MeetingNotesErrorCode, MeetingSummary,
    },
    openai_compatible::{ChatMessage, ChatRole, request_body},
};

use super::{
    failed,
    prompts::{
        CLEAN_SYSTEM_PROMPT, SummaryScope, clean_user_message, reduce_user_message,
        summary_system_prompt, summary_user_message,
    },
};

pub const MINIMUM_BODY_CHARACTERS: usize = 4_000;

const CLOSING_CHARACTERS: [char; 16] = [
    '"', '\'', '”', '’', ')', '）', ']', '］', '}', '｝', '」', '』', '》', '〉', '】', '〕',
];
const SENTENCE_END_CHARACTERS: [char; 9] = ['.', '!', '?', ';', '。', '！', '？', '；', '…'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanChunk {
    pub index: u32,
    pub parts: Vec<CleanedPart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryChunk {
    pub index: u32,
    pub segments: Vec<CleanedSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReduceGroup {
    pub candidates: Vec<MeetingSummary>,
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryPlan {
    Direct,
    MapReduce(Vec<SummaryChunk>),
}

pub fn chat_messages<'a>(system: &'a str, user: &'a str) -> Vec<ChatMessage<'a>> {
    vec![
        ChatMessage {
            content: system,
            role: ChatRole::System,
        },
        ChatMessage {
            content: user,
            role: ChatRole::User,
        },
    ]
}

/// Meters the exact request body the transport sends, counted in Unicode
/// scalars.
pub fn measure_request(model: &str, messages: &[ChatMessage<'_>]) -> usize {
    request_body(model, messages).chars().count()
}

/// Greedy bin packing on whole transcript segments in ascending id order. Only a
/// segment that cannot fit a request on its own is split into parts.
pub fn plan_clean_chunks(snapshot: &InputSnapshot) -> Result<Vec<CleanChunk>, MeetingNotesError> {
    let budget = budget_with_headroom(snapshot, measure_clean(snapshot, &[]))?;
    let mut chunks: Vec<CleanChunk> = Vec::new();
    let mut current: Vec<CleanedPart> = Vec::new();
    for segment in &snapshot.source.selected_segments {
        current.push(CleanedPart {
            part_index: 0,
            segment_id: segment.id,
            text: segment.text.clone(),
        });
        if measure_clean(snapshot, &current) <= budget {
            continue;
        }
        let whole = current.pop().expect("the candidate part was just pushed");
        if !current.is_empty() {
            close_chunk(&mut chunks, &mut current);
            current.push(whole);
            if measure_clean(snapshot, &current) <= budget {
                continue;
            }
            current.pop();
        }
        split_segment(
            snapshot,
            budget,
            segment.id,
            &segment.text,
            &mut chunks,
            &mut current,
        )?;
    }
    if !current.is_empty() {
        close_chunk(&mut chunks, &mut current);
    }
    Ok(chunks)
}

pub fn plan_summary(
    snapshot: &InputSnapshot,
    cleaned: &CleanResult,
) -> Result<SummaryPlan, MeetingNotesError> {
    let direct_budget = budget_with_headroom(
        snapshot,
        measure_summary(snapshot, SummaryScope::Direct, &[]),
    )?;
    if measure_summary(snapshot, SummaryScope::Direct, &cleaned.segments) <= direct_budget {
        return Ok(SummaryPlan::Direct);
    }
    let budget = budget_with_headroom(snapshot, measure_summary(snapshot, SummaryScope::Map, &[]))?;
    let mut chunks: Vec<SummaryChunk> = Vec::new();
    let mut current: Vec<CleanedSegment> = Vec::new();
    for segment in &cleaned.segments {
        current.push(segment.clone());
        if measure_summary(snapshot, SummaryScope::Map, &current) <= budget {
            continue;
        }
        let segment = current
            .pop()
            .expect("the candidate segment was just pushed");
        if !current.is_empty() {
            chunks.push(SummaryChunk {
                index: chunks.len() as u32,
                segments: take(&mut current),
            });
        }
        current.push(segment);
        if measure_summary(snapshot, SummaryScope::Map, &current) > budget {
            return Err(failed(MeetingNotesErrorCode::SummaryCandidateTooLarge));
        }
    }
    if !current.is_empty() {
        chunks.push(SummaryChunk {
            index: chunks.len() as u32,
            segments: current,
        });
    }
    Ok(SummaryPlan::MapReduce(chunks))
}

/// Packs adjacent candidates in their original order so a reduce level keeps the
/// meeting timeline.
pub fn plan_reduce_groups(
    snapshot: &InputSnapshot,
    candidates: &[MeetingSummary],
) -> Result<Vec<ReduceGroup>, MeetingNotesError> {
    let budget = budget_with_headroom(snapshot, measure_reduce(snapshot, &[]))?;
    let mut groups: Vec<ReduceGroup> = Vec::new();
    let mut current: Vec<MeetingSummary> = Vec::new();
    for candidate in candidates {
        current.push(candidate.clone());
        if measure_reduce(snapshot, &current) <= budget {
            continue;
        }
        let candidate = current.pop().expect("the candidate was just pushed");
        if !current.is_empty() {
            groups.push(ReduceGroup {
                candidates: take(&mut current),
                index: groups.len() as u32,
            });
        }
        current.push(candidate);
        if measure_reduce(snapshot, &current) > budget {
            return Err(failed(MeetingNotesErrorCode::SummaryCandidateTooLarge));
        }
    }
    if !current.is_empty() {
        groups.push(ReduceGroup {
            candidates: current,
            index: groups.len() as u32,
        });
    }
    Ok(groups)
}

fn measure_clean(snapshot: &InputSnapshot, parts: &[CleanedPart]) -> usize {
    let user = clean_user_message(snapshot, parts);
    measure_request(
        &snapshot.provider.model,
        &chat_messages(CLEAN_SYSTEM_PROMPT, &user),
    )
}

fn measure_summary(
    snapshot: &InputSnapshot,
    scope: SummaryScope,
    segments: &[CleanedSegment],
) -> usize {
    let system = summary_system_prompt(scope);
    let user = summary_user_message(snapshot, segments);
    measure_request(&snapshot.provider.model, &chat_messages(&system, &user))
}

fn measure_reduce(snapshot: &InputSnapshot, candidates: &[MeetingSummary]) -> usize {
    let system = summary_system_prompt(SummaryScope::Reduce);
    let user = reduce_user_message(snapshot, candidates);
    measure_request(&snapshot.provider.model, &chat_messages(&system, &user))
}

/// The system message and the effective context must leave room for real body
/// text; otherwise no request is ever built.
fn budget_with_headroom(
    snapshot: &InputSnapshot,
    overhead: usize,
) -> Result<usize, MeetingNotesError> {
    let budget = snapshot.provider.max_input_characters as usize;
    if overhead + MINIMUM_BODY_CHARACTERS > budget {
        return Err(MeetingNotesError::context_too_large(
            overhead,
            budget.saturating_sub(MINIMUM_BODY_CHARACTERS),
        ));
    }
    Ok(budget)
}

fn split_segment(
    snapshot: &InputSnapshot,
    budget: usize,
    segment_id: u32,
    text: &str,
    chunks: &mut Vec<CleanChunk>,
    current: &mut Vec<CleanedPart>,
) -> Result<(), MeetingNotesError> {
    let mut remaining = text;
    let mut part_index = 0;
    while !remaining.is_empty() {
        let taken = fitting_prefix(snapshot, budget, segment_id, part_index, remaining, current)?;
        current.push(CleanedPart {
            part_index,
            segment_id,
            text: remaining[..taken].to_owned(),
        });
        remaining = &remaining[taken..];
        part_index += 1;
        if !remaining.is_empty() {
            close_chunk(chunks, current);
        }
    }
    Ok(())
}

/// Binary search on the scalar count, then a backoff to the last sentence
/// boundary. The result is always a `char` boundary.
fn fitting_prefix(
    snapshot: &InputSnapshot,
    budget: usize,
    segment_id: u32,
    part_index: u32,
    remaining: &str,
    current: &mut Vec<CleanedPart>,
) -> Result<usize, MeetingNotesError> {
    let mut fits = |characters: usize| {
        current.push(CleanedPart {
            part_index,
            segment_id,
            text: prefix(remaining, characters).to_owned(),
        });
        let measured = measure_clean(snapshot, current);
        current.pop();
        measured <= budget
    };
    let total = remaining.chars().count();
    if fits(total) {
        return Ok(remaining.len());
    }
    let mut low = 0;
    let mut high = total;
    while low + 1 < high {
        let middle = low + (high - low) / 2;
        if fits(middle) {
            low = middle;
        } else {
            high = middle;
        }
    }
    if low == 0 {
        return Err(MeetingNotesError::context_too_large(
            budget,
            budget.saturating_sub(MINIMUM_BODY_CHARACTERS),
        ));
    }
    let fitting = prefix(remaining, low);
    Ok(sentence_boundary(fitting).unwrap_or(fitting.len()))
}

fn close_chunk(chunks: &mut Vec<CleanChunk>, current: &mut Vec<CleanedPart>) {
    chunks.push(CleanChunk {
        index: chunks.len() as u32,
        parts: take(current),
    });
}

fn prefix(text: &str, characters: usize) -> &str {
    match text.char_indices().nth(characters) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

/// Keeps a sentence boundary only when it still fills half of the fitting
/// prefix, so sparse punctuation cannot shred a long segment.
fn sentence_boundary(text: &str) -> Option<usize> {
    let boundary = last_sentence_boundary(text)?;
    let covered = text[..boundary].chars().count();
    (covered * 2 >= text.chars().count()).then_some(boundary)
}

fn last_sentence_boundary(text: &str) -> Option<usize> {
    let mut boundary = None;
    let mut contiguous = None;
    for (index, character) in text.char_indices() {
        let end = index + character.len_utf8();
        if SENTENCE_END_CHARACTERS.contains(&character)
            || (CLOSING_CHARACTERS.contains(&character) && contiguous == Some(index))
        {
            boundary = Some(end);
            contiguous = Some(end);
        } else {
            contiguous = None;
        }
    }
    boundary
}
