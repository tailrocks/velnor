//! Isolation identity shared by every Firecracker/guest/Docker/net/cgroup object.

use std::path::{Path, PathBuf};

/// One GitHub job's isolation ID and generation. Cleanup may target only this pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IsolationIdentity {
    pub id: String,
    pub generation: u64,
}

impl IsolationIdentity {
    #[must_use]
    pub fn new(job_id: impl Into<String>, generation: u64) -> Self {
        Self {
            id: job_id.into(),
            generation,
        }
    }

    #[must_use]
    pub fn as_jailer_id(&self) -> String {
        // Jailer --id: alphanumeric and hyphens, max 64.
        let mut raw = format!("{}-{}", sanitize(&self.id), self.generation);
        raw.truncate(64);
        raw
    }

    #[must_use]
    pub fn label(&self) -> String {
        format!("velnor.isolation={}/{}", self.id, self.generation)
    }
}

fn sanitize(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// Host resources owned by one isolation identity. Teardown deletes only these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationResources {
    pub identity: IsolationIdentity,
    pub chroot: PathBuf,
    pub netns: PathBuf,
    pub tap: String,
    pub vsock: PathBuf,
    pub writable_disk: PathBuf,
}

impl IsolationResources {
    #[must_use]
    pub fn for_identity(identity: IsolationIdentity, run_root: &Path) -> Self {
        let jailer_id = identity.as_jailer_id();
        Self {
            chroot: run_root
                .join("jailer")
                .join("firecracker")
                .join(&jailer_id)
                .join("root"),
            netns: PathBuf::from("/var/run/netns").join(&jailer_id),
            tap: format!("vt{jailer_id}"),
            vsock: run_root.join("vsock").join(format!("{jailer_id}.sock")),
            writable_disk: run_root.join("disks").join(format!("{jailer_id}.rw.ext4")),
            identity,
        }
    }

    /// Paths teardown may delete. Never a broad name-based prune.
    #[must_use]
    pub fn teardown_paths(&self) -> Vec<&Path> {
        vec![
            self.chroot.as_path(),
            self.netns.as_path(),
            self.vsock.as_path(),
            self.writable_disk.as_path(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jailer_id_is_bounded_and_stable() {
        let id = IsolationIdentity::new("job/with spaces", 3);
        let jailer = id.as_jailer_id();
        assert!(jailer.len() <= 64);
        assert!(!jailer.contains('/'));
        assert!(jailer.ends_with("-3"), "{jailer}");
    }
}
