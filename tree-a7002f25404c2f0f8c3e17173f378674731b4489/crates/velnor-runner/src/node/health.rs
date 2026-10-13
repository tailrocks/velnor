//! Unix-socket + atomic file health document.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use velnor_model::HealthDocument;

/// Bound once per process; each cycle rewrites `health.json` and answers one
/// pending Unix client with the current document.
pub struct HealthServer {
    dir: PathBuf,
    listener: Option<UnixListener>,
}

impl HealthServer {
    /// # Errors
    /// Directory creation or socket bind failures.
    pub fn bind(dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let socket = dir.join("health.sock");
        let _ = std::fs::remove_file(&socket);
        // macOS sockaddr_un is short; a long temp path must not take the
        // guardian down. The atomic health.json file remains authoritative.
        let listener = UnixListener::bind(&socket).ok().and_then(|listener| {
            listener.set_nonblocking(true).ok()?;
            Some(listener)
        });
        Ok(Self { dir, listener })
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// # Errors
    /// File write failures.
    pub fn publish(&self, document: &HealthDocument) -> anyhow::Result<PathBuf> {
        let json = serde_json::to_vec(document)?;
        let file = self.dir.join("health.json");
        let tmp = self.dir.join(".health.json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &file)?;
        if let Some(listener) = &self.listener
            && let Ok((mut stream, _)) = listener.accept()
        {
            let _ = stream.write_all(&json);
        }
        Ok(file)
    }
}

/// Read the health document from a Unix socket, falling back to `health.json`.
///
/// # Errors
/// Missing document or JSON decode failure.
pub fn fetch(dir: &Path) -> anyhow::Result<HealthDocument> {
    let socket = dir.join("health.sock");
    if let Some(document) = fetch_from_socket(&socket) {
        return Ok(document);
    }
    let bytes = std::fs::read(dir.join("health.json"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn fetch_from_socket(socket: &Path) -> Option<HealthDocument> {
    if !socket.exists() {
        return None;
    }
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    if buf.is_empty() {
        return None;
    }
    serde_json::from_slice(&buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_socket_payload_falls_back_to_atomic_health_file() {
        let dir = PathBuf::from("/tmp").join(format!("velnor-health-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let expected = HealthDocument::empty();
        std::fs::write(
            dir.join("health.json"),
            serde_json::to_vec(&expected).unwrap(),
        )
        .unwrap();

        let listener = UnixListener::bind(dir.join("health.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"{invalid").unwrap();
        });

        assert_eq!(fetch(&dir).unwrap(), expected);
        server.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stalled_socket_falls_back_to_atomic_health_file() {
        let dir = PathBuf::from("/tmp").join(format!("velnor-health-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let expected = HealthDocument::empty();
        std::fs::write(
            dir.join("health.json"),
            serde_json::to_vec(&expected).unwrap(),
        )
        .unwrap();

        let listener = UnixListener::bind(dir.join("health.sock")).unwrap();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            accepted_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(stream);
        });

        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let fetch_dir = dir.clone();
        let fetcher = std::thread::spawn(move || {
            result_tx.send(fetch(&fetch_dir)).unwrap();
        });
        accepted_rx.recv().unwrap();
        let result = result_rx.recv_timeout(Duration::from_secs(3));
        release_tx.send(()).unwrap();
        fetcher.join().unwrap();
        server.join().unwrap();
        let fetched_before_peer_release = result
            .expect("health socket read did not time out")
            .unwrap();
        assert_eq!(fetched_before_peer_release, expected);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
