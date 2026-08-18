use std::collections::HashMap;

use crate::meeting_notes_document::{
    ActionItem, BackgroundStatement, MeetingNotesError, MeetingNotesErrorCode, MeetingSummary,
    SourcedStatement,
};

use super::failed;

pub const MAX_REDUCE_DEPTH: u32 = 8;

/// Merges adjacent candidates in their original order. Two statements are
/// equivalent when their text matches after trimming, collapsing whitespace runs
/// and lowercasing; equivalent statements keep the union of their source ids.
pub fn merge_summary_candidates(candidates: &[MeetingSummary]) -> MeetingSummary {
    MeetingSummary {
        action_items: merge_action_items(candidates),
        background: merge_background(candidates),
        decisions: merge_statements(candidates.iter().flat_map(|entry| &entry.decisions)),
        key_points: merge_statements(candidates.iter().flat_map(|entry| &entry.key_points)),
        open_questions: merge_statements(candidates.iter().flat_map(|entry| &entry.open_questions)),
        overview: merge_overview(candidates),
        title: candidates
            .iter()
            .map(|entry| entry.title.trim())
            .find(|title| !title.is_empty())
            .unwrap_or_default()
            .to_owned(),
    }
}

/// Every level must drop the candidate count or shrink the serialized
/// candidates by at least a fifth, and the recursion stops at depth 8.
pub fn ensure_reduce_converged(
    previous: &[MeetingSummary],
    next: &[MeetingSummary],
    level: u32,
) -> Result<(), MeetingNotesError> {
    if level > MAX_REDUCE_DEPTH {
        return Err(failed(MeetingNotesErrorCode::SummaryReduceDidNotConverge));
    }
    if next.len() < previous.len() {
        return Ok(());
    }
    if serialized_characters(next) * 5 <= serialized_characters(previous) * 4 {
        return Ok(());
    }
    Err(failed(MeetingNotesErrorCode::SummaryReduceDidNotConverge))
}

pub fn serialized_characters(candidates: &[MeetingSummary]) -> usize {
    candidates
        .iter()
        .map(|candidate| {
            serde_json::to_string(candidate)
                .unwrap_or_default()
                .chars()
                .count()
        })
        .sum()
}

fn merge_overview(candidates: &[MeetingSummary]) -> SourcedStatement {
    let merged = merge_statements(
        candidates
            .iter()
            .map(|candidate| &candidate.overview)
            .filter(|overview| !overview.text.trim().is_empty()),
    );
    SourcedStatement {
        source_segment_ids: unique_sorted(
            merged
                .iter()
                .flat_map(|statement| statement.source_segment_ids.iter().copied()),
        ),
        text: merged
            .iter()
            .map(|statement| statement.text.trim())
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn merge_statements<'a>(
    statements: impl Iterator<Item = &'a SourcedStatement>,
) -> Vec<SourcedStatement> {
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut merged: Vec<SourcedStatement> = Vec::new();
    for statement in statements {
        match positions.get(&statement_key(&statement.text)) {
            Some(&position) => merged[position]
                .source_segment_ids
                .extend(statement.source_segment_ids.iter().copied()),
            None => {
                positions.insert(statement_key(&statement.text), merged.len());
                merged.push(statement.clone());
            }
        }
    }
    for statement in &mut merged {
        statement.source_segment_ids = unique_sorted(statement.source_segment_ids.iter().copied());
    }
    merged
}

fn merge_background(candidates: &[MeetingSummary]) -> Vec<BackgroundStatement> {
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut merged: Vec<BackgroundStatement> = Vec::new();
    for entry in candidates
        .iter()
        .flat_map(|candidate| &candidate.background)
    {
        match positions.get(&statement_key(&entry.text)) {
            Some(&position) => {
                for path in &entry.context_paths {
                    if !merged[position].context_paths.contains(path) {
                        merged[position].context_paths.push(path.clone());
                    }
                }
            }
            None => {
                positions.insert(statement_key(&entry.text), merged.len());
                merged.push(entry.clone());
            }
        }
    }
    merged
}

fn merge_action_items(candidates: &[MeetingSummary]) -> Vec<ActionItem> {
    let mut positions: HashMap<String, usize> = HashMap::new();
    let mut merged: Vec<ActionItem> = Vec::new();
    for item in candidates
        .iter()
        .flat_map(|candidate| &candidate.action_items)
    {
        match positions.get(&action_item_key(item)) {
            Some(&position) => merged[position]
                .source_segment_ids
                .extend(item.source_segment_ids.iter().copied()),
            None => {
                positions.insert(action_item_key(item), merged.len());
                merged.push(item.clone());
            }
        }
    }
    for item in &mut merged {
        item.source_segment_ids = unique_sorted(item.source_segment_ids.iter().copied());
    }
    merged
}

fn action_item_key(item: &ActionItem) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        statement_key(&item.task),
        statement_key(item.owner.as_deref().unwrap_or_default()),
        statement_key(item.due_date.as_deref().unwrap_or_default())
    )
}

fn statement_key(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn unique_sorted(ids: impl Iterator<Item = u32>) -> Vec<u32> {
    let mut unique = ids.collect::<Vec<_>>();
    unique.sort_unstable();
    unique.dedup();
    unique
}
