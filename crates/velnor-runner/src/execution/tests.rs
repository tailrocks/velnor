use super::*;
use crate::executor::CommandResult;
use std::path::{Path, PathBuf};
use velnor_model::{ExecutionBackendKind, ExecutionFile};

fn seed_microvm_world(fs: &mut MemoryFs, root: &Path) {
    fs.create_dir_all(root).unwrap();
    for (name, bytes) in [
        ("firecracker", b"fc".as_slice()),
        ("jailer", b"jailer".as_slice()),
        ("vmlinux", b"kernel".as_slice()),
        ("rootfs.ext4", b"rootfs".as_slice()),
        ("velnor-guest-agent", b"agent".as_slice()),
    ] {
        fs.write(&root.join(name), bytes).unwrap();
    }
    fs.write(Path::new("/dev/kvm"), b"kvm").unwrap();
    let checksums = ArtifactChecksums {
        firecracker_version: FIRECRACKER_VERSION.to_string(),
        firecracker: hex_sha256(b"fc"),
        jailer: hex_sha256(b"jailer"),
        kernel: hex_sha256(b"kernel"),
        rootfs: hex_sha256(b"rootfs"),
        guest_agent: hex_sha256(b"agent"),
        snapshot: None,
    };
    fs.write(
        &root.join("manifest.json"),
        &serde_json::to_vec(&checksums).unwrap(),
    )
    .unwrap();
}

fn world<'a>(
    fs: &'a mut MemoryFs,
    runner: &'a mut RecordingCommands,
    api: &'a mut RecordingFirecracker,
    kvm: &'a Path,
    artifacts: &'a Path,
    docker: &'a Path,
) -> ExecutionWorld<'a> {
    ExecutionWorld {
        kvm,
        artifact_root: artifacts,
        host_docker_socket: docker,
        runner,
        firecracker: api,
        host_fs: fs,
    }
}

#[test]
fn only_docker_and_microvm_are_accepted() {
    let docker = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
    assert_eq!(docker.backend(), ExecutionBackendKind::Docker);
    let microvm = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    assert_eq!(microvm.backend(), ExecutionBackendKind::MicroVm);
    let err = ExecutionFile::parse_toml("[execution]\nbackend = \"qemu\"\n").unwrap_err();
    assert_eq!(err.field, "[execution] backend");
}

#[test]
fn load_execution_file_twice_agrees() {
    let dir = std::env::temp_dir().join(format!(
        "velnor-exec-toml-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("execution.toml"),
        "[execution]\nbackend = \"docker\"\n",
    )
    .unwrap();
    let first = load_execution_file(&dir, None).unwrap();
    let second = load_execution_file(&dir, None).unwrap();
    assert_eq!(first, second);
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn microvm_missing_kvm_does_not_invoke_host_docker() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let mut runner = RecordingCommands::default();
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm-missing");
    let artifacts = PathBuf::from("/microvm");
    let docker = PathBuf::from("/var/run/docker.sock");
    fs.write(&docker, b"socket").unwrap();
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    let error = preflight_selected(&file, &mut world).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("kvm"), "{text}");
    assert!(text.contains("docker backend was not used"), "{text}");
    assert!(runner.calls.is_empty());
    assert!(api.calls.is_empty());
    assert!(
        host_docker_executor(RecordingCommands::default(), ExecutionBackendKind::MicroVm).is_err()
    );
}

#[test]
fn docker_backend_does_not_boot_firecracker() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let docker = PathBuf::from("/var/run/docker.sock");
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands {
        next: CommandResult {
            code: 0,
            stdout: "docker".into(),
            stderr: String::new(),
        },
        ..RecordingCommands::default()
    };
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let artifacts = PathBuf::from("/microvm");
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    preflight_selected(&file, &mut world).unwrap();
    let session = open_session(&file, IsolationIdentity::new("job-docker", 1), &mut world).unwrap();
    assert_eq!(session.kind, ExecutionBackendKind::Docker);
    assert!(api.calls.is_empty());
    assert!(runner.calls.iter().any(|(program, _)| program == "docker"));
}

#[test]
fn contract_success_failure_cancel_identical_conclusions() {
    let mut docker_out = run_plan(ExecutionBackendKind::Docker, false);
    let mut micro_out = run_plan(ExecutionBackendKind::MicroVm, false);
    assert_eq!(docker_out.conclusion, "success");
    assert_eq!(docker_out.conclusion, micro_out.conclusion);
    assert_eq!(docker_out.exit_code, micro_out.exit_code);
    assert!(docker_out.masked);
    assert!(micro_out.masked);
    assert!(docker_out.cleaned);
    assert!(micro_out.cleaned);
    assert!(docker_out.command_file.is_some());
    assert_eq!(docker_out.command_file, micro_out.command_file);

    docker_out = run_cancel(ExecutionBackendKind::Docker);
    micro_out = run_cancel(ExecutionBackendKind::MicroVm);
    assert_eq!(docker_out.conclusion, "cancelled");
    assert_eq!(docker_out.conclusion, micro_out.conclusion);
}

fn run_plan(kind: ExecutionBackendKind, cancel: bool) -> super::backend::ExecutionOutcome {
    let toml = match kind {
        ExecutionBackendKind::Docker => "[execution]\nbackend = \"docker\"\n",
        ExecutionBackendKind::MicroVm => "[execution]\nbackend = \"microvm\"\n",
    };
    let file = ExecutionFile::parse_toml(toml).unwrap();
    let mut fs = MemoryFs::default();
    let docker = PathBuf::from("/var/run/docker.sock");
    fs.write(&docker, b"socket").unwrap();
    let artifacts = PathBuf::from("/microvm");
    if kind == ExecutionBackendKind::MicroVm {
        seed_microvm_world(&mut fs, &artifacts);
    }
    let mut runner = RecordingCommands {
        next: CommandResult {
            code: 0,
            stdout: "ok".into(),
            stderr: String::new(),
        },
        ..RecordingCommands::default()
    };
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    let mut session = open_session(&file, IsolationIdentity::new("job-1", 1), &mut world).unwrap();
    session.reserve(&mut world).unwrap();
    let mut plan = ValidatedPlan::example_success("job-1");
    plan.cancel_requested = cancel;
    session.prepare(&plan, &mut world).unwrap();
    session.start(&mut world).unwrap();
    if cancel {
        session.cancel(&mut world).unwrap();
    } else {
        session.execute(&plan, &mut world).unwrap();
    }
    if kind == ExecutionBackendKind::Docker {
        assert!(session.used_host_docker());
        assert!(!session.used_firecracker());
    } else {
        assert!(!session.used_host_docker());
        assert!(session.used_firecracker());
    }
    let mut outcome = session.collect().unwrap();
    let torn = session.teardown(&mut world).unwrap();
    outcome.cleaned = torn.cleaned;
    outcome
}

fn run_cancel(kind: ExecutionBackendKind) -> super::backend::ExecutionOutcome {
    run_plan(kind, true)
}

#[test]
fn collect_while_live_is_forbidden_and_snapshot_mismatch_fails_closed() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let docker = PathBuf::from("/var/run/docker.sock");
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands {
        next: CommandResult {
            code: 0,
            stdout: "ok".into(),
            stderr: String::new(),
        },
        ..RecordingCommands::default()
    };
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let artifacts = PathBuf::from("/microvm");
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    let mut session = open_session(&file, IsolationIdentity::new("job-2", 1), &mut world).unwrap();
    assert!(session.collect().is_err());
    api.fail_load_snapshot = true;
    let err = restore_or_cold_boot(
        &mut api,
        Path::new("/snap.mem"),
        Path::new("/snap.vmstate"),
        "0.0.0",
    )
    .unwrap_err();
    assert_eq!(err.requirement, "guest.snapshot");
    assert!(err.to_string().contains("docker backend was not used"));
}
