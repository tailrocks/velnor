//! Secure, generation-fenced handoff of one broker assignment to a transient worker.
//!
//! The envelope deliberately contains the original [`TaskAgentMessage`] without
//! parsing and rebuilding it.  The file is a short-lived local IPC boundary;
//! callers must keep its parent directory private to the daemon.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use velnor_model::Generation;

use crate::protocol::TaskAgentMessage;

/// Version of the on-disk handoff schema.
pub const HANDOFF_SCHEMA_VERSION: u32 = 1;

#[must_use]
pub fn path(state_dir: &Path, nonce: &str) -> PathBuf {
    state_dir.join("handoffs").join(format!("{nonce}.json"))
}

#[must_use]
pub fn completion_path(state_dir: &Path, nonce: &str) -> PathBuf {
    state_dir.join("handoffs").join(format!("{nonce}.done"))
}

/// Complete assignment identity and the exact broker message delivered to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssignmentHandoff {
    pub schema_version: u32,
    pub slot_id: String,
    pub generation: Generation,
    pub nonce: String,
    pub session_id: String,
    pub broker_url: String,
    pub slot_index: usize,
    pub config_dir: PathBuf,
    pub task_agent_message: TaskAgentMessage,
}

impl AssignmentHandoff {
    /// Construct a current-schema handoff envelope.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        slot_id: String,
        generation: Generation,
        nonce: String,
        session_id: String,
        broker_url: String,
        slot_index: usize,
        config_dir: PathBuf,
        task_agent_message: TaskAgentMessage,
    ) -> Self {
        Self {
            schema_version: HANDOFF_SCHEMA_VERSION,
            slot_id,
            generation,
            nonce,
            session_id,
            broker_url,
            slot_index,
            config_dir,
            task_agent_message,
        }
    }

    /// Reject a handoff that belongs to another slot, generation, or assignment.
    pub fn validate_identity(
        &self,
        slot_id: &str,
        generation: Generation,
        nonce: &str,
    ) -> std::result::Result<(), HandoffValidationError> {
        if self.schema_version != HANDOFF_SCHEMA_VERSION {
            return Err(HandoffValidationError::Schema {
                expected: HANDOFF_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        if self.slot_id != slot_id {
            return Err(HandoffValidationError::Slot {
                expected: slot_id.to_owned(),
                actual: self.slot_id.clone(),
            });
        }
        if self.generation != generation {
            return Err(HandoffValidationError::Generation {
                expected: generation,
                actual: self.generation,
            });
        }
        if self.nonce != nonce {
            return Err(HandoffValidationError::Nonce);
        }
        Ok(())
    }
}

/// Why a handoff cannot be used by the expected worker.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HandoffValidationError {
    #[error("unsupported handoff schema {actual}; expected {expected}")]
    Schema { expected: u32, actual: u32 },
    #[error("handoff slot mismatch: expected {expected}, got {actual}")]
    Slot { expected: String, actual: String },
    #[error("handoff generation mismatch: expected {expected:?}, got {actual:?}")]
    Generation {
        expected: Generation,
        actual: Generation,
    },
    #[error("handoff nonce mismatch")]
    Nonce,
}

/// Atomically write a handoff with mode `0600` and durable file/directory metadata.
pub fn write_atomic(path: &Path, handoff: &AssignmentHandoff) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("create handoff directory {}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 0700 {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec(handoff).context("serialize assignment handoff")?;
    let temporary = parent.join(format!(".{}.{}.tmp", path_name(path), Uuid::new_v4()));

    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("create handoff temp file {}", temporary.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write handoff temp file {}", temporary.display()))?;
        set_private_mode(&temporary)?;
        file.sync_all()
            .with_context(|| format!("fsync handoff {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("atomically install assignment handoff {}", path.display()))?;
        fs::File::open(parent)
            .with_context(|| format!("open handoff directory {}", parent.display()))?
            .sync_all()
            .with_context(|| format!("fsync handoff directory {}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Read one handoff without consuming it.
pub fn read(path: &Path) -> Result<AssignmentHandoff> {
    reject_non_regular(path)?;
    let bytes =
        fs::read(path).with_context(|| format!("read assignment handoff {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse assignment handoff {}", path.display()))
}

/// Atomically claim, read, and remove a handoff.
pub fn read_and_remove(path: &Path) -> Result<AssignmentHandoff> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let claimed = parent.join(format!(".{}.{}.claimed", path_name(path), Uuid::new_v4()));
    fs::rename(path, &claimed)
        .with_context(|| format!("claim assignment handoff {}", path.display()))?;

    let result = read(&claimed);
    match result {
        Ok(handoff) => {
            fs::remove_file(&claimed)
                .with_context(|| format!("remove consumed handoff {}", claimed.display()))?;
            sync_directory(parent)?;
            Ok(handoff)
        }
        Err(error) => {
            // Do not overwrite a newer handoff concurrently published at `path`.
            if !path.exists() {
                let _ = fs::rename(&claimed, path);
            } else {
                let _ = fs::remove_file(&claimed);
            }
            Err(error)
        }
    }
}

fn path_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("handoff")
}

fn reject_non_regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect assignment handoff {}", path.display()))?;
    if !metadata.file_type().is_file() {
        anyhow::bail!(
            "assignment handoff is not a regular file: {}",
            path.display()
        );
    }
    ensure_private_mode(&metadata, path)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_mode(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn ensure_private_mode(metadata: &fs::Metadata, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        anyhow::bail!(
            "assignment handoff is not private (expected mode 0600): {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_mode(_metadata: &fs::Metadata, _path: &Path) -> Result<()> {
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("open handoff directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("fsync handoff directory {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("velnor-handoff-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        path
    }

    fn sample() -> AssignmentHandoff {
        AssignmentHandoff::new(
            "scope-3".into(),
            Generation(7),
            "assignment-nonce".into(),
            "session-9".into(),
            "https://broker.example".into(),
            3,
            PathBuf::from("/var/lib/velnor/config/3"),
            TaskAgentMessage {
                message_id: 42,
                message_type: "RunnerJobRequest".into(),
                body: "{\"runner_request_id\":\"req-1\"}".into(),
                iv_base64: Some("iv".into()),
            },
        )
    }

    #[test]
    fn roundtrip_preserves_exact_message_and_consumes_file() {
        let dir = temp_dir();
        let path = dir.join("assignment.json");
        let expected = sample();
        write_atomic(&path, &expected).unwrap();
        assert_eq!(read(&path).unwrap(), expected);
        assert_eq!(read_and_remove(&path).unwrap(), expected);
        assert!(!path.exists());
        fs::remove_dir(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn handoff_is_private_before_and_after_replace() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let path = dir.join("assignment.json");
        write_atomic(&path, &sample()).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        write_atomic(&path, &sample()).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn stale_generation_and_nonce_are_rejected() {
        let handoff = sample();
        assert!(matches!(
            handoff.validate_identity("scope-3", Generation(8), "assignment-nonce"),
            Err(HandoffValidationError::Generation { .. })
        ));
        assert_eq!(
            handoff.validate_identity("scope-3", Generation(7), "other"),
            Err(HandoffValidationError::Nonce)
        );
    }
}
