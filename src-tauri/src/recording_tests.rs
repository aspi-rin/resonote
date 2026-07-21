use super::worker::{
    OutputRouter, RecordingWorkerContext, SourceConfig, SourcePipeline, drain_ready, process_event,
    recording_worker, switch_source,
};
use super::*;
use crate::{
    audio::TARGET_SAMPLE_RATE,
    capture::CapturedAudio,
    settings::{AudioFormat, TranslationSettings},
};
use claxon::FlacReader;
use std::time::Instant;

#[test]
fn mixes_synthetic_microphone_and_system_audio() {
    let directory = tempfile::tempdir().unwrap();
    let settings = AudioSettings {
        format: AudioFormat::Flac,
        source: AudioSourceMode::Mixed,
        ..AudioSettings::default()
    };
    let archive = RecordingArchive::create(directory.path(), &settings).unwrap();
    let session_dir = archive.session_dir().to_path_buf();
    let (capture_sender, capture_receiver) = crossbeam_channel::unbounded();
    let (stop_sender, stop_receiver) = crossbeam_channel::bounded(1);
    for (source, value) in [
        (CaptureSource::Microphone, 0.25),
        (CaptureSource::System, 0.5),
    ] {
        capture_sender
            .send(CaptureEvent::Audio(CapturedAudio {
                captured_at: Instant::now(),
                channels: 1,
                sample_rate: TARGET_SAMPLE_RATE,
                samples: vec![value; 1_600],
                sequence: 0,
                source,
            }))
            .unwrap();
    }
    drop(capture_sender);
    stop_sender.send(()).unwrap();
    let status = Arc::new(RwLock::new(RecordingStatus::default()));
    let result = recording_worker(
        archive,
        capture_receiver,
        stop_receiver,
        RecordingWorkerContext {
            observer: Arc::new(|_| {}),
            source: Arc::new(RwLock::new(settings.source)),
            settings,
            status: status.clone(),
            transcription: None,
            transcription_settings: TranscriptionSettings::default(),
            translation_settings: TranslationSettings::default(),
            vad_model: None,
        },
    )
    .unwrap();

    assert_eq!(result.segments[0].sample_count, 1_600);
    let mut reader = FlacReader::open(session_dir.join("audio-0000.flac")).unwrap();
    let samples: Vec<i32> = reader.samples().map(Result::unwrap).collect();
    assert_eq!(samples.len(), 1_600);
    assert!(
        samples
            .iter()
            .all(|sample| (24_574..=24_576).contains(sample))
    );
    assert_eq!(
        status.read().unwrap().captured_samples,
        result.segments[0].sample_count
    );
}

#[test]
fn switches_audio_source_without_restarting_the_archive() {
    let directory = tempfile::tempdir().unwrap();
    let settings = AudioSettings {
        format: AudioFormat::Flac,
        source: AudioSourceMode::Microphone,
        ..AudioSettings::default()
    };
    let mut archive = RecordingArchive::create(directory.path(), &settings).unwrap();
    let session_dir = archive.session_dir().to_path_buf();
    let status = Arc::new(RwLock::new(RecordingStatus::default()));
    let mut microphone = None;
    let mut system = None;
    let mut active_source = AudioSourceMode::Microphone;

    {
        let mut output = OutputRouter::new(
            &mut archive,
            TranscriptionSettings::default(),
            TranslationSettings::default(),
            None,
            status,
            None,
        )
        .unwrap();
        process_event(
            synthetic_audio(CaptureSource::Microphone, 0.25),
            &mut microphone,
            &mut system,
            &output.status,
        )
        .unwrap();
        drain_ready(
            &mut output,
            &settings,
            active_source,
            &mut microphone,
            &mut system,
        )
        .unwrap();

        switch_source(
            &mut output,
            AudioSourceMode::System,
            &mut active_source,
            &mut microphone,
            &mut system,
        );
        process_event(
            synthetic_audio(CaptureSource::System, 0.5),
            &mut microphone,
            &mut system,
            &output.status,
        )
        .unwrap();
        drain_ready(
            &mut output,
            &settings,
            active_source,
            &mut microphone,
            &mut system,
        )
        .unwrap();
        output.finish().unwrap();
    }

    let manifest = archive.complete().unwrap();
    assert_eq!(manifest.audio_source, AudioSourceMode::Mixed);
    assert_eq!(manifest.segments[0].sample_count, 3_200);
    let mut reader = FlacReader::open(session_dir.join("audio-0000.flac")).unwrap();
    let samples: Vec<i32> = reader.samples().map(Result::unwrap).collect();
    assert!(
        samples[..1_600]
            .iter()
            .all(|sample| (8_190..=8_192).contains(sample))
    );
    assert!(
        samples[1_600..]
            .iter()
            .all(|sample| (16_382..=16_384).contains(sample))
    );
}

fn synthetic_audio(source: CaptureSource, value: f32) -> CaptureEvent {
    CaptureEvent::Audio(CapturedAudio {
        captured_at: Instant::now(),
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        samples: vec![value; 1_600],
        sequence: 0,
        source,
    })
}

#[test]
fn rejects_sequence_gaps_instead_of_hiding_dropped_audio() {
    let mut pipeline = SourcePipeline::new(SourceConfig {
        sample_rate: TARGET_SAMPLE_RATE,
    })
    .unwrap();
    let error = pipeline
        .push(CapturedAudio {
            captured_at: Instant::now(),
            channels: 1,
            sample_rate: TARGET_SAMPLE_RATE,
            samples: vec![0.0; 160],
            sequence: 2,
            source: CaptureSource::System,
        })
        .unwrap_err();
    assert!(matches!(error, RecordingError::CaptureSequence { .. }));
}

#[test]
fn archives_silence_without_vad_in_recording_only_test_service() {
    let directory = tempfile::tempdir().unwrap();
    let settings = AudioSettings {
        format: AudioFormat::Wav,
        ..AudioSettings::default()
    };
    let mut archive = RecordingArchive::create(directory.path(), &settings).unwrap();
    let status = Arc::new(RwLock::new(RecordingStatus::default()));
    {
        let mut output = OutputRouter::new(
            &mut archive,
            TranscriptionSettings::default(),
            TranslationSettings::default(),
            None,
            status.clone(),
            None,
        )
        .unwrap();
        output
            .write(&vec![0.0; TARGET_SAMPLE_RATE as usize])
            .unwrap();
        output.finish().unwrap();
    }
    let manifest = archive.complete().unwrap();

    assert_eq!(manifest.segments.len(), 1);
    assert_eq!(
        status.read().unwrap().captured_samples,
        u64::from(TARGET_SAMPLE_RATE)
    );
}

#[test]
fn missing_vad_model_does_not_leave_recording_in_starting_phase() {
    let directory = tempfile::tempdir().unwrap();
    let manager =
        Arc::new(crate::models::ModelManager::new(directory.path().join("models")).unwrap());
    let transcription = Arc::new(TranscriptionService::new(manager.clone()));
    let service = RecordingService::with_services(
        directory.path().join("recordings"),
        Arc::new(|_| {}),
        Some(transcription),
        Some(manager),
    );

    assert!(matches!(
        service.start(
            AudioSettings::default(),
            TranscriptionSettings::default(),
            TranslationSettings::default(),
        ),
        Err(RecordingError::Model(ModelError::NotInstalled(_)))
    ));
    assert_eq!(service.status().phase, RecordingPhase::Idle);
}

#[test]
#[ignore = "requires a system output device"]
fn records_default_system_loopback_to_disk() {
    let directory = tempfile::tempdir().unwrap();
    let service = RecordingService::new(directory.path().to_path_buf());
    let settings = AudioSettings {
        format: AudioFormat::Wav,
        output_directory: Some(directory.path().to_path_buf()),
        source: AudioSourceMode::System,
        ..AudioSettings::default()
    };

    service
        .start(
            settings,
            TranscriptionSettings::default(),
            TranslationSettings::default(),
        )
        .unwrap();
    thread::sleep(Duration::from_secs(1));
    let stopped = service.stop().unwrap();

    assert_eq!(stopped.phase, RecordingPhase::Idle);
    assert!(stopped.captured_samples > 0);
    assert!(
        stopped
            .session_directory
            .unwrap()
            .join("session.json")
            .exists()
    );
}

#[test]
#[ignore = "requires microphone and system output devices"]
fn records_default_mixed_sources_to_disk() {
    let directory = tempfile::tempdir().unwrap();
    let service = RecordingService::new(directory.path().to_path_buf());
    let settings = AudioSettings {
        format: AudioFormat::Wav,
        output_directory: Some(directory.path().to_path_buf()),
        source: AudioSourceMode::Mixed,
        ..AudioSettings::default()
    };

    service
        .start(
            settings,
            TranscriptionSettings::default(),
            TranslationSettings::default(),
        )
        .unwrap();
    thread::sleep(Duration::from_secs(1));
    let stopped = service.stop().unwrap();

    assert_eq!(stopped.phase, RecordingPhase::Idle);
    assert!(stopped.captured_samples > 0);
    assert!(stopped.microphone_device.is_some());
    assert!(stopped.system_device.is_some());
}

#[test]
#[ignore = "requires microphone and system output devices"]
fn switches_default_sources_while_recording() {
    let directory = tempfile::tempdir().unwrap();
    let service = RecordingService::new(directory.path().to_path_buf());
    let settings = AudioSettings {
        format: AudioFormat::Wav,
        output_directory: Some(directory.path().to_path_buf()),
        source: AudioSourceMode::Microphone,
        ..AudioSettings::default()
    };

    service
        .start(
            settings,
            TranscriptionSettings::default(),
            TranslationSettings::default(),
        )
        .unwrap();
    thread::sleep(Duration::from_millis(250));
    let system = service.set_source(AudioSourceMode::System).unwrap();
    assert_eq!(system.audio_source, AudioSourceMode::System);
    assert!(system.system_device.is_some());
    thread::sleep(Duration::from_millis(250));
    let mixed = service.set_source(AudioSourceMode::Mixed).unwrap();
    assert_eq!(mixed.audio_source, AudioSourceMode::Mixed);
    assert!(mixed.microphone_device.is_some());
    assert!(mixed.system_device.is_some());
    thread::sleep(Duration::from_millis(250));
    let stopped = service.stop().unwrap();

    assert_eq!(stopped.phase, RecordingPhase::Idle);
    assert!(stopped.captured_samples > 0);
}
