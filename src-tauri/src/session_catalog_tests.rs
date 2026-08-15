use super::*;
use crate::{
    settings::{AudioFormat, AudioSourceMode},
    storage::ArchiveStatus,
};

fn write_session(root: &Path, relative: &str, session_id: &str) -> PathBuf {
    let directory = root.join(relative).join(session_id);
    fs::create_dir_all(&directory).unwrap();
    let manifest = SessionManifest {
        audio_format: AudioFormat::Flac,
        audio_source: AudioSourceMode::Mixed,
        completed_at: None,
        last_error: None,
        sample_rate: 16_000,
        schema_version: 1,
        segment_minutes: 60,
        segments: Vec::new(),
        session_id: session_id.to_owned(),
        started_at: Utc::now(),
        status: ArchiveStatus::Completed,
    };
    fs::write(
        directory.join(MANIFEST_NAME),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::canonicalize(directory).unwrap()
}

fn catalog(directory: &Path) -> SessionCatalog {
    SessionCatalog::open(directory.join(DOCUMENT_NAME)).unwrap()
}

#[test]
fn resolves_a_session_under_a_registered_custom_root() {
    let directory = tempfile::tempdir().unwrap();
    let custom_root = directory.path().join("custom output");
    let expected = write_session(&custom_root, "2026/08/15", "session-1");
    let catalog = catalog(directory.path());

    assert!(catalog.register_root(&custom_root).unwrap());

    assert_eq!(catalog.resolve("session-1").unwrap(), expected);
}

#[test]
fn refuses_a_session_that_is_only_reachable_through_a_symlink() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("recordings");
    fs::create_dir_all(&root).unwrap();
    let outside = directory.path().join("outside");
    write_session(&outside, "2026/08/15", "session-1");
    let catalog = catalog(directory.path());
    catalog.register_root(&root).unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, root.join("linked")).unwrap();
    #[cfg(windows)]
    if std::os::windows::fs::symlink_dir(&outside, root.join("linked")).is_err() {
        return;
    }

    assert!(matches!(
        catalog.resolve("session-1"),
        Err(CatalogError::SessionNotFound(_))
    ));
}

#[test]
fn rejects_a_directory_that_escapes_the_registered_root() {
    let directory = tempfile::tempdir().unwrap();
    let inside = write_session(
        &directory.path().join("recordings"),
        "2026/08/15",
        "session-1",
    );
    let outside = write_session(directory.path(), "outside", "session-1");
    let root = fs::canonicalize(directory.path().join("recordings")).unwrap();

    assert_eq!(
        claimed_directory(&inside.join(MANIFEST_NAME), "session-1", &root),
        Some(inside)
    );
    assert_eq!(
        claimed_directory(&outside.join(MANIFEST_NAME), "session-1", &root),
        None
    );
}

#[test]
fn never_accepts_the_registered_root_itself_as_a_session() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("recordings");
    fs::create_dir_all(&root).unwrap();
    let manifest = SessionManifest {
        audio_format: AudioFormat::Flac,
        audio_source: AudioSourceMode::Mixed,
        completed_at: None,
        last_error: None,
        sample_rate: 16_000,
        schema_version: 1,
        segment_minutes: 60,
        segments: Vec::new(),
        session_id: "session-1".to_owned(),
        started_at: Utc::now(),
        status: ArchiveStatus::Completed,
    };
    fs::write(
        root.join(MANIFEST_NAME),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let catalog = catalog(directory.path());
    catalog.register_root(&root).unwrap();

    assert!(matches!(
        catalog.resolve("session-1"),
        Err(CatalogError::SessionNotFound(_))
    ));
}

#[test]
fn reports_a_conflict_when_two_roots_claim_the_same_session_id() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    write_session(&first, "2026/08/15", "session-1");
    write_session(&second, "2026/08/15", "session-1");
    let catalog = catalog(directory.path());
    catalog.register_root(&first).unwrap();
    catalog.register_root(&second).unwrap();

    assert!(matches!(
        catalog.resolve("session-1"),
        Err(CatalogError::SessionIdConflict(_))
    ));
}

#[test]
fn resolves_a_session_beyond_the_history_display_limit() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("recordings");
    let expected = write_session(&root, "2026/08/15", "session-0000");
    for index in 1..520 {
        write_session(&root, "2026/08/15", &format!("session-{index:04}"));
    }
    let catalog = catalog(directory.path());
    catalog.register_root(&root).unwrap();

    assert_eq!(catalog.resolve("session-0000").unwrap(), expected);
    assert_eq!(
        catalog.resolve("session-0519").unwrap(),
        root.join("2026/08/15/session-0519").canonicalize().unwrap()
    );
}

#[test]
fn registering_the_same_root_twice_does_not_rewrite_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("recordings");
    fs::create_dir_all(&root).unwrap();
    let catalog = catalog(directory.path());
    let path = directory.path().join(DOCUMENT_NAME);

    assert!(catalog.register_root(&root).unwrap());
    let first = fs::read(&path).unwrap();

    assert!(!catalog.register_root(&root).unwrap());
    assert!(!catalog.register_root(&root.join(".")).unwrap());
    assert_eq!(fs::read(&path).unwrap(), first);
    assert_eq!(catalog.roots().len(), 1);
}

#[test]
fn keeps_registered_roots_across_reopen_and_rejects_a_blank_session_id() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("recordings");
    write_session(&root, "2026/08/15", "session-1");
    catalog(directory.path()).register_root(&root).unwrap();

    let reopened = catalog(directory.path());

    assert_eq!(reopened.roots().len(), 1);
    assert!(reopened.resolve("session-1").is_ok());
    assert!(matches!(
        reopened.resolve("   "),
        Err(CatalogError::InvalidSessionId)
    ));
}

#[test]
fn starts_empty_when_the_stored_catalog_is_corrupt() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join(DOCUMENT_NAME), b"not json").unwrap();

    assert!(catalog(directory.path()).roots().is_empty());
}
