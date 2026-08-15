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
    meeting_notes::MeetingNotesService,
    meeting_notes_document::MeetingNotesError,
    session_catalog::{CatalogError, SessionCatalog},
    session_lifecycle::SessionLifecycle,
    settings::{AudioFormat, AudioSourceMode},
    storage::{ArchiveStatus, SessionManifest},
    transcription::{
        TranscriptDocument, TranscriptDocumentStatus, TranscriptSegmentStatus, TranscriptionService,
    },
    translation::{
        TranslationDocument, TranslationDocumentStatus, TranslationSegmentStatus,
        TranslationService,
    },
};

const MANIFEST_NAME: &str = "session.json";
const TRANSCRIPT_NAME: &str = "transcript.json";
const TRANSLATION_NAME: &str = "translation.json";
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
    pub translation_preview: String,
    pub translation_status: Option<TranslationDocumentStatus>,
    pub translation_target_language: Option<String>,
}

/// The workers a delete notifies before the session directory can go.
pub struct HistoryWorkers {
    pub meeting_notes: Arc<MeetingNotesService>,
    pub transcription: Arc<TranscriptionService>,
    pub translation: Arc<TranslationService>,
}

pub struct HistoryService {
    catalog: Arc<SessionCatalog>,
    lifecycle: Arc<SessionLifecycle>,
    root: PathBuf,
    workers: Option<HistoryWorkers>,
}

impl HistoryService {
    pub fn new(
        root: PathBuf,
        catalog: Arc<SessionCatalog>,
        lifecycle: Arc<SessionLifecycle>,
    ) -> Self {
        Self {
            catalog,
            lifecycle,
            root,
            workers: None,
        }
    }

    pub fn with_workers(
        root: PathBuf,
        catalog: Arc<SessionCatalog>,
        lifecycle: Arc<SessionLifecycle>,
        workers: HistoryWorkers,
    ) -> Self {
        Self {
            catalog,
            lifecycle,
            root,
            workers: Some(workers),
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
        open_directory(&self.resolve(session_id)?)
    }

    /// Deletes in the one order that survives a crash: mark, forget everywhere,
    /// then remove the directory under the lifecycle lock. A failure after the
    /// marker exists leaves it in place for the startup sweep to finish.
    pub fn delete_session(&self, session_id: &str) -> Result<(), HistoryError> {
        let directory = self.resolve(session_id)?;
        let manifest: SessionManifest =
            serde_json::from_slice(&fs::read(directory.join(MANIFEST_NAME))?)?;
        if manifest.session_id != session_id || manifest.status == ArchiveStatus::Recording {
            return Err(HistoryError::ActiveSession(session_id.to_owned()));
        }
        self.ensure_inside_registered_root(&directory)?;
        self.lifecycle.begin_delete(&directory)?;
        if let Some(workers) = &self.workers {
            workers.transcription.forget_session(&directory);
            workers.translation.forget(&directory);
            workers.meeting_notes.forget(session_id)?;
        }
        self.lifecycle.finish_delete(&directory)?;
        Ok(())
    }

    pub fn session_transcript(
        &self,
        session_id: &str,
    ) -> Result<Option<TranscriptDocument>, HistoryError> {
        read_json(&self.resolve(session_id)?.join(TRANSCRIPT_NAME))
    }

    pub fn session_translation(
        &self,
        session_id: &str,
    ) -> Result<Option<TranslationDocument>, HistoryError> {
        read_json(&self.resolve(session_id)?.join(TRANSLATION_NAME))
    }

    /// Single session lookups go through the catalog, so a registered custom
    /// output root and a history longer than the display limit both resolve.
    fn resolve(&self, session_id: &str) -> Result<PathBuf, HistoryError> {
        if session_id.trim().is_empty() {
            return Err(HistoryError::InvalidSessionId);
        }
        let directory = self.catalog.resolve(session_id)?;
        if self.lifecycle.is_deleting(&directory) {
            return Err(HistoryError::SessionDeleted(session_id.to_owned()));
        }
        Ok(directory)
    }

    fn ensure_inside_registered_root(&self, directory: &Path) -> Result<(), HistoryError> {
        let inside = self
            .catalog
            .roots()
            .iter()
            .any(|root| directory != root && directory.starts_with(root));
        if inside {
            Ok(())
        } else {
            Err(HistoryError::UnsafeSessionPath(directory.to_path_buf()))
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, HistoryError> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
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
    let translation_path = directory.join(TRANSLATION_NAME);
    let translation = if translation_path.exists() {
        Some(serde_json::from_slice::<TranslationDocument>(&fs::read(
            translation_path,
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
        translation_preview: translation
            .as_ref()
            .map(translation_preview)
            .unwrap_or_default(),
        translation_status: translation.as_ref().map(|item| item.status),
        translation_target_language: translation.map(|item| item.target_language),
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

fn translation_preview(document: &TranslationDocument) -> String {
    let joined = document
        .segments
        .iter()
        .filter(|segment| segment.status == TranslationSegmentStatus::Complete)
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
    #[error(transparent)]
    Catalog(CatalogError),
    #[error("recording manifest path is invalid")]
    InvalidManifestPath,
    #[error("session id cannot be empty")]
    InvalidSessionId,
    #[error("failed to access recording history: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse recording history: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    MeetingNotes(#[from] MeetingNotesError),
    #[error("recording session '{0}' was deleted")]
    SessionDeleted(String),
    #[error("more than one directory claims recording session '{0}'")]
    SessionIdConflict(String),
    #[error("recording session '{0}' was not found")]
    SessionNotFound(String),
    #[error("recording session path is outside the managed history root: {0}")]
    UnsafeSessionPath(PathBuf),
}

impl From<CatalogError> for HistoryError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::InvalidSessionId => Self::InvalidSessionId,
            CatalogError::SessionIdConflict(session_id) => Self::SessionIdConflict(session_id),
            CatalogError::SessionNotFound(session_id) => Self::SessionNotFound(session_id),
            other => Self::Catalog(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        meeting_notes_document::{GlobalContextStore, OutputLanguage},
        models::ModelManager,
        openai_compatible::test_support::ScriptedChatClient,
        settings::{AudioSettings, ProviderCredentials, TranslationSnapshot},
        storage::RecordingArchive,
        transcription::TranscriptDocumentStatus,
    };

    struct Fixture {
        catalog: Arc<SessionCatalog>,
        directory: tempfile::TempDir,
        lifecycle: Arc<SessionLifecycle>,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let catalog =
            Arc::new(SessionCatalog::open(directory.path().join("session-catalog.json")).unwrap());
        Fixture {
            catalog,
            directory,
            lifecycle: SessionLifecycle::new(),
        }
    }

    impl Fixture {
        fn root(&self) -> PathBuf {
            self.directory.path().to_path_buf()
        }

        fn register(&self, root: &Path) {
            fs::create_dir_all(root).unwrap();
            self.catalog.register_root(root).unwrap();
        }

        fn service(&self) -> HistoryService {
            self.register(&self.root());
            HistoryService::new(self.root(), self.catalog.clone(), self.lifecycle.clone())
        }

        fn service_with_workers(&self) -> HistoryService {
            self.register(&self.root());
            let manager =
                Arc::new(ModelManager::new(self.directory.path().join("models")).unwrap());
            let meeting_notes = Arc::new(
                MeetingNotesService::with_chat_client(
                    self.catalog.clone(),
                    Arc::new(
                        GlobalContextStore::open(self.directory.path().join("global.json"))
                            .unwrap(),
                    ),
                    self.lifecycle.clone(),
                    ProviderCredentials::default(),
                    Arc::new(|_| {}),
                    Arc::new(ScriptedChatClient::new(Vec::new())),
                    Arc::new(|_| {}),
                )
                .unwrap(),
            );
            HistoryService::with_workers(
                self.root(),
                self.catalog.clone(),
                self.lifecycle.clone(),
                HistoryWorkers {
                    meeting_notes,
                    transcription: Arc::new(TranscriptionService::with_completion_observer(
                        manager,
                        Arc::new(|_| {}),
                        Arc::new(|_| {}),
                        Arc::new(|_, _, _, _| {}),
                        self.lifecycle.clone(),
                    )),
                    translation: Arc::new(
                        TranslationService::with_chat_client(
                            Arc::new(|_| {}),
                            ProviderCredentials::default(),
                            Arc::new(ScriptedChatClient::new(Vec::new())),
                            self.lifecycle.clone(),
                        )
                        .unwrap(),
                    ),
                },
            )
        }
    }

    fn write_session(root: &Path, session_id: &str, started_at: DateTime<Utc>) -> PathBuf {
        let directory = root.join(session_id);
        fs::create_dir_all(&directory).unwrap();
        let manifest = SessionManifest {
            audio_format: AudioFormat::Flac,
            audio_source: AudioSourceMode::Microphone,
            completed_at: Some(started_at),
            last_error: None,
            sample_rate: 16_000,
            schema_version: 1,
            segment_minutes: 30,
            segments: Vec::new(),
            session_id: session_id.to_owned(),
            started_at,
            status: ArchiveStatus::Completed,
        };
        fs::write(
            directory.join(MANIFEST_NAME),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        directory
    }

    fn write_transcript(session_dir: &Path, session_id: &str) {
        let document = TranscriptDocument {
            forced_language: "auto".to_owned(),
            model_id: "qwen3-asr-0.6b-int8".to_owned(),
            schema_version: 1,
            segments: Vec::new(),
            session_id: session_id.to_owned(),
            status: TranscriptDocumentStatus::Complete,
            threads: 2,
            translation: TranslationSnapshot::default(),
            unload_after_idle_minutes: 10,
            updated_at: Utc::now(),
        };
        fs::write(
            session_dir.join(TRANSCRIPT_NAME),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn lists_newest_sessions_and_summarizes_duration() {
        let fixture = fixture();
        let mut archive =
            RecordingArchive::create(&fixture.root(), &AudioSettings::default()).unwrap();
        archive.append(&vec![0.0; 16_000]).unwrap();
        let manifest = archive.complete().unwrap();

        let entries = fixture.service().list(20).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].session_id, manifest.session_id);
        assert_eq!(entries[0].duration_ms, 1_000);
        assert_eq!(entries[0].segment_count, 1);
    }

    #[test]
    fn skips_symlink_free_invalid_manifests_instead_of_failing_the_list() {
        let fixture = fixture();
        let session = fixture.root().join("bad-session");
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join(MANIFEST_NAME), b"not json").unwrap();

        assert!(fixture.service().list(20).unwrap().is_empty());
    }

    #[test]
    fn deletes_only_the_requested_completed_session() {
        let fixture = fixture();
        let first = RecordingArchive::create(&fixture.root(), &AudioSettings::default()).unwrap();
        let first_path = first.session_dir().to_path_buf();
        let first_id = first.complete().unwrap().session_id;
        let second = RecordingArchive::create(&fixture.root(), &AudioSettings::default()).unwrap();
        let second_path = second.session_dir().to_path_buf();
        second.complete().unwrap();
        let service = fixture.service_with_workers();

        service.delete_session(&first_id).unwrap();

        assert!(!first_path.exists());
        assert!(second_path.exists());
    }

    #[test]
    fn returns_the_transcript_document_for_a_session() {
        let fixture = fixture();
        let mut archive =
            RecordingArchive::create(&fixture.root(), &AudioSettings::default()).unwrap();
        archive.append(&vec![0.0; 16_000]).unwrap();
        let session_dir = archive.session_dir().to_path_buf();
        let manifest = archive.complete().unwrap();
        write_transcript(&session_dir, &manifest.session_id);

        let fetched = fixture
            .service()
            .session_transcript(&manifest.session_id)
            .unwrap()
            .expect("transcript document exists");

        assert_eq!(fetched.session_id, manifest.session_id);
        assert_eq!(fetched.status, TranscriptDocumentStatus::Complete);
    }

    #[test]
    fn reports_a_missing_transcript_as_none() {
        let fixture = fixture();
        let archive = RecordingArchive::create(&fixture.root(), &AudioSettings::default()).unwrap();
        let manifest = archive.complete().unwrap();

        assert_eq!(
            fixture
                .service()
                .session_transcript(&manifest.session_id)
                .unwrap(),
            None
        );
    }

    #[test]
    fn refuses_to_delete_an_active_session() {
        let fixture = fixture();
        let archive = RecordingArchive::create(&fixture.root(), &AudioSettings::default()).unwrap();
        let session_id = archive.manifest().session_id.clone();
        let service = fixture.service();

        assert!(matches!(
            service.delete_session(&session_id),
            Err(HistoryError::ActiveSession(_))
        ));
        assert!(archive.session_dir().exists());
        assert!(!archive.session_dir().join(".deleting").exists());
    }

    #[test]
    fn a_delete_marks_forgets_and_then_removes_the_session() {
        let fixture = fixture();
        let session = write_session(&fixture.root(), "session-1", Utc::now());
        let canonical = fs::canonicalize(&session).unwrap();
        let service = fixture.service_with_workers();

        service.delete_session("session-1").unwrap();

        assert!(!session.exists());
        assert!(fixture.lifecycle.is_deleting(&canonical));
    }

    #[test]
    fn a_session_being_deleted_is_no_longer_readable() {
        let fixture = fixture();
        let session = write_session(&fixture.root(), "session-1", Utc::now());
        write_transcript(&session, "session-1");
        let service = fixture.service();
        fixture
            .lifecycle
            .begin_delete(&fs::canonicalize(&session).unwrap())
            .unwrap();

        assert!(matches!(
            service.session_transcript("session-1"),
            Err(HistoryError::SessionDeleted(_))
        ));
        assert!(matches!(
            service.session_translation("session-1"),
            Err(HistoryError::SessionDeleted(_))
        ));
        assert!(matches!(
            service.delete_session("session-1"),
            Err(HistoryError::SessionDeleted(_))
        ));
    }

    #[test]
    fn a_removed_session_is_reported_as_missing() {
        let fixture = fixture();
        write_session(&fixture.root(), "session-1", Utc::now());
        let service = fixture.service_with_workers();

        service.delete_session("session-1").unwrap();

        assert!(matches!(
            service.session_transcript("session-1"),
            Err(HistoryError::SessionNotFound(_))
        ));
    }

    /// AC-40: a custom output root stays reachable by session id even when the
    /// display limit hides the session from the listing.
    #[test]
    fn resolves_a_custom_root_session_beyond_the_history_display_limit() {
        let fixture = fixture();
        let custom_root = fixture.directory.path().join("custom output");
        fixture.register(&custom_root);
        let hidden = write_session(&custom_root, "hidden-session", DateTime::UNIX_EPOCH);
        write_transcript(&hidden, "hidden-session");
        for index in 0..MAX_HISTORY_LIMIT + 1 {
            write_session(&fixture.root(), &format!("session-{index:04}"), Utc::now());
        }
        let service = fixture.service_with_workers();
        let analysis = Arc::new(
            MeetingNotesService::with_chat_client(
                fixture.catalog.clone(),
                Arc::new(
                    GlobalContextStore::open(fixture.directory.path().join("global-2.json"))
                        .unwrap(),
                ),
                fixture.lifecycle.clone(),
                ProviderCredentials::default(),
                Arc::new(|_| {}),
                Arc::new(ScriptedChatClient::new(Vec::new())),
                Arc::new(|_| {}),
            )
            .unwrap(),
        );

        let listed = service.list(MAX_HISTORY_LIMIT).unwrap();
        assert_eq!(listed.len(), MAX_HISTORY_LIMIT);
        assert!(
            listed
                .iter()
                .all(|entry| entry.session_id != "hidden-session")
        );
        assert!(
            service
                .session_transcript("hidden-session")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            analysis
                .load("hidden-session", OutputLanguage::EnUs)
                .unwrap()
                .session_id,
            "hidden-session"
        );

        service.delete_session("hidden-session").unwrap();
        assert!(!hidden.exists());
    }

    /// AC-44: the marker a crashed delete left behind is finished by the startup
    /// sweep, and the session is gone from history afterwards.
    #[test]
    fn a_marker_left_by_a_crashed_delete_is_finished_on_the_next_start() {
        let fixture = fixture();
        let session = write_session(&fixture.root(), "session-1", Utc::now());
        write_transcript(&session, "session-1");
        SessionLifecycle::new()
            .begin_delete(&fs::canonicalize(&session).unwrap())
            .unwrap();

        let restarted = SessionLifecycle::new();
        assert_eq!(restarted.recover_roots(&[fixture.root()]).unwrap(), 1);

        let service = HistoryService::new(fixture.root(), fixture.catalog.clone(), restarted);
        assert!(!session.exists());
        assert!(service.list(20).unwrap().is_empty());
        assert!(matches!(
            service.session_transcript("session-1"),
            Err(HistoryError::SessionNotFound(_))
        ));
    }
}
