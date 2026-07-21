use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use super::*;
use sha2::{Digest, Sha256};

#[test]
fn cancelling_and_waiting_does_not_return_until_the_install_task_exits() {
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(ModelManager::new(directory.path().to_path_buf()).unwrap());
    let (install_locked_tx, install_locked_rx) = mpsc::channel();
    let (release_install_tx, release_install_rx) = mpsc::channel();
    let installer_manager = Arc::clone(&manager);
    let installer = thread::spawn(move || {
        let _guard = installer_manager.install_lock.lock().unwrap();
        install_locked_tx.send(()).unwrap();
        release_install_rx.recv().unwrap();
    });
    install_locked_rx.recv().unwrap();

    let (cancelled_tx, cancelled_rx) = mpsc::channel();
    let cancelling_manager = Arc::clone(&manager);
    let canceller = thread::spawn(move || {
        cancelling_manager.cancel_install_and_wait();
        cancelled_tx.send(()).unwrap();
    });

    while !manager.cancel.load(Ordering::Acquire) {
        thread::yield_now();
    }
    assert!(matches!(
        cancelled_rx.recv_timeout(Duration::from_millis(20)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    release_install_tx.send(()).unwrap();
    cancelled_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    installer.join().unwrap();
    canceller.join().unwrap();
}

#[test]
fn resumes_and_verifies_an_interrupted_download() {
    let content = b"verified model artifact".to_vec();
    let digest = hex::encode(Sha256::digest(&content));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server_content = content.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            request.push_str(&line);
        }
        assert!(request.to_ascii_lowercase().contains("range: bytes=8-"));
        let remaining = &server_content[8..];
        write!(
            stream,
            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            remaining.len()
        )
        .unwrap();
        stream.write_all(remaining).unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let artifact = ArtifactSpec {
        file_name: "artifact.bin".to_owned(),
        kind: ArtifactKind::VadModel,
        sha256: digest,
        size: content.len() as u64,
        url: format!("http://{address}/artifact.bin"),
    };
    let manager = ModelManager::new(directory.path().to_path_buf()).unwrap();
    let final_path = directory.path().join("artifact.bin");
    fs::write(partial_path(&final_path), &content[..8]).unwrap();
    manager
        .download_artifact(&artifact, &final_path, 0, artifact.size, "test")
        .unwrap();
    server.join().unwrap();
    assert_eq!(fs::read(final_path).unwrap(), content);
}

#[test]
fn restarts_a_complete_partial_file_with_the_wrong_checksum() {
    let content = b"correct replacement bytes".to_vec();
    let digest = hex::encode(Sha256::digest(&content));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server_content = content.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            request.push_str(&line);
        }
        assert!(!request.to_ascii_lowercase().contains("range:"));
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            server_content.len()
        )
        .unwrap();
        stream.write_all(&server_content).unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let artifact = ArtifactSpec {
        file_name: "artifact.bin".to_owned(),
        kind: ArtifactKind::VadModel,
        sha256: digest,
        size: content.len() as u64,
        url: format!("http://{address}/artifact.bin"),
    };
    let manager = ModelManager::new(directory.path().to_path_buf()).unwrap();
    let final_path = directory.path().join("artifact.bin");
    fs::write(partial_path(&final_path), vec![b'x'; content.len()]).unwrap();

    manager
        .download_artifact(&artifact, &final_path, 0, artifact.size, "test")
        .unwrap();
    server.join().unwrap();

    assert_eq!(fs::read(final_path).unwrap(), content);
}

#[test]
fn catalog_uses_pinned_assets_for_every_model() {
    let catalog = model_catalog();
    assert_eq!(
        catalog
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        [DEFAULT_MODEL_ID, "qwen3-asr-1.7b-int8"]
    );
    for entry in catalog {
        let spec = model_spec(&entry.id).unwrap();
        assert_eq!(
            entry.total_bytes,
            spec.artifacts.iter().map(|item| item.size).sum::<u64>()
        );
        assert!(spec.artifacts.iter().all(|item| item.sha256.len() == 64));
        assert!(
            spec.artifacts
                .iter()
                .any(|item| item.kind == ArtifactKind::VadModel)
        );
        assert!(!spec.required_files.is_empty());
        if !spec
            .artifacts
            .iter()
            .any(|item| item.kind == ArtifactKind::ModelArchive)
        {
            for required in &spec.required_files {
                let expected = format!("{}/{}", spec.model_directory, required);
                assert!(spec.artifacts.iter().any(|item| {
                    item.kind == ArtifactKind::ModelFile && item.file_name == expected
                }));
            }
        }
    }

    let qwen_1_7b = model_spec("qwen3-asr-1.7b-int8").unwrap();
    assert!(
        qwen_1_7b
            .artifacts
            .iter()
            .any(|item| item.kind == ArtifactKind::ModelFile)
    );
}

#[test]
fn rejects_a_wrong_checksum_before_publishing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bad.bin");
    fs::write(&path, b"not the expected data").unwrap();
    assert!(matches!(
        verify_sha256(&path, &"0".repeat(64)).unwrap_err(),
        ModelError::ChecksumMismatch { .. }
    ));
}

#[test]
fn marker_validation_does_not_rehash_model_files() {
    let directory = tempfile::tempdir().unwrap();
    let manager = ModelManager::new(directory.path().to_path_buf()).unwrap();
    let expected = b"verified-model";
    let spec = ModelPackageSpec {
        artifacts: vec![ArtifactSpec {
            file_name: "model/model.bin".to_owned(),
            kind: ArtifactKind::ModelFile,
            sha256: hex::encode(Sha256::digest(expected)),
            size: expected.len() as u64,
            url: "https://example.invalid/model.bin".to_owned(),
        }],
        display_name: "Test model".to_owned(),
        id: "test-model".to_owned(),
        model_directory: "model",
        required_files: vec!["model.bin"],
        revision: "test-revision".to_owned(),
    };
    let package = manager.package_dir(&spec);
    fs::create_dir_all(package.join("model")).unwrap();
    fs::write(package.join("model/model.bin"), b"changed-model!").unwrap();
    save_marker(&package.join("install.json"), &spec).unwrap();

    assert!(manager.read_valid_marker(&spec).unwrap().is_some());
}

#[test]
fn marker_validation_still_rehashes_the_small_vad_file() {
    let directory = tempfile::tempdir().unwrap();
    let manager = ModelManager::new(directory.path().to_path_buf()).unwrap();
    let model = b"model";
    let vad = b"verified-vad";
    let spec = ModelPackageSpec {
        artifacts: vec![
            ArtifactSpec {
                file_name: "model/model.bin".to_owned(),
                kind: ArtifactKind::ModelFile,
                sha256: hex::encode(Sha256::digest(model)),
                size: model.len() as u64,
                url: "https://example.invalid/model.bin".to_owned(),
            },
            ArtifactSpec {
                file_name: "silero_vad.onnx".to_owned(),
                kind: ArtifactKind::VadModel,
                sha256: hex::encode(Sha256::digest(vad)),
                size: vad.len() as u64,
                url: "https://example.invalid/silero_vad.onnx".to_owned(),
            },
        ],
        display_name: "Test model".to_owned(),
        id: "test-model".to_owned(),
        model_directory: "model",
        required_files: vec!["model.bin"],
        revision: "test-revision".to_owned(),
    };
    let package = manager.package_dir(&spec);
    fs::create_dir_all(package.join("model")).unwrap();
    fs::write(package.join("model/model.bin"), model).unwrap();
    fs::write(package.join("silero_vad.onnx"), b"changed-vad!").unwrap();
    save_marker(&package.join("install.json"), &spec).unwrap();

    assert!(manager.read_valid_marker(&spec).unwrap().is_none());
}
