use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct AudioBlock {
    pub channels: u16,
    pub sample_rate: u32,
    pub samples: Vec<f32>,
}

impl AudioBlock {
    pub fn new(
        samples: Vec<f32>,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, AudioBlockError> {
        if sample_rate == 0 {
            return Err(AudioBlockError::InvalidSampleRate);
        }
        if channels == 0 {
            return Err(AudioBlockError::InvalidChannelCount);
        }
        if !samples.len().is_multiple_of(usize::from(channels)) {
            return Err(AudioBlockError::IncompleteFrame {
                channels,
                samples: samples.len(),
            });
        }
        Ok(Self {
            channels,
            sample_rate,
            samples,
        })
    }

    pub fn frame_count(&self) -> usize {
        self.samples.len() / usize::from(self.channels)
    }

    pub fn into_mono(self) -> Vec<f32> {
        let channel_count = usize::from(self.channels);
        if channel_count == 1 {
            return self.samples.into_iter().map(sanitize_sample).collect();
        }
        self.samples
            .chunks_exact(channel_count)
            .map(|frame| {
                frame.iter().copied().map(sanitize_sample).sum::<f32>() / channel_count as f32
            })
            .collect()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AudioBlockError {
    #[error("audio channel count must be greater than zero")]
    InvalidChannelCount,
    #[error("audio sample rate must be greater than zero")]
    InvalidSampleRate,
    #[error("{samples} samples do not form complete frames for {channels} channels")]
    IncompleteFrame { channels: u16, samples: usize },
}

pub fn mix_mono(
    microphone: &[f32],
    system: &[f32],
    microphone_gain: f32,
    system_gain: f32,
) -> Vec<f32> {
    let frame_count = microphone.len().max(system.len());
    let mic_gain = sanitize_gain(microphone_gain);
    let output_gain = sanitize_gain(system_gain);
    (0..frame_count)
        .map(|index| {
            let mic = microphone.get(index).copied().unwrap_or_default();
            let system = system.get(index).copied().unwrap_or_default();
            (sanitize_sample(mic) * mic_gain + sanitize_sample(system) * output_gain)
                .clamp(-1.0, 1.0)
        })
        .collect()
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum = samples
        .iter()
        .copied()
        .map(sanitize_sample)
        .map(|sample| sample * sample)
        .sum::<f32>();
    (sum / samples.len() as f32).sqrt()
}

pub fn rms_db(samples: &[f32]) -> f32 {
    let value = rms(samples);
    if value <= 0.0 {
        -80.0
    } else {
        (20.0 * value.log10()).max(-80.0)
    }
}

pub fn waveform_bins(samples: &[f32], bin_count: usize) -> Vec<f32> {
    if bin_count == 0 {
        return Vec::new();
    }
    if samples.is_empty() {
        return vec![0.0; bin_count];
    }
    (0..bin_count)
        .map(|index| {
            let start = index * samples.len() / bin_count;
            let end = ((index + 1) * samples.len() / bin_count).max(start + 1);
            samples[start.min(samples.len() - 1)..end.min(samples.len())]
                .iter()
                .copied()
                .map(sanitize_sample)
                .map(f32::abs)
                .fold(0.0, f32::max)
        })
        .collect()
}

fn sanitize_sample(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn sanitize_gain(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 4.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmixes_interleaved_stereo() {
        let block = AudioBlock::new(vec![1.0, -1.0, 0.5, 0.25], 48_000, 2).unwrap();

        assert_eq!(block.frame_count(), 2);
        assert_eq!(block.into_mono(), vec![0.0, 0.375]);
    }

    #[test]
    fn rejects_partial_interleaved_frame() {
        let error = AudioBlock::new(vec![0.0, 1.0, 0.0], 48_000, 2).unwrap_err();
        assert_eq!(
            error,
            AudioBlockError::IncompleteFrame {
                channels: 2,
                samples: 3
            }
        );
    }

    #[test]
    fn mixes_sources_and_prevents_clipping() {
        let mixed = mix_mono(&[0.75, -0.5], &[0.75, -0.75], 1.0, 1.0);
        assert_eq!(mixed, vec![1.0, -1.0]);
    }

    #[test]
    fn creates_stable_waveform_bins() {
        let bins = waveform_bins(&[-0.1, 0.4, -0.8, 0.2], 2);
        assert_eq!(bins, vec![0.4, 0.8]);
        assert_eq!(waveform_bins(&[], 3), vec![0.0; 3]);
    }
}
