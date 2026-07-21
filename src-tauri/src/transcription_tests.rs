use std::{
    fs,
    sync::{Arc, Mutex},
};

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
            &TranslationSettings::default(),
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
fn updates_languages_for_an_active_session() {
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
    service
        .enqueue(
            &session,
            "session-1",
            &segment,
            &TranscriptionSettings::default(),
            &TranslationSettings::default(),
        )
        .unwrap();
    let transcription = TranscriptionSettings {
        language: "Japanese".to_owned(),
        ..TranscriptionSettings::default()
    };
    let translation = TranslationSettings {
        enabled: false,
        target_language: "English".to_owned(),
        ..TranslationSettings::default()
    };

    service
        .update_session_languages(&session, "session-1", &transcription, &translation)
        .unwrap();

    let document = load_document(&session.join(DOCUMENT_NAME)).unwrap();
    assert_eq!(document.forced_language, "Japanese");
    assert_eq!(document.translation, translation);
}

#[test]
fn notifies_segment_observer_when_speech_is_enqueued() {
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(ModelManager::new(directory.path().join("models")).unwrap());
    let updates: Arc<Mutex<Vec<TranscriptSegmentUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = updates.clone();
    let service = TranscriptionService::with_observers(
        manager,
        Arc::new(|_| {}),
        Arc::new(move |update| sink.lock().unwrap().push(update)),
    );
    let session = directory.path().join("session");
    fs::create_dir_all(&session).unwrap();
    let segment = SpeechSegment {
        end_sample: 3_200,
        peak_probability: 0.9,
        samples: vec![0.25; 1_600],
        start_sample: 1_600,
    };

    service
        .enqueue(
            &session,
            "session-1",
            &segment,
            &TranscriptionSettings::default(),
            &TranslationSettings::default(),
        )
        .unwrap();

    let captured = updates.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].session_id, "session-1");
    assert_eq!(captured[0].segment.status, TranscriptSegmentStatus::Pending);
    assert_eq!(captured[0].segment.start_ms, 100);
    assert_eq!(captured[0].segment.end_ms, 200);
}

#[test]
fn publishes_resolved_segment_snapshots_for_the_frontend() {
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(ModelManager::new(directory.path().join("models")).unwrap());
    let updates: Arc<Mutex<Vec<TranscriptSegmentUpdate>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = updates.clone();
    let service = TranscriptionService::with_observers(
        manager,
        Arc::new(|_| {}),
        Arc::new(move |update| sink.lock().unwrap().push(update)),
    );
    let segment = TranscriptSegment {
        attempts: 1,
        audio_file: "speech/speech-000001.wav".to_owned(),
        detected_language: "Chinese".to_owned(),
        end_ms: 1_250,
        error: None,
        id: 1,
        peak_probability: 0.95,
        start_ms: 250,
        status: TranscriptSegmentStatus::Complete,
        text: "测试完成".to_owned(),
    };

    service.inner.publish_segment("session-1", &segment);

    let captured = updates.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].session_id, "session-1");
    assert_eq!(captured[0].segment, segment);
    let payload = serde_json::to_value(&captured[0]).unwrap();
    assert_eq!(payload["sessionId"], "session-1");
    assert_eq!(payload["segment"]["startMs"], 250);
    assert_eq!(payload["segment"]["status"], "complete");
    assert_eq!(payload["segment"]["text"], "测试完成");
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
        translation: TranslationSettings::default(),
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
            &TranslationSettings::default(),
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
