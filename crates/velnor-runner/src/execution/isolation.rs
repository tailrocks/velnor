//! Isolation identity shared by every Firecracker/guest/Docker/net/cgroup object.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

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

/// Linux interface names are capped at IFNAMSIZ-1 = 15. Jailer id can be 64.
fn tap_name(jailer_id: &str) -> String {
    let digest = Sha256::digest(jailer_id.as_bytes());
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    format!("vt{:012x}", u64::from_be_bytes(prefix) & 0xFFFF_FFFF_FFFF)
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
    pub chroot_base: PathBuf,
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
        let chroot_base = run_root.join("jailer");
        Self {
            chroot: chroot_base
                .join("firecracker")
                .join(&jailer_id)
                .join("root"),
            chroot_base,
            netns: PathBuf::from("/var/run/netns").join(&jailer_id),
            tap: tap_name(&jailer_id),
            vsock: run_root.join("vsock").join(format!("{jailer_id}.sock")),
            writable_disk: run_root.join("disks").join(format!("{jailer_id}.rw.ext4")),
            identity,
        }
    }

    /// Jailer API socket: `{chroot}/run/firecracker.socket`, not the vsock UDS.
    #[must_use]
    pub fn api_socket(&self) -> PathBuf {
        self.chroot.join("run").join("firecracker.socket")
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
    use std::path::Path;

    #[test]
    fn jailer_id_is_bounded_and_stable() {
        let id = IsolationIdentity::new("job/with spaces", 3);
        let jailer = id.as_jailer_id();
        assert!(jailer.len() <= 64);
        assert!(!jailer.contains('/'));
        assert!(jailer.ends_with("-3"), "{jailer}");
    }

    #[test]
    fn tap_name_fits_ifnamesz_and_api_socket_is_under_chroot() {
        let long = IsolationIdentity::new("j".repeat(200), 7);
        let resources = IsolationResources::for_identity(long, Path::new("/run/velnor"));
        assert!(resources.tap.len() <= 15, "{}", resources.tap);
        assert!(resources.tap.starts_with("vt"));
        assert!(resources
            .api_socket()
            .ends_with("root/run/firecracker.socket"));
        assert_ne!(resources.api_socket(), resources.vsock);
        let other = IsolationResources::for_identity(
            IsolationIdentity::new("other", 7),
            Path::new("/run/velnor"),
        );
        assert_ne!(resources.tap, other.tap);
    }
}
