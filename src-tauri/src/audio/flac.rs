use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Seek, SeekFrom, Write},
    path::PathBuf,
};

use thiserror::Error;

const BLOCK_SIZE: usize = 4_096;
const BITS_PER_SAMPLE: u8 = 16;
const STREAMINFO_OFFSET: u64 = 8;

/// A low-overhead, streaming FLAC encoder for mono 16-bit PCM.
///
/// It deliberately uses FLAC's verbatim subframe type. That trades compression
/// ratio for predictable CPU and memory use while still producing a standards-
/// compliant, lossless FLAC stream with independently checksummed frames.
pub struct StreamingFlacWriter {
    finalized: bool,
    max_frame_size: u32,
    min_block_size: u16,
    min_frame_size: u32,
    path: PathBuf,
    pending: Vec<i16>,
    sample_rate: u32,
    total_samples: u64,
    writer: BufWriter<File>,
}

impl StreamingFlacWriter {
    pub fn create(path: PathBuf, sample_rate: u32) -> Result<Self, FlacError> {
        if !(1..=655_350).contains(&sample_rate) {
            return Err(FlacError::InvalidSampleRate);
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
        writer.write_all(b"fLaC")?;
        writer.write_all(&[0x80, 0, 0, 34])?;
        writer.write_all(&streaminfo(
            sample_rate,
            BLOCK_SIZE as u16,
            BLOCK_SIZE as u16,
            0,
            0,
            0,
        ))?;

        Ok(Self {
            finalized: false,
            max_frame_size: 0,
            min_block_size: BLOCK_SIZE as u16,
            min_frame_size: u32::MAX,
            path,
            pending: Vec::with_capacity(BLOCK_SIZE),
            sample_rate,
            total_samples: 0,
            writer,
        })
    }

    pub fn append(&mut self, samples: &[f32]) -> Result<(), FlacError> {
        for sample in samples {
            let finite = if sample.is_finite() { *sample } else { 0.0 };
            self.pending
                .push((finite.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16);
            if self.pending.len() == BLOCK_SIZE {
                self.write_pending_frame()?;
            }
        }
        Ok(())
    }

    /// Commits the pending samples as a checksummed frame and synchronizes the
    /// stream metadata and audio data to disk.
    pub fn checkpoint(&mut self) -> Result<(), FlacError> {
        self.write_pending_frame()?;
        self.writer.flush()?;
        self.write_streaminfo()?;
        self.writer.get_ref().sync_data()?;
        Ok(())
    }

    pub fn finalize(mut self) -> Result<FlacSummary, FlacError> {
        if self.pending.is_empty() && self.total_samples == 0 {
            return Err(FlacError::EmptyStream);
        }
        self.finish()?;
        self.finalized = true;
        Ok(FlacSummary {
            path: self.path.clone(),
            sample_rate: self.sample_rate,
            sample_count: self.total_samples,
        })
    }

    fn write_pending_frame(&mut self) -> Result<(), FlacError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let samples = std::mem::take(&mut self.pending);
        self.pending = Vec::with_capacity(BLOCK_SIZE);
        let frame = encode_verbatim_frame(self.total_samples, &samples)?;
        self.writer.write_all(&frame)?;
        self.total_samples = self
            .total_samples
            .checked_add(samples.len() as u64)
            .filter(|count| *count < (1_u64 << 36))
            .ok_or(FlacError::StreamTooLong)?;
        self.min_block_size = self.min_block_size.min(samples.len() as u16);
        let frame_size = u32::try_from(frame.len()).map_err(|_| FlacError::StreamTooLong)?;
        self.min_frame_size = self.min_frame_size.min(frame_size);
        self.max_frame_size = self.max_frame_size.max(frame_size);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), FlacError> {
        self.write_pending_frame()?;
        self.write_streaminfo()?;
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        Ok(())
    }

    fn write_streaminfo(&mut self) -> Result<(), FlacError> {
        let min_frame_size = if self.min_frame_size == u32::MAX {
            0
        } else {
            self.min_frame_size
        };
        self.writer.flush()?;
        self.writer.seek(SeekFrom::Start(STREAMINFO_OFFSET))?;
        self.writer.write_all(&streaminfo(
            self.sample_rate,
            self.min_block_size,
            BLOCK_SIZE as u16,
            min_frame_size,
            self.max_frame_size,
            self.total_samples,
        ))?;
        self.writer.flush()?;
        self.writer.seek(SeekFrom::End(0))?;
        Ok(())
    }
}

impl Drop for StreamingFlacWriter {
    fn drop(&mut self) {
        if !self.finalized && self.total_samples + self.pending.len() as u64 > 0 {
            if let Err(error) = self.finish() {
                tracing::warn!(?error, path = %self.path.display(), "failed to finalize FLAC stream");
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlacSummary {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub sample_count: u64,
}

impl FlacSummary {
    pub fn duration_millis(&self) -> u64 {
        self.sample_count.saturating_mul(1_000) / u64::from(self.sample_rate)
    }
}

#[derive(Debug, Error)]
pub enum FlacError {
    #[error("a FLAC stream must contain at least one sample")]
    EmptyStream,
    #[error("sample rate is outside FLAC's supported range")]
    InvalidSampleRate,
    #[error("failed to access FLAC file: {0}")]
    Io(#[from] std::io::Error),
    #[error("FLAC stream is too long")]
    StreamTooLong,
}

fn streaminfo(
    sample_rate: u32,
    min_block_size: u16,
    max_block_size: u16,
    min_frame_size: u32,
    max_frame_size: u32,
    total_samples: u64,
) -> [u8; 34] {
    let mut info = [0_u8; 34];
    info[0..2].copy_from_slice(&min_block_size.to_be_bytes());
    info[2..4].copy_from_slice(&max_block_size.to_be_bytes());
    put_u24(&mut info[4..7], min_frame_size);
    put_u24(&mut info[7..10], max_frame_size);
    let properties =
        (u64::from(sample_rate) << 44) | (u64::from(BITS_PER_SAMPLE - 1) << 36) | total_samples;
    info[10..18].copy_from_slice(&properties.to_be_bytes());
    // A zero MD5 field explicitly means that the signature is unavailable.
    info
}

fn put_u24(target: &mut [u8], value: u32) {
    let bytes = value.to_be_bytes();
    target.copy_from_slice(&bytes[1..4]);
}

fn encode_verbatim_frame(sample_number: u64, samples: &[i16]) -> Result<Vec<u8>, FlacError> {
    if samples.is_empty() || samples.len() > BLOCK_SIZE {
        return Err(FlacError::StreamTooLong);
    }
    let full_block = samples.len() == BLOCK_SIZE;
    let mut frame = Vec::with_capacity(samples.len() * 2 + 16);
    // Variable-blocking strategy: the UTF-8 integer is the first sample's
    // absolute index, so any checkpoint may safely end with a short frame.
    frame.extend_from_slice(&[0xff, 0xf9]);
    frame.push(if full_block { 0xc0 } else { 0x70 });
    // Mono, 16-bit samples, reserved bit zero.
    frame.push(0x08);
    encode_utf8_uint(sample_number, &mut frame)?;
    if !full_block {
        frame.extend_from_slice(&((samples.len() - 1) as u16).to_be_bytes());
    }
    frame.push(crc8(&frame));
    // Zero pad, verbatim subframe type (000001), no wasted bits.
    frame.push(0x02);
    for sample in samples {
        frame.extend_from_slice(&sample.to_be_bytes());
    }
    let checksum = crc16(&frame);
    frame.extend_from_slice(&checksum.to_be_bytes());
    Ok(frame)
}

fn encode_utf8_uint(value: u64, output: &mut Vec<u8>) -> Result<(), FlacError> {
    let length = match value {
        0..=0x7f => 1,
        0x80..=0x7ff => 2,
        0x800..=0xffff => 3,
        0x1_0000..=0x1f_ffff => 4,
        0x20_0000..=0x3ff_ffff => 5,
        0x400_0000..=0x7fff_ffff => 6,
        0x8000_0000..=0xf_ffff_ffff => 7,
        _ => return Err(FlacError::StreamTooLong),
    };
    if length == 1 {
        output.push(value as u8);
        return Ok(());
    }
    let first_payload_bits = 7 - length;
    let prefix = (!0_u8) << (8 - length);
    output.push(prefix | ((value >> (6 * (length - 1))) as u8 & ((1 << first_payload_bits) - 1)));
    for index in (0..length - 1).rev() {
        output.push(0x80 | ((value >> (6 * index)) as u8 & 0x3f));
    }
    Ok(())
}

fn crc8(bytes: &[u8]) -> u8 {
    let mut crc = 0_u8;
    for byte in bytes {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0_u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use claxon::FlacReader;

    #[test]
    fn round_trips_multiple_and_partial_frames() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stream.flac");
        let expected: Vec<i16> = (0..9_001)
            .map(|index| (((index as f32 * 0.031).sin()) * 20_000.0).round() as i16)
            .collect();
        let input: Vec<f32> = expected
            .iter()
            .map(|sample| f32::from(*sample) / f32::from(i16::MAX))
            .collect();

        let mut writer = StreamingFlacWriter::create(path.clone(), 16_000).unwrap();
        for chunk in input.chunks(317) {
            writer.append(chunk).unwrap();
        }
        let summary = writer.finalize().unwrap();
        assert_eq!(summary.sample_count, expected.len() as u64);

        let mut reader = FlacReader::open(path).unwrap();
        let info = reader.streaminfo();
        assert_eq!(info.sample_rate, 16_000);
        assert_eq!(info.channels, 1);
        assert_eq!(info.bits_per_sample, 16);
        let decoded: Vec<i32> = reader.samples().map(Result::unwrap).collect();
        assert_eq!(
            decoded,
            expected
                .iter()
                .map(|sample| i32::from(*sample))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn drop_leaves_a_decodable_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("interrupted.flac");
        {
            let mut writer = StreamingFlacWriter::create(path.clone(), 16_000).unwrap();
            writer.append(&vec![0.25; 5_000]).unwrap();
        }

        let mut reader = FlacReader::open(path).unwrap();
        assert_eq!(reader.samples().count(), 5_000);
    }
}
