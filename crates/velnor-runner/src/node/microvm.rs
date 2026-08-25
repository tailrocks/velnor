//! Explicit microVM and job-executor boundary. Production VMM is Firecracker.

use velnor_model::{JobExecutorKind, MicroVmControl, MicroVmKind};

/// Production microVM selection. Cloud Hypervisor is constructible for
/// fixtures but cannot activate on the production path.
#[must_use]
pub fn production_microvm() -> MicroVmKind {
    MicroVmKind::PRODUCTION
}

/// Production control path: Firecracker HTTP API plus jailer.
#[must_use]
pub fn production_microvm_control() -> MicroVmControl {
    MicroVmControl::PRODUCTION
}

/// Live job executor. Host Docker remains transitional until Plans 012/017.
#[must_use]
pub fn live_job_executor() -> JobExecutorKind {
    JobExecutorKind::LIVE
}

#[cfg(test)]
mod tests {
    use super::*;
    use velnor_model::{IsolationRejected, MicroVmControl, MicroVmKind};

    #[test]
    fn production_microvm_is_firecracker() {
        assert_eq!(production_microvm(), MicroVmKind::Firecracker);
        assert!(production_microvm().activate_production().is_ok());
        assert!(MicroVmKind::CloudHypervisor.activate_production().is_err());
    }

    #[test]
    fn kata_and_firecracker_containerd_are_not_production() {
        assert_eq!(
            production_microvm_control(),
            MicroVmControl::DirectApiAndJailer
        );
        assert!(production_microvm_control().activate_production().is_ok());
        assert!(MicroVmControl::KataContainers
            .activate_production()
            .is_err());
        assert!(MicroVmControl::FirecrackerContainerd
            .activate_production()
            .is_err());
    }

    #[test]
    fn live_job_executor_is_host_docker() {
        assert_eq!(live_job_executor(), JobExecutorKind::HostDocker);
        assert!(live_job_executor().activate_live().is_ok());
        assert!(JobExecutorKind::MicroVm.activate_live().is_err());
        assert!(IsolationRejected::VirtioFs.activate_production().is_err());
    }
}
