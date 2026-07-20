use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};
use thiserror::Error;

use crate::audio::TARGET_SAMPLE_RATE;

pub const VAD_FRAME_SAMPLES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VadConfig {
    pub activation_threshold: f32,
    pub max_segment_ms: u32,
    pub min_silence_ms: u32,
    pub min_speech_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            activation_threshold: 0.5,
            max_segment_ms: 30_000,
            min_silence_ms: 500,
            min_speech_ms: 250,
        }
    }
}

impl VadConfig {
    pub fn validate(&self) -> Result<(), VadError> {
        if !self.activation_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.activation_threshold)
        {
            return Err(VadError::InvalidConfig(
                "activationThreshold must be between 0 and 1".to_owned(),
            ));
        }
        if self.min_speech_ms == 0 || self.min_silence_ms == 0 {
            return Err(VadError::InvalidConfig(
                "minimum speech and silence durations must be positive".to_owned(),
            ));
        }
        if self.max_segment_ms < self.min_speech_ms {
            return Err(VadError::InvalidConfig(
                "maxSegmentMs cannot be shorter than minSpeechMs".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechSegment {
    pub end_sample: u64,
    pub peak_probability: f32,
    pub samples: Vec<f32>,
    pub start_sample: u64,
}

impl SpeechSegment {
    pub fn duration_ms(&self) -> u64 {
        (self.end_sample - self.start_sample).saturating_mul(1_000) / u64::from(TARGET_SAMPLE_RATE)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct VadOutput {
    pub latest_probability: f32,
    pub segments: Vec<SpeechSegment>,
}

pub struct VoiceActivitySegmenter {
    detector: VoiceActivityDetector,
    latest_probability: f32,
    pending: VecDeque<f32>,
    processed_samples: u64,
}

impl VoiceActivitySegmenter {
    pub fn new(config: VadConfig, model_path: &Path, threads: u16) -> Result<Self, VadError> {
        config.validate()?;
        let model = model_path
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| VadError::NonUnicodePath(model_path.to_path_buf()))?;
        let detector_config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(model),
                threshold: config.activation_threshold,
                min_silence_duration: config.min_silence_ms as f32 / 1_000.0,
                min_speech_duration: config.min_speech_ms as f32 / 1_000.0,
                window_size: VAD_FRAME_SAMPLES as i32,
                max_speech_duration: config.max_segment_ms as f32 / 1_000.0,
            },
            sample_rate: TARGET_SAMPLE_RATE as i32,
            num_threads: i32::from(threads.clamp(1, 16)),
            provider: Some("cpu".to_owned()),
            ..VadModelConfig::default()
        };
        let buffer_seconds = (config.max_segment_ms as f32 / 1_000.0 + 5.0).max(10.0);
        let detector = VoiceActivityDetector::create(&detector_config, buffer_seconds)
            .ok_or(VadError::CreateDetector)?;
        Ok(Self {
            detector,
            latest_probability: 0.0,
            pending: VecDeque::with_capacity(VAD_FRAME_SAMPLES * 2),
            processed_samples: 0,
        })
    }

    pub fn push(&mut self, samples: &[f32]) -> VadOutput {
        self.pending.extend(samples.iter().copied().map(sanitize));
        let mut segments = Vec::new();
        while self.pending.len() >= VAD_FRAME_SAMPLES {
            let frame: Vec<f32> = self.pending.drain(..VAD_FRAME_SAMPLES).collect();
            self.detector.accept_waveform(&frame);
            self.processed_samples = self.processed_samples.saturating_add(frame.len() as u64);
            self.latest_probability = if self.detector.detected() { 1.0 } else { 0.0 };
            segments.extend(self.drain_segments());
        }
        VadOutput {
            latest_probability: self.latest_probability,
            segments,
        }
    }

    pub fn finish(&mut self) -> VadOutput {
        if !self.pending.is_empty() {
            let valid = self.pending.len();
            let mut frame = vec![0.0; VAD_FRAME_SAMPLES];
            for item in frame.iter_mut().take(valid) {
                *item = self.pending.pop_front().unwrap_or_default();
            }
            self.detector.accept_waveform(&frame);
            self.processed_samples = self.processed_samples.saturating_add(valid as u64);
        }
        self.detector.flush();
        self.latest_probability = 0.0;
        VadOutput {
            latest_probability: 0.0,
            segments: self.drain_segments(),
        }
    }

    pub fn reset(&mut self) {
        self.detector.reset();
        self.latest_probability = 0.0;
        self.pending.clear();
        self.processed_samples = 0;
    }

    pub fn processed_samples(&self) -> u64 {
        self.processed_samples
    }

    fn drain_segments(&self) -> Vec<SpeechSegment> {
        let mut output = Vec::new();
        while let Some(segment) = self.detector.front() {
            let start = u64::try_from(segment.start()).unwrap_or_default();
            let available = self.processed_samples.saturating_sub(start) as usize;
            let samples = segment.samples()[..segment.samples().len().min(available)].to_vec();
            let end = start.saturating_add(samples.len() as u64);
            if !samples.is_empty() {
                output.push(SpeechSegment {
                    end_sample: end,
                    peak_probability: 1.0,
                    samples,
                    start_sample: start,
                });
            }
            drop(segment);
            self.detector.pop();
        }
        output
    }
}

fn sanitize(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VadError {
    #[error("sherpa-onnx could not create the Silero VAD detector")]
    CreateDetector,
    #[error("invalid VAD configuration: {0}")]
    InvalidConfig(String),
    #[error("VAD model path is not valid Unicode: {0}")]
    NonUnicodePath(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_threshold_and_durations() {
        let mut config = VadConfig {
            activation_threshold: 1.1,
            ..VadConfig::default()
        };
        assert!(config.validate().is_err());
        config = VadConfig {
            max_segment_ms: 100,
            min_speech_ms: 200,
            ..VadConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn reports_segment_duration_in_milliseconds() {
        let segment = SpeechSegment {
            start_sample: 8_000,
            end_sample: 16_000,
            peak_probability: 1.0,
            samples: vec![],
        };
        assert_eq!(segment.duration_ms(), 500);
    }
}
