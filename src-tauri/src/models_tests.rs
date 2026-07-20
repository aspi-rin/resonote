use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    thread,
};

use super::*;
use sha2::{Digest, Sha256};

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
fn catalog_uses_pinned_sherpa_assets() {
    let spec = model_spec(CATALOG_MODEL_ID).unwrap();
    assert_eq!(spec.revision, CATALOG_REVISION);
    assert_eq!(spec.artifacts.len(), 2);
    assert!(spec.artifacts.iter().all(|item| item.sha256.len() == 64));
    assert!(
        spec.artifacts
            .iter()
            .any(|item| item.kind == ArtifactKind::ModelArchive)
    );
    assert!(
        spec.artifacts
            .iter()
            .any(|item| item.kind == ArtifactKind::VadModel)
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
