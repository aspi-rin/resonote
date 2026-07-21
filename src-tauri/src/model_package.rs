use std::{
    fs::{self, File},
    path::{Component, Path, PathBuf},
};

use bzip2::read::BzDecoder;

use crate::models::ModelError;

pub fn model_directory(package: &Path, directory: &str) -> PathBuf {
    package.join(directory)
}

pub fn is_ready(package: &Path, directory: &str, required_files: &[&str]) -> bool {
    let model = model_directory(package, directory);
    required_files.iter().all(|name| {
        model
            .join(name)
            .metadata()
            .is_ok_and(|item| item.is_file() && item.len() > 0)
    })
}

pub fn prepare(
    package: &Path,
    archive_path: &Path,
    directory: &str,
    required_files: &[&str],
) -> Result<(), ModelError> {
    if is_ready(package, directory, required_files) {
        return Ok(());
    }
    let temporary = tempfile::Builder::new()
        .prefix(".extract-")
        .tempdir_in(package)?;
    let extracted = temporary.path().join("contents");
    fs::create_dir(&extracted)?;
    extract_archive(archive_path, &extracted)?;
    let source = find_model_root(&extracted, required_files, 0)
        .ok_or_else(|| ModelError::MissingModelAsset(format!("{directory} ONNX model layout")))?;
    let destination = model_directory(package, directory);
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }
    fs::rename(source, &destination)?;
    if !is_ready(package, directory, required_files) {
        return Err(ModelError::MissingModelAsset(format!(
            "{directory} ONNX model layout"
        )));
    }
    Ok(())
}

fn extract_archive(archive_path: &Path, destination: &Path) -> Result<(), ModelError> {
    let input = File::open(archive_path)?;
    let decoder = BzDecoder::new(input);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let unsafe_path = path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
        let kind = entry.header().entry_type();
        if unsafe_path || kind.is_symlink() || kind.is_hard_link() {
            return Err(ModelError::UnsafeArchive(path.display().to_string()));
        }
        if !entry.unpack_in(destination)? {
            return Err(ModelError::UnsafeArchive(path.display().to_string()));
        }
    }
    Ok(())
}

fn find_model_root(directory: &Path, required_files: &[&str], depth: usize) -> Option<PathBuf> {
    if required_files
        .iter()
        .all(|name| directory.join(name).is_file())
    {
        return Some(directory.to_path_buf());
    }
    if depth >= 3 {
        return None;
    }
    fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            entry.file_type().ok().filter(|kind| kind.is_dir())?;
            find_model_root(&entry.path(), required_files, depth + 1)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_the_qwen_model_layout() {
        let root = tempfile::tempdir().unwrap();
        let required_files = [
            "conv_frontend.onnx",
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "tokenizer/vocab.json",
            "tokenizer/merges.txt",
            "tokenizer/tokenizer_config.json",
        ];
        let model = model_directory(root.path(), "qwen3-asr");
        for name in &required_files {
            let path = model.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"asset").unwrap();
        }
        assert!(is_ready(root.path(), "qwen3-asr", &required_files));
    }
}
