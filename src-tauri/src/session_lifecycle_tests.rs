use std::{thread, time::Duration};

use super::*;

fn session(root: &Path, name: &str) -> PathBuf {
    let directory = root.join(name);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("session.json"), b"{}").unwrap();
    directory
}

#[test]
fn a_marked_session_stays_deleting_until_the_directory_is_gone() {
    let directory = tempfile::tempdir().unwrap();
    let session = session(directory.path(), "session");
    let lifecycle = SessionLifecycle::new();

    assert!(!lifecycle.is_deleting(&session));
    lifecycle.begin_delete(&session).unwrap();

    assert!(lifecycle.is_deleting(&session));
    assert!(session.join(MARKER_NAME).is_file());
    lifecycle.finish_delete(&session).unwrap();
    assert!(!session.exists());
    assert!(lifecycle.is_deleting(&session));
}

#[test]
fn a_second_begin_delete_resumes_the_first_one() {
    let directory = tempfile::tempdir().unwrap();
    let session = session(directory.path(), "session");
    let lifecycle = SessionLifecycle::new();

    lifecycle.begin_delete(&session).unwrap();
    lifecycle.begin_delete(&session).unwrap();

    assert!(session.join(MARKER_NAME).is_file());
}

/// A fresh process sees the marker but no tombstone, exactly like a restart.
#[test]
fn recovery_finishes_a_marked_session_and_leaves_the_others_alone() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("recordings");
    let marked = session(&root.join("2026/08/15"), "session-1");
    let kept = session(&root.join("2026/08/15"), "session-2");
    SessionLifecycle::new().begin_delete(&marked).unwrap();

    let restarted = SessionLifecycle::new();
    let completed = restarted
        .recover_roots(std::slice::from_ref(&root))
        .unwrap();

    assert_eq!(completed, 1);
    assert!(!marked.exists());
    assert!(kept.exists());
    assert_eq!(restarted.recover_roots(&[root]).unwrap(), 0);
}

#[test]
fn recovery_ignores_a_root_that_does_not_exist() {
    let directory = tempfile::tempdir().unwrap();

    let completed = SessionLifecycle::new()
        .recover_roots(&[directory.path().join("missing")])
        .unwrap();

    assert_eq!(completed, 0);
}

#[test]
fn a_delete_waits_for_an_in_progress_commit() {
    let directory = tempfile::tempdir().unwrap();
    let session = session(directory.path(), "session");
    let lifecycle = SessionLifecycle::new();
    let commit = lifecycle.session_lock(&session);
    let guard = commit.lock().unwrap();
    let deleter = {
        let lifecycle = lifecycle.clone();
        let session = session.clone();
        thread::spawn(move || {
            lifecycle.begin_delete(&session).unwrap();
            lifecycle.finish_delete(&session).unwrap();
        })
    };

    thread::sleep(Duration::from_millis(50));
    fs::write(session.join("analysis.json"), b"{}").unwrap();
    assert!(session.exists(), "the delete cut into a running commit");
    drop(guard);

    deleter.join().unwrap();
    assert!(!session.exists());
}
