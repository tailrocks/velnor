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
        jailer_version: JAILER_VERSION.to_string(),
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
        vsock: None,
        docker_engine: None,
        allow_inline_guest_plan: true,
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

#[test]
fn contract_timeout_and_failure_match_across_backends() {
    for fail in [false, true] {
        let mut docker_plan = ValidatedPlan::example_success("job-x");
        docker_plan.fail = fail;
        docker_plan.timeout_ms = if fail { 60_000 } else { 0 };
        let docker = run_custom(ExecutionBackendKind::Docker, docker_plan.clone());
        let micro = run_custom(ExecutionBackendKind::MicroVm, docker_plan);
        assert_eq!(docker.conclusion, micro.conclusion);
        assert!(docker.cleaned && micro.cleaned);
    }
}

fn run_custom(kind: ExecutionBackendKind, plan: ValidatedPlan) -> super::backend::ExecutionOutcome {
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
    crate::execution::run_validated_job(
        &file,
        IsolationIdentity::new("job-x", 1),
        &plan,
        &mut world,
    )
    .unwrap()
}

#[test]
fn faults_fail_closed_without_host_docker_or_sibling_teardown() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = PathBuf::from("/var/run/docker.sock");
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands {
        next: CommandResult {
            code: 0,
            stdout: String::new(),
            stderr: "jailer killed".into(),
        },
        fail_spawn: Some("jailer killed".into()),
        ..RecordingCommands::default()
    };
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    let mut session =
        open_session(&file, IsolationIdentity::new("job-fault", 1), &mut world).unwrap();
    session.reserve(&mut world).unwrap();
    let err = session
        .prepare(&ValidatedPlan::example_success("job-fault"), &mut world)
        .unwrap_err();
    assert!(err.to_string().contains("jailer"), "{err}");
    assert!(!session.used_host_docker());

    let victim = IsolationResources::for_identity(
        IsolationIdentity::new("job-victim", 1),
        Path::new("/run"),
    );
    let sibling = IsolationIdentity::new("job-sibling", 2);
    assert!(crate::execution::teardown_is_exact(&victim, &sibling));

    let snap = SnapshotIdentity::production_template("x86_64", "linux-6.1");
    let mut other = snap.clone();
    other.firecracker_version = "0.0.0".into();
    assert!(snap.restore_or_cold_boot(&other).is_err());

    let stale = IsolationIdentity::new("job-stale", 1);
    let live = IsolationIdentity::new("job-stale", 2);
    assert_ne!(stale.as_jailer_id(), live.as_jailer_id());
}

#[test]
fn snapshot_checksum_mismatch_cold_boots() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let generation =
        MicroVmGeneration::from_set(&MicroVmArtifactSet::load(&artifacts, &fs).unwrap());
    let mut identity = SnapshotIdentity::from_generation(&generation, "x86_64", "linux-6.1");
    identity.rootfs = "0".repeat(64);
    fs.write(
        &artifacts.join("snapshot.identity.json"),
        &serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
    fs.write(&artifacts.join("snapshot.mem"), b"snap").unwrap();
    let checksums = ArtifactChecksums {
        firecracker_version: FIRECRACKER_VERSION.to_string(),
        jailer_version: JAILER_VERSION.to_string(),
        firecracker: hex_sha256(b"fc"),
        jailer: hex_sha256(b"jailer"),
        kernel: hex_sha256(b"kernel"),
        rootfs: hex_sha256(b"rootfs"),
        guest_agent: hex_sha256(b"agent"),
        snapshot: Some(hex_sha256(b"snap")),
    };
    fs.write(
        &artifacts.join("manifest.json"),
        &serde_json::to_vec(&checksums).unwrap(),
    )
    .unwrap();
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
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        let mut session =
            open_session(&file, IsolationIdentity::new("job-snap", 1), &mut world).unwrap();
        session.reserve(&mut world).unwrap();
        session
            .prepare(&ValidatedPlan::example_success("job-snap"), &mut world)
            .unwrap();
    }
    assert!(
        api.calls
            .iter()
            .any(|call| call.starts_with("put_boot_source")),
        "{:?}",
        api.calls
    );
    assert!(!api
        .calls
        .iter()
        .any(|call| call.starts_with("load_snapshot")));
    assert!(
        runner
            .calls
            .iter()
            .any(|(program, args)| program.ends_with("jailer") && args.iter().any(|a| a == "--id")),
        "jailer must start a VMM before restore/cold-boot: {:?}",
        runner.calls
    );
}

#[test]
fn matching_snapshot_loads_only_after_jailer() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    fs.write(&artifacts.join("snapshot.mem"), b"snap").unwrap();
    fs.write(&artifacts.join("snapshot.vmstate"), b"vmstate")
        .unwrap();
    let generation =
        MicroVmGeneration::from_set(&MicroVmArtifactSet::load(&artifacts, &fs).unwrap());
    let identity =
        SnapshotIdentity::from_generation(&generation, std::env::consts::ARCH, "linux-6.1");
    fs.write(
        &artifacts.join("snapshot.identity.json"),
        &serde_json::to_vec(&identity).unwrap(),
    )
    .unwrap();
    let checksums = ArtifactChecksums {
        firecracker_version: FIRECRACKER_VERSION.to_string(),
        jailer_version: JAILER_VERSION.to_string(),
        firecracker: hex_sha256(b"fc"),
        jailer: hex_sha256(b"jailer"),
        kernel: hex_sha256(b"kernel"),
        rootfs: hex_sha256(b"rootfs"),
        guest_agent: hex_sha256(b"agent"),
        snapshot: Some(hex_sha256(b"snap")),
    };
    fs.write(
        &artifacts.join("manifest.json"),
        &serde_json::to_vec(&checksums).unwrap(),
    )
    .unwrap();
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
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        let mut session =
            open_session(&file, IsolationIdentity::new("job-restore", 1), &mut world).unwrap();
        session.reserve(&mut world).unwrap();
        session
            .prepare(&ValidatedPlan::example_success("job-restore"), &mut world)
            .unwrap();
    }
    let jailer_at = runner
        .calls
        .iter()
        .position(|(program, args)| program.ends_with("jailer") && args.iter().any(|a| a == "--id"))
        .expect("jailer must start before load_snapshot");
    assert!(
        api.calls
            .iter()
            .any(|call| call.starts_with("load_snapshot")),
        "{:?}",
        api.calls
    );
    assert!(!api
        .calls
        .iter()
        .any(|call| call.starts_with("put_boot_source")));
    let _ = jailer_at;
}

#[test]
fn golden_snapshot_create_pauses_then_writes_sidecar() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
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
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        let mut session =
            open_session(&file, IsolationIdentity::new("job-gold", 1), &mut world).unwrap();
        session.reserve(&mut world).unwrap();
        session
            .prepare(&ValidatedPlan::example_success("job-gold"), &mut world)
            .unwrap();
        session.start(&mut world).unwrap();
    }
    assert!(
        api.calls.iter().any(|call| call == "pause_vm"),
        "{:?}",
        api.calls
    );
    assert!(
        api.calls
            .iter()
            .any(|call| call.starts_with("create_snapshot")),
        "{:?}",
        api.calls
    );
    assert!(
        api.calls.iter().any(|call| call == "resume_vm"),
        "{:?}",
        api.calls
    );
    let sidecar = fs
        .read(&artifacts.join("snapshot.identity.json"))
        .expect("golden sidecar");
    let identity: SnapshotIdentity = serde_json::from_slice(&sidecar).unwrap();
    assert!(identity.credential_free);
}

#[test]
fn golden_snapshot_create_refuses_job_credentials() {
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = PathBuf::from("/var/run/docker.sock");
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands::default();
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    let mut events = Vec::new();
    let err = create_golden_snapshot(
        &mut world,
        GuestReady {
            agent_listening: true,
            docker_healthy: true,
            job_credentials_absent: false,
        },
        &mut events,
    )
    .unwrap_err();
    assert!(err.to_string().contains("credential-free"), "{err}");
    assert!(!api
        .calls
        .iter()
        .any(|call| call.starts_with("create_snapshot")));
}

#[test]
fn shipped_guest_image_rejects_virtio_fs() {
    crate::execution::validate_kernel_config(include_str!("../../../../microvm/kernel.config"))
        .unwrap();
    crate::execution::validate_guest_toml(include_str!("../../../../microvm/guest.toml")).unwrap();
}

#[test]
fn microvm_advertise_requires_coherent_packaged_generation() {
    let dir = std::env::temp_dir().join(format!(
        "velnor-microvm-gen-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, bytes) in [
        ("firecracker", b"fc".as_slice()),
        ("jailer", b"jailer".as_slice()),
        ("vmlinux", b"kernel".as_slice()),
        ("rootfs.ext4", b"rootfs".as_slice()),
        ("velnor-guest-agent", b"agent".as_slice()),
    ] {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    let checksums = ArtifactChecksums {
        firecracker_version: FIRECRACKER_VERSION.to_string(),
        jailer_version: JAILER_VERSION.to_string(),
        firecracker: hex_sha256(b"fc"),
        jailer: hex_sha256(b"jailer"),
        kernel: hex_sha256(b"kernel"),
        rootfs: hex_sha256(b"rootfs"),
        guest_agent: hex_sha256(b"agent"),
        snapshot: None,
    };
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec(&checksums).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join(crate::node::prove::EXECUTOR_OK), b"ok\n").unwrap();
    assert!(!executor_is_proven_at(
        &dir,
        ExecutionBackendKind::MicroVm,
        Path::new("/no-docker.sock"),
        &dir
    ));
    let generation = packaged_generation(&dir, &RealHostFs).unwrap();
    crate::node::prove::write_microvm_executor_ok(&dir, &generation).unwrap();
    assert!(executor_is_proven_at(
        &dir,
        ExecutionBackendKind::MicroVm,
        Path::new("/no-docker.sock"),
        &dir
    ));
    std::fs::write(dir.join("firecracker"), b"other-generation").unwrap();
    assert!(!executor_is_proven_at(
        &dir,
        ExecutionBackendKind::MicroVm,
        Path::new("/no-docker.sock"),
        &dir
    ));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn docker_backend_uses_production_engine_when_present() {
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
    struct StubEngine;
    impl ProductionDockerEngine for StubEngine {
        fn execute_github_job(
            &mut self,
            events: &mut Vec<ExecutionEvent>,
        ) -> Result<(), ExecutionError> {
            events.push(ExecutionEvent::HostDockerInvoked(
                "production-engine".into(),
            ));
            events.push(ExecutionEvent::Log {
                stream: 1,
                line: "*** engine ***".into(),
            });
            Ok(())
        }
    }
    let mut engine = StubEngine;
    let outcome = {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        world.docker_engine = Some(&mut engine);
        crate::execution::run_validated_job(
            &file,
            IsolationIdentity::new("job-engine", 1),
            &ValidatedPlan::example_success("job-engine"),
            &mut world,
        )
        .unwrap()
    };
    assert_eq!(outcome.conclusion, "success");
    assert!(outcome.masked);
    assert!(runner
        .calls
        .iter()
        .all(|(_, args)| !args.iter().any(|arg| arg == "exec")));
}

#[test]
fn microvm_execute_sends_deliver_plan_over_vsock() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
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
    let mut vsock = LoopbackVsock::default();
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        world.allow_inline_guest_plan = false;
        let mut session =
            open_session(&file, IsolationIdentity::new("job-vsock", 1), &mut world).unwrap();
        session.reserve(&mut world).unwrap();
        let plan = ValidatedPlan::example_success("job-vsock");
        session.prepare(&plan, &mut world).unwrap();
        session.start(&mut world).unwrap();
        world.vsock = Some(&mut vsock);
        session.execute(&plan, &mut world).unwrap();
        assert!(!session.used_host_docker());
        assert!(session.used_firecracker());
    }
    assert!(matches!(
        vsock.sent[0],
        velnor_model::VsockMessage::DeliverPlan { .. }
    ));
    assert!(!runner
        .calls
        .iter()
        .any(|(program, args)| { program == "docker" && args.iter().any(|arg| arg == "exec") }));
}

#[test]
fn microvm_execute_without_vsock_fails_closed() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
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
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    world.allow_inline_guest_plan = false;
    let mut session =
        open_session(&file, IsolationIdentity::new("job-novsock", 1), &mut world).unwrap();
    session.reserve(&mut world).unwrap();
    let plan = ValidatedPlan::example_success("job-novsock");
    session.prepare(&plan, &mut world).unwrap();
    session.start(&mut world).unwrap();
    let error = session.execute(&plan, &mut world).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("vsock"), "{text}");
    assert!(text.contains("docker backend was not used"), "{text}");
    assert!(!session.used_host_docker());
}

fn script_step(id: &str, script: &str) -> crate::script_step::ScriptStep {
    crate::script_step::ScriptStep {
        id: id.into(),
        display_name: id.into(),
        script: script.into(),
        shell: crate::container::Shell::Bash,
        working_directory_container: "/__w".into(),
        env: Vec::new(),
        condition: None,
        continue_on_error: false,
        timeout_minutes: None,
    }
}

#[test]
fn both_backends_execute_the_same_admitted_plan() {
    let steps = [script_step("run", "echo run")];
    let plan = ValidatedPlan::from_script_steps(
        "job-shared",
        "velnor/job-ubuntu:26.04",
        &steps,
        vec!["postgres:16".into()],
    );
    assert_eq!(plan.job_container_image, "velnor/job-ubuntu:26.04");
    assert_eq!(plan.scripts, vec!["echo run".to_string()]);
    assert_eq!(plan.service_images, vec!["postgres:16".to_string()]);
    let guest = plan.to_guest("job-shared", 1);
    assert_eq!(guest.image, plan.job_container_image);
    assert_eq!(guest.steps[0].script, "echo run");
    let docker = run_custom(ExecutionBackendKind::Docker, plan.clone());
    let micro = run_custom(ExecutionBackendKind::MicroVm, plan);
    assert_eq!(docker.conclusion, micro.conclusion);
    assert_eq!(docker.exit_code, micro.exit_code);
    assert_eq!(docker.command_file, micro.command_file);
    assert!(docker.cleaned && micro.cleaned);
}

#[test]
fn microvm_spawns_jailer_with_api_socket_and_unique_net() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
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
    let isolation = IsolationIdentity::new("job-net", 1);
    let resources = IsolationResources::for_identity(isolation.clone(), &artifacts);
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        let mut session = open_session(&file, isolation, &mut world).unwrap();
        session.reserve(&mut world).unwrap();
        let plan = ValidatedPlan::example_success("job-net");
        session.prepare(&plan, &mut world).unwrap();
        session.start(&mut world).unwrap();
        session.execute(&plan, &mut world).unwrap();
        let _ = session.collect().unwrap();
        session.teardown(&mut world).unwrap();
    }
    assert_eq!(runner.spawned.len(), 1, "{:?}", runner.calls);
    assert_eq!(runner.killed, vec![runner.spawned[0].pid]);
    assert!(
        runner.calls.iter().any(
            |(program, args)| program == "ip" && args.windows(2).any(|w| w == ["netns", "add"])
        ),
        "{:?}",
        runner.calls
    );
    assert!(
        runner.calls.iter().any(|(program, args)| {
            program == "ip"
                && args.windows(2).any(|w| w == ["tuntap", "add"])
                && args.iter().any(|arg| arg == &resources.tap)
        }),
        "{:?}",
        runner.calls
    );
    assert!(
        runner.calls.iter().any(|(program, args)| {
            program.ends_with("jailer")
                && args.iter().any(|arg| arg == "--chroot-base-dir")
                && args
                    .windows(2)
                    .any(|w| w == ["--api-sock", "/run/firecracker.socket"])
                && !args.iter().any(|arg| arg == "--daemonize")
        }),
        "{:?}",
        runner.calls
    );
    assert!(
        runner.calls.iter().any(|(program, args)| {
            program == "ip" && args.windows(2).any(|w| w == ["netns", "delete"])
        }),
        "{:?}",
        runner.calls
    );
    assert_ne!(resources.api_socket(), resources.vsock);
}

#[test]
fn fixture_parity_yaml_keeps_lanes_choice() {
    let yaml = include_str!("../../../../docs/fixture-backend-parity.yml");
    assert!(yaml.contains("lanes:"), "{yaml}");
    assert!(yaml.contains("options: [velnor, github, both]"), "{yaml}");
    assert!(
        !yaml.contains("\n      backend:"),
        "fixture must not add a repository-controlled backend input: {yaml}"
    );
}
