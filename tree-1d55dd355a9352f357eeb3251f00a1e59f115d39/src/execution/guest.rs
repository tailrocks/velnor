//! Reproducible guest kernel/rootfs spec. Bytes are built on Linux CI.

use velnor_model::MicroVmPreflightFailure;

/// Pinned guest kernel (Firecracker-tested 6.1 LTS class).
pub const KERNEL_VERSION: &str = "6.1.102";
pub const KERNEL_TARBALL: &str =
    "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.1.102.tar.xz";
/// kernel.org sha256 of [`KERNEL_TARBALL`].
pub const KERNEL_TARBALL_SHA256: &str =
    "1ba5f93b411ead7587fe48b2eec6c656f6796d31f5e406d236913c77512497ec";
/// Immutable Ubuntu archive snapshot used to make rootfs package selection
/// reproducible across release rebuilds.
pub const UBUNTU_SNAPSHOT: &str = "https://snapshot.ubuntu.com/ubuntu/20260826T000000Z";

/// Required kconfig tokens. Undocumented extras in the fragment are rejected
/// only when they enable forbidden features.
pub const REQUIRED_KCONFIG: &[&str] = &[
    "CONFIG_VIRTIO=y",
    "CONFIG_VIRTIO_BLK=y",
    "CONFIG_VIRTIO_NET=y",
    "CONFIG_VIRTIO_VSOCKETS=y",
    "CONFIG_VIRTIO_MMIO=y",
    "CONFIG_OVERLAY_FS=y",
    "CONFIG_KVM_GUEST=y",
    "CONFIG_BINFMT_MISC=y",
    "CONFIG_NETFILTER=y",
    "CONFIG_NF_TABLES=y",
];

pub const FORBIDDEN_KCONFIG: &[&str] = &[
    "CONFIG_VIRTIO_FS=y",
    "CONFIG_FUSE_FS=y",
    "CONFIG_PCI=y",
    "CONFIG_USB=y",
];

pub const ROOTFS_PACKAGES: &[&str] = &[
    "containerd",
    "runc",
    "docker-engine",
    "buildkit",
    "iptables",
    "nftables",
    "ca-certificates",
    "iproute2",
];

pub const FORBIDDEN_PACKAGES: &[&str] = &["openssh-server", "openssh-sftp-server"];

/// Required options after `tinyconfig`+`olddefconfig` for `arch`.
///
/// `CONFIG_KVM_GUEST` exists only on x86. aarch64 uses `CONFIG_PARAVIRT`.
#[must_use]
pub fn required_kconfig_for_arch(arch: &str) -> Vec<&'static str> {
    match arch {
        "aarch64" | "arm64" => REQUIRED_KCONFIG
            .iter()
            .copied()
            .filter(|option| *option != "CONFIG_KVM_GUEST=y")
            .chain(std::iter::once("CONFIG_PARAVIRT=y"))
            .collect(),
        _ => REQUIRED_KCONFIG.to_vec(),
    }
}

/// Validate a kernel.config fragment against the production allow/deny lists.
///
/// # Errors
/// Missing required options or enabled forbidden options.
pub fn validate_kernel_config(text: &str) -> Result<(), MicroVmPreflightFailure> {
    validate_kernel_options(text, REQUIRED_KCONFIG)
}

/// Validate a built `.config` for `arch`. Unknown-on-arch symbols are not required.
///
/// # Errors
/// Missing required options or enabled forbidden options.
pub fn validate_built_kernel_config(text: &str, arch: &str) -> Result<(), MicroVmPreflightFailure> {
    let required = required_kconfig_for_arch(arch);
    validate_kernel_options(text, &required)
}

fn validate_kernel_options(text: &str, required: &[&str]) -> Result<(), MicroVmPreflightFailure> {
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|option| !text.lines().any(|line| line.trim() == *option))
        .collect();
    if !missing.is_empty() {
        return Err(MicroVmPreflightFailure::new(
            "guest.kernel",
            format!("kernel.config missing {}", missing.join(", ")),
        ));
    }
    for forbidden in FORBIDDEN_KCONFIG {
        if text.lines().any(|line| line.trim() == *forbidden) {
            return Err(MicroVmPreflightFailure::new(
                "guest.kernel",
                format!("kernel.config enables forbidden {forbidden}"),
            ));
        }
    }
    if text.contains("virtio-fs") || text.contains("CONFIG_VIRTIO_FS=y") {
        return Err(MicroVmPreflightFailure::new(
            "guest.kernel",
            "virtio-fs is forbidden; isolation is virtio-block + vsock",
        ));
    }
    Ok(())
}

/// Validate rootfs package list.
///
/// # Errors
/// SSH server or missing Docker/containerd/BuildKit.
pub fn validate_rootfs_packages(packages: &[&str]) -> Result<(), MicroVmPreflightFailure> {
    for required in ["containerd", "runc", "docker-engine", "buildkit"] {
        if !packages.contains(&required) {
            return Err(MicroVmPreflightFailure::new(
                "guest.rootfs",
                format!("rootfs missing {required}"),
            ));
        }
    }
    for forbidden in FORBIDDEN_PACKAGES {
        if packages.contains(forbidden) {
            return Err(MicroVmPreflightFailure::new(
                "guest.rootfs",
                format!("rootfs must not contain {forbidden} (no SSH in production)"),
            ));
        }
    }
    Ok(())
}

/// Parse `microvm/guest.toml` kernel_version.
///
/// # Errors
/// Missing or mismatched kernel version.
pub fn validate_guest_toml(text: &str) -> Result<(), MicroVmPreflightFailure> {
    if !text.contains(&format!("kernel_version = \"{KERNEL_VERSION}\"")) {
        return Err(MicroVmPreflightFailure::new(
            "guest.spec",
            format!("guest.toml must pin kernel_version {KERNEL_VERSION}"),
        ));
    }
    if !text.contains(&format!(
        "kernel_tarball_sha256 = \"{KERNEL_TARBALL_SHA256}\""
    )) {
        return Err(MicroVmPreflightFailure::new(
            "guest.spec",
            "guest.toml must pin kernel_tarball_sha256",
        ));
    }
    if !text.contains(&format!("ubuntu_snapshot = \"{UBUNTU_SNAPSHOT}\"")) {
        return Err(MicroVmPreflightFailure::new(
            "guest.spec",
            "guest.toml must pin ubuntu_snapshot",
        ));
    }
    if !text.contains("no_sshd = true") {
        return Err(MicroVmPreflightFailure::new(
            "guest.spec",
            "guest.toml must set no_sshd = true",
        ));
    }
    if !text.contains("no_host_docker_socket = true") {
        return Err(MicroVmPreflightFailure::new(
            "guest.spec",
            "guest.toml must set no_host_docker_socket = true",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_kernel_config_is_production() {
        let text = include_str!("../../../../microvm/kernel.config");
        validate_kernel_config(text).unwrap();
    }

    #[test]
    fn shipped_guest_toml_is_production() {
        let text = include_str!("../../../../microvm/guest.toml");
        validate_guest_toml(text).unwrap();
        validate_rootfs_packages(ROOTFS_PACKAGES).unwrap();
    }

    #[test]
    fn virtio_fs_and_sshd_fail_closed() {
        let mut text = REQUIRED_KCONFIG.join("\n");
        text.push_str("\nCONFIG_VIRTIO_FS=y\n");
        let err = validate_kernel_config(&text).unwrap_err();
        assert_eq!(err.requirement, "guest.kernel");
        assert!(err.to_string().contains("CONFIG_VIRTIO_FS=y"));
        let err = validate_rootfs_packages(&[
            "containerd",
            "runc",
            "docker-engine",
            "buildkit",
            "openssh-server",
        ])
        .unwrap_err();
        assert_eq!(err.requirement, "guest.rootfs");
        assert!(err.to_string().contains("docker backend was not used"));
    }

    #[test]
    fn aarch64_built_config_requires_paravirt_not_kvm_guest() {
        let mut lines: Vec<&str> = REQUIRED_KCONFIG
            .iter()
            .copied()
            .filter(|option| *option != "CONFIG_KVM_GUEST=y")
            .collect();
        lines.push("CONFIG_PARAVIRT=y");
        let text = lines.join("\n");
        validate_built_kernel_config(&text, "aarch64").unwrap();
        let err = validate_kernel_config(&text).unwrap_err();
        assert!(err.to_string().contains("CONFIG_KVM_GUEST=y"));
        let err = validate_built_kernel_config(&text.replace("CONFIG_PARAVIRT=y", ""), "aarch64")
            .unwrap_err();
        assert!(err.to_string().contains("CONFIG_PARAVIRT=y"));
    }

    #[test]
    fn x86_built_config_requires_kvm_guest() {
        let text = REQUIRED_KCONFIG
            .iter()
            .copied()
            .filter(|option| *option != "CONFIG_KVM_GUEST=y")
            .collect::<Vec<_>>()
            .join("\n");
        let err = validate_built_kernel_config(&text, "x86_64").unwrap_err();
        assert!(err.to_string().contains("CONFIG_KVM_GUEST=y"));
    }

    #[test]
    fn missing_required_options_are_all_reported() {
        let err = validate_kernel_config("CONFIG_VIRTIO=y\n").unwrap_err();
        assert!(err.to_string().contains("CONFIG_VIRTIO_NET=y"));
        assert!(err.to_string().contains("CONFIG_VIRTIO_BLK=y"));
    }
}
