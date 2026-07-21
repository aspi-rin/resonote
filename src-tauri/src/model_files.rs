use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use super::{
    ArtifactKind, ArtifactSpec, InstallMarker, InstalledArtifact, ModelDownloadPhase,
    ModelDownloadStatus, ModelError, ModelPackageSpec,
};
use crate::model_package;

pub(super) fn missing_status(model_id: &str, total: u64) -> ModelDownloadStatus {
    ModelDownloadStatus {
        current_file: None,
        downloaded_bytes: 0,
        error: None,
        model_id: model_id.to_owned(),
        phase: ModelDownloadPhase::Missing,
        total_bytes: total,
    }
}

pub(super) fn partial_downloaded_bytes(directory: &Path, spec: &ModelPackageSpec) -> u64 {
    spec.artifacts
        .iter()
        .map(|artifact| {
            if artifact.kind == ArtifactKind::ModelArchive
                && model_package::is_ready(directory, spec.model_directory, &spec.required_files)
            {
                return artifact.size;
            }
            let final_path = directory.join(&artifact.file_name);
            if final_path
                .metadata()
                .is_ok_and(|item| item.len() == artifact.size)
            {
                artifact.size
            } else {
                partial_path(&final_path)
                    .metadata()
                    .map_or(0, |item| item.len().min(artifact.size))
            }
        })
        .sum()
}

pub(super) fn partial_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "{}.partial",
        path.extension()
            .and_then(|item| item.to_str())
            .unwrap_or("download")
    ))
}

pub(super) fn file_matches(path: &Path, artifact: &ArtifactSpec) -> Result<bool, ModelError> {
    if !file_has_expected_size(path, artifact) {
        return Ok(false);
    }
    match verify_sha256(path, &artifact.sha256) {
        Ok(()) => Ok(true),
        Err(ModelError::ChecksumMismatch { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn file_has_expected_size(path: &Path, artifact: &ArtifactSpec) -> bool {
    path.metadata()
        .is_ok_and(|item| item.is_file() && item.len() == artifact.size)
}

pub(super) fn replace_with(final_path: &Path, replacement: &Path) -> Result<(), ModelError> {
    if final_path.exists() {
        fs::remove_file(final_path)?;
    }
    fs::rename(replacement, final_path)?;
    Ok(())
}

pub(super) fn verify_sha256(path: &Path, expected: &str) -> Result<(), ModelError> {
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1_024];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let actual = hex::encode(hasher.finalize());
    if actual == expected {
        Ok(())
    } else {
        Err(ModelError::ChecksumMismatch {
            actual,
            expected: expected.to_owned(),
            file: path
                .file_name()
                .and_then(|item| item.to_str())
                .unwrap_or("model artifact")
                .to_owned(),
        })
    }
}

pub(super) fn save_marker(path: &Path, spec: &ModelPackageSpec) -> Result<(), ModelError> {
    let marker = InstallMarker {
        artifacts: spec
            .artifacts
            .iter()
            .map(|item| InstalledArtifact {
                file_name: item.file_name.clone(),
                sha256: item.sha256.clone(),
                size: item.size,
            })
            .collect(),
        installed_at: chrono::Utc::now(),
        model_id: spec.id.clone(),
        revision: spec.revision.clone(),
        schema_version: 2,
    };
    let mut temporary = NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))?;
    serde_json::to_writer_pretty(&mut temporary, &marker)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}
