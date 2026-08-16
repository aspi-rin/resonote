use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::meeting_notes_document::{
    ActionItem, BackgroundStatement, CleanPartResult, CleanResult, CleanedPart, CleanedSegment,
    ContextSnapshot, InputSnapshot, MeetingNotesError, MeetingNotesErrorCode, MeetingSummary,
    SourcedStatement,
};

use super::failed;

/// Accepts bare JSON or exactly one Markdown JSON fence. Any other prose around
/// the payload fails the block so it can be retried.
pub fn parse_clean_response(
    body: &str,
    requested: &[CleanedPart],
) -> Result<CleanPartResult, MeetingNotesError> {
    let invalid = || failed(MeetingNotesErrorCode::CleanOutputInvalid);
    let payload = json_payload(body).ok_or_else(invalid)?;
    let parsed: CleanPartResult = serde_json::from_str(payload).map_err(|_| invalid())?;
    if parsed.parts.len() != requested.len() {
        return Err(invalid());
    }
    for (returned, expected) in parsed.parts.iter().zip(requested) {
        let same_key = returned.segment_id == expected.segment_id
            && returned.part_index == expected.part_index;
        if !same_key || returned.text.trim().is_empty() {
            return Err(invalid());
        }
    }
    Ok(parsed)
}

/// Joins every part of a segment in `partIndex` order and restores the frozen
/// ids and timestamps: the model never owns them.
pub fn reassemble_clean_result(
    snapshot: &InputSnapshot,
    parts: &[CleanedPart],
) -> Result<CleanResult, MeetingNotesError> {
    let invalid = || failed(MeetingNotesErrorCode::CleanOutputInvalid);
    let mut grouped: HashMap<u32, Vec<&CleanedPart>> = HashMap::new();
    for part in parts {
        grouped.entry(part.segment_id).or_default().push(part);
    }
    let mut segments = Vec::with_capacity(snapshot.source.selected_segments.len());
    for source in &snapshot.source.selected_segments {
        let mut owned = grouped.remove(&source.id).ok_or_else(invalid)?;
        owned.sort_by_key(|part| part.part_index);
        let count = owned.len() as u32;
        if !owned.iter().map(|part| part.part_index).eq(0..count) {
            return Err(invalid());
        }
        let text = owned
            .iter()
            .map(|part| part.text.as_str())
            .collect::<String>();
        if text.trim().is_empty() {
            return Err(invalid());
        }
        segments.push(CleanedSegment {
            end_ms: source.end_ms,
            segment_id: source.id,
            start_ms: source.start_ms,
            text,
        });
    }
    if !grouped.is_empty() {
        return Err(invalid());
    }
    Ok(CleanResult { segments })
}

pub fn parse_summary_response(
    body: &str,
    cleaned: &CleanResult,
    context: &ContextSnapshot,
) -> Result<MeetingSummary, MeetingNotesError> {
    let invalid = || failed(MeetingNotesErrorCode::SummaryOutputInvalid);
    let payload = json_payload(body).ok_or_else(invalid)?;
    let parsed: MeetingSummary = serde_json::from_str(payload).map_err(|_| invalid())?;
    let known = cleaned
        .segments
        .iter()
        .map(|segment| segment.segment_id)
        .collect::<HashSet<_>>();
    let context = serde_json::to_value(context).map_err(|_| invalid())?;
    let title = parsed.title.trim().to_owned();
    if title.is_empty() {
        return Err(invalid());
    }
    Ok(MeetingSummary {
        action_items: parsed
            .action_items
            .iter()
            .map(|item| validate_action_item(item, &known))
            .collect::<Result<_, _>>()?,
        background: parsed
            .background
            .iter()
            .map(|entry| validate_background(entry, &context))
            .collect::<Result<_, _>>()?,
        decisions: validate_statements(&parsed.decisions, &known)?,
        key_points: validate_statements(&parsed.key_points, &known)?,
        open_questions: validate_statements(&parsed.open_questions, &known)?,
        overview: validate_statement(&parsed.overview, &known)?,
        title,
    })
}

/// RFC 6901 pointer resolution against the frozen context snapshot, with `~1`
/// and `~0` unescaped in that order.
pub fn resolve_context_pointer<'a>(context: &'a Value, pointer: &str) -> Option<&'a Value> {
    if !pointer.starts_with('/') {
        return None;
    }
    let mut current = context;
    for token in pointer.split('/').skip(1) {
        let token = token.replace("~1", "/").replace("~0", "~");
        current = match current {
            Value::Array(items) => items.get(array_index(&token)?)?,
            Value::Object(fields) => fields.get(&token)?,
            _ => return None,
        };
    }
    Some(current)
}

/// The one place a model reply is unwrapped: bare JSON or exactly one Markdown
/// JSON fence, and nothing else.
pub fn json_payload(body: &str) -> Option<&str> {
    let trimmed = body.trim();
    let Some(fenced) = trimmed.strip_prefix("```") else {
        return Some(trimmed);
    };
    let (tag, remainder) = fenced.split_once('\n')?;
    if !matches!(tag.trim().to_ascii_lowercase().as_str(), "" | "json") {
        return None;
    }
    let end = remainder.find("```")?;
    remainder[end + 3..]
        .trim()
        .is_empty()
        .then(|| remainder[..end].trim())
}

fn array_index(token: &str) -> Option<usize> {
    if token.len() > 1 && token.starts_with('0') {
        return None;
    }
    token.parse().ok()
}

fn validate_statements(
    statements: &[SourcedStatement],
    known: &HashSet<u32>,
) -> Result<Vec<SourcedStatement>, MeetingNotesError> {
    statements
        .iter()
        .map(|statement| validate_statement(statement, known))
        .collect()
}

fn validate_statement(
    statement: &SourcedStatement,
    known: &HashSet<u32>,
) -> Result<SourcedStatement, MeetingNotesError> {
    let text = statement.text.trim().to_owned();
    if text.is_empty() {
        return Err(failed(MeetingNotesErrorCode::SummaryOutputInvalid));
    }
    Ok(SourcedStatement {
        source_segment_ids: validate_source_ids(&statement.source_segment_ids, known)?,
        text,
    })
}

fn validate_action_item(
    item: &ActionItem,
    known: &HashSet<u32>,
) -> Result<ActionItem, MeetingNotesError> {
    let task = item.task.trim().to_owned();
    if task.is_empty() {
        return Err(failed(MeetingNotesErrorCode::SummaryOutputInvalid));
    }
    Ok(ActionItem {
        due_date: optional(item.due_date.as_deref()),
        owner: optional(item.owner.as_deref()),
        source_segment_ids: validate_source_ids(&item.source_segment_ids, known)?,
        task,
    })
}

fn validate_background(
    entry: &BackgroundStatement,
    context: &Value,
) -> Result<BackgroundStatement, MeetingNotesError> {
    let text = entry.text.trim().to_owned();
    let resolves = entry
        .context_paths
        .iter()
        .all(|path| resolve_context_pointer(context, path.trim()).is_some());
    if text.is_empty() || entry.context_paths.is_empty() || !resolves {
        return Err(failed(MeetingNotesErrorCode::SummaryOutputInvalid));
    }
    Ok(BackgroundStatement {
        context_paths: entry
            .context_paths
            .iter()
            .map(|path| path.trim().to_owned())
            .collect(),
        text,
    })
}

fn validate_source_ids(ids: &[u32], known: &HashSet<u32>) -> Result<Vec<u32>, MeetingNotesError> {
    let mut seen = HashSet::new();
    if ids.is_empty() || !ids.iter().all(|id| known.contains(id) && seen.insert(*id)) {
        return Err(failed(MeetingNotesErrorCode::SummaryOutputInvalid));
    }
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    Ok(sorted)
}

fn optional(value: Option<&str>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
