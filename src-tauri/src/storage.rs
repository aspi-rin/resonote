use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Datelike, Local, Utc};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    audio::{
        FlacError, RecoverableWavWriter, StreamingFlacWriter, TARGET_SAMPLE_RATE, WavError,
        repair_wav_header,
    },
    settings::{AudioFormat, AudioSettings, AudioSourceMode},
};

const MANIFEST_NAME: &str = "session.json";
const WAV_HEADER_BYTES: u64 = 44;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArchiveStatus {
    Recording,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentManifest {
    pub duration_ms: u64,
    pub file_name: String,
    pub finalized: bool,
    pub index: u32,
    pub sample_count: u64,
    pub started_offset_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionManifest {
    pub audio_format: AudioFormat,
    pub audio_source: AudioSourceMode,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub sample_rate: u32,
    pub schema_version: u32,
    pub segment_minutes: u32,
    pub segments: Vec<SegmentManifest>,
    pub session_id: String,
    pub started_at: DateTime<Utc>,
    pub status: ArchiveStatus,
}

pub struct RecordingArchive {
    active: Option<ActiveSegment>,
    manifest: SessionManifest,
    manifest_path: PathBuf,
    segment_sample_limit: u64,
    session_dir: PathBuf,
}

impl RecordingArchive {
    pub fn create(root: &Path, settings: &AudioSettings) -> Result<Self, StorageError> {
        let segment_sample_limit = u64::from(settings.segment_minutes)
            .checked_mul(60)
            .and_then(|seconds| seconds.checked_mul(u64::from(TARGET_SAMPLE_RATE)))
            .ok_or(StorageError::InvalidSegmentDuration)?;
        Self::create_inner(root, settings, segment_sample_limit)
    }

    fn create_inner(
        root: &Path,
        settings: &AudioSettings,
        segment_sample_limit: u64,
    ) -> Result<Self, StorageError> {
        if segment_sample_limit == 0 {
            return Err(StorageError::InvalidSegmentDuration);
        }
        let started_at = Utc::now();
        let local_date = Local::now();
        let session_id = format!(
            "{}-{}",
            started_at.format("%Y%m%dT%H%M%S%.3fZ"),
            Uuid::new_v4().simple()
        );
        let session_dir = root
            .join(format!("{:04}", local_date.year()))
            .join(format!("{:02}", local_date.month()))
            .join(format!("{:02}", local_date.day()))
            .join(&session_id);
        fs::create_dir_all(&session_dir)?;
        let manifest_path = session_dir.join(MANIFEST_NAME);
        let manifest = SessionManifest {
            audio_format: settings.format,
            audio_source: settings.source,
            completed_at: None,
            last_error: None,
            sample_rate: TARGET_SAMPLE_RATE,
            schema_version: 1,
            segment_minutes: settings.segment_minutes,
            segments: Vec::new(),
            session_id,
            started_at,
            status: ArchiveStatus::Recording,
        };
        save_manifest(&manifest_path, &manifest)?;
        Ok(Self {
            active: None,
            manifest,
            manifest_path,
            segment_sample_limit,
            session_dir,
        })
    }

    pub fn append(&mut self, mut samples: &[f32]) -> Result<(), StorageError> {
        while !samples.is_empty() {
            if self.active.is_none() {
                self.open_segment()?;
            }
            let active = self.active.as_mut().expect("segment was just opened");
            let remaining = self.segment_sample_limit - active.sample_count;
            let take = usize::try_from(remaining.min(samples.len() as u64))
                .map_err(|_| StorageError::InvalidSegmentDuration)?;
            active.writer.append(&samples[..take])?;
            active.sample_count += take as u64;
            samples = &samples[take..];
            if active.sample_count == self.segment_sample_limit {
                self.finalize_active_segment()?;
            }
        }
        Ok(())
    }

    pub fn checkpoint(&mut self) -> Result<(), StorageError> {
        if let Some(active) = self.active.as_mut() {
            active.writer.checkpoint()?;
            self.update_active_manifest();
        }
        save_manifest(&self.manifest_path, &self.manifest)
    }

    pub fn include_source(&mut self, source: AudioSourceMode) {
        if self.manifest.audio_source != source {
            self.manifest.audio_source = AudioSourceMode::Mixed;
        }
    }

    pub fn complete(mut self) -> Result<SessionManifest, StorageError> {
        self.finalize_active_segment()?;
        self.manifest.status = ArchiveStatus::Completed;
        self.manifest.completed_at = Some(Utc::now());
        save_manifest(&self.manifest_path, &self.manifest)?;
        Ok(self.manifest.clone())
    }

    pub fn fail(mut self, error: impl Into<String>) -> Result<SessionManifest, StorageError> {
        if self.active.is_some() {
            self.checkpoint()?;
        }
        self.manifest.status = ArchiveStatus::Failed;
        self.manifest.completed_at = Some(Utc::now());
        self.manifest.last_error = Some(error.into());
        save_manifest(&self.manifest_path, &self.manifest)?;
        Ok(self.manifest.clone())
    }

    pub fn manifest(&self) -> &SessionManifest {
        &self.manifest
    }

    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    fn open_segment(&mut self) -> Result<(), StorageError> {
        let index = u32::try_from(self.manifest.segments.len())
            .map_err(|_| StorageError::TooManySegments)?;
        let extension = match self.manifest.audio_format {
            AudioFormat::Flac => "flac",
            AudioFormat::Wav => "wav",
        };
        let file_name = format!("audio-{index:04}.{extension}");
        let path = self.session_dir.join(&file_name);
        let writer = match self.manifest.audio_format {
            AudioFormat::Flac => {
                AudioWriter::Flac(StreamingFlacWriter::create(path, TARGET_SAMPLE_RATE)?)
            }
            AudioFormat::Wav => {
                AudioWriter::Wav(RecoverableWavWriter::create(path, TARGET_SAMPLE_RATE)?)
            }
        };
        let started_offset_ms = self
            .manifest
            .segments
            .iter()
            .map(|segment| segment.sample_count)
            .sum::<u64>()
            .saturating_mul(1_000)
            / u64::from(TARGET_SAMPLE_RATE);
        self.manifest.segments.push(SegmentManifest {
            duration_ms: 0,
            file_name: file_name.clone(),
            finalized: false,
            index,
            sample_count: 0,
            started_offset_ms,
        });
        self.active = Some(ActiveSegment {
            index,
            sample_count: 0,
            writer,
        });
        save_manifest(&self.manifest_path, &self.manifest)
    }

    fn update_active_manifest(&mut self) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let segment = &mut self.manifest.segments[active.index as usize];
        segment.sample_count = active.sample_count;
        segment.duration_ms =
            active.sample_count.saturating_mul(1_000) / u64::from(TARGET_SAMPLE_RATE);
    }

    fn finalize_active_segment(&mut self) -> Result<(), StorageError> {
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        let index = active.index as usize;
        let sample_count = active.sample_count;
        active.writer.finalize()?;
        let segment = &mut self.manifest.segments[index];
        segment.sample_count = sample_count;
        segment.duration_ms = sample_count.saturating_mul(1_000) / u64::from(TARGET_SAMPLE_RATE);
        segment.finalized = true;
        save_manifest(&self.manifest_path, &self.manifest)
    }
}

struct ActiveSegment {
    index: u32,
    sample_count: u64,
    writer: AudioWriter,
}

enum AudioWriter {
    Flac(StreamingFlacWriter),
    Wav(RecoverableWavWriter),
}

impl AudioWriter {
    fn append(&mut self, samples: &[f32]) -> Result<(), StorageError> {
        match self {
            Self::Flac(writer) => writer.append(samples)?,
            Self::Wav(writer) => writer.append(samples)?,
        }
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<(), StorageError> {
        match self {
            Self::Flac(writer) => writer.checkpoint()?,
            Self::Wav(writer) => writer.checkpoint()?,
        }
        Ok(())
    }

    fn finalize(self) -> Result<(), StorageError> {
        match self {
            Self::Flac(writer) => {
                writer.finalize()?;
            }
            Self::Wav(writer) => {
                writer.finalize()?;
            }
        }
        Ok(())
    }
}

pub fn recover_interrupted_sessions(root: &Path) -> Result<Vec<SessionManifest>, StorageError> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    collect_manifest_paths(root, 0, &mut paths)?;
    let mut recovered = Vec::new();
    for path in paths {
        let mut manifest: SessionManifest = serde_json::from_slice(&fs::read(&path)?)?;
        if manifest.status != ArchiveStatus::Recording {
            continue;
        }
        if let Some(segment) = manifest.segments.last_mut()
            && !segment.finalized
            && safe_file_name(&segment.file_name)
            && manifest.audio_format == AudioFormat::Wav
        {
            let audio_path = path.parent().unwrap_or(root).join(&segment.file_name);
            if audio_path
                .metadata()
                .is_ok_and(|metadata| metadata.len() >= WAV_HEADER_BYTES)
            {
                let summary = repair_wav_header(&audio_path, manifest.sample_rate)?;
                segment.sample_count = summary.sample_count();
                segment.duration_ms = summary.duration_millis();
            }
        }
        manifest.status = ArchiveStatus::Interrupted;
        manifest.completed_at = Some(Utc::now());
        save_manifest(&path, &manifest)?;
        recovered.push(manifest);
    }
    Ok(recovered)
}

fn collect_manifest_paths(
    directory: &Path,
    depth: usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    if depth > 5 {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_manifest_paths(&entry.path(), depth + 1, output)?;
        } else if entry.file_name() == MANIFEST_NAME {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn safe_file_name(value: &str) -> bool {
    let path = Path::new(value);
    path.components().count() == 1 && matches!(path.components().next(), Some(Component::Normal(_)))
}

fn save_manifest(path: &Path, manifest: &SessionManifest) -> Result<(), StorageError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, manifest)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error(transparent)]
    Flac(#[from] FlacError),
    #[error("failed to access recording archive: {0}")]
    Io(#[from] std::io::Error),
    #[error("segment duration is invalid")]
    InvalidSegmentDuration,
    #[error("failed to parse recording manifest: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to persist recording manifest: {0}")]
    Persist(#[from] tempfile::PersistError),
    #[error("recording contains too many segments")]
    TooManySegments,
    #[error(transparent)]
    Wav(#[from] WavError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use claxon::FlacReader;

    #[test]
    fn splits_audio_at_exact_sample_boundaries() {
        let directory = tempfile::tempdir().unwrap();
        let settings = AudioSettings::default();
        let mut archive =
            RecordingArchive::create_inner(directory.path(), &settings, 1_000).unwrap();
        archive.append(&vec![0.125; 2_501]).unwrap();
        let session_dir = archive.session_dir().to_path_buf();
        let manifest = archive.complete().unwrap();

        assert_eq!(manifest.status, ArchiveStatus::Completed);
        assert_eq!(
            manifest
                .segments
                .iter()
                .map(|segment| segment.sample_count)
                .collect::<Vec<_>>(),
            vec![1_000, 1_000, 501]
        );
        for segment in &manifest.segments {
            assert!(segment.finalized);
            let mut reader = FlacReader::open(session_dir.join(&segment.file_name)).unwrap();
            assert_eq!(reader.samples().count(), segment.sample_count as usize);
        }
    }

    #[test]
    fn recovers_an_interrupted_wav_manifest_and_header() {
        let directory = tempfile::tempdir().unwrap();
        let settings = AudioSettings {
            format: AudioFormat::Wav,
            ..AudioSettings::default()
        };
        let mut archive =
            RecordingArchive::create_inner(directory.path(), &settings, 10_000).unwrap();
        archive.append(&vec![0.25; 1_234]).unwrap();
        archive.checkpoint().unwrap();
        let session_dir = archive.session_dir().to_path_buf();
        std::mem::forget(archive);

        let recovered = recover_interrupted_sessions(directory.path()).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].status, ArchiveStatus::Interrupted);
        assert_eq!(recovered[0].segments[0].sample_count, 1_234);

        let bytes = fs::read(session_dir.join("audio-0000.wav")).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 2_468);
    }

    #[test]
    fn rejects_manifest_path_traversal_names() {
        assert!(safe_file_name("audio-0000.wav"));
        assert!(!safe_file_name("../outside.wav"));
        assert!(!safe_file_name("folder/audio.wav"));
    }
}
