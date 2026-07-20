use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use reqwest::{StatusCode, blocking::Client, header::RANGE};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model_package;

#[path = "model_catalog.rs"]
mod catalog;
#[path = "model_files.rs"]
mod files;

pub use catalog::model_catalog;
use catalog::model_spec;
use files::{
    file_matches, missing_status, partial_downloaded_bytes, partial_path, replace_with,
    save_marker, verify_sha256,
};

const CATALOG_MODEL_ID: &str = "qwen3-asr-0.6b-int8";
const CATALOG_REVISION: &str = "sherpa-onnx-1.13.4-qwen3-2026-03-25";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

pub type ModelStatusObserver = Arc<dyn Fn(ModelDownloadStatus) + Send + Sync + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactKind {
    ModelArchive,
    VadModel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSpec {
    pub file_name: String,
    pub kind: ArtifactKind,
    pub sha256: String,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPackageSpec {
    pub artifacts: Vec<ArtifactSpec>,
    pub display_name: String,
    pub id: String,
    pub revision: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ModelDownloadPhase {
    Missing,
    Downloading,
    Downloaded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDownloadStatus {
    pub current_file: Option<String>,
    pub downloaded_bytes: u64,
    pub error: Option<String>,
    pub model_id: String,
    pub phase: ModelDownloadPhase,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledModel {
    pub conv_frontend: PathBuf,
    pub decoder: PathBuf,
    pub encoder: PathBuf,
    pub model_id: String,
    pub revision: String,
    pub tokenizer: PathBuf,
    pub vad_model: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallMarker {
    artifacts: Vec<InstalledArtifact>,
    installed_at: chrono::DateTime<chrono::Utc>,
    model_id: String,
    revision: String,
    schema_version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstalledArtifact {
    file_name: String,
    sha256: String,
    size: u64,
}

pub struct ModelManager {
    cancel: AtomicBool,
    client: Client,
    install_lock: Mutex<()>,
    observer: ModelStatusObserver,
    root: PathBuf,
    status: RwLock<ModelDownloadStatus>,
}

impl ModelManager {
    pub fn new(root: PathBuf) -> Result<Self, ModelError> {
        Self::with_observer(root, Arc::new(|_| {}))
    }

    pub fn with_observer(root: PathBuf, observer: ModelStatusObserver) -> Result<Self, ModelError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .user_agent("Resonote/0.1 model-manager")
            .build()?;
        Ok(Self {
            cancel: AtomicBool::new(false),
            client,
            install_lock: Mutex::new(()),
            observer,
            root,
            status: RwLock::new(missing_status(CATALOG_MODEL_ID, 0)),
        })
    }

    pub fn status(&self, model_id: &str) -> Result<ModelDownloadStatus, ModelError> {
        let spec = model_spec(model_id)?;
        let current = self
            .status
            .read()
            .unwrap_or_else(|item| item.into_inner())
            .clone();
        if current.model_id == model_id && current.phase == ModelDownloadPhase::Downloading {
            return Ok(current);
        }
        let total = spec.artifacts.iter().map(|item| item.size).sum();
        let installed = self.read_valid_marker(&spec)?.is_some();
        Ok(ModelDownloadStatus {
            current_file: None,
            downloaded_bytes: if installed {
                total
            } else {
                partial_downloaded_bytes(&self.package_dir(&spec), &spec)
            },
            error: None,
            model_id: model_id.to_owned(),
            phase: if installed {
                ModelDownloadPhase::Downloaded
            } else {
                ModelDownloadPhase::Missing
            },
            total_bytes: total,
        })
    }

    pub fn install(&self, model_id: &str) -> Result<ModelDownloadStatus, ModelError> {
        let _guard = self
            .install_lock
            .try_lock()
            .map_err(|_| ModelError::InstallInProgress)?;
        let spec = model_spec(model_id)?;
        self.install_package(&spec)
    }

    pub fn installed_model(&self, model_id: &str) -> Result<InstalledModel, ModelError> {
        let spec = model_spec(model_id)?;
        if self.read_valid_marker(&spec)?.is_none() {
            return Err(ModelError::NotInstalled(model_id.to_owned()));
        }
        let package = self.package_dir(&spec);
        let model = model_package::model_directory(&package);
        Ok(InstalledModel {
            conv_frontend: model.join("conv_frontend.onnx"),
            decoder: model.join("decoder.int8.onnx"),
            encoder: model.join("encoder.int8.onnx"),
            model_id: spec.id,
            revision: spec.revision,
            tokenizer: model.join("tokenizer"),
            vad_model: package.join("silero_vad.onnx"),
        })
    }

    pub fn cancel_install(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    fn install_package(&self, spec: &ModelPackageSpec) -> Result<ModelDownloadStatus, ModelError> {
        self.cancel.store(false, Ordering::Release);
        let package = self.package_dir(spec);
        fs::create_dir_all(&package)?;
        let total = spec.artifacts.iter().map(|item| item.size).sum();
        let mut completed = 0;
        for artifact in &spec.artifacts {
            if self.cancel.load(Ordering::Acquire) {
                return self.cancelled(spec, completed, total);
            }
            let final_path = package.join(&artifact.file_name);
            let extracted =
                artifact.kind == ArtifactKind::ModelArchive && model_package::is_ready(&package);
            if extracted || file_matches(&final_path, artifact)? {
                completed += artifact.size;
                continue;
            }
            self.publish(ModelDownloadStatus {
                current_file: Some(artifact.file_name.clone()),
                downloaded_bytes: completed,
                error: None,
                model_id: spec.id.clone(),
                phase: ModelDownloadPhase::Downloading,
                total_bytes: total,
            });
            if let Err(error) =
                self.download_artifact(artifact, &final_path, completed, total, &spec.id)
            {
                if matches!(error, ModelError::Cancelled) {
                    return self.cancelled(spec, partial_downloaded_bytes(&package, spec), total);
                }
                self.publish(ModelDownloadStatus {
                    current_file: Some(artifact.file_name.clone()),
                    downloaded_bytes: completed,
                    error: Some(error.to_string()),
                    model_id: spec.id.clone(),
                    phase: ModelDownloadPhase::Failed,
                    total_bytes: total,
                });
                return Err(error);
            }
            completed += artifact.size;
        }
        let archive = spec
            .artifacts
            .iter()
            .find(|item| item.kind == ArtifactKind::ModelArchive)
            .ok_or(ModelError::IncompleteCatalog)?;
        if !model_package::is_ready(&package) {
            model_package::prepare(&package, &package.join(&archive.file_name))?;
        }
        let archive_path = package.join(&archive.file_name);
        if archive_path.exists() {
            fs::remove_file(archive_path)?;
        }
        save_marker(&package.join("install.json"), spec)?;
        let status = ModelDownloadStatus {
            current_file: None,
            downloaded_bytes: total,
            error: None,
            model_id: spec.id.clone(),
            phase: ModelDownloadPhase::Downloaded,
            total_bytes: total,
        };
        self.publish(status.clone());
        Ok(status)
    }

    fn download_artifact(
        &self,
        artifact: &ArtifactSpec,
        final_path: &Path,
        completed: u64,
        total: u64,
        model_id: &str,
    ) -> Result<(), ModelError> {
        let partial = partial_path(final_path);
        let mut existing = partial.metadata().map_or(0, |item| item.len());
        if existing > artifact.size {
            OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&partial)?;
            existing = 0;
        }
        if existing == artifact.size {
            match verify_sha256(&partial, &artifact.sha256) {
                Ok(()) => {
                    replace_with(final_path, &partial)?;
                    return Ok(());
                }
                Err(ModelError::ChecksumMismatch { .. }) => {
                    OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(&partial)?;
                    existing = 0;
                }
                Err(error) => return Err(error),
            }
        }
        let mut request = self.client.get(&artifact.url);
        if existing > 0 {
            request = request.header(RANGE, format!("bytes={existing}-"));
        }
        let mut response = request.send()?.error_for_status()?;
        let resumed = existing > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
        if existing > 0 && !resumed {
            existing = 0;
        }
        let mut output = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(!resumed)
            .open(&partial)?;
        if resumed {
            output.seek(SeekFrom::End(0))?;
        }
        let mut downloaded = existing;
        let mut buffer = [0_u8; 64 * 1_024];
        let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
        loop {
            if self.cancel.load(Ordering::Acquire) {
                output.sync_data()?;
                return Err(ModelError::Cancelled);
            }
            let count = response.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            downloaded = downloaded.saturating_add(count as u64);
            if downloaded > artifact.size {
                return Err(ModelError::SizeMismatch {
                    actual: downloaded,
                    expected: artifact.size,
                    file: artifact.file_name.clone(),
                });
            }
            output.write_all(&buffer[..count])?;
            if last_progress.elapsed() >= PROGRESS_INTERVAL {
                self.publish(ModelDownloadStatus {
                    current_file: Some(artifact.file_name.clone()),
                    downloaded_bytes: completed + downloaded,
                    error: None,
                    model_id: model_id.to_owned(),
                    phase: ModelDownloadPhase::Downloading,
                    total_bytes: total,
                });
                last_progress = Instant::now();
            }
        }
        output.sync_all()?;
        drop(output);
        if downloaded != artifact.size {
            return Err(ModelError::SizeMismatch {
                actual: downloaded,
                expected: artifact.size,
                file: artifact.file_name.clone(),
            });
        }
        verify_sha256(&partial, &artifact.sha256)?;
        replace_with(final_path, &partial)
    }

    fn read_valid_marker(
        &self,
        spec: &ModelPackageSpec,
    ) -> Result<Option<InstallMarker>, ModelError> {
        let package = self.package_dir(spec);
        let path = package.join("install.json");
        if !path.exists() {
            return Ok(None);
        }
        let marker: InstallMarker = serde_json::from_slice(&fs::read(path)?)?;
        if marker.model_id != spec.id
            || marker.revision != spec.revision
            || marker.artifacts.len() != spec.artifacts.len()
            || !model_package::is_ready(&package)
        {
            return Ok(None);
        }
        for artifact in &spec.artifacts {
            let marked = marker.artifacts.iter().any(|item| {
                item.file_name == artifact.file_name
                    && item.sha256 == artifact.sha256
                    && item.size == artifact.size
            });
            if !marked
                || (artifact.kind == ArtifactKind::VadModel
                    && !file_matches(&package.join(&artifact.file_name), artifact)?)
            {
                return Ok(None);
            }
        }
        Ok(Some(marker))
    }

    fn package_dir(&self, spec: &ModelPackageSpec) -> PathBuf {
        self.root.join(&spec.id).join(&spec.revision)
    }

    fn cancelled(
        &self,
        spec: &ModelPackageSpec,
        downloaded: u64,
        total: u64,
    ) -> Result<ModelDownloadStatus, ModelError> {
        self.publish(ModelDownloadStatus {
            current_file: None,
            downloaded_bytes: downloaded,
            error: None,
            model_id: spec.id.clone(),
            phase: ModelDownloadPhase::Cancelled,
            total_bytes: total,
        });
        Err(ModelError::Cancelled)
    }

    fn publish(&self, status: ModelDownloadStatus) {
        *self.status.write().unwrap_or_else(|item| item.into_inner()) = status.clone();
        (self.observer)(status);
    }
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("model installation was cancelled")]
    Cancelled,
    #[error("checksum mismatch for {file}: expected {expected}, received {actual}")]
    ChecksumMismatch {
        actual: String,
        expected: String,
        file: String,
    },
    #[error("model catalog is incomplete")]
    IncompleteCatalog,
    #[error("another model installation is already running")]
    InstallInProgress,
    #[error("failed to access model storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("model package is missing required asset: {0}")]
    MissingModelAsset(String),
    #[error("model '{0}' is not installed")]
    NotInstalled(String),
    #[error("failed to parse model install marker: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to persist model install marker: {0}")]
    Persist(#[from] tempfile::PersistError),
    #[error("model download failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("size mismatch for {file}: expected {expected} bytes, received {actual}")]
    SizeMismatch {
        actual: u64,
        expected: u64,
        file: String,
    },
    #[error("model archive contains an unsafe entry: {0}")]
    UnsafeArchive(String),
    #[error("unknown model '{0}'")]
    UnknownModel(String),
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
