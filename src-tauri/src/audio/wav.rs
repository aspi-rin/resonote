use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use thiserror::Error;

const HEADER_SIZE: u64 = 44;
const BITS_PER_SAMPLE: u16 = 16;

pub struct RecoverableWavWriter {
    data_bytes: u64,
    path: PathBuf,
    sample_rate: u32,
    writer: BufWriter<File>,
}

impl RecoverableWavWriter {
    pub fn create(path: PathBuf, sample_rate: u32) -> Result<Self, WavError> {
        if sample_rate == 0 {
            return Err(WavError::InvalidSampleRate);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&wav_header(sample_rate, 0))?;
        Ok(Self {
            data_bytes: 0,
            path,
            sample_rate,
            writer,
        })
    }

    pub fn append(&mut self, samples: &[f32]) -> Result<(), WavError> {
        let additional_bytes = samples.len() as u64 * 2;
        let next_size = self.data_bytes.saturating_add(additional_bytes);
        if next_size > u64::from(u32::MAX - 36) {
            return Err(WavError::FileTooLarge);
        }
        let mut encoded = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            let finite = if sample.is_finite() { *sample } else { 0.0 };
            let value = (finite.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
            encoded.extend_from_slice(&value.to_le_bytes());
        }
        self.writer.write_all(&encoded)?;
        self.data_bytes = next_size;
        Ok(())
    }

    pub fn checkpoint(&mut self) -> Result<(), WavError> {
        self.writer.flush()?;
        self.writer.seek(SeekFrom::Start(0))?;
        self.writer
            .write_all(&wav_header(self.sample_rate, self.data_bytes as u32))?;
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        self.writer.seek(SeekFrom::End(0))?;
        Ok(())
    }

    pub fn finalize(mut self) -> Result<WavSummary, WavError> {
        self.checkpoint()?;
        self.writer.get_ref().sync_all()?;
        Ok(WavSummary {
            data_bytes: self.data_bytes,
            path: self.path.clone(),
            sample_rate: self.sample_rate,
        })
    }
}

impl Drop for RecoverableWavWriter {
    fn drop(&mut self) {
        if let Err(error) = self.checkpoint() {
            tracing::warn!(?error, path = %self.path.display(), "failed to checkpoint WAV header");
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavSummary {
    pub data_bytes: u64,
    pub path: PathBuf,
    pub sample_rate: u32,
}

impl WavSummary {
    pub fn sample_count(&self) -> u64 {
        self.data_bytes / 2
    }

    pub fn duration_millis(&self) -> u64 {
        self.sample_count().saturating_mul(1_000) / u64::from(self.sample_rate)
    }
}

#[derive(Debug, Error)]
pub enum WavError {
    #[error("WAV segment exceeded the RIFF 32-bit size limit")]
    FileTooLarge,
    #[error("sample rate must be greater than zero")]
    InvalidSampleRate,
    #[error("failed to access WAV file: {0}")]
    Io(#[from] std::io::Error),
    #[error("file is too short to be a recoverable WAV file")]
    Truncated,
}

pub fn repair_wav_header(path: &Path, sample_rate: u32) -> Result<WavSummary, WavError> {
    if sample_rate == 0 {
        return Err(WavError::InvalidSampleRate);
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let length = file.metadata()?.len();
    if length < HEADER_SIZE {
        return Err(WavError::Truncated);
    }
    let data_bytes = length - HEADER_SIZE;
    if data_bytes > u64::from(u32::MAX - 36) {
        return Err(WavError::FileTooLarge);
    }
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&wav_header(sample_rate, data_bytes as u32))?;
    file.sync_all()?;
    Ok(WavSummary {
        data_bytes,
        path: path.to_path_buf(),
        sample_rate,
    })
}

fn wav_header(sample_rate: u32, data_bytes: u32) -> [u8; HEADER_SIZE as usize] {
    let mut header = [0_u8; HEADER_SIZE as usize];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(36 + data_bytes).to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16_u32.to_le_bytes());
    header[20..22].copy_from_slice(&1_u16.to_le_bytes());
    header[22..24].copy_from_slice(&1_u16.to_le_bytes());
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    header[28..32].copy_from_slice(&(sample_rate * 2).to_le_bytes());
    header[32..34].copy_from_slice(&2_u16.to_le_bytes());
    header[34..36].copy_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn finalizes_header_and_duration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.wav");
        let mut writer = RecoverableWavWriter::create(path.clone(), 16_000).unwrap();
        writer.append(&vec![0.25; 16_000]).unwrap();
        let summary = writer.finalize().unwrap();

        assert_eq!(summary.duration_millis(), 1_000);
        let mut bytes = Vec::new();
        File::open(path).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            32_000
        );
    }

    #[test]
    fn repairs_a_stale_header_after_interruption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("interrupted.wav");
        let mut file = File::create(&path).unwrap();
        file.write_all(&wav_header(16_000, 0)).unwrap();
        file.write_all(&[0_u8; 320]).unwrap();
        drop(file);

        let summary = repair_wav_header(&path, 16_000).unwrap();
        assert_eq!(summary.data_bytes, 320);
        let mut bytes = Vec::new();
        File::open(path).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 320);
    }
}
