use std::path::Path;

use super::{DOCUMENT_NAME, TranscriptionPhase, TranscriptionService, is_retryable, load_document};

impl TranscriptionService {
    /// Drops one session from the worker queue and from the pending count. The
    /// directory itself is removed by the shared lifecycle once every worker has
    /// forgotten the session, so a live recording is never touched here.
    pub fn forget_session(&self, session_dir: &Path) {
        self.inner
            .known_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_dir);
        let lock = self.inner.document_lock(session_dir);
        let pending = {
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            load_document(&session_dir.join(DOCUMENT_NAME))
                .map(|document| {
                    document
                        .segments
                        .iter()
                        .filter(|item| is_retryable(item))
                        .count()
                })
                .unwrap_or_default()
        };
        self.inner.publish(|status| {
            status.pending_segments = status.pending_segments.saturating_sub(pending);
            if status.pending_segments == 0 {
                status.current_session_id = None;
                status.phase = TranscriptionPhase::Idle;
            }
        });
    }
}
