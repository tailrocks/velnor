//! Golden snapshot identity. Mismatch never restores permissively.

use std::path::{Path, PathBuf};

use velnor_model::MicroVmPreflightFailure;

use super::artifacts::MicroVmGeneration;
use super::guest::KERNEL_VERSION;
use super::isolation::IsolationIdentity;
use super::HostFs;
use super::FIRECRACKER_VERSION;

/// Exact tuple a snapshot is bound to. Kernel/rootfs/agent fields are SHA-256s
/// of packaged bytes, not version labels.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotIdentity {
    /// Exact job isolation identity. Snapshots are never reusable across jobs.
    pub isolation_id: String,
    pub generation: u64,
    pub firecracker_version: String,
    pub snapshot_format: String,
    pub arch: String,
    pub host_kernel_class: String,
    pub guest_kernel: String,
    pub rootfs: String,
    pub guest_agent: String,
    pub docker_version: String,
    pub image_set: String,
    /// Firecracker CPU template. Snapshots are not portable across templates.
    #[serde(default = "none_cpu_template")]
    pub cpu_template: String,
    /// Golden snapshots are credential-free. Missing sidecar field is false.
    #[serde(default)]
    pub credential_free: bool,
}

fn none_cpu_template() -> String {
    "None".to_string()
}

/// Proof the guest is a golden (no job) VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestReady {
    pub agent_listening: bool,
    pub docker_healthy: bool,
    pub job_credentials_absent: bool,
}

impl GuestReady {
    /// # Errors
    /// Agent/Docker not ready, or a job credential is present.
    pub fn credential_free_or_err(self) -> Result<(), MicroVmPreflightFailure> {
        if !self.agent_listening {
            return Err(MicroVmPreflightFailure::new(
                "guest.snapshot",
                "guest agent is not listening; golden snapshot refused",
            ));
        }
        if !self.docker_healthy {
            return Err(MicroVmPreflightFailure::new(
                "guest.snapshot",
                "guest Docker is not healthy; golden snapshot refused",
            ));
        }
        if !self.job_credentials_absent {
            return Err(MicroVmPreflightFailure::new(
                "guest.snapshot",
                "job credentials present; golden snapshot must be credential-free",
            ));
        }
        Ok(())
    }
}

/// Sidecar next to `snapshot.mem`.
#[must_use]
pub fn identity_sidecar_path(mem: &Path) -> PathBuf {
    mem.with_extension("identity.json")
}

/// Firecracker vmstate next to `snapshot.mem`.
#[must_use]
pub fn vmstate_path(mem: &Path) -> PathBuf {
    mem.with_extension("vmstate")
}

/// Persist the identity that restore must match exactly.
///
/// # Errors
/// Write failure.
pub fn write_identity(
    fs: &mut dyn HostFs,
    mem: &Path,
    identity: &SnapshotIdentity,
) -> Result<(), MicroVmPreflightFailure> {
    let body = serde_json::to_vec_pretty(identity)
        .map_err(|error| MicroVmPreflightFailure::new("guest.snapshot", error.to_string()))?;
    fs.write(&identity_sidecar_path(mem), &body)
        .map_err(|detail| MicroVmPreflightFailure::new("guest.snapshot", detail))
}

/// Load a sidecar. Missing/unreadable is an invalidate, not a panic.
///
/// # Errors
/// Missing or invalid sidecar.
pub fn read_identity(
    fs: &dyn HostFs,
    mem: &Path,
) -> Result<SnapshotIdentity, MicroVmPreflightFailure> {
    let bytes = fs.read(&identity_sidecar_path(mem)).map_err(|detail| {
        MicroVmPreflightFailure::new("guest.snapshot", format!("sidecar missing: {detail}"))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        MicroVmPreflightFailure::new("guest.snapshot", format!("sidecar unreadable: {error}"))
    })
}

impl SnapshotIdentity {
    #[must_use]
    pub fn production_template(
        arch: &str,
        host_kernel_class: &str,
        isolation: &IsolationIdentity,
    ) -> Self {
        Self {
            isolation_id: isolation.id.clone(),
            generation: isolation.generation,
            firecracker_version: FIRECRACKER_VERSION.to_string(),
            snapshot_format: "Full".to_string(),
            arch: arch.to_string(),
            host_kernel_class: host_kernel_class.to_string(),
            guest_kernel: KERNEL_VERSION.to_string(),
            rootfs: "velnor-guest-rootfs".to_string(),
            guest_agent: env!("CARGO_PKG_VERSION").to_string(),
            docker_version: "pinned".to_string(),
            image_set: "velnor/job-ubuntu:26.04".to_string(),
            cpu_template: none_cpu_template(),
            credential_free: true,
        }
    }

    /// Bind a snapshot to the packaged artifact generation.
    #[must_use]
    pub fn from_generation(
        artifact_generation: &MicroVmGeneration,
        arch: &str,
        host_kernel_class: &str,
        isolation: &IsolationIdentity,
    ) -> Self {
        Self {
            isolation_id: isolation.id.clone(),
            generation: isolation.generation,
            firecracker_version: artifact_generation.firecracker_version.clone(),
            snapshot_format: "Full".to_string(),
            arch: arch.to_string(),
            host_kernel_class: host_kernel_class.to_string(),
            guest_kernel: artifact_generation.kernel.clone(),
            rootfs: artifact_generation.rootfs.clone(),
            guest_agent: artifact_generation.guest_agent.clone(),
            docker_version: "pinned".to_string(),
            image_set: "velnor/job-ubuntu:26.04".to_string(),
            cpu_template: none_cpu_template(),
            credential_free: true,
        }
    }

    /// Restore is allowed only on an exact match. Otherwise cold boot.
    ///
    /// # Errors
    /// [`MicroVmPreflightFailure`] naming the mismatched field.
    pub fn restore_or_cold_boot(&self, current: &Self) -> Result<(), MicroVmPreflightFailure> {
        if self.isolation_id != current.isolation_id {
            return Err(MicroVmPreflightFailure::new(
                "guest.snapshot",
                format!(
                    "isolation_id {} != {}; cold boot required",
                    self.isolation_id, current.isolation_id
                ),
            ));
        }
        if self.generation != current.generation {
            return Err(MicroVmPreflightFailure::new(
                "guest.snapshot",
                format!(
                    "generation {} != {}; cold boot required",
                    self.generation, current.generation
                ),
            ));
        }
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
            ("cpu_template", &self.cpu_template, &current.cpu_template),
        ];
        for (name, left, right) in fields {
            if left != right {
                return Err(MicroVmPreflightFailure::new(
                    "guest.snapshot",
                    format!("{name} {left} != {right}; cold boot required"),
                ));
            }
        }
        if !self.credential_free || !current.credential_free {
            return Err(MicroVmPreflightFailure::new(
                "guest.snapshot",
                "credential-bearing snapshot cannot restore; cold boot required",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatch_fails_closed_without_permissive_restore() {
        let isolation = IsolationIdentity::new("job-1", 1);
        let snap = SnapshotIdentity::production_template("x86_64", "linux-6.1", &isolation);
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
        let isolation = IsolationIdentity::new("job-1", 1);
        let snap =
            SnapshotIdentity::from_generation(&generation, "x86_64", "linux-6.1", &isolation);
        assert_eq!(snap.guest_kernel, "c".repeat(64));
        assert_eq!(snap.rootfs, "d".repeat(64));
        let mut current = snap.clone();
        current.rootfs = "f".repeat(64);
        let err = snap.restore_or_cold_boot(&current).unwrap_err();
        assert_eq!(err.requirement, "guest.snapshot");
        assert!(err.to_string().contains("rootfs"));
        assert!(err.to_string().contains("cold boot required"));
    }

    #[test]
    fn credential_bearing_snapshot_never_restores() {
        let isolation = IsolationIdentity::new("job-1", 1);
        let snap = SnapshotIdentity::production_template("x86_64", "linux-6.1", &isolation);
        let mut dirty = snap.clone();
        dirty.credential_free = false;
        let err = snap.restore_or_cold_boot(&dirty).unwrap_err();
        assert_eq!(err.requirement, "guest.snapshot");
        assert!(err.to_string().contains("credential-bearing"));
        let missing: SnapshotIdentity = serde_json::from_str(
            r#"{"isolation_id":"job-1","generation":1,"firecracker_version":"1.16.1","snapshot_format":"Full","arch":"x86_64","host_kernel_class":"linux-6.1","guest_kernel":"k","rootfs":"r","guest_agent":"a","docker_version":"pinned","image_set":"velnor/job-ubuntu:26.04"}"#,
        )
        .unwrap();
        assert!(!missing.credential_free);
        assert_eq!(missing.cpu_template, "None");
    }

    #[test]
    fn golden_create_refuses_job_credentials() {
        let err = GuestReady {
            agent_listening: true,
            docker_healthy: true,
            job_credentials_absent: false,
        }
        .credential_free_or_err()
        .unwrap_err();
        assert!(err.to_string().contains("credential-free"));
        GuestReady {
            agent_listening: true,
            docker_healthy: true,
            job_credentials_absent: true,
        }
        .credential_free_or_err()
        .unwrap();
    }
}
