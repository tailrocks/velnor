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
        if let Some(listener) = &self.listener {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(&json);
            }
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
    if socket.exists() {
        if let Ok(mut stream) = UnixStream::connect(&socket) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf)?;
            if !buf.is_empty() {
                return Ok(serde_json::from_slice(&buf)?);
            }
        }
    }
    let bytes = std::fs::read(dir.join("health.json"))?;
    Ok(serde_json::from_slice(&bytes)?)
}
