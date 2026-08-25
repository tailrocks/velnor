//! Pinned Firecracker, jailer, kernel, rootfs, and guest-agent artifacts.

use std::path::Path;

use sha2::{Digest, Sha256};
use velnor_model::MicroVmPreflightFailure;

use super::HostFs;

/// Official Firecracker release pinned at implementation time.
pub const FIRECRACKER_VERSION: &str = "1.16.1";
/// Matching jailer (same Firecracker release).
pub const JAILER_VERSION: &str = "1.16.1";

/// Checksummed microVM artifact set bound to one Velnor release identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicroVmArtifactSet {
    pub firecracker: Artifact,
    pub jailer: Artifact,
    pub kernel: Artifact,
    pub rootfs: Artifact,
    pub guest_agent: Artifact,
    pub snapshot: Option<Artifact>,
}

/// One pinned file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub name: &'static str,
    pub path: std::path::PathBuf,
    pub sha256: String,
}

impl MicroVmArtifactSet {
    /// Layout under `/usr/share/velnor/microvm` (packaged) or a test root.
    #[must_use]
    pub fn from_root(root: &Path, checksums: ArtifactChecksums) -> Self {
        Self {
            firecracker: Artifact {
                name: "firecracker",
                path: root.join("firecracker"),
                sha256: checksums.firecracker,
            },
            jailer: Artifact {
                name: "jailer",
                path: root.join("jailer"),
                sha256: checksums.jailer,
            },
            kernel: Artifact {
                name: "kernel",
                path: root.join("vmlinux"),
                sha256: checksums.kernel,
            },
            rootfs: Artifact {
                name: "rootfs",
                path: root.join("rootfs.ext4"),
                sha256: checksums.rootfs,
            },
            guest_agent: Artifact {
                name: "guest-agent",
                path: root.join("velnor-guest-agent"),
                sha256: checksums.guest_agent,
            },
            snapshot: checksums.snapshot.map(|sha256| Artifact {
                name: "snapshot",
                path: root.join("snapshot.mem"),
                sha256,
            }),
        }
    }

    /// Load checksums from `manifest.json` next to the binaries.
    ///
    /// # Errors
    /// Missing or invalid manifest.
    pub fn load(root: &Path, fs: &dyn HostFs) -> Result<Self, MicroVmPreflightFailure> {
        let manifest_path = root.join("manifest.json");
        let bytes = fs
            .read(&manifest_path)
            .map_err(|detail| MicroVmPreflightFailure::new("artifacts.manifest", detail))?;
        let checksums: ArtifactChecksums = serde_json::from_slice(&bytes).map_err(|error| {
            MicroVmPreflightFailure::new(
                "artifacts.manifest",
                format!("invalid manifest.json: {error}"),
            )
        })?;
        if checksums.firecracker_version != FIRECRACKER_VERSION {
            return Err(MicroVmPreflightFailure::new(
                "artifacts.firecracker_version",
                format!(
                    "manifest {} != pinned {FIRECRACKER_VERSION}",
                    checksums.firecracker_version
                ),
            ));
        }
        Ok(Self::from_root(root, checksums))
    }
}

/// Checksums recorded in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactChecksums {
    pub firecracker_version: String,
    pub firecracker: String,
    pub jailer: String,
    pub kernel: String,
    pub rootfs: String,
    pub guest_agent: String,
    #[serde(default)]
    pub snapshot: Option<String>,
}

/// Verify every required artifact exists and matches its checksum.
///
/// # Errors
/// [`MicroVmPreflightFailure`] names the exact missing or mismatched file.
pub fn verify_microvm_artifacts(
    set: &MicroVmArtifactSet,
    fs: &dyn HostFs,
) -> Result<(), MicroVmPreflightFailure> {
    for artifact in [
        &set.firecracker,
        &set.jailer,
        &set.kernel,
        &set.rootfs,
        &set.guest_agent,
    ] {
        verify_one(artifact, fs)?;
    }
    if let Some(snapshot) = &set.snapshot {
        verify_one(snapshot, fs)?;
    }
    Ok(())
}

fn verify_one(artifact: &Artifact, fs: &dyn HostFs) -> Result<(), MicroVmPreflightFailure> {
    let bytes = fs.read(&artifact.path).map_err(|detail| {
        MicroVmPreflightFailure::new(
            match artifact.name {
                "firecracker" => "firecracker",
                "jailer" => "jailer",
                "kernel" => "guest.kernel",
                "rootfs" => "guest.rootfs",
                "guest-agent" => "guest.agent",
                "snapshot" => "guest.snapshot",
                other => other,
            },
            detail,
        )
    })?;
    let digest = hex_sha256(&bytes);
    if digest != artifact.sha256 {
        return Err(MicroVmPreflightFailure::new(
            "artifacts.checksum",
            format!(
                "{} digest {digest} != manifest {}",
                artifact.name, artifact.sha256
            ),
        ));
    }
    Ok(())
}

#[must_use]
pub fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::MemoryFs;
    use std::path::PathBuf;

    #[test]
    fn mismatch_or_missing_artifact_fails_closed() {
        let root = PathBuf::from("/microvm");
        let mut fs = MemoryFs::default();
        let payload = b"firecracker-bin";
        fs.write(&root.join("firecracker"), payload).unwrap();
        let checksums = ArtifactChecksums {
            firecracker_version: FIRECRACKER_VERSION.to_string(),
            firecracker: hex_sha256(payload),
            jailer: hex_sha256(b"jailer"),
            kernel: hex_sha256(b"kernel"),
            rootfs: hex_sha256(b"rootfs"),
            guest_agent: hex_sha256(b"agent"),
            snapshot: None,
        };
        let set = MicroVmArtifactSet::from_root(&root, checksums);
        let error = verify_microvm_artifacts(&set, &fs).unwrap_err();
        assert_eq!(error.requirement, "jailer");
        assert!(error.to_string().contains("docker backend was not used"));
    }
}
