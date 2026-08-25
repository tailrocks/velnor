//! Golden snapshot identity. Mismatch never restores permissively.

use velnor_model::MicroVmPreflightFailure;

use super::artifacts::MicroVmGeneration;
use super::guest::KERNEL_VERSION;
use super::FIRECRACKER_VERSION;

/// Exact tuple a snapshot is bound to. Kernel/rootfs/agent fields are SHA-256s
/// of packaged bytes, not version labels.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotIdentity {
    pub firecracker_version: String,
    pub snapshot_format: String,
    pub arch: String,
    pub host_kernel_class: String,
    pub guest_kernel: String,
    pub rootfs: String,
    pub guest_agent: String,
    pub docker_version: String,
    pub image_set: String,
}

impl SnapshotIdentity {
    #[must_use]
    pub fn production_template(arch: &str, host_kernel_class: &str) -> Self {
        Self {
            firecracker_version: FIRECRACKER_VERSION.to_string(),
            snapshot_format: "Full".to_string(),
            arch: arch.to_string(),
            host_kernel_class: host_kernel_class.to_string(),
            guest_kernel: KERNEL_VERSION.to_string(),
            rootfs: "velnor-guest-rootfs".to_string(),
            guest_agent: env!("CARGO_PKG_VERSION").to_string(),
            docker_version: "pinned".to_string(),
            image_set: "velnor/job-ubuntu:26.04".to_string(),
        }
    }

    /// Bind a snapshot to the packaged artifact generation.
    #[must_use]
    pub fn from_generation(
        generation: &MicroVmGeneration,
        arch: &str,
        host_kernel_class: &str,
    ) -> Self {
        Self {
            firecracker_version: generation.firecracker_version.clone(),
            snapshot_format: "Full".to_string(),
            arch: arch.to_string(),
            host_kernel_class: host_kernel_class.to_string(),
            guest_kernel: generation.kernel.clone(),
            rootfs: generation.rootfs.clone(),
            guest_agent: generation.guest_agent.clone(),
            docker_version: "pinned".to_string(),
            image_set: "velnor/job-ubuntu:26.04".to_string(),
        }
    }

    /// Restore is allowed only on an exact match. Otherwise cold boot.
    ///
    /// # Errors
    /// [`MicroVmPreflightFailure`] naming the mismatched field.
    pub fn restore_or_cold_boot(&self, current: &Self) -> Result<(), MicroVmPreflightFailure> {
        let fields = [
            (
                "firecracker_version",
                &self.firecracker_version,
                &current.firecracker_version,
            ),
            (
                "snapshot_format",
                &self.snapshot_format,
                &current.snapshot_format,
            ),
            ("arch", &self.arch, &current.arch),
            (
                "host_kernel_class",
                &self.host_kernel_class,
                &current.host_kernel_class,
            ),
            ("guest_kernel", &self.guest_kernel, &current.guest_kernel),
            ("rootfs", &self.rootfs, &current.rootfs),
            ("guest_agent", &self.guest_agent, &current.guest_agent),
            (
                "docker_version",
                &self.docker_version,
                &current.docker_version,
            ),
            ("image_set", &self.image_set, &current.image_set),
        ];
        for (name, left, right) in fields {
            if left != right {
                return Err(MicroVmPreflightFailure::new(
                    "guest.snapshot",
                    format!("{name} {left} != {right}; cold boot required"),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatch_fails_closed_without_permissive_restore() {
        let snap = SnapshotIdentity::production_template("x86_64", "linux-6.1");
        let mut current = snap.clone();
        current.guest_kernel = "6.6.0".into();
        let err = snap.restore_or_cold_boot(&current).unwrap_err();
        assert_eq!(err.requirement, "guest.snapshot");
        assert!(err.to_string().contains("cold boot required"));
        assert!(err.to_string().contains("docker backend was not used"));
        snap.restore_or_cold_boot(&snap).unwrap();
    }

    #[test]
    fn generation_checksum_mismatch_never_restores() {
        let generation = crate::execution::MicroVmGeneration {
            velnor_version: "0.1.211".into(),
            firecracker_version: FIRECRACKER_VERSION.to_string(),
            jailer_version: crate::execution::JAILER_VERSION.to_string(),
            kernel_version: KERNEL_VERSION.to_string(),
            firecracker: "a".repeat(64),
            jailer: "b".repeat(64),
            kernel: "c".repeat(64),
            rootfs: "d".repeat(64),
            guest_agent: "e".repeat(64),
        };
        let snap = SnapshotIdentity::from_generation(&generation, "x86_64", "linux-6.1");
        assert_eq!(snap.guest_kernel, "c".repeat(64));
        assert_eq!(snap.rootfs, "d".repeat(64));
        let mut current = snap.clone();
        current.rootfs = "f".repeat(64);
        let err = snap.restore_or_cold_boot(&current).unwrap_err();
        assert_eq!(err.requirement, "guest.snapshot");
        assert!(err.to_string().contains("rootfs"));
        assert!(err.to_string().contains("cold boot required"));
    }
}
