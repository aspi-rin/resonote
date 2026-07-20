use std::{fs, sync::Arc};

use chrono::Utc;

use super::*;

#[test]
fn durably_enqueues_speech_without_an_installed_model() {
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(ModelManager::new(directory.path().join("models")).unwrap());
    let service = TranscriptionService::new(manager);
    let session = directory.path().join("session");
    fs::create_dir_all(&session).unwrap();
    let segment = SpeechSegment {
        end_sample: 3_200,
        peak_probability: 0.9,
        samples: vec![0.25; 1_600],
        start_sample: 1_600,
    };

    let queued = service
        .enqueue(
            &session,
            "session-1",
            &segment,
            &TranscriptionSettings::default(),
        )
        .unwrap();
    let document = load_document(&session.join(DOCUMENT_NAME)).unwrap();

    assert_eq!(queued.start_ms, 100);
    assert_eq!(queued.end_ms, 200);
    assert_eq!(document.segments.len(), 1);
    assert_eq!(
        document.segments[0].status,
        TranscriptSegmentStatus::Pending
    );
    assert!(session.join(&document.segments[0].audio_file).exists());
}

#[test]
fn recovery_returns_processing_segments_to_pending() {
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(ModelManager::new(directory.path().join("models")).unwrap());
    let service = TranscriptionService::new(manager);
    let session = directory.path().join("session");
    fs::create_dir_all(&session).unwrap();
    let document = TranscriptDocument {
        forced_language: "auto".to_owned(),
        model_id: "qwen3-asr-0.6b-int8".to_owned(),
        schema_version: 1,
        segments: vec![TranscriptSegment {
            attempts: 1,
            audio_file: "speech/speech-000001.wav".to_owned(),
            detected_language: String::new(),
            end_ms: 500,
            error: None,
            id: 1,
            peak_probability: 0.8,
            start_ms: 0,
            status: TranscriptSegmentStatus::Processing,
            text: String::new(),
        }],
        session_id: "session-1".to_owned(),
        status: TranscriptDocumentStatus::Processing,
        threads: 2,
        unload_after_idle_minutes: 10,
        updated_at: Utc::now(),
    };
    save_document(&session.join(DOCUMENT_NAME), &document).unwrap();

    assert_eq!(service.recover_root(directory.path()).unwrap(), 1);
    let recovered = load_document(&session.join(DOCUMENT_NAME)).unwrap();
    assert_eq!(
        recovered.segments[0].status,
        TranscriptSegmentStatus::Pending
    );
    assert_eq!(recovered.status, TranscriptDocumentStatus::Pending);
}

#[test]
fn deleting_a_session_removes_its_pending_queue_count() {
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(ModelManager::new(directory.path().join("models")).unwrap());
    let service = TranscriptionService::new(manager);
    let session = directory.path().join("session");
    fs::create_dir_all(&session).unwrap();
    let segment = SpeechSegment {
        end_sample: 1_600,
        peak_probability: 1.0,
        samples: vec![0.25; 1_600],
        start_sample: 0,
    };
    service
        .enqueue(
            &session,
            "session-1",
            &segment,
            &TranscriptionSettings::default(),
        )
        .unwrap();
    assert_eq!(service.status().pending_segments, 1);

    service.delete_session_directory(&session).unwrap();

    assert!(!session.exists());
    assert_eq!(service.status().pending_segments, 0);
}

#[test]
fn maps_common_language_codes() {
    assert_eq!(qwen_language("auto"), None);
    assert_eq!(qwen_language("zh-CN"), Some("Chinese"));
    assert_eq!(qwen_language("ja"), Some("Japanese"));
}
