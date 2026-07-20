use std::path::Path;

use super::{
    DOCUMENT_NAME, TranscriptionError, TranscriptionPhase, TranscriptionService, is_retryable,
    load_document,
};

impl TranscriptionService {
    pub fn delete_session_directory(&self, session_dir: &Path) -> Result<(), TranscriptionError> {
        self.inner
            .known_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_dir);
        let lock = self.inner.document_lock(session_dir);
        let guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let pending = load_document(&session_dir.join(DOCUMENT_NAME))
            .map(|document| {
                document
                    .segments
                    .iter()
                    .filter(|item| is_retryable(item))
                    .count()
            })
            .unwrap_or_default();
        if session_dir.exists() {
            std::fs::remove_dir_all(session_dir)?;
        }
        drop(guard);
        self.inner
            .document_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_dir);
        self.inner.publish(|status| {
            status.pending_segments = status.pending_segments.saturating_sub(pending);
            if status.pending_segments == 0 {
                status.current_session_id = None;
                status.phase = TranscriptionPhase::Idle;
            }
        });
        Ok(())
    }
}
