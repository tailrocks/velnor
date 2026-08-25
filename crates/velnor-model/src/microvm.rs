//! Production microVM and job-executor selection.
//!
//! Live jobs stay on host Docker until Plans 012/017 prove Build L3. This
//! module records the chosen VMM, control path, and isolation transports; it
//! is not a live Firecracker client.

use serde::{Deserialize, Serialize};

/// Official Firecracker spec. Boot/overhead numbers are cited, not measured.
/// <https://firecracker-microvm.github.io/>
pub const FIRECRACKER_SPEC_URL: &str = "https://firecracker-microvm.github.io/";
/// <https://github.com/firecracker-microvm/firecracker>
pub const FIRECRACKER_REPO_URL: &str = "https://github.com/firecracker-microvm/firecracker";

/// Firecracker's five-device model. No virtio-fs.
pub const FIRECRACKER_DEVICES: [&str; 5] = [
    "virtio-net",
    "virtio-block",
    "virtio-vsock",
    "serial",
    "i8042",
];

/// Jailer second-line isolation for a Firecracker process.
pub const JAILER_CONTROLS: [&str; 4] = ["cgroup", "namespace", "seccomp", "privilege_drop"];

/// How a job's containers run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobExecutorKind {
    /// Live transitional: Velnor → host Docker → job + service containers.
    HostDocker,
    /// Selected isolation (not live; Plans 012/017): Velnor → Firecracker/KVM →
    /// guest Linux → guest-local Docker.
    MicroVm,
}

impl JobExecutorKind {
    /// Packaged default until `execution.toml` selects otherwise.
    pub const PACKAGED_DEFAULT: Self = Self::HostDocker;
    /// Isolation backend when `[execution] backend = "microvm"`.
    pub const ISOLATION: Self = Self::MicroVm;

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostDocker => "host_docker",
            Self::MicroVm => "micro_vm",
        }
    }

    /// Both operator backends are selectable. Preflight, not this enum, is the
    /// fail-closed gate.
    ///
    /// # Errors
    /// Never: both variants are live-selectable.
    pub fn activate_live(self) -> Result<(), MicroVmNotLive> {
        let _ = self;
        Ok(())
    }
}

/// Why the MicroVM job executor cannot take live traffic yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicroVmNotLive {
    pub requested: JobExecutorKind,
}

impl std::fmt::Display for MicroVmNotLive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "job executor {} is not live; host Docker remains the named transitional backend until Plans 012 and 017",
            self.requested.as_str()
        )
    }
}

impl std::error::Error for MicroVmNotLive {}

/// Which VMM a MicroVM backend may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicroVmKind {
    /// Production selection: Rust VMM on Linux KVM.
    Firecracker,
    /// Fallback only after an estate workflow proves Firecracker's device
    /// model cannot support it. Not live, not silent.
    CloudHypervisor,
}

impl MicroVmKind {
    /// The only VMM allowed to activate as production.
    pub const PRODUCTION: Self = Self::Firecracker;

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Firecracker => "firecracker",
            Self::CloudHypervisor => "cloud_hypervisor",
        }
    }

    /// Cloud Hypervisor is not an allowed production activate.
    ///
    /// # Errors
    /// [`MicroVmNotProven`] when `self` is not [`Self::PRODUCTION`].
    pub fn activate_production(self) -> Result<(), MicroVmNotProven> {
        if self == Self::PRODUCTION {
            Ok(())
        } else {
            Err(MicroVmNotProven { requested: self })
        }
    }
}

/// Why Cloud Hypervisor cannot take production microVM traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicroVmNotProven {
    pub requested: MicroVmKind,
}

impl std::fmt::Display for MicroVmNotProven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "microVM {} is not production; estate workflow has not proven Firecracker device model insufficient ({FIRECRACKER_SPEC_URL})",
            self.requested.as_str()
        )
    }
}

impl std::error::Error for MicroVmNotProven {}

/// How Velnor starts Firecracker. Kata and firecracker-containerd are not
/// product orchestration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicroVmControl {
    /// Production: Firecracker HTTP API plus jailer.
    DirectApiAndJailer,
    /// Not a product path.
    KataContainers,
    /// Not a product path.
    FirecrackerContainerd,
}

impl MicroVmControl {
    /// The only control path allowed to activate as production.
    pub const PRODUCTION: Self = Self::DirectApiAndJailer;

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirectApiAndJailer => "direct_api_and_jailer",
            Self::KataContainers => "kata_containers",
            Self::FirecrackerContainerd => "firecracker_containerd",
        }
    }

    /// Kata Containers and firecracker-containerd cannot activate.
    ///
    /// # Errors
    /// [`MicroVmControlRejected`] when `self` is not [`Self::PRODUCTION`].
    pub fn activate_production(self) -> Result<(), MicroVmControlRejected> {
        if self == Self::PRODUCTION {
            Ok(())
        } else {
            Err(MicroVmControlRejected { requested: self })
        }
    }
}

/// Why a non-API/jailer control path cannot be production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MicroVmControlRejected {
    pub requested: MicroVmControl,
}

impl std::fmt::Display for MicroVmControlRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "microVM control {} is not a product orchestration path; Velnor starts Firecracker through its HTTP API and jailer",
            self.requested.as_str()
        )
    }
}

impl std::error::Error for MicroVmControlRejected {}

/// Production guest isolation transports. All three are the Firecracker path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestIsolation {
    ImmutableBlock,
    JobLocalWritableDisk,
    BoundedVsock,
}

impl GuestIsolation {
    pub const PRODUCTION: [Self; 3] = [
        Self::ImmutableBlock,
        Self::JobLocalWritableDisk,
        Self::BoundedVsock,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ImmutableBlock => "immutable_block",
            Self::JobLocalWritableDisk => "job_local_writable_disk",
            Self::BoundedVsock => "bounded_vsock",
        }
    }

    /// Every production isolation variant is allowed.
    pub fn activate_production(self) -> Result<(), IsolationRejected> {
        match self {
            Self::ImmutableBlock | Self::JobLocalWritableDisk | Self::BoundedVsock => Ok(()),
        }
    }
}

/// Isolation paths Firecracker production rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationRejected {
    VirtioFs,
    HostDirectoryPassthrough,
    PciPassthrough,
    Gpu,
    WindowsGuest,
    Usb,
    LegacyDeviceModel,
}

impl IsolationRejected {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VirtioFs => "virtio_fs",
            Self::HostDirectoryPassthrough => "host_directory_passthrough",
            Self::PciPassthrough => "pci_passthrough",
            Self::Gpu => "gpu",
            Self::WindowsGuest => "windows_guest",
            Self::Usb => "usb",
            Self::LegacyDeviceModel => "legacy_device_model",
        }
    }

    /// Never a production isolation path.
    ///
    /// # Errors
    /// Always [`IsolationRejected`]: none of these transports may activate.
    pub fn activate_production(self) -> Result<(), Self> {
        Err(self)
    }
}

impl std::fmt::Display for IsolationRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "isolation {} is rejected; production uses immutable block, job-local writable disk, and bounded vsock",
            self.as_str()
        )
    }
}

impl std::error::Error for IsolationRejected {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_microvm_is_firecracker() {
        assert_eq!(MicroVmKind::PRODUCTION, MicroVmKind::Firecracker);
        assert!(MicroVmKind::Firecracker.activate_production().is_ok());
        assert_eq!(MicroVmKind::Firecracker.as_str(), "firecracker");
    }

    #[test]
    fn cloud_hypervisor_is_not_proven() {
        let err = MicroVmKind::CloudHypervisor
            .activate_production()
            .expect_err("Cloud Hypervisor must not activate");
        assert_eq!(err.requested, MicroVmKind::CloudHypervisor);
        let message = err.to_string();
        assert!(message.contains("not production"), "{message}");
        assert!(
            message.contains("Firecracker device model insufficient"),
            "{message}"
        );
    }

    #[test]
    fn kata_and_firecracker_containerd_are_not_production() {
        assert_eq!(
            MicroVmControl::PRODUCTION,
            MicroVmControl::DirectApiAndJailer
        );
        assert!(MicroVmControl::DirectApiAndJailer
            .activate_production()
            .is_ok());
        assert!(MicroVmControl::KataContainers
            .activate_production()
            .is_err());
        assert!(MicroVmControl::FirecrackerContainerd
            .activate_production()
            .is_err());
        let kata = MicroVmControl::KataContainers
            .activate_production()
            .expect_err("Kata must not activate");
        assert!(
            kata.to_string()
                .contains("not a product orchestration path"),
            "{kata}"
        );
    }

    #[test]
    fn both_backends_are_live_selectable() {
        assert_eq!(
            JobExecutorKind::PACKAGED_DEFAULT,
            JobExecutorKind::HostDocker
        );
        assert!(JobExecutorKind::HostDocker.activate_live().is_ok());
        assert!(JobExecutorKind::MicroVm.activate_live().is_ok());
        assert_eq!(JobExecutorKind::ISOLATION, JobExecutorKind::MicroVm);
    }

    #[test]
    fn guest_isolation_rejects_virtio_fs() {
        for path in GuestIsolation::PRODUCTION {
            assert!(path.activate_production().is_ok(), "{}", path.as_str());
        }
        assert!(IsolationRejected::VirtioFs.activate_production().is_err());
        assert!(IsolationRejected::HostDirectoryPassthrough
            .activate_production()
            .is_err());
        assert!(IsolationRejected::PciPassthrough
            .activate_production()
            .is_err());
        assert!(IsolationRejected::Gpu.activate_production().is_err());
        assert!(IsolationRejected::WindowsGuest
            .activate_production()
            .is_err());
        assert!(IsolationRejected::Usb.activate_production().is_err());
        assert!(IsolationRejected::LegacyDeviceModel
            .activate_production()
            .is_err());
    }

    #[test]
    fn firecracker_devices_are_the_five_device_model() {
        assert_eq!(
            FIRECRACKER_DEVICES,
            [
                "virtio-net",
                "virtio-block",
                "virtio-vsock",
                "serial",
                "i8042"
            ]
        );
        assert!(!FIRECRACKER_DEVICES.contains(&"virtio-fs"));
        assert_eq!(
            JAILER_CONTROLS,
            ["cgroup", "namespace", "seccomp", "privilege_drop"]
        );
        assert_eq!(
            FIRECRACKER_SPEC_URL,
            "https://firecracker-microvm.github.io/"
        );
        assert_eq!(
            FIRECRACKER_REPO_URL,
            "https://github.com/firecracker-microvm/firecracker"
        );
    }
}
