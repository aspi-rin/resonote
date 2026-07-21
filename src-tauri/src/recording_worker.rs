use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use crate::{
    audio::{AudioBlock, StreamResampler, TARGET_SAMPLE_RATE, mix_mono, rms_db, waveform_bins},
    capture::{CaptureEvent, CaptureSource, CapturedAudio},
    settings::{AudioSettings, AudioSourceMode, TranscriptionSettings},
    storage::{RecordingArchive, SessionManifest},
    transcription::TranscriptionService,
    vad::{VadOutput, VoiceActivitySegmenter},
};
use crossbeam_channel::{Receiver, RecvTimeoutError};

use super::{
    CHECKPOINT_INTERVAL, MAX_BUFFERED_SECONDS, MIX_CHUNK_SAMPLES, RecordingError, RecordingPhase,
    RecordingStatus, STATUS_INTERVAL, StatusObserver,
};

#[derive(Clone, Copy)]
pub(super) struct SourceConfig {
    pub(super) sample_rate: u32,
}

pub(super) struct SourcePipeline {
    expected_sequence: u64,
    input_rate: u32,
    pending: VecDeque<f32>,
    resampler: StreamResampler,
}

impl SourcePipeline {
    pub(super) fn new(config: SourceConfig) -> Result<Self, RecordingError> {
        Ok(Self {
            expected_sequence: 0,
            input_rate: config.sample_rate,
            pending: VecDeque::new(),
            resampler: StreamResampler::to_speech_rate(config.sample_rate)?,
        })
    }

    pub(super) fn push(&mut self, audio: CapturedAudio) -> Result<SourceSnapshot, RecordingError> {
        if audio.sample_rate != self.input_rate {
            return Err(RecordingError::SampleRateChanged {
                actual: audio.sample_rate,
                expected: self.input_rate,
                source_kind: audio.source,
            });
        }
        if audio.sequence != self.expected_sequence {
            return Err(RecordingError::CaptureSequence {
                actual: audio.sequence,
                capture_source: audio.source,
                expected: self.expected_sequence,
            });
        }
        self.expected_sequence += 1;
        let block = AudioBlock::new(audio.samples, audio.sample_rate, audio.channels)?;
        let mono = block.into_mono();
        let level = rms_db(&mono);
        let waveform = waveform_bins(&mono, 48);
        self.pending.extend(self.resampler.push(&mono)?);
        if self.pending.len() > TARGET_SAMPLE_RATE as usize * MAX_BUFFERED_SECONDS {
            return Err(RecordingError::AudioBufferOverflow(audio.source));
        }
        Ok(SourceSnapshot { level, waveform })
    }

    pub(super) fn finish(&mut self) -> Result<(), RecordingError> {
        self.pending.extend(self.resampler.finish()?);
        Ok(())
    }

    fn take(&mut self, count: usize) -> Vec<f32> {
        self.pending
            .drain(..count.min(self.pending.len()))
            .collect()
    }

    fn clear_pending(&mut self) {
        self.pending.clear();
    }
}

#[derive(Debug)]
pub(super) struct SourceSnapshot {
    level: f32,
    waveform: Vec<f32>,
}

pub(super) struct RecordingWorkerContext {
    pub(super) observer: StatusObserver,
    pub(super) settings: AudioSettings,
    pub(super) source: Arc<RwLock<AudioSourceMode>>,
    pub(super) status: Arc<RwLock<RecordingStatus>>,
    pub(super) transcription: Option<Arc<TranscriptionService>>,
    pub(super) transcription_settings: TranscriptionSettings,
    pub(super) vad_model: Option<PathBuf>,
}

pub(super) fn recording_worker(
    mut archive: RecordingArchive,
    capture: Receiver<CaptureEvent>,
    stop: Receiver<()>,
    context: RecordingWorkerContext,
) -> Result<SessionManifest, RecordingError> {
    let outcome = process_audio(&mut archive, &capture, &stop, &context);
    match outcome {
        Ok(()) => {
            let manifest = archive.complete()?;
            update_shared_status(&context.status, &context.observer, |current| {
                current.elapsed_ms =
                    current.captured_samples.saturating_mul(1_000) / u64::from(TARGET_SAMPLE_RATE);
                current.phase = RecordingPhase::Idle;
                current.segment_count = manifest.segments.len();
            });
            Ok(manifest)
        }
        Err(error) => {
            let message = error.to_string();
            let _ = archive.fail(message.clone());
            update_shared_status(&context.status, &context.observer, |current| {
                current.error = Some(message);
                current.phase = RecordingPhase::Failed;
            });
            Err(error)
        }
    }
}

fn process_audio(
    archive: &mut RecordingArchive,
    capture: &Receiver<CaptureEvent>,
    stop: &Receiver<()>,
    context: &RecordingWorkerContext,
) -> Result<(), RecordingError> {
    let started = Instant::now();
    let mut last_checkpoint = started;
    let mut last_status = started - STATUS_INTERVAL;
    let mut active_source = context.settings.source;
    let mut microphone = None;
    let mut system = None;
    let mut output = OutputRouter::new(
        archive,
        context.transcription_settings.clone(),
        context.transcription.clone(),
        context.status.clone(),
        context.vad_model.as_deref(),
    )?;

    loop {
        let requested_source = *context
            .source
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        switch_source(
            &mut output,
            requested_source,
            &mut active_source,
            &mut microphone,
            &mut system,
        );
        if stop.try_recv().is_ok() {
            for event in capture.try_iter() {
                process_event(event, &mut microphone, &mut system, &context.status)?;
            }
            break;
        }
        match capture.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => process_event(event, &mut microphone, &mut system, &context.status)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        drain_ready(
            &mut output,
            &context.settings,
            active_source,
            &mut microphone,
            &mut system,
        )?;
        let now = Instant::now();
        if now.duration_since(last_checkpoint) >= CHECKPOINT_INTERVAL {
            output.archive.checkpoint()?;
            last_checkpoint = now;
        }
        if now.duration_since(last_status) >= STATUS_INTERVAL {
            update_shared_status(&context.status, &context.observer, |current| {
                current.elapsed_ms = started.elapsed().as_millis() as u64;
                current.segment_count = output.archive.manifest().segments.len();
            });
            last_status = now;
        }
    }

    if let Some(pipeline) = microphone.as_mut() {
        pipeline.finish()?;
    }
    if let Some(pipeline) = system.as_mut() {
        pipeline.finish()?;
    }
    let requested_source = *context
        .source
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    switch_source(
        &mut output,
        requested_source,
        &mut active_source,
        &mut microphone,
        &mut system,
    );
    drain_final(
        &mut output,
        &context.settings,
        active_source,
        &mut microphone,
        &mut system,
    )?;
    output.finish()?;
    output.archive.checkpoint()?;
    Ok(())
}

pub(super) fn process_event(
    event: CaptureEvent,
    microphone: &mut Option<SourcePipeline>,
    system: &mut Option<SourcePipeline>,
    status: &Arc<RwLock<RecordingStatus>>,
) -> Result<(), RecordingError> {
    match event {
        CaptureEvent::Audio(audio) => {
            let source = audio.source;
            let pipeline = match source {
                CaptureSource::Microphone => microphone,
                CaptureSource::System => system,
            };
            if pipeline.is_none() {
                *pipeline = Some(SourcePipeline::new(SourceConfig {
                    sample_rate: audio.sample_rate,
                })?);
            }
            let snapshot = pipeline
                .as_mut()
                .expect("source pipeline was initialized")
                .push(audio)?;
            let mut current = status
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match source {
                CaptureSource::Microphone => {
                    current.microphone_db = snapshot.level;
                    current.microphone_waveform = snapshot.waveform;
                }
                CaptureSource::System => {
                    current.system_db = snapshot.level;
                    current.system_waveform = snapshot.waveform;
                }
            }
            Ok(())
        }
        CaptureEvent::Error { message, source } => Err(RecordingError::CaptureStream {
            capture_source: source,
            message,
        }),
    }
}

pub(super) fn switch_source(
    output: &mut OutputRouter<'_>,
    requested: AudioSourceMode,
    active: &mut AudioSourceMode,
    microphone: &mut Option<SourcePipeline>,
    system: &mut Option<SourcePipeline>,
) {
    if requested == *active {
        return;
    }
    if let Some(pipeline) = microphone {
        pipeline.clear_pending();
    }
    if let Some(pipeline) = system {
        pipeline.clear_pending();
    }
    output.include_source(requested);
    *active = requested;
}

pub(super) fn drain_ready(
    output: &mut OutputRouter<'_>,
    settings: &AudioSettings,
    source: AudioSourceMode,
    microphone: &mut Option<SourcePipeline>,
    system: &mut Option<SourcePipeline>,
) -> Result<(), RecordingError> {
    match source {
        AudioSourceMode::Microphone => {
            if let Some(pipeline) = system {
                pipeline.clear_pending();
            }
            let count = microphone
                .as_ref()
                .map_or(0, |pipeline| pipeline.pending.len());
            write_single(output, microphone, count, settings.microphone_gain)
        }
        AudioSourceMode::System => {
            if let Some(pipeline) = microphone {
                pipeline.clear_pending();
            }
            let count = system.as_ref().map_or(0, |pipeline| pipeline.pending.len());
            write_single(output, system, count, settings.system_gain)
        }
        AudioSourceMode::Mixed => {
            let count = microphone
                .as_ref()
                .zip(system.as_ref())
                .map_or(0, |(mic, output)| {
                    mic.pending.len().min(output.pending.len())
                });
            write_mixed(output, microphone, system, count, settings)
        }
    }
}

fn drain_final(
    output: &mut OutputRouter<'_>,
    settings: &AudioSettings,
    source: AudioSourceMode,
    microphone: &mut Option<SourcePipeline>,
    system: &mut Option<SourcePipeline>,
) -> Result<(), RecordingError> {
    match source {
        AudioSourceMode::Mixed => {
            let count = microphone
                .as_ref()
                .zip(system.as_ref())
                .map_or(0, |(mic, output)| {
                    mic.pending.len().max(output.pending.len())
                });
            write_mixed(output, microphone, system, count, settings)
        }
        _ => drain_ready(output, settings, source, microphone, system),
    }
}

fn write_single(
    output: &mut OutputRouter<'_>,
    pipeline: &mut Option<SourcePipeline>,
    mut count: usize,
    gain: f32,
) -> Result<(), RecordingError> {
    let Some(pipeline) = pipeline.as_mut() else {
        return Ok(());
    };
    while count > 0 {
        let take = count.min(MIX_CHUNK_SAMPLES);
        let samples: Vec<f32> = pipeline
            .take(take)
            .into_iter()
            .map(|sample| (sample * gain).clamp(-1.0, 1.0))
            .collect();
        output.write(&samples)?;
        count -= take;
    }
    Ok(())
}

fn write_mixed(
    output: &mut OutputRouter<'_>,
    microphone: &mut Option<SourcePipeline>,
    system: &mut Option<SourcePipeline>,
    mut count: usize,
    settings: &AudioSettings,
) -> Result<(), RecordingError> {
    let Some(microphone) = microphone.as_mut() else {
        return Ok(());
    };
    let Some(system) = system.as_mut() else {
        return Ok(());
    };
    while count > 0 {
        let take = count.min(MIX_CHUNK_SAMPLES);
        let mixed = mix_mono(
            &microphone.take(take),
            &system.take(take),
            settings.microphone_gain,
            settings.system_gain,
        );
        output.write(&mixed)?;
        count -= take;
    }
    Ok(())
}

pub(super) struct OutputRouter<'a> {
    archive: &'a mut RecordingArchive,
    session_directory: PathBuf,
    session_id: String,
    pub(super) status: Arc<RwLock<RecordingStatus>>,
    pub(super) transcription: Option<Arc<TranscriptionService>>,
    pub(super) transcription_settings: TranscriptionSettings,
    vad: Option<VoiceActivitySegmenter>,
}

impl<'a> OutputRouter<'a> {
    pub(super) fn new(
        archive: &'a mut RecordingArchive,
        transcription_settings: TranscriptionSettings,
        transcription: Option<Arc<TranscriptionService>>,
        status: Arc<RwLock<RecordingStatus>>,
        vad_model: Option<&std::path::Path>,
    ) -> Result<Self, RecordingError> {
        let vad = vad_model
            .map(|path| {
                VoiceActivitySegmenter::new(
                    transcription_settings.vad,
                    path,
                    transcription_settings.threads,
                )
            })
            .transpose()?;
        Ok(Self {
            session_directory: archive.session_dir().to_path_buf(),
            session_id: archive.manifest().session_id.clone(),
            archive,
            status,
            transcription,
            transcription_settings,
            vad,
        })
    }

    fn include_source(&mut self, source: AudioSourceMode) {
        self.archive.include_source(source);
    }

    pub(super) fn write(&mut self, samples: &[f32]) -> Result<(), RecordingError> {
        {
            let mut current = self
                .status
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            current.captured_samples = current
                .captured_samples
                .saturating_add(samples.len() as u64);
        }
        self.archive.append(samples)?;
        if let Some(vad) = self.vad.as_mut() {
            let output = vad.push(samples);
            self.handle_vad_output(output)?;
        }
        Ok(())
    }

    pub(super) fn finish(&mut self) -> Result<(), RecordingError> {
        if let Some(vad) = self.vad.as_mut() {
            let output = vad.finish();
            self.handle_vad_output(output)?;
        }
        Ok(())
    }

    fn handle_vad_output(&mut self, output: VadOutput) -> Result<(), RecordingError> {
        self.status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .vad_probability = output.latest_probability;
        for segment in output.segments {
            if let Some(transcription) = &self.transcription {
                if let Err(error) = transcription.enqueue(
                    &self.session_directory,
                    &self.session_id,
                    &segment,
                    &self.transcription_settings,
                ) {
                    self.report_transcription_error(error.to_string());
                }
            }
        }
        Ok(())
    }

    fn report_transcription_error(&self, message: String) {
        tracing::warn!(error = %message, "failed to queue speech for transcription");
        self.status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .transcription_error = Some(message);
    }
}

fn update_shared_status(
    status: &Arc<RwLock<RecordingStatus>>,
    observer: &StatusObserver,
    mutate: impl FnOnce(&mut RecordingStatus),
) {
    let updated = {
        let mut current = status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        mutate(&mut current);
        current.clone()
    };
    observer(updated);
}
