use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use thiserror::Error;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;
const INPUT_CHUNK_FRAMES: usize = 1_024;

pub struct StreamResampler {
    input_rate: u32,
    output_rate: u32,
    pending: Vec<f32>,
    resampler: Option<Fft<f32>>,
    delay_remaining: usize,
    total_input_frames: u64,
    total_output_frames: u64,
}

impl StreamResampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Result<Self, ResampleStreamError> {
        if input_rate == 0 || output_rate == 0 {
            return Err(ResampleStreamError::InvalidSampleRate);
        }
        let resampler = if input_rate == output_rate {
            None
        } else {
            Some(
                Fft::<f32>::new(
                    input_rate as usize,
                    output_rate as usize,
                    INPUT_CHUNK_FRAMES,
                    1,
                    FixedSync::Input,
                )
                .map_err(|error| ResampleStreamError::Backend(error.to_string()))?,
            )
        };
        let delay_remaining = resampler
            .as_ref()
            .map(Resampler::output_delay)
            .unwrap_or_default();
        Ok(Self {
            input_rate,
            output_rate,
            pending: Vec::with_capacity(INPUT_CHUNK_FRAMES * 2),
            resampler,
            delay_remaining,
            total_input_frames: 0,
            total_output_frames: 0,
        })
    }

    pub fn to_speech_rate(input_rate: u32) -> Result<Self, ResampleStreamError> {
        Self::new(input_rate, TARGET_SAMPLE_RATE)
    }

    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<f32>, ResampleStreamError> {
        self.total_input_frames = self.total_input_frames.saturating_add(samples.len() as u64);
        let Some(resampler) = self.resampler.as_mut() else {
            self.total_output_frames = self
                .total_output_frames
                .saturating_add(samples.len() as u64);
            return Ok(samples.to_vec());
        };

        self.pending.extend_from_slice(samples);
        let mut consumed = 0;
        let mut output = Vec::new();
        loop {
            let needed = resampler.input_frames_next();
            if self.pending.len() - consumed < needed {
                break;
            }
            let input = InterleavedSlice::new(&self.pending[consumed..], 1, needed)
                .map_err(|error| ResampleStreamError::Backend(error.to_string()))?;
            let chunk = resampler
                .process(&input, None)
                .map_err(|error| ResampleStreamError::Backend(error.to_string()))?
                .take_data();
            append_without_delay(&mut output, chunk, &mut self.delay_remaining);
            consumed += needed;
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        self.total_output_frames = self.total_output_frames.saturating_add(output.len() as u64);
        Ok(output)
    }

    pub fn finish(&mut self) -> Result<Vec<f32>, ResampleStreamError> {
        let Some(resampler) = self.resampler.as_mut() else {
            return Ok(Vec::new());
        };
        let needed = resampler.input_frames_next();
        let available = self.pending.len();
        let mut padded = vec![0.0; needed];
        padded[..available].copy_from_slice(&self.pending);
        self.pending.clear();
        let input = InterleavedSlice::new(&padded, 1, needed)
            .map_err(|error| ResampleStreamError::Backend(error.to_string()))?;
        let indexing = rubato::Indexing::new().partial_len(available);
        let chunk = resampler
            .process(&input, Some(&indexing))
            .map_err(|error| ResampleStreamError::Backend(error.to_string()))?
            .take_data();
        let mut output = Vec::new();
        append_without_delay(&mut output, chunk, &mut self.delay_remaining);

        let expected_total = self
            .total_input_frames
            .saturating_mul(u64::from(self.output_rate))
            .div_ceil(u64::from(self.input_rate));
        let zero_indexing = rubato::Indexing::new().partial_len(0);
        while self.total_output_frames + (output.len() as u64) < expected_total {
            let chunk = resampler
                .process(&input, Some(&zero_indexing))
                .map_err(|error| ResampleStreamError::Backend(error.to_string()))?
                .take_data();
            append_without_delay(&mut output, chunk, &mut self.delay_remaining);
        }
        let remaining = expected_total.saturating_sub(self.total_output_frames) as usize;
        output.truncate(remaining);
        self.total_output_frames = self.total_output_frames.saturating_add(output.len() as u64);
        Ok(output)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResampleStreamError {
    #[error("sample rates must be greater than zero")]
    InvalidSampleRate,
    #[error("resampler failed: {0}")]
    Backend(String),
}

fn append_without_delay(output: &mut Vec<f32>, chunk: Vec<f32>, delay_remaining: &mut usize) {
    let skipped = (*delay_remaining).min(chunk.len());
    *delay_remaining -= skipped;
    output.extend_from_slice(&chunk[skipped..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_matching_sample_rate() {
        let mut resampler = StreamResampler::to_speech_rate(TARGET_SAMPLE_RATE).unwrap();
        let output = resampler.push(&[0.1, -0.2, 0.3]).unwrap();
        assert_eq!(output, vec![0.1, -0.2, 0.3]);
        assert!(resampler.finish().unwrap().is_empty());
    }

    #[test]
    fn converts_48khz_to_16khz_with_expected_duration() {
        let mut resampler = StreamResampler::to_speech_rate(48_000).unwrap();
        let input: Vec<f32> = (0..48_000)
            .map(|index| (index as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin())
            .collect();
        let mut output = Vec::new();
        for chunk in input.chunks(777) {
            output.extend(resampler.push(chunk).unwrap());
        }
        output.extend(resampler.finish().unwrap());

        assert!((15_999..=16_001).contains(&output.len()));
        assert!(output.iter().all(|sample| sample.is_finite()));
    }
}
