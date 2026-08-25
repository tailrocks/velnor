//! Reproducible guest kernel and rootfs. Linux CI produces the bytes.

use std::path::{Path, PathBuf};

use velnor_model::MicroVmPreflightFailure;

use super::artifacts::{hex_sha256, ArtifactChecksums, FIRECRACKER_VERSION, JAILER_VERSION};
use super::guest::{
    validate_built_kernel_config, validate_guest_toml, validate_kernel_config,
    validate_rootfs_packages, KERNEL_TARBALL_SHA256, KERNEL_VERSION, ROOTFS_PACKAGES,
};
use super::HostFs;
use crate::executor::{CommandResult, CommandRunner};

/// Boot options and kconfig parents merged onto the isolation fragment.
///
/// `tinyconfig` is allnoconfig. Menu parents without `default y` (notably
/// `CONFIG_NETDEVICES`) stay n, and `olddefconfig` then drops children such as
/// `CONFIG_VIRTIO_NET`. `CONFIG_HYPERVISOR_GUEST` is x86-only; aarch64 drops it.
pub const BOOT_KCONFIG: &[&str] = &[
    "CONFIG_EXT4_FS=y",
    "CONFIG_DEVTMPFS=y",
    "CONFIG_DEVTMPFS_MOUNT=y",
    "CONFIG_PROC_FS=y",
    "CONFIG_SYSFS=y",
    "CONFIG_TMPFS=y",
    "CONFIG_UNIX=y",
    "CONFIG_NET=y",
    "CONFIG_INET=y",
    "CONFIG_BLOCK=y",
    "CONFIG_BLK_DEV=y",
    "CONFIG_NETDEVICES=y",
    "CONFIG_NET_CORE=y",
    "CONFIG_VIRTIO_MENU=y",
    "CONFIG_VSOCKETS=y",
    "CONFIG_HYPERVISOR_GUEST=y",
    "CONFIG_PARAVIRT=y",
    "CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y",
    "CONFIG_VIRTIO_CONSOLE=y",
    "CONFIG_PRINTK=y",
    "CONFIG_TTY=y",
    "CONFIG_SERIAL_8250=y",
    "CONFIG_SERIAL_8250_CONSOLE=y",
    "CONFIG_BINFMT_ELF=y",
    "CONFIG_SHMEM=y",
    "CONFIG_CGROUPS=y",
    "CONFIG_MEMCG=y",
    "CONFIG_BLK_CGROUP=y",
    "CONFIG_CGROUP_SCHED=y",
    "CONFIG_CGROUP_PIDS=y",
    "CONFIG_CGROUP_FREEZER=y",
    "CONFIG_CGROUP_DEVICE=y",
    "CONFIG_CGROUP_CPUACCT=y",
    "CONFIG_CPUSETS=y",
    "CONFIG_NAMESPACES=y",
    "CONFIG_NET_NS=y",
    "CONFIG_PID_NS=y",
    "CONFIG_USER_NS=y",
    "CONFIG_UTS_NS=y",
    "CONFIG_IPC_NS=y",
    "CONFIG_BRIDGE=y",
    "CONFIG_VETH=y",
    "CONFIG_PACKET=y",
    "CONFIG_NETFILTER_XTABLES=y",
    "CONFIG_NF_CONNTRACK=y",
    "CONFIG_IP_NF_IPTABLES=y",
    "CONFIG_IP_NF_FILTER=y",
    "CONFIG_IP_NF_NAT=y",
    "CONFIG_NF_NAT=y",
    "CONFIG_KEYS=y",
    "CONFIG_POSIX_MQUEUE=y",
    "CONFIG_SECCOMP=y",
    "CONFIG_SECCOMP_FILTER=y",
];

/// Guest image architecture. Matches Firecracker release tarball names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestArch {
    X86_64,
    Aarch64,
}

impl GuestArch {
    /// # Errors
    /// Unknown architecture name.
    pub fn parse(raw: &str) -> Result<Self, MicroVmPreflightFailure> {
        match raw {
            "x86_64" | "amd64" => Ok(Self::X86_64),
            "aarch64" | "arm64" => Ok(Self::Aarch64),
            other => Err(MicroVmPreflightFailure::new(
                "guest.arch",
                format!("unsupported guest arch {other}"),
            )),
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }

    #[must_use]
    pub fn make_arch(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "arm64",
        }
    }
}

/// Isolation fragment plus boot options. Forbidden features stay unset.
#[must_use]
pub fn merged_kernel_fragment(isolation_fragment: &str) -> String {
    let mut lines: Vec<String> = isolation_fragment
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect();
    for option in BOOT_KCONFIG {
        if !lines.iter().any(|line| line == option) {
            lines.push((*option).to_string());
        }
    }
    for forbidden in super::guest::FORBIDDEN_KCONFIG {
        let name = forbidden.trim_end_matches("=y");
        lines.retain(|line| line != forbidden && !line.starts_with(&format!("{name}=")));
        lines.push(format!("# {name} is not set"));
    }
    lines.sort();
    lines.dedup();
    let mut out = String::from("# Merged Velnor Firecracker guest kernel fragment.\n");
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Verify kernel.org tarball bytes against the pinned digest.
///
/// # Errors
/// Digest mismatch.
pub fn verify_kernel_tarball(bytes: &[u8]) -> Result<(), MicroVmPreflightFailure> {
    let digest = hex_sha256(bytes);
    if digest == KERNEL_TARBALL_SHA256 {
        Ok(())
    } else {
        Err(MicroVmPreflightFailure::new(
            "guest.kernel",
            format!(
                "linux-{KERNEL_VERSION} tarball digest {digest} != pin {KERNEL_TARBALL_SHA256}"
            ),
        ))
    }
}

/// Fail closed unless every packaged microVM file exists and hashes.
///
/// # Errors
/// Missing file, UNSET pin, or checksum mismatch against `expected`.
pub fn stage_release_dir(
    root: &Path,
    fs: &mut dyn HostFs,
    expected: Option<&ArtifactChecksums>,
) -> Result<ArtifactChecksums, MicroVmPreflightFailure> {
    let mut hashes = Vec::new();
    for (field, name) in [
        ("firecracker", "firecracker"),
        ("jailer", "jailer"),
        ("kernel", "vmlinux"),
        ("rootfs", "rootfs.ext4"),
        ("guest_agent", "velnor-guest-agent"),
    ] {
        let path = root.join(name);
        let bytes = fs.read(&path).map_err(|detail| {
            MicroVmPreflightFailure::new(
                match field {
                    "kernel" => "guest.kernel",
                    "rootfs" => "guest.rootfs",
                    "guest_agent" => "guest.agent",
                    other => other,
                },
                format!("release missing {}: {detail}", path.display()),
            )
        })?;
        hashes.push((field, hex_sha256(&bytes)));
    }
    let staged = ArtifactChecksums {
        firecracker_version: FIRECRACKER_VERSION.to_string(),
        jailer_version: JAILER_VERSION.to_string(),
        firecracker: hashes[0].1.clone(),
        jailer: hashes[1].1.clone(),
        kernel: hashes[2].1.clone(),
        rootfs: hashes[3].1.clone(),
        guest_agent: hashes[4].1.clone(),
        snapshot: None,
    };
    if let Some(expected) = expected {
        for (field, got, want) in [
            (
                "firecracker",
                staged.firecracker.as_str(),
                expected.firecracker.as_str(),
            ),
            ("jailer", staged.jailer.as_str(), expected.jailer.as_str()),
            ("kernel", staged.kernel.as_str(), expected.kernel.as_str()),
            ("rootfs", staged.rootfs.as_str(), expected.rootfs.as_str()),
            (
                "guest_agent",
                staged.guest_agent.as_str(),
                expected.guest_agent.as_str(),
            ),
        ] {
            if got != want {
                return Err(MicroVmPreflightFailure::new(
                    "artifacts.checksum",
                    format!("{field} staged {got} != expected {want}"),
                ));
            }
        }
    }
    let body = serde_json::to_vec_pretty(&staged)
        .map_err(|error| MicroVmPreflightFailure::new("artifacts.manifest", error.to_string()))?;
    fs.write(&root.join("manifest.json"), &body)
        .map_err(|detail| MicroVmPreflightFailure::new("artifacts.manifest", detail))?;
    Ok(staged)
}

/// Build from pinned spec using the process command runner.
///
/// # Errors
/// See [`build_guest_image`].
pub fn build_guest_image_cli(
    work_dir: &Path,
    output_dir: &Path,
    arch: GuestArch,
    kernel_tarball: &[u8],
    guest_agent: Option<&[u8]>,
) -> Result<(PathBuf, PathBuf), MicroVmPreflightFailure> {
    let mut runner = crate::executor::ProcessCommandRunner;
    build_guest_image(
        &mut runner,
        GuestImageRequest {
            work_dir,
            output_dir,
            arch,
            kernel_tarball,
            isolation_fragment: include_str!("../../../../microvm/kernel.config"),
            guest_toml: include_str!("../../../../microvm/guest.toml"),
            guest_agent,
        },
    )
}

/// Inputs for [`build_guest_image`].
pub struct GuestImageRequest<'a> {
    pub work_dir: &'a Path,
    pub output_dir: &'a Path,
    pub arch: GuestArch,
    pub kernel_tarball: &'a [u8],
    pub isolation_fragment: &'a str,
    pub guest_toml: &'a str,
    pub guest_agent: Option<&'a [u8]>,
}

/// Linux-only kernel+rootfs build from the pinned tarball and spec.
///
/// # Errors
/// Non-Linux host, command failure, or spec violation.
pub fn build_guest_image(
    runner: &mut dyn CommandRunner,
    request: GuestImageRequest<'_>,
) -> Result<(PathBuf, PathBuf), MicroVmPreflightFailure> {
    if !cfg!(target_os = "linux") {
        return Err(MicroVmPreflightFailure::new(
            "guest.kernel",
            "guest image build requires Linux; macOS cannot produce kernel/rootfs bytes",
        ));
    }
    let GuestImageRequest {
        work_dir,
        output_dir,
        arch,
        kernel_tarball,
        isolation_fragment,
        guest_toml,
        guest_agent,
    } = request;
    verify_kernel_tarball(kernel_tarball)?;
    validate_kernel_config(isolation_fragment)?;
    validate_guest_toml(guest_toml)?;
    validate_rootfs_packages(ROOTFS_PACKAGES)?;
    let fragment = merged_kernel_fragment(isolation_fragment);
    validate_kernel_config(&fragment)?;

    let src = work_dir.join(format!("linux-{KERNEL_VERSION}"));
    let tarball_path = work_dir.join(format!("linux-{KERNEL_VERSION}.tar.xz"));
    std::fs::create_dir_all(work_dir).map_err(|error| {
        MicroVmPreflightFailure::new("guest.kernel", format!("create work dir: {error}"))
    })?;
    std::fs::write(&tarball_path, kernel_tarball).map_err(|error| {
        MicroVmPreflightFailure::new("guest.kernel", format!("write tarball: {error}"))
    })?;
    run(
        runner,
        "tar",
        &[
            "-xJf".into(),
            tarball_path.display().to_string(),
            "-C".into(),
            work_dir.display().to_string(),
        ],
        "guest.kernel",
    )?;

    let fragment_path = src.join("velnor.fragment");
    std::fs::write(&fragment_path, fragment.as_bytes()).map_err(|error| {
        MicroVmPreflightFailure::new("guest.kernel", format!("write fragment: {error}"))
    })?;
    run(
        runner,
        "make",
        &make_kernel_args(src.as_path(), arch, &["tinyconfig"]),
        "guest.kernel",
    )?;
    run(
        runner,
        "bash",
        &[
            "-lc".into(),
            format!(
                "cd {} && ./scripts/kconfig/merge_config.sh -m .config velnor.fragment",
                src.display()
            ),
        ],
        "guest.kernel",
    )?;
    run(
        runner,
        "make",
        &make_kernel_args(src.as_path(), arch, &["olddefconfig"]),
        "guest.kernel",
    )?;
    let config_text = std::fs::read_to_string(src.join(".config")).map_err(|error| {
        MicroVmPreflightFailure::new("guest.kernel", format!("read .config: {error}"))
    })?;
    if let Err(error) = validate_built_kernel_config(&config_text, arch.as_str()) {
        let dump = work_dir.join("kernel.config.rejected");
        let _ = std::fs::write(&dump, config_text.as_bytes());
        return Err(error);
    }
    let jobs = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(2)
        .to_string();
    run(
        runner,
        "make",
        &make_kernel_args(src.as_path(), arch, &["-j", jobs.as_str(), "vmlinux"]),
        "guest.kernel",
    )?;

    std::fs::create_dir_all(output_dir).map_err(|error| {
        MicroVmPreflightFailure::new("guest.kernel", format!("create output: {error}"))
    })?;
    let vmlinux_out = output_dir.join("vmlinux");
    std::fs::copy(src.join("vmlinux"), &vmlinux_out).map_err(|error| {
        MicroVmPreflightFailure::new("guest.kernel", format!("copy vmlinux: {error}"))
    })?;

    let rootfs_out = output_dir.join("rootfs.ext4");
    build_rootfs(runner, work_dir, &rootfs_out, arch, guest_agent)?;
    Ok((vmlinux_out, rootfs_out))
}

fn cross_compile(arch: GuestArch) -> Option<&'static str> {
    match (std::env::consts::ARCH, arch) {
        ("x86_64", GuestArch::Aarch64) => Some("aarch64-linux-gnu-"),
        ("aarch64", GuestArch::X86_64) => Some("x86_64-linux-gnu-"),
        _ => None,
    }
}

/// Fixed identity so `vmlinux` bytes do not include wall-clock or builder host.
fn make_kernel_args(src: &Path, arch: GuestArch, extra: &[&str]) -> Vec<String> {
    let mut args = vec![
        "-C".into(),
        src.display().to_string(),
        format!("ARCH={}", arch.make_arch()),
        "KBUILD_BUILD_USER=velnor".into(),
        "KBUILD_BUILD_HOST=velnor-guest".into(),
        "KBUILD_BUILD_TIMESTAMP=1970-01-01".into(),
    ];
    if let Some(cross) = cross_compile(arch) {
        args.push(format!("CROSS_COMPILE={cross}"));
    }
    args.extend(extra.iter().map(|value| (*value).to_string()));
    args
}

fn build_rootfs(
    runner: &mut dyn CommandRunner,
    work_dir: &Path,
    output: &Path,
    arch: GuestArch,
    guest_agent: Option<&[u8]>,
) -> Result<(), MicroVmPreflightFailure> {
    let tree = work_dir.join("rootfs-tree");
    let includes = ROOTFS_PACKAGES
        .iter()
        .map(|package| debian_package(package).to_string())
        .collect::<Vec<_>>()
        .join(",");
    let deb_arch = match arch {
        GuestArch::X86_64 => "amd64",
        GuestArch::Aarch64 => "arm64",
    };
    let mmdebstrap = runner.run(
        "env",
        &[
            "SOURCE_DATE_EPOCH=0".into(),
            "mmdebstrap".into(),
            "--variant=minbase".into(),
            format!("--include={includes}"),
            format!("--architectures={deb_arch}"),
            "--components=main,universe".into(),
            "noble".into(),
            tree.display().to_string(),
        ],
    );
    match mmdebstrap {
        Ok(result) if result.code == 0 => {}
        Ok(result) => {
            return Err(MicroVmPreflightFailure::new(
                "guest.rootfs",
                format!("mmdebstrap exited {}: {}", result.code, result.stderr),
            ));
        }
        Err(_) => {
            run(
                runner,
                "env",
                &[
                    "SOURCE_DATE_EPOCH=0".into(),
                    "debootstrap".into(),
                    "--variant=minbase".into(),
                    format!("--include={includes}"),
                    format!("--arch={deb_arch}"),
                    "noble".into(),
                    tree.display().to_string(),
                ],
                "guest.rootfs",
            )?;
        }
    }
    if tree.join("usr/sbin/sshd").is_file() {
        return Err(MicroVmPreflightFailure::new(
            "guest.rootfs",
            "rootfs contains sshd; production guest has no SSH server",
        ));
    }
    if let Some(bytes) = guest_agent {
        let dest = tree.join("usr/bin/velnor-guest-agent");
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                MicroVmPreflightFailure::new("guest.agent", format!("create bin: {error}"))
            })?;
        }
        std::fs::write(&dest, bytes).map_err(|error| {
            MicroVmPreflightFailure::new("guest.agent", format!("write guest-agent: {error}"))
        })?;
    }
    let init = tree.join("init");
    std::fs::write(
        &init,
        b"#!/bin/sh\nset -e\nmount -t proc proc /proc || true\nmount -t sysfs sys /sys || true\nmount -t devtmpfs dev /dev || true\nif command -v dockerd >/dev/null 2>&1; then dockerd &\nfi\nexec /usr/bin/velnor-guest-agent\n",
    )
    .map_err(|error| MicroVmPreflightFailure::new("guest.rootfs", format!("write init: {error}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&init, std::fs::Permissions::from_mode(0o755)).map_err(
            |error| MicroVmPreflightFailure::new("guest.rootfs", format!("chmod init: {error}")),
        )?;
    }
    run(
        runner,
        "env",
        &[
            "SOURCE_DATE_EPOCH=0".into(),
            "mke2fs".into(),
            "-t".into(),
            "ext4".into(),
            "-U".into(),
            "00000000-0000-0000-0000-000000000001".into(),
            "-L".into(),
            "velnor-guest".into(),
            "-E".into(),
            "hash_seed=00000000-0000-0000-0000-000000000002".into(),
            "-d".into(),
            tree.display().to_string(),
            output.display().to_string(),
            "1024m".into(),
        ],
        "guest.rootfs",
    )?;
    Ok(())
}

fn debian_package(spec: &str) -> &str {
    match spec {
        "docker-engine" => "docker.io",
        // Ubuntu has no `buildkit` binary package; BuildKit ships as docker-buildx.
        "buildkit" => "docker-buildx",
        other => other,
    }
}

fn run(
    runner: &mut dyn CommandRunner,
    program: &str,
    args: &[String],
    requirement: &'static str,
) -> Result<CommandResult, MicroVmPreflightFailure> {
    let result = runner
        .run(program, args)
        .map_err(|error| MicroVmPreflightFailure::new(requirement, error.to_string()))?;
    if result.code != 0 {
        return Err(MicroVmPreflightFailure::new(
            requirement,
            format!("{program} exited {}: {}", result.code, result.stderr),
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{MemoryFs, RecordingCommands};
    use crate::executor::CommandResult;

    #[test]
    fn kbuild_identity_is_fixed() {
        let args = make_kernel_args(Path::new("/k"), GuestArch::Aarch64, &["vmlinux"]);
        assert!(args.iter().any(|arg| arg == "KBUILD_BUILD_USER=velnor"));
        assert!(args
            .iter()
            .any(|arg| arg == "KBUILD_BUILD_HOST=velnor-guest"));
        assert!(args
            .iter()
            .any(|arg| arg == "KBUILD_BUILD_TIMESTAMP=1970-01-01"));
    }

    #[test]
    fn debian_package_maps_engine_and_buildkit() {
        assert_eq!(debian_package("docker-engine"), "docker.io");
        assert_eq!(debian_package("buildkit"), "docker-buildx");
        assert_eq!(debian_package("containerd"), "containerd");
        assert_eq!(debian_package("runc"), "runc");
    }

    #[test]
    fn kernel_tarball_mismatch_fails_closed() {
        let error = verify_kernel_tarball(b"not-the-kernel").unwrap_err();
        assert_eq!(error.requirement, "guest.kernel");
        assert!(error.to_string().contains(KERNEL_TARBALL_SHA256));
    }

    #[test]
    fn merged_fragment_keeps_isolation_and_forbids_virtio_fs() {
        let merged = merged_kernel_fragment(include_str!("../../../../microvm/kernel.config"));
        validate_kernel_config(&merged).unwrap();
        assert!(merged.contains("CONFIG_EXT4_FS=y"));
        assert!(merged.contains("CONFIG_NETDEVICES=y"));
        assert!(merged.contains("CONFIG_NET_CORE=y"));
        assert!(merged.contains("CONFIG_VIRTIO_MENU=y"));
        assert!(merged.contains("CONFIG_VSOCKETS=y"));
        assert!(merged.contains("CONFIG_PARAVIRT=y"));
        assert!(merged.contains("# CONFIG_VIRTIO_FS is not set"));
        assert!(merged.contains("# CONFIG_PCI is not set"));
        assert!(!merged.contains("CONFIG_PCI=y"));
    }

    #[test]
    fn stage_fails_when_kernel_or_rootfs_missing() {
        let mut fs = MemoryFs::default();
        let root = PathBuf::from("/release/microvm");
        fs.write(&root.join("firecracker"), b"fc").unwrap();
        fs.write(&root.join("jailer"), b"jailer").unwrap();
        fs.write(&root.join("velnor-guest-agent"), b"agent")
            .unwrap();
        let error = stage_release_dir(&root, &mut fs, None).unwrap_err();
        assert_eq!(error.requirement, "guest.kernel");
        assert!(error.to_string().contains("vmlinux"));
    }

    #[test]
    fn stage_writes_manifest_and_rejects_hash_drift() {
        let mut fs = MemoryFs::default();
        let root = PathBuf::from("/release/microvm");
        fs.write(&root.join("firecracker"), b"fc").unwrap();
        fs.write(&root.join("jailer"), b"jailer").unwrap();
        fs.write(&root.join("vmlinux"), b"kernel").unwrap();
        fs.write(&root.join("rootfs.ext4"), b"rootfs").unwrap();
        fs.write(&root.join("velnor-guest-agent"), b"agent")
            .unwrap();
        let staged = stage_release_dir(&root, &mut fs, None).unwrap();
        assert_eq!(staged.kernel, hex_sha256(b"kernel"));
        assert_eq!(staged.rootfs, hex_sha256(b"rootfs"));
        let mut expected = staged.clone();
        expected.kernel = "0".repeat(64);
        let error = stage_release_dir(&root, &mut fs, Some(&expected)).unwrap_err();
        assert_eq!(error.requirement, "artifacts.checksum");
    }

    #[test]
    fn build_fails_closed_off_linux_without_invoking_make() {
        let mut runner = RecordingCommands {
            next: CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            ..RecordingCommands::default()
        };
        let error = build_guest_image(
            &mut runner,
            GuestImageRequest {
                work_dir: Path::new("/work"),
                output_dir: Path::new("/out"),
                arch: GuestArch::Aarch64,
                kernel_tarball: b"tarball",
                isolation_fragment: include_str!("../../../../microvm/kernel.config"),
                guest_toml: include_str!("../../../../microvm/guest.toml"),
                guest_agent: None,
            },
        )
        .unwrap_err();
        if cfg!(target_os = "linux") {
            assert_eq!(error.requirement, "guest.kernel");
        } else {
            assert_eq!(error.requirement, "guest.kernel");
            assert!(error.to_string().contains("requires Linux"));
            assert!(runner.calls.is_empty());
        }
    }
}
