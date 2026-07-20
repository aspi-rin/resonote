use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;

use crate::{
    settings::{AudioFormat, AudioSourceMode},
    storage::{ArchiveStatus, SessionManifest},
    transcription::{
        TranscriptDocument, TranscriptDocumentStatus, TranscriptSegmentStatus, TranscriptionError,
        TranscriptionService,
    },
};

const MANIFEST_NAME: &str = "session.json";
const TRANSCRIPT_NAME: &str = "transcript.json";
const MAX_SCAN_DEPTH: usize = 5;
const MAX_HISTORY_LIMIT: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub audio_format: AudioFormat,
    pub audio_source: AudioSourceMode,
    pub completed_at: Option<DateTime<Utc>>,
    pub directory: PathBuf,
    pub duration_ms: u64,
    pub segment_count: usize,
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub status: ArchiveStatus,
    pub transcript_preview: String,
    pub transcript_segment_count: usize,
    pub transcript_status: Option<TranscriptDocumentStatus>,
}

pub struct HistoryService {
    root: PathBuf,
    transcription: Option<Arc<TranscriptionService>>,
}

impl HistoryService {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            transcription: None,
        }
    }

    pub fn with_transcription(root: PathBuf, transcription: Arc<TranscriptionService>) -> Self {
        Self {
            root,
            transcription: Some(transcription),
        }
    }

    pub fn list(&self, limit: usize) -> Result<Vec<HistoryEntry>, HistoryError> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut manifests = Vec::new();
        collect_named_files(&self.root, MANIFEST_NAME, 0, MAX_SCAN_DEPTH, &mut manifests)?;
        let mut entries = manifests
            .into_iter()
            .filter_map(|path| match history_entry(&path) {
                Ok(entry) => Some(entry),
                Err(error) => {
                    tracing::warn!(path = %path.display(), ?error, "skipping invalid history entry");
                    None
                }
            })
            .collect::<Vec<_>>();
        entries.sort_unstable_by_key(|entry| std::cmp::Reverse(entry.started_at));
        entries.truncate(limit.clamp(1, MAX_HISTORY_LIMIT));
        Ok(entries)
    }

    pub fn open_session_directory(&self, session_id: &str) -> Result<(), HistoryError> {
        if session_id.trim().is_empty() {
            return Err(HistoryError::InvalidSessionId);
        }
        let entry = self
            .list(MAX_HISTORY_LIMIT)?
            .into_iter()
            .find(|entry| entry.session_id == session_id)
            .ok_or_else(|| HistoryError::SessionNotFound(session_id.to_owned()))?;
        open_directory(&entry.directory)
    }

    pub fn delete_session(&self, session_id: &str) -> Result<(), HistoryError> {
        if session_id.trim().is_empty() {
            return Err(HistoryError::InvalidSessionId);
        }
        let entry = self
            .list(MAX_HISTORY_LIMIT)?
            .into_iter()
            .find(|entry| entry.session_id == session_id)
            .ok_or_else(|| HistoryError::SessionNotFound(session_id.to_owned()))?;
        if entry.status == ArchiveStatus::Recording {
            return Err(HistoryError::ActiveSession(session_id.to_owned()));
        }
        let root = fs::canonicalize(&self.root)?;
        let directory = fs::canonicalize(&entry.directory)?;
        if directory == root || !directory.starts_with(&root) {
            return Err(HistoryError::UnsafeSessionPath(directory));
        }
        let manifest: SessionManifest =
            serde_json::from_slice(&fs::read(directory.join(MANIFEST_NAME))?)?;
        if manifest.session_id != session_id || manifest.status == ArchiveStatus::Recording {
            return Err(HistoryError::ActiveSession(session_id.to_owned()));
        }
        if let Some(transcription) = &self.transcription {
            transcription.delete_session_directory(&directory)?;
        } else {
            fs::remove_dir_all(directory)?;
        }
        Ok(())
    }
}

fn history_entry(manifest_path: &Path) -> Result<HistoryEntry, HistoryError> {
    let manifest: SessionManifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    let directory = manifest_path
        .parent()
        .ok_or(HistoryError::InvalidManifestPath)?
        .to_path_buf();
    let transcript_path = directory.join(TRANSCRIPT_NAME);
    let transcript = if transcript_path.exists() {
        Some(serde_json::from_slice::<TranscriptDocument>(&fs::read(
            transcript_path,
        )?)?)
    } else {
        None
    };
    let duration_ms = manifest
        .segments
        .iter()
        .map(|segment| segment.duration_ms)
        .sum();
    let transcript_preview = transcript
        .as_ref()
        .map(transcript_preview)
        .unwrap_or_default();
    Ok(HistoryEntry {
        audio_format: manifest.audio_format,
        audio_source: manifest.audio_source,
        completed_at: manifest.completed_at,
        directory,
        duration_ms,
        segment_count: manifest.segments.len(),
        session_id: manifest.session_id,
        started_at: manifest.started_at,
        status: manifest.status,
        transcript_preview,
        transcript_segment_count: transcript.as_ref().map_or(0, |item| item.segments.len()),
        transcript_status: transcript.map(|item| item.status),
    })
}

fn transcript_preview(document: &TranscriptDocument) -> String {
    let joined = document
        .segments
        .iter()
        .filter(|segment| segment.status == TranscriptSegmentStatus::Complete)
        .map(|segment| segment.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut preview = joined.chars().take(160).collect::<String>();
    if joined.chars().count() > 160 {
        preview.push('…');
    }
    preview
}

fn collect_named_files(
    directory: &Path,
    name: &str,
    depth: usize,
    max_depth: usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    if depth > max_depth {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_named_files(&entry.path(), name, depth + 1, max_depth, output)?;
        } else if entry.file_name() == name {
            output.push(entry.path());
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_directory(path: &Path) -> Result<(), HistoryError> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    Command::new("explorer.exe")
        .arg(path)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_directory(path: &Path) -> Result<(), HistoryError> {
    Command::new("open").arg(path).spawn()?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("recording session '{0}' is still active")]
    ActiveSession(String),
    #[error("recording manifest path is invalid")]
    InvalidManifestPath,
    #[error("session id cannot be empty")]
    InvalidSessionId,
    #[error("failed to access recording history: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse recording history: {0}")]
    Json(#[from] serde_json::Error),
    #[error("recording session '{0}' was not found")]
    SessionNotFound(String),
    #[error(transparent)]
    Transcription(#[from] TranscriptionError),
    #[error("recording session path is outside the managed history root: {0}")]
    UnsafeSessionPath(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{settings::AudioSettings, storage::RecordingArchive};

    #[test]
    fn lists_newest_sessions_and_summarizes_duration() {
        let directory = tempfile::tempdir().unwrap();
        let settings = AudioSettings::default();
        let mut archive = RecordingArchive::create(directory.path(), &settings).unwrap();
        archive.append(&vec![0.0; 16_000]).unwrap();
        let manifest = archive.complete().unwrap();
        let service = HistoryService::new(directory.path().to_path_buf());

        let entries = service.list(20).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, manifest.session_id);
        assert_eq!(entries[0].duration_ms, 1_000);
        assert_eq!(entries[0].segment_count, 1);
    }

    #[test]
    fn skips_symlink_free_invalid_manifests_instead_of_failing_the_list() {
        let directory = tempfile::tempdir().unwrap();
        let session = directory.path().join("bad-session");
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join(MANIFEST_NAME), b"not json").unwrap();

        let entries = HistoryService::new(directory.path().to_path_buf())
            .list(20)
            .unwrap();

        assert!(entries.is_empty());
    }

    #[test]
    fn deletes_only_the_requested_completed_session() {
        let directory = tempfile::tempdir().unwrap();
        let first = RecordingArchive::create(directory.path(), &AudioSettings::default()).unwrap();
        let first_path = first.session_dir().to_path_buf();
        let first_id = first.complete().unwrap().session_id;
        let second = RecordingArchive::create(directory.path(), &AudioSettings::default()).unwrap();
        let second_path = second.session_dir().to_path_buf();
        second.complete().unwrap();
        let service = HistoryService::new(directory.path().to_path_buf());

        service.delete_session(&first_id).unwrap();

        assert!(!first_path.exists());
        assert!(second_path.exists());
    }

    #[test]
    fn refuses_to_delete_an_active_session() {
        let directory = tempfile::tempdir().unwrap();
        let archive =
            RecordingArchive::create(directory.path(), &AudioSettings::default()).unwrap();
        let session_id = archive.manifest().session_id.clone();
        let service = HistoryService::new(directory.path().to_path_buf());

        assert!(matches!(
            service.delete_session(&session_id),
            Err(HistoryError::ActiveSession(_))
        ));
        assert!(archive.session_dir().exists());
    }
}
