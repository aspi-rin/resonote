use super::*;
use sha2::{Digest, Sha256};

fn entity(id: &str, canonical_name: &str) -> ContextEntity {
    ContextEntity {
        aliases: vec![format!("  {canonical_name} alias  ")],
        canonical_name: format!("  {canonical_name}  "),
        common_asr_errors: vec![format!("{canonical_name}-misheard"), "   ".to_owned()],
        description: "  a description  ".to_owned(),
        id: id.to_owned(),
        origin: None,
    }
}

fn glossary(id: &str, term: &str) -> GlossaryEntry {
    GlossaryEntry {
        aliases: vec![format!(" {term}-short ")],
        common_asr_errors: Vec::new(),
        id: id.to_owned(),
        meaning: format!("  meaning of {term} "),
        origin: None,
        term: format!(" {term} "),
    }
}

fn populated_global() -> GlobalContextContent {
    GlobalContextContent {
        free_text: "  extra notes  ".to_owned(),
        glossary: vec![glossary("", "SLO"), glossary("", "MTTR")],
        identity: ContextIdentity {
            aliases: vec!["  Xin  ".to_owned(), "".to_owned()],
            canonical_name: "  Ye Xin  ".to_owned(),
            common_asr_errors: vec!["Ye Shin".to_owned()],
        },
        knowledge_background: " audio pipelines ".to_owned(),
        preferred_languages: vec![" zh-CN ".to_owned(), " en-US ".to_owned()],
        recurring_organizations: vec![entity("", "Acme")],
        recurring_people: vec![entity("", "Alice"), entity("", "Bob")],
        recurring_products_and_projects: vec![entity("", "Resonote")],
        roles_and_affiliations: vec![" staff engineer ".to_owned()],
        timezone: " Asia/Shanghai ".to_owned(),
    }
}

fn populated_meeting() -> MeetingContextContent {
    MeetingContextContent {
        agenda: vec![" review ".to_owned(), "   ".to_owned(), " plan ".to_owned()],
        background: " prior release ".to_owned(),
        constraints: vec![" ship before friday ".to_owned()],
        date: " 2026-08-15 ".to_owned(),
        free_text: " misc ".to_owned(),
        glossary: vec![glossary("", "P0")],
        key_numbers: vec![" 48000 ".to_owned()],
        participants: vec![
            ParticipantContext {
                entity: entity("", "Alice"),
                role: " host ".to_owned(),
                speaker_label: Some(" spk_0 ".to_owned()),
            },
            ParticipantContext {
                entity: entity("", "Bob"),
                role: " guest ".to_owned(),
                speaker_label: Some("   ".to_owned()),
            },
        ],
        prior_facts_and_decisions: vec![" budget approved ".to_owned()],
        purpose: " decide the release date ".to_owned(),
        questions_to_discuss: vec![" who owns rollout ".to_owned()],
        source_notes: vec![" design doc ".to_owned()],
        title: " release sync ".to_owned(),
    }
}

fn store(directory: &Path) -> GlobalContextStore {
    GlobalContextStore::open(directory.join("global-context.json")).unwrap()
}

fn hash(rendered: &str) -> String {
    hex::encode(Sha256::digest(rendered.as_bytes()))
}

fn source_snapshot() -> SourceSnapshot {
    SourceSnapshot {
        complete_segment_ids: vec![1, 2],
        completed_segment_count: 2,
        failed_segment_count: 0,
        failed_segment_ids: Vec::new(),
        selected_segments: vec![SourceSegmentSnapshot {
            end_ms: 2_000,
            id: 1,
            start_ms: 0,
            text: "hello".to_owned(),
        }],
        session_status: SourceSessionStatus::Completed,
        skipped_empty_segment_ids: vec![3],
        skipped_segment_count: 1,
        transcript_sha256: "a".repeat(64),
        transcript_status: SourceTranscriptStatus::Complete,
    }
}

fn input_snapshot() -> InputSnapshot {
    InputSnapshot {
        chunker_version: "1".to_owned(),
        context: ContextSnapshot {
            captured_at: DateTime::UNIX_EPOCH,
            global: normalize_global(&populated_global()),
            global_revision: 1,
            meeting: normalize_meeting(&populated_meeting()),
            meeting_revision: 1,
            merge_policy_version: "1".to_owned(),
            rendered_sha256: "b".repeat(64),
            schema_version: 1,
        },
        output_language: OutputLanguage::ZhCn,
        pipeline_version: "1".to_owned(),
        prompt_version: "1".to_owned(),
        provider: ProviderSnapshot {
            auth_mode: ProviderAuthMode::None,
            endpoint: "http://127.0.0.1:8000/v1".to_owned(),
            max_input_characters: 48_000,
            model: "local-model".to_owned(),
            request_timeout_seconds: 180,
        },
        source: source_snapshot(),
    }
}

fn run_with_checkpoints() -> MeetingNotesRun {
    MeetingNotesRun {
        accept_partial: true,
        cancellation_requested: false,
        clean_checkpoint: CleanCheckpoint {
            units: vec![CheckpointUnit {
                attempts: 2,
                error: None,
                input_sha256: "c".repeat(64),
                next_retry_at: None,
                output: Some(CleanPartResult {
                    parts: vec![CleanedPart {
                        part_index: 0,
                        segment_id: 1,
                        text: "Hello.".to_owned(),
                    }],
                }),
                output_sha256: Some("d".repeat(64)),
                source_keys: vec!["1:0".to_owned()],
                stage: CheckpointStage::Clean,
                state: CheckpointUnitState::Committed,
                unit_id: "clean-0".to_owned(),
            }],
        },
        created_at: DateTime::UNIX_EPOCH,
        error: Some(RunError {
            cause_code: Some(MeetingNotesErrorCode::ProviderTimeout),
            code: MeetingNotesErrorCode::RetryExhausted,
            http_status: None,
            message_key: MeetingNotesErrorCode::RetryExhausted.message_key(),
            retryable: false,
            stage: Some(RunStage::Summarizing),
        }),
        generation: 3,
        input_fingerprint: "e".repeat(64),
        input_snapshot: input_snapshot(),
        job_id: "job-1".to_owned(),
        progress: RunProgress {
            completed_chunks: 1,
            total_chunks: 2,
        },
        regeneration_nonce: 1,
        run_fingerprint: "f".repeat(64),
        stage: Some(RunStage::Summarizing),
        state: AnalysisState::Failed,
        summary_checkpoint: SummaryCheckpoint {
            candidates: vec![MeetingSummary::default()],
            mode: SummaryMode::MapReduce,
            reduce_level: 1,
            units: vec![CheckpointUnit {
                attempts: 1,
                error: None,
                input_sha256: "0".repeat(64),
                next_retry_at: Some(DateTime::UNIX_EPOCH),
                output: None,
                output_sha256: None,
                source_keys: vec!["1".to_owned()],
                stage: CheckpointStage::SummaryMap,
                state: CheckpointUnitState::Pending,
                unit_id: "summary-0".to_owned(),
            }],
        },
        updated_at: DateTime::UNIX_EPOCH,
    }
}

#[test]
fn restores_every_global_field_order_and_revision_after_a_reload() {
    let directory = tempfile::tempdir().unwrap();
    let saved = store(directory.path())
        .save(0, &populated_global())
        .unwrap();

    let reloaded = store(directory.path()).document();

    assert_eq!(reloaded, saved);
    assert_eq!(reloaded.revision, 1);
    assert_eq!(reloaded.schema_version, 1);
    assert_eq!(reloaded.content.identity.canonical_name, "Ye Xin");
    assert_eq!(reloaded.content.identity.aliases, vec!["Xin".to_owned()]);
    assert_eq!(
        reloaded
            .content
            .recurring_people
            .iter()
            .map(|person| person.canonical_name.as_str())
            .collect::<Vec<_>>(),
        vec!["Alice", "Bob"]
    );
    assert_eq!(
        reloaded
            .content
            .glossary
            .iter()
            .map(|entry| entry.term.as_str())
            .collect::<Vec<_>>(),
        vec!["SLO", "MTTR"]
    );
    assert_eq!(
        reloaded.content.recurring_people[0].common_asr_errors.len(),
        1
    );
    assert_eq!(reloaded.content.preferred_languages, vec!["zh-CN", "en-US"]);
}

#[test]
fn reports_revision_zero_and_an_empty_template_before_the_first_save() {
    let directory = tempfile::tempdir().unwrap();

    let document = store(directory.path()).document();

    assert_eq!(document.revision, 0);
    assert_eq!(document.content, GlobalContextContent::default());
    assert!(!directory.path().join("global-context.json").exists());
}

#[test]
fn assigns_stable_uuids_to_blank_ids_that_survive_a_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());

    let saved = store.save(0, &populated_global()).unwrap();

    let ids = saved
        .content
        .recurring_people
        .iter()
        .map(|person| person.id.clone())
        .collect::<Vec<_>>();
    assert!(ids.iter().all(|id| Uuid::parse_str(id).is_ok()));
    assert_ne!(ids[0], ids[1]);
    assert_eq!(
        store.save(1, &saved.content).unwrap().content,
        saved.content
    );
    assert_eq!(
        GlobalContextStore::open(directory.path().join("global-context.json"))
            .unwrap()
            .document()
            .content
            .recurring_people
            .iter()
            .map(|person| person.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
}

#[test]
fn reassigns_duplicate_ids_inside_the_same_list() {
    let content = GlobalContextContent {
        glossary: vec![glossary("shared", "SLO"), glossary("shared", "MTTR")],
        recurring_people: vec![entity("same", "Alice"), entity("same", "Bob")],
        ..GlobalContextContent::default()
    };

    let normalized = normalize_global(&content);

    assert_eq!(normalized.recurring_people[0].id, "same");
    assert_ne!(normalized.recurring_people[1].id, "same");
    assert!(Uuid::parse_str(&normalized.recurring_people[1].id).is_ok());
    assert_eq!(normalized.glossary[0].id, "shared");
    assert_ne!(normalized.glossary[1].id, "shared");
}

#[test]
fn normalization_is_idempotent_and_drops_blank_entries() {
    let global = normalize_global(&GlobalContextContent {
        preferred_languages: vec!["  ".to_owned(), " zh-CN ".to_owned()],
        recurring_people: vec![entity("", "Alice"), ContextEntity::default()],
        ..populated_global()
    });
    let meeting = normalize_meeting(&MeetingContextContent {
        participants: vec![
            ParticipantContext::default(),
            ParticipantContext {
                entity: entity("", "Alice"),
                role: " host ".to_owned(),
                speaker_label: None,
            },
        ],
        ..populated_meeting()
    });

    assert_eq!(normalize_global(&global), global);
    assert_eq!(normalize_meeting(&meeting), meeting);
    assert_eq!(global.preferred_languages, vec!["zh-CN"]);
    assert_eq!(global.recurring_people.len(), 1);
    assert_eq!(meeting.participants.len(), 1);
    assert_eq!(meeting.participants[0].role, "host");
    assert_eq!(meeting.participants[0].speaker_label, None);
    assert_eq!(meeting.agenda, vec!["review", "plan"]);
    assert_eq!(meeting.title, "release sync");
}

#[test]
fn keeps_the_revision_and_rendered_hash_when_the_same_content_is_saved_again() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    let first = store.save(0, &populated_global()).unwrap();
    let first_hash = hash(&render_global(&first.content));

    let second = store.save(1, &first.content).unwrap();

    assert_eq!(second.revision, 1);
    assert_eq!(second.updated_at, first.updated_at);
    assert_eq!(hash(&render_global(&second.content)), first_hash);
    assert_eq!(store.document(), first);
}

#[test]
fn rejects_a_save_whose_expected_revision_does_not_match() {
    let directory = tempfile::tempdir().unwrap();
    let store = store(directory.path());
    store.save(0, &populated_global()).unwrap();

    let error = store.save(0, &GlobalContextContent::default()).unwrap_err();

    assert_eq!(error.code(), MeetingNotesErrorCode::ContextRevisionConflict);
    assert_eq!(
        error.payload().message_key,
        "meetingNotesErrorContextRevisionConflict"
    );
    assert!(!error.payload().retryable);
    assert_eq!(store.document().revision, 1);
}

#[test]
fn rejects_a_render_that_exceeds_the_single_layer_limit() {
    let directory = tempfile::tempdir().unwrap();
    let baseline = rendered_character_count(&render_global(&GlobalContextContent::default()));
    let content = GlobalContextContent {
        free_text: "a".repeat(GLOBAL_CONTEXT_CHARACTER_LIMIT + 1 - baseline),
        ..GlobalContextContent::default()
    };
    assert_eq!(
        rendered_character_count(&render_global(&content)),
        GLOBAL_CONTEXT_CHARACTER_LIMIT + 1
    );

    let payload = store(directory.path())
        .save(0, &content)
        .unwrap_err()
        .payload();

    assert_eq!(payload.code, MeetingNotesErrorCode::ContextTooLarge);
    assert_eq!(payload.params["characters"], 20_001);
    assert_eq!(payload.params["limit"], 20_000);
    assert!(!directory.path().join("global-context.json").exists());
}

#[test]
fn measures_capacity_in_unicode_characters_and_renders_global_before_meeting() {
    let content = GlobalContextContent {
        free_text: "日".repeat(10),
        ..GlobalContextContent::default()
    };
    let rendered = render_global(&content);

    assert_eq!(
        rendered.chars().count(),
        rendered_character_count(&rendered)
    );
    assert!(rendered.len() > rendered.chars().count());
    assert!(ensure_within_limit(&rendered, GLOBAL_CONTEXT_CHARACTER_LIMIT).is_ok());

    let merged = render_merged(&content, &MeetingContextContent::default());
    assert!(merged.find("globalContext").unwrap() < merged.find("meetingContext").unwrap());
    assert_eq!(MERGED_CONTEXT_CHARACTER_LIMIT, 30_000);
}

#[test]
fn saves_reloads_and_versions_the_meeting_context_of_a_session() {
    let directory = tempfile::tempdir().unwrap();
    let session = directory.path().join("session");
    fs::create_dir_all(&session).unwrap();

    let saved = save_meeting_context(&session, "session-1", 0, &populated_meeting()).unwrap();

    assert_eq!(saved.revision, 1);
    assert_eq!(saved.content.participants[0].entity.canonical_name, "Alice");
    let document = load_document(&session.join(DOCUMENT_NAME)).unwrap();
    assert_eq!(document.document_revision, 1);
    assert_eq!(document.session_id, "session-1");
    assert_eq!(document.meeting_context, saved);

    let unchanged = save_meeting_context(&session, "session-1", 1, &saved.content).unwrap();
    assert_eq!(unchanged, saved);
    assert_eq!(
        load_document(&session.join(DOCUMENT_NAME))
            .unwrap()
            .document_revision,
        1
    );
    assert_eq!(
        save_meeting_context(&session, "session-1", 0, &saved.content)
            .unwrap_err()
            .code(),
        MeetingNotesErrorCode::ContextRevisionConflict
    );
}

#[test]
fn round_trips_an_analysis_document_that_carries_checkpoints() {
    let directory = tempfile::tempdir().unwrap();
    let session = directory.path().join("session");
    fs::create_dir_all(&session).unwrap();
    let path = session.join(DOCUMENT_NAME);
    let mut document = new_document("session-1");
    document.current_run = Some(run_with_checkpoints());
    document.last_successful_result = Some(AnalysisResult {
        cleaned: CleanResult {
            segments: vec![CleanedSegment {
                end_ms: 2_000,
                segment_id: 1,
                start_ms: 0,
                text: "Hello.".to_owned(),
            }],
        },
        generated_at: DateTime::UNIX_EPOCH,
        input_fingerprint: "e".repeat(64),
        input_quality: InputQuality {
            completed_segment_count: 2,
            failed_segment_count: 0,
            kind: InputQualityKind::Partial,
            skipped_segment_count: 1,
        },
        input_snapshot: input_snapshot(),
        output_language: OutputLanguage::ZhCn,
        run_fingerprint: "f".repeat(64),
        summary: MeetingSummary::default(),
    });

    save_document(&path, &document).unwrap();

    assert_eq!(load_document(&path).unwrap(), document);
    let json = serde_json::to_string(&document).unwrap();
    assert!(json.contains("\"cleanCheckpoint\""));
    assert!(json.contains("\"outputLanguage\":\"zh-CN\""));
}

#[test]
fn refuses_to_recreate_a_removed_session_directory() {
    let directory = tempfile::tempdir().unwrap();
    let session = directory.path().join("session");

    let error =
        save_document(&session.join(DOCUMENT_NAME), &new_document("session-1")).unwrap_err();

    assert_eq!(error.code(), MeetingNotesErrorCode::SessionDeleted);
    assert!(!session.exists());
}

#[test]
fn keeps_checkpoints_out_of_the_session_analysis_view() {
    let run = run_with_checkpoints();
    let view = SessionAnalysisView {
        current_run: Some(MeetingNotesRunView {
            cancellation_requested: run.cancellation_requested,
            created_at: run.created_at,
            error: run.error.clone(),
            generation: run.generation,
            input_fingerprint: run.input_fingerprint.clone(),
            job_id: run.job_id.clone(),
            progress: run.progress,
            run_fingerprint: run.run_fingerprint.clone(),
            stage: run.stage,
            state: run.state,
            updated_at: run.updated_at,
        }),
        document_revision: 4,
        freshness: AnalysisFreshness::Stale,
        last_successful_result: None,
        meeting_context: empty_meeting_context(),
        session_id: "session-1".to_owned(),
        stale_reasons: vec![StaleReason::MeetingContextChanged],
    };

    let json = serde_json::to_string(&view).unwrap();

    assert!(!json.to_lowercase().contains("checkpoint"));
    assert!(!json.contains("acceptPartial"));
    assert!(!json.contains("regenerationNonce"));
    assert!(!json.contains("inputSnapshot"));
    assert!(json.contains("\"staleReasons\":[\"meetingContextChanged\"]"));
}

#[test]
fn serializes_every_stable_error_code_exactly_once() {
    let codes = [
        MeetingNotesErrorCode::MeetingNotesNotConfigured,
        MeetingNotesErrorCode::InvalidEndpoint,
        MeetingNotesErrorCode::InsecureEndpoint,
        MeetingNotesErrorCode::ResponseTooLarge,
        MeetingNotesErrorCode::SessionNotFound,
        MeetingNotesErrorCode::SessionIdConflict,
        MeetingNotesErrorCode::SessionDeleted,
        MeetingNotesErrorCode::SessionStillRecording,
        MeetingNotesErrorCode::TranscriptNotReady,
        MeetingNotesErrorCode::TranscriptDocumentCorrupt,
        MeetingNotesErrorCode::TranscriptInvalid,
        MeetingNotesErrorCode::NoTranscriptContent,
        MeetingNotesErrorCode::PartialConfirmationRequired,
        MeetingNotesErrorCode::ContextDraftInvalid,
        MeetingNotesErrorCode::ContextRevisionConflict,
        MeetingNotesErrorCode::ContextTooLarge,
        MeetingNotesErrorCode::ProviderChanged,
        MeetingNotesErrorCode::ProviderUnauthorized,
        MeetingNotesErrorCode::ProviderForbidden,
        MeetingNotesErrorCode::ProviderRateLimited,
        MeetingNotesErrorCode::ProviderTimeout,
        MeetingNotesErrorCode::ProviderUnavailable,
        MeetingNotesErrorCode::ProviderResponseInvalid,
        MeetingNotesErrorCode::ProviderOutputTruncated,
        MeetingNotesErrorCode::AnalysisBusy,
        MeetingNotesErrorCode::CleanOutputInvalid,
        MeetingNotesErrorCode::SummaryOutputInvalid,
        MeetingNotesErrorCode::SummaryReduceDidNotConverge,
        MeetingNotesErrorCode::SummaryCandidateTooLarge,
        MeetingNotesErrorCode::RetryExhausted,
        MeetingNotesErrorCode::AnalysisCheckpointCorrupt,
        MeetingNotesErrorCode::AnalysisDocumentCorrupt,
        MeetingNotesErrorCode::IoError,
    ];

    let serialized = codes
        .iter()
        .map(|code| serde_json::to_string(code).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(codes.len(), 33);
    assert_eq!(
        serialized
            .iter()
            .map(|value| value.trim_matches('"').to_owned())
            .collect::<HashSet<_>>()
            .len(),
        33
    );
    for (code, value) in codes.iter().zip(&serialized) {
        assert_eq!(value.trim_matches('"'), code.as_str());
    }
    assert_eq!(
        MeetingNotesErrorCode::IoError.message_key(),
        "meetingNotesErrorIoError"
    );
    assert_eq!(
        MeetingNotesErrorCode::ProviderOutputTruncated.message_key(),
        "meetingNotesErrorProviderOutputTruncated"
    );
    assert_eq!(
        chat_error_code(&ChatError::OutputTruncated),
        MeetingNotesErrorCode::ProviderOutputTruncated
    );
    assert!(MeetingNotesErrorCode::ProviderTimeout.retryable());
    assert!(MeetingNotesErrorCode::ProviderOutputTruncated.retryable());
    assert!(!MeetingNotesErrorCode::ContextTooLarge.retryable());
}

#[test]
fn maps_catalog_failures_onto_stable_session_codes() {
    assert_eq!(
        MeetingNotesError::from(CatalogError::SessionNotFound("a".to_owned())).code(),
        MeetingNotesErrorCode::SessionNotFound
    );
    assert_eq!(
        MeetingNotesError::from(CatalogError::InvalidSessionId).code(),
        MeetingNotesErrorCode::SessionNotFound
    );
    assert_eq!(
        MeetingNotesError::from(CatalogError::SessionIdConflict("a".to_owned())).code(),
        MeetingNotesErrorCode::SessionIdConflict
    );
}

#[test]
fn keeps_the_error_payload_free_of_context_text() {
    let payload = MeetingNotesError::context_too_large(20_001, 20_000).payload();

    let json = serde_json::to_string(&payload).unwrap();

    assert_eq!(
        json,
        "{\"code\":\"CONTEXT_TOO_LARGE\",\"messageKey\":\"meetingNotesErrorContextTooLarge\",\"params\":{\"characters\":20001,\"limit\":20000},\"retryable\":false}"
    );
}
