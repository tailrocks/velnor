//! Pinned Firecracker, jailer, kernel, rootfs, and guest-agent artifacts.

use std::path::Path;

use sha2::{Digest, Sha256};
use velnor_model::MicroVmPreflightFailure;

use super::HostFs;

/// Official Firecracker release pinned at implementation time.
pub const FIRECRACKER_VERSION: &str = "1.16.1";
/// Matching jailer (same Firecracker release).
pub const JAILER_VERSION: &str = "1.16.1";
/// Packaged artifact root inside the Debian identity.
pub const PACKAGED_MICROVM_ROOT: &str = "/usr/share/velnor/microvm";

const PINS_JSON: &str = include_str!("../../../../microvm/pins.json");

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
        let file: ManifestFile = serde_json::from_slice(&bytes).map_err(|error| {
            MicroVmPreflightFailure::new(
                "artifacts.manifest",
                format!("invalid manifest.json: {error}"),
            )
        })?;
        let arch = host_arch()?;
        let mut checksums = ArtifactChecksums {
            firecracker_version: file.firecracker_version,
            jailer_version: file.jailer_version,
            firecracker: file.firecracker.resolve("firecracker", arch)?,
            jailer: file.jailer.resolve("jailer", arch)?,
            kernel: file.kernel.resolve("kernel", arch)?,
            rootfs: file.rootfs.resolve("rootfs", arch)?,
            guest_agent: file.guest_agent.resolve("guest_agent", arch)?,
            snapshot: file.snapshot,
        };
        parse_pins()?;
        require_pin_version(
            "artifacts.firecracker_version",
            &checksums.firecracker_version,
            FIRECRACKER_VERSION,
        )?;
        require_pin_version(
            "artifacts.jailer_version",
            &checksums.jailer_version,
            JAILER_VERSION,
        )?;
        checksums.firecracker = require_sha256("firecracker", &checksums.firecracker)?;
        checksums.jailer = require_sha256("jailer", &checksums.jailer)?;
        checksums.kernel = require_sha256("kernel", &checksums.kernel)?;
        checksums.rootfs = require_sha256("rootfs", &checksums.rootfs)?;
        checksums.guest_agent = require_sha256("guest_agent", &checksums.guest_agent)?;
        Ok(Self::from_root(root, checksums))
    }
}

fn require_pin_version(
    field: &'static str,
    got: &str,
    expected: &str,
) -> Result<(), MicroVmPreflightFailure> {
    if got == expected {
        Ok(())
    } else {
        Err(MicroVmPreflightFailure::new(
            field,
            format!("manifest {got} != pinned {expected}"),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum PinnedDigest {
    One(String),
    ByArch(std::collections::BTreeMap<String, String>),
}

impl PinnedDigest {
    fn resolve(&self, field: &str, arch: &str) -> Result<String, MicroVmPreflightFailure> {
        match self {
            Self::One(value) => Ok(value.clone()),
            Self::ByArch(map) => map.get(arch).cloned().ok_or_else(|| {
                MicroVmPreflightFailure::new(
                    "artifacts.checksum",
                    format!("{field} has no pin for {arch}"),
                )
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
struct ManifestFile {
    firecracker_version: String,
    jailer_version: String,
    firecracker: PinnedDigest,
    jailer: PinnedDigest,
    kernel: PinnedDigest,
    rootfs: PinnedDigest,
    guest_agent: PinnedDigest,
    #[serde(default)]
    snapshot: Option<String>,
}

fn host_arch() -> Result<&'static str, MicroVmPreflightFailure> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => Err(MicroVmPreflightFailure::new(
            "guest.arch",
            format!("no microVM artifact pin for {other}"),
        )),
    }
}

fn require_sha256(field: &str, value: &str) -> Result<String, MicroVmPreflightFailure> {
    if value.starts_with("UNSET")
        || value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(MicroVmPreflightFailure::new(
            "artifacts.checksum",
            format!("{field} is not a pinned sha256"),
        ));
    }
    Ok(value.to_ascii_lowercase())
}

/// Checksums recorded in `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactChecksums {
    pub firecracker_version: String,
    pub jailer_version: String,
    pub firecracker: String,
    pub jailer: String,
    pub kernel: String,
    pub rootfs: String,
    pub guest_agent: String,
    #[serde(default)]
    pub snapshot: Option<String>,
}

/// Exact Firecracker/jailer/kernel/rootfs/agent generation bound to one package.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MicroVmGeneration {
    pub velnor_version: String,
    pub firecracker_version: String,
    pub jailer_version: String,
    pub kernel_version: String,
    pub firecracker: String,
    pub jailer: String,
    pub kernel: String,
    pub rootfs: String,
    pub guest_agent: String,
}

impl MicroVmGeneration {
    #[must_use]
    pub fn from_set(set: &MicroVmArtifactSet) -> Self {
        Self {
            velnor_version: env!("CARGO_PKG_VERSION").to_string(),
            firecracker_version: FIRECRACKER_VERSION.to_string(),
            jailer_version: JAILER_VERSION.to_string(),
            kernel_version: crate::execution::KERNEL_VERSION.to_string(),
            firecracker: set.firecracker.sha256.clone(),
            jailer: set.jailer.sha256.clone(),
            kernel: set.kernel.sha256.clone(),
            rootfs: set.rootfs.sha256.clone(),
            guest_agent: set.guest_agent.sha256.clone(),
        }
    }
}

/// Official release tarball pin for one architecture.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
struct FirecrackerTarballPin {
    url: String,
    sha256: String,
    firecracker_member: String,
    jailer_member: String,
    firecracker_sha256: String,
    jailer_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
struct MicroVmPinsFile {
    firecracker_version: String,
    jailer_version: String,
    kernel_version: String,
    kernel_tarball: String,
    kernel_tarball_sha256: String,
    tarballs: std::collections::BTreeMap<String, FirecrackerTarballPin>,
}

/// Load and verify the packaged generation under `root`.
///
/// # Errors
/// Missing files, UNSET pins, or checksum mismatch.
pub fn packaged_generation(
    root: &Path,
    fs: &dyn HostFs,
) -> Result<MicroVmGeneration, MicroVmPreflightFailure> {
    let set = MicroVmArtifactSet::load(root, fs)?;
    verify_microvm_artifacts(&set, fs)?;
    Ok(MicroVmGeneration::from_set(&set))
}

/// Mixed Firecracker/jailer/kernel/rootfs/agent generations cannot advertise.
///
/// # Errors
/// The first mismatched field.
pub fn require_coherent_generation(
    proven: &MicroVmGeneration,
    packaged: &MicroVmGeneration,
) -> Result<(), MicroVmPreflightFailure> {
    let checks = [
        (
            "velnor_version",
            proven.velnor_version.as_str(),
            packaged.velnor_version.as_str(),
        ),
        (
            "firecracker_version",
            proven.firecracker_version.as_str(),
            packaged.firecracker_version.as_str(),
        ),
        (
            "jailer_version",
            proven.jailer_version.as_str(),
            packaged.jailer_version.as_str(),
        ),
        (
            "kernel_version",
            proven.kernel_version.as_str(),
            packaged.kernel_version.as_str(),
        ),
        (
            "firecracker",
            proven.firecracker.as_str(),
            packaged.firecracker.as_str(),
        ),
        ("jailer", proven.jailer.as_str(), packaged.jailer.as_str()),
        ("kernel", proven.kernel.as_str(), packaged.kernel.as_str()),
        ("rootfs", proven.rootfs.as_str(), packaged.rootfs.as_str()),
        (
            "guest_agent",
            proven.guest_agent.as_str(),
            packaged.guest_agent.as_str(),
        ),
    ];
    for (field, left, right) in checks {
        if left != right {
            return Err(MicroVmPreflightFailure::new(
                "artifacts.generation",
                format!("{field} proven {left} != packaged {right}"),
            ));
        }
    }
    Ok(())
}

fn parse_pins() -> Result<MicroVmPinsFile, MicroVmPreflightFailure> {
    let pins: MicroVmPinsFile = serde_json::from_str(PINS_JSON).map_err(|error| {
        MicroVmPreflightFailure::new("artifacts.pins", format!("invalid pins.json: {error}"))
    })?;
    require_pin_version(
        "artifacts.firecracker_version",
        &pins.firecracker_version,
        FIRECRACKER_VERSION,
    )?;
    require_pin_version(
        "artifacts.jailer_version",
        &pins.jailer_version,
        JAILER_VERSION,
    )?;
    require_pin_version(
        "guest.kernel",
        &pins.kernel_version,
        crate::execution::KERNEL_VERSION,
    )?;
    require_sha256("kernel_tarball", &pins.kernel_tarball_sha256)?;
    for arch in ["x86_64", "aarch64"] {
        let tarball = pins.tarballs.get(arch).ok_or_else(|| {
            MicroVmPreflightFailure::new(
                "artifacts.firecracker_tarball",
                format!("no Firecracker tarball pin for {arch}"),
            )
        })?;
        require_sha256(&format!("{arch}.tarball"), &tarball.sha256)?;
        require_sha256(&format!("{arch}.firecracker"), &tarball.firecracker_sha256)?;
        require_sha256(&format!("{arch}.jailer"), &tarball.jailer_sha256)?;
    }
    Ok(pins)
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
            jailer_version: JAILER_VERSION.to_string(),
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

    #[test]
    fn unset_manifest_checksums_fail_closed() {
        let root = PathBuf::from("/microvm");
        let mut fs = MemoryFs::default();
        let mut value: serde_json::Value =
            serde_json::from_str(include_str!("../../../../microvm/manifest.json")).unwrap();
        value["guest_agent"] = serde_json::Value::String("UNSET_FILL".into());
        fs.write(
            &root.join("manifest.json"),
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let error = MicroVmArtifactSet::load(&root, &fs).unwrap_err();
        assert_eq!(error.requirement, "artifacts.checksum");
    }

    #[test]
    fn shipped_manifest_guest_agent_is_pinned_sha256() {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("../../../../microvm/manifest.json")).unwrap();
        let pins = match &value["guest_agent"] {
            serde_json::Value::String(one) => vec![one.as_str()],
            serde_json::Value::Object(map) => {
                map.values().filter_map(|v| v.as_str()).collect::<Vec<_>>()
            }
            other => panic!("guest_agent pin shape {other}"),
        };
        assert!(!pins.is_empty());
        for pin in pins {
            assert_eq!(pin.len(), 64, "{pin}");
            assert!(pin.bytes().all(|b| b.is_ascii_hexdigit()), "{pin}");
            assert!(!pin.starts_with("UNSET"), "{pin}");
        }
    }

    #[test]
    fn mixed_generation_fails_closed() {
        let mut left = MicroVmGeneration {
            velnor_version: "0.1.211".into(),
            firecracker_version: FIRECRACKER_VERSION.to_string(),
            jailer_version: JAILER_VERSION.to_string(),
            kernel_version: crate::execution::KERNEL_VERSION.to_string(),
            firecracker: "a".repeat(64),
            jailer: "b".repeat(64),
            kernel: "c".repeat(64),
            rootfs: "d".repeat(64),
            guest_agent: "e".repeat(64),
        };
        let right = left.clone();
        left.firecracker = "f".repeat(64);
        let error = require_coherent_generation(&left, &right).unwrap_err();
        assert_eq!(error.requirement, "artifacts.generation");
        assert!(error.to_string().contains("firecracker"), "{error}");
        assert!(error.to_string().contains("docker backend was not used"));
    }

    #[test]
    fn official_tarball_pins_match_constants() {
        let pins = parse_pins().unwrap();
        let x86 = pins.tarballs.get("x86_64").unwrap();
        let arm = pins.tarballs.get("aarch64").unwrap();
        assert_eq!(x86.sha256.len(), 64);
        assert_eq!(arm.sha256.len(), 64);
        assert!(x86.url.contains(FIRECRACKER_VERSION));
        assert!(arm.url.contains(FIRECRACKER_VERSION));
        assert_ne!(hex_sha256(b"not-the-tarball"), x86.sha256);
    }
}
