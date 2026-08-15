use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::RwLock,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use thiserror::Error;

use crate::storage::SessionManifest;

pub const DOCUMENT_NAME: &str = "session-catalog.json";
const MANIFEST_NAME: &str = "session.json";
const MAX_SCAN_DEPTH: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCatalogDocument {
    pub canonical_roots: Vec<PathBuf>,
    pub schema_version: u32,
    pub updated_at: DateTime<Utc>,
}

/// Registry of the canonical roots the app writes recordings into. It resolves a
/// session id to a directory without ever accepting a path from the frontend.
pub struct SessionCatalog {
    path: PathBuf,
    roots: RwLock<Vec<PathBuf>>,
}

impl SessionCatalog {
    pub fn open(path: PathBuf) -> Result<Self, CatalogError> {
        let roots = match load_document(&path) {
            Ok(document) => document.canonical_roots,
            Err(error) => {
                tracing::warn!(?error, "starting from an empty session catalog");
                Vec::new()
            }
        };
        Ok(Self {
            path,
            roots: RwLock::new(deduplicate(roots)),
        })
    }

    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Returns true when the catalog changed and was persisted. A root that does
    /// not exist yet is not registered: the archive registers the real root once
    /// it creates the first session there.
    pub fn register_root(&self, root: &Path) -> Result<bool, CatalogError> {
        let Ok(canonical) = fs::canonicalize(root) else {
            return Ok(false);
        };
        if !canonical.is_dir() {
            return Ok(false);
        }
        let mut roots = self
            .roots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if roots.contains(&canonical) {
            return Ok(false);
        }
        roots.push(canonical);
        save_document(
            &self.path,
            &SessionCatalogDocument {
                canonical_roots: roots.clone(),
                schema_version: 1,
                updated_at: Utc::now(),
            },
        )?;
        Ok(true)
    }

    pub fn resolve(&self, session_id: &str) -> Result<PathBuf, CatalogError> {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err(CatalogError::InvalidSessionId);
        }
        let mut matches: Vec<PathBuf> = Vec::new();
        for root in self.roots() {
            let Ok(root) = fs::canonicalize(&root) else {
                continue;
            };
            let mut manifests = Vec::new();
            collect_manifests(&root, 0, &mut manifests)?;
            for manifest_path in manifests {
                let Some(directory) = claimed_directory(&manifest_path, session_id, &root) else {
                    continue;
                };
                if !matches.contains(&directory) {
                    matches.push(directory);
                }
            }
        }
        match matches.len() {
            0 => Err(CatalogError::SessionNotFound(session_id.to_owned())),
            1 => Ok(matches.remove(0)),
            _ => Err(CatalogError::SessionIdConflict(session_id.to_owned())),
        }
    }
}

/// Accepts a manifest only when its canonical directory stays strictly inside
/// the registered root, so a symlink or traversal never escapes the catalog.
fn claimed_directory(manifest_path: &Path, session_id: &str, root: &Path) -> Option<PathBuf> {
    let bytes = fs::read(manifest_path).ok()?;
    let manifest: SessionManifest = serde_json::from_slice(&bytes).ok()?;
    if manifest.session_id != session_id {
        return None;
    }
    let directory = fs::canonicalize(manifest_path.parent()?).ok()?;
    (directory != root && directory.starts_with(root)).then_some(directory)
}

fn deduplicate(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique = Vec::with_capacity(roots.len());
    for root in roots {
        if !unique.contains(&root) {
            unique.push(root);
        }
    }
    unique
}

fn collect_manifests(
    directory: &Path,
    depth: usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    if depth > MAX_SCAN_DEPTH {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_manifests(&entry.path(), depth + 1, output)?;
        } else if entry.file_name() == MANIFEST_NAME {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn load_document(path: &Path) -> Result<SessionCatalogDocument, CatalogError> {
    if !path.exists() {
        return Ok(SessionCatalogDocument {
            canonical_roots: Vec::new(),
            schema_version: 1,
            updated_at: Utc::now(),
        });
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn save_document(path: &Path, document: &SessionCatalogDocument) -> Result<(), CatalogError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, document)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("session id cannot be empty")]
    InvalidSessionId,
    #[error("failed to access the session catalog: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse the session catalog: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to persist the session catalog: {0}")]
    Persist(#[from] tempfile::PersistError),
    #[error("more than one directory claims recording session '{0}'")]
    SessionIdConflict(String),
    #[error("recording session '{0}' was not found")]
    SessionNotFound(String),
}

#[cfg(test)]
#[path = "session_catalog_tests.rs"]
mod tests;
