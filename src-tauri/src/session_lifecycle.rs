use std::{
    collections::{HashMap, HashSet},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const MARKER_NAME: &str = ".deleting";
const MAX_SCAN_DEPTH: usize = 5;

/// The per-session lifecycle/commit lock transcription, translation, meeting
/// notes and history deletion share, plus the in-memory tombstones a delete
/// leaves behind.
///
/// Lock order: the per-session lock from [`SessionLifecycle::session_lock`] is
/// the outermost one, and callers hold it across the tombstone check and the
/// write it protects. The two registry maps below and every service's own state
/// (queues, known sessions, corrupt sets) are taken either inside that lock or
/// on their own, never with a session lock acquired underneath them.
#[derive(Default)]
pub struct SessionLifecycle {
    locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    tombstones: Mutex<HashSet<PathBuf>>,
}

impl SessionLifecycle {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn session_lock(&self, session_dir: &Path) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(session_dir.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Opens a deletion: the marker survives a crash and the tombstone stops
    /// in-flight workers from committing. An existing marker resumes the
    /// deletion instead of failing it. The tombstone is kept until the process
    /// exits, which outlives every response still in flight for the session.
    pub fn begin_delete(&self, session_dir: &Path) -> Result<(), std::io::Error> {
        let lock = self.session_lock(session_dir);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.tombstones
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_dir.to_path_buf());
        match fs::File::create_new(session_dir.join(MARKER_NAME)) {
            Ok(marker) => marker.sync_all(),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn is_deleting(&self, session_dir: &Path) -> bool {
        self.tombstones
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(session_dir)
            || session_dir.join(MARKER_NAME).exists()
    }

    /// Removes the session directory, marker included, under the lifecycle lock
    /// so no worker can be halfway through a commit.
    pub fn finish_delete(&self, session_dir: &Path) -> Result<(), std::io::Error> {
        let lock = self.session_lock(session_dir);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        self.tombstones
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_dir.to_path_buf());
        if session_dir.exists() {
            fs::remove_dir_all(session_dir)?;
        }
        Ok(())
    }

    /// Finishes the deletions a crash left behind. Runs before any worker queue
    /// scan, so a half deleted session never re-enters a queue.
    pub fn recover_roots(&self, roots: &[PathBuf]) -> Result<usize, std::io::Error> {
        let mut marked = Vec::new();
        for root in roots {
            if root.is_dir() {
                collect_marked_sessions(root, 0, &mut marked)?;
            }
        }
        let mut completed = 0;
        for session_dir in marked {
            match self.finish_delete(&session_dir) {
                Ok(()) => completed += 1,
                Err(error) => {
                    tracing::warn!(
                        path = %session_dir.display(),
                        ?error,
                        "failed to finish an interrupted session deletion"
                    );
                }
            }
        }
        Ok(completed)
    }
}

fn collect_marked_sessions(
    directory: &Path,
    depth: usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    if depth > MAX_SCAN_DEPTH {
        return Ok(());
    }
    // Depth zero is the registered root itself, which is never a session.
    if depth > 0 && directory.join(MARKER_NAME).is_file() {
        output.push(directory.to_path_buf());
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_marked_sessions(&entry.path(), depth + 1, output)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "session_lifecycle_tests.rs"]
mod tests;
