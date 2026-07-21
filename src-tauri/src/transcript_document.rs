use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use super::{DOCUMENT_NAME, TranscriptionError};
use crate::audio::TARGET_SAMPLE_RATE;

const MAX_ATTEMPTS: u32 = 3;

pub(super) fn qwen_language(language: &str) -> Option<&str> {
    match language.trim() {
        "" | "auto" => None,
        "zh" | "zh-CN" | "Chinese" => Some("Chinese"),
        "en" | "en-US" | "English" => Some("English"),
        "yue" | "Cantonese" => Some("Cantonese"),
        "ja" | "Japanese" => Some("Japanese"),
        "ko" | "Korean" => Some("Korean"),
        other => Some(other),
    }
}

pub(super) fn is_retryable(segment: &TranscriptSegment) -> bool {
    matches!(
        segment.status,
        TranscriptSegmentStatus::Pending | TranscriptSegmentStatus::Processing
    ) || (segment.status == TranscriptSegmentStatus::Failed && segment.attempts < MAX_ATTEMPTS)
}

pub(super) fn sample_to_ms(sample: u64) -> u64 {
    sample.saturating_mul(1_000) / u64::from(TARGET_SAMPLE_RATE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptDocumentStatus {
    Pending,
    Processing,
    Complete,
    Partial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptSegmentStatus {
    Pending,
    Processing,
    Complete,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub attempts: u32,
    pub audio_file: String,
    pub detected_language: String,
    pub end_ms: u64,
    pub error: Option<String>,
    pub id: u32,
    pub peak_probability: f32,
    pub start_ms: u64,
    pub status: TranscriptSegmentStatus,
    pub text: String,
}

/// Snapshot of one segment's persisted state, published whenever it changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegmentUpdate {
    pub segment: TranscriptSegment,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptDocument {
    pub forced_language: String,
    pub model_id: String,
    pub schema_version: u32,
    pub segments: Vec<TranscriptSegment>,
    pub session_id: String,
    pub status: TranscriptDocumentStatus,
    pub threads: u16,
    pub unload_after_idle_minutes: u32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptionPhase {
    Idle,
    WaitingForModel,
    LoadingModel,
    Transcribing,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionStatus {
    pub current_session_id: Option<String>,
    pub error: Option<String>,
    pub model_id: Option<String>,
    pub model_loaded: bool,
    pub pending_segments: usize,
    pub phase: TranscriptionPhase,
}

impl Default for TranscriptionStatus {
    fn default() -> Self {
        Self {
            current_session_id: None,
            error: None,
            model_id: None,
            model_loaded: false,
            pending_segments: 0,
            phase: TranscriptionPhase::Idle,
        }
    }
}

pub(super) fn load_document(path: &Path) -> Result<TranscriptDocument, TranscriptionError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub(super) fn save_document(
    path: &Path,
    document: &TranscriptDocument,
) -> Result<(), TranscriptionError> {
    let parent = path
        .parent()
        .ok_or(TranscriptionError::InvalidDocumentPath)?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, document)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

pub(super) fn collect_documents(
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
