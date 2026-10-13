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

fn microvm_plan(job_id: impl Into<String>) -> ValidatedPlan {
    let mut plan = ValidatedPlan::example_success(job_id);
    plan.command_files.clear();
    plan
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
}

#[test]
fn superseded_docker_script_executor_paths_are_gone() {
    let execution = include_str!("mod.rs");
    let runner = include_str!("../runner.rs");
    let executor = include_str!("../executor.rs");
    for source in [execution, runner, executor] {
        assert!(
            !source.contains("DockerScriptExecutor"),
            "DockerScriptExecutor must not remain as a parallel executor"
        );
        assert!(
            !source.contains("host_docker_executor"),
            "host_docker_executor must not construct a host Docker engine outside ValidatedPlan"
        );
    }
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
    assert!(micro_out.command_file.is_none());

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
            // The runner masks secrets as `***`; one such line proves the
            // masked outcome flag without relying on step banners.
            stdout: "*** ok ***".into(),
            stderr: String::new(),
        },
        ..RecordingCommands::default()
    };
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    let mut session = open_session(&file, IsolationIdentity::new("job-1", 1), &mut world).unwrap();
    session.reserve(&mut world).unwrap();
    let mut plan = if kind == ExecutionBackendKind::MicroVm {
        microvm_plan("job-1")
    } else {
        ValidatedPlan::example_success("job-1")
    };
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
    let plan = if kind == ExecutionBackendKind::MicroVm {
        let mut plan = plan;
        plan.command_files.clear();
        plan
    } else {
        plan
    };
    let job_id = plan.job_id.clone();
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    crate::execution::run_validated_job(&file, IsolationIdentity::new(job_id, 1), &plan, &mut world)
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
        .prepare(&microvm_plan("job-fault"), &mut world)
        .unwrap_err();
    assert!(err.to_string().contains("jailer"), "{err}");
    assert!(!session.used_host_docker());

    let victim = IsolationResources::for_identity(
        IsolationIdentity::new("job-victim", 1),
        Path::new("/run"),
    );
    let sibling = IsolationIdentity::new("job-sibling", 2);
    assert!(crate::execution::teardown_is_exact(&victim, &sibling));

    let isolation = IsolationIdentity::new("job-fault", 1);
    let snap = SnapshotIdentity::production_template("x86_64", "linux-6.1", &isolation);
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
    let isolation = IsolationIdentity::new("job-snap", 1);
    let mut identity =
        SnapshotIdentity::from_generation(&generation, "x86_64", "linux-6.1", &isolation);
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
    let mut vsock = LoopbackVsock::with_ready("job-snap", 1);
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        world.vsock = Some(&mut vsock);
        let mut session = open_session(&file, isolation, &mut world).unwrap();
        session.reserve(&mut world).unwrap();
        session
            .prepare(&microvm_plan("job-snap"), &mut world)
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
    let isolation = IsolationIdentity::new("job-restore", 1);
    let identity = SnapshotIdentity::from_generation(
        &generation,
        std::env::consts::ARCH,
        "linux-6.1",
        &isolation,
    );
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
    let mut vsock = LoopbackVsock::with_ready("job-restore", 1);
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        world.allow_inline_guest_plan = false;
        world.vsock = Some(&mut vsock);
        let mut session = open_session(&file, isolation, &mut world).unwrap();
        session.reserve(&mut world).unwrap();
        session
            .prepare(&microvm_plan("job-restore"), &mut world)
            .unwrap();
        session.start(&mut world).unwrap();
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
    assert!(vsock.sent.iter().any(|message| matches!(
        message,
        velnor_model::VsockMessage::GuestIdentity { restored: true, .. }
    )));
    let _ = jailer_at;
}

#[test]
fn wrong_job_snapshot_identity_cold_boots_without_load() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    fs.write(&artifacts.join("snapshot.mem"), b"snap").unwrap();
    fs.write(&artifacts.join("snapshot.vmstate"), b"vmstate")
        .unwrap();
    let generation =
        MicroVmGeneration::from_set(&MicroVmArtifactSet::load(&artifacts, &fs).unwrap());
    let foreign = IsolationIdentity::new("job-foreign", 1);
    let identity = SnapshotIdentity::from_generation(
        &generation,
        std::env::consts::ARCH,
        "linux-6.1",
        &foreign,
    );
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
    let current = IsolationIdentity::new("job-current", 1);
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    let mut session = open_session(&file, current, &mut world).unwrap();
    session.reserve(&mut world).unwrap();
    session
        .prepare(&microvm_plan("job-current"), &mut world)
        .unwrap();

    assert!(
        api.calls
            .iter()
            .any(|call| call.starts_with("put_boot_source")),
        "wrong-job snapshot must cold boot: {:?}",
        api.calls
    );
    assert!(!api
        .calls
        .iter()
        .any(|call| call.starts_with("load_snapshot")));
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
    let mut vsock = LoopbackVsock::with_ready("job-gold", 1);
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        world.allow_inline_guest_plan = false;
        world.vsock = Some(&mut vsock);
        let mut session =
            open_session(&file, IsolationIdentity::new("job-gold", 1), &mut world).unwrap();
        session.reserve(&mut world).unwrap();
        session
            .prepare(&microvm_plan("job-gold"), &mut world)
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
    let instance_start = api
        .calls
        .iter()
        .position(|call| call == "instance_start")
        .expect("cold VM must start before readiness");
    let pause = api
        .calls
        .iter()
        .position(|call| call == "pause_vm")
        .expect("snapshot must pause after readiness");
    assert!(instance_start < pause, "{:?}", api.calls);
    assert!(vsock
        .sent
        .iter()
        .any(|message| matches!(message, velnor_model::VsockMessage::PrepareSnapshot)));
    let identities: Vec<bool> = vsock
        .sent
        .iter()
        .filter_map(|message| match message {
            velnor_model::VsockMessage::GuestIdentity { restored, .. } => Some(*restored),
            _ => None,
        })
        .collect();
    assert_eq!(identities, vec![false, false]);
    let sidecar = fs
        .read(&artifacts.join("snapshot.identity.json"))
        .expect("golden sidecar");
    let identity: SnapshotIdentity = serde_json::from_slice(&sidecar).unwrap();
    assert!(identity.credential_free);
    assert_eq!(identity.isolation_id, "job-gold");
    assert_eq!(identity.generation, 1);
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
        &IsolationIdentity::new("job-gold", 1),
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
            events.push(ExecutionEvent::Log {
                stream: 1,
                line: "[velnor-step engine]".into(),
            });
            events.push(ExecutionEvent::JobCompleted {
                conclusion: velnor_model::JobConclusion::Success,
                exit_code: 0,
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
    let mut vsock = LoopbackVsock::with_ready("job-vsock", 1);
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        world.allow_inline_guest_plan = false;
        world.vsock = Some(&mut vsock);
        let mut session =
            open_session(&file, IsolationIdentity::new("job-vsock", 1), &mut world).unwrap();
        session.reserve(&mut world).unwrap();
        let plan = microvm_plan("job-vsock");
        session.prepare(&plan, &mut world).unwrap();
        session.start(&mut world).unwrap();
        session.execute(&plan, &mut world).unwrap();
        assert!(!session.used_host_docker());
        assert!(session.used_firecracker());
    }
    assert!(vsock
        .sent
        .iter()
        .any(|message| matches!(message, velnor_model::VsockMessage::GuestIdentity { .. })));
    assert!(vsock
        .sent
        .iter()
        .any(|message| matches!(message, velnor_model::VsockMessage::DeliverPlan { .. })));
    let deliver = vsock
        .sent
        .iter()
        .find_map(|message| match message {
            velnor_model::VsockMessage::DeliverPlan {
                execution_nonce,
                plan_sha256,
                plan_bytes,
                ..
            } => Some((execution_nonce, plan_sha256, plan_bytes)),
            _ => None,
        })
        .expect("deliver plan");
    assert!(!deliver.0.is_empty());
    assert_eq!(deliver.1, &hex_sha256(deliver.2));
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
    let plan = microvm_plan("job-novsock");
    session.prepare(&plan, &mut world).unwrap();
    let error = session.start(&mut world).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("guest.identity"), "{text}");
    assert!(text.contains("docker backend was not used"), "{text}");
    assert!(!session.used_host_docker());
    assert!(api.calls.iter().any(|call| call == "instance_start"));
}

#[test]
fn microvm_rejects_unsupported_guest_plan_before_network_setup() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = PathBuf::from("/var/run/docker.sock");
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands::default();
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    let mut session = open_session(
        &file,
        IsolationIdentity::new("job-unsupported", 1),
        &mut world,
    )
    .unwrap();
    session.reserve(&mut world).unwrap();
    let mut plan = microvm_plan("job-unsupported");
    plan.command_files = vec!["GITHUB_OUTPUT".into()];
    let error = session.prepare(&plan, &mut world).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("field 'guest.command_files'"), "{text}");
    assert!(text.contains("received '[\"GITHUB_OUTPUT\"]'"), "{text}");
    assert!(
        text.contains("accepted 'empty until guest result-file transfer is implemented'"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "manifest version {}",
            crate::manifest::MANIFEST_VERSION
        )),
        "{text}"
    );
    assert!(
        runner.spawned.is_empty(),
        "jailer started: {:?}",
        runner.spawned
    );
    assert!(
        runner.calls.iter().all(|(program, _)| program != "ip"),
        "network setup ran: {:?}",
        runner.calls
    );
    assert!(
        api.calls.is_empty(),
        "Firecracker API was called: {:?}",
        api.calls
    );
}

#[test]
fn microvm_rejects_mismatched_teardown_ack_identity() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = PathBuf::from("/var/run/docker.sock");
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands::default();
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let mut vsock =
        LoopbackVsock::with_ready("job-ack", 2).with_teardown_ack("job-old", "job-ack", 2);
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    world.allow_inline_guest_plan = false;
    world.vsock = Some(&mut vsock);
    let mut session =
        open_session(&file, IsolationIdentity::new("job-ack", 2), &mut world).unwrap();
    session.reserve(&mut world).unwrap();
    let plan = microvm_plan("job-ack");
    session.prepare(&plan, &mut world).unwrap();
    session.start(&mut world).unwrap();
    let error = session.execute(&plan, &mut world).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("vsock.teardown_ack"), "{text}");
    assert!(text.contains("identity"), "{text}");
    session.teardown(&mut world).unwrap();
}

#[test]
fn microvm_rejects_replayed_teardown_proof() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = PathBuf::from("/var/run/docker.sock");
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands::default();
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let plan = microvm_plan("job-replay");
    let digest = hex_sha256(&plan.to_guest("job-replay", 2).encode().unwrap());
    let mut vsock =
        LoopbackVsock::with_ready("job-replay", 2).with_teardown_proof("replayed-nonce", digest);
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    world.allow_inline_guest_plan = false;
    world.vsock = Some(&mut vsock);
    let mut session =
        open_session(&file, IsolationIdentity::new("job-replay", 2), &mut world).unwrap();
    session.reserve(&mut world).unwrap();
    session.prepare(&plan, &mut world).unwrap();
    session.start(&mut world).unwrap();
    let error = session.execute(&plan, &mut world).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("vsock.teardown_ack"), "{text}");
    assert!(text.contains("identity"), "{text}");
    session.teardown(&mut world).unwrap();
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
    assert_eq!(plan.steps[0].script, "echo run");
    assert!(plan.steps[0].action.is_none());
    assert_eq!(plan.services[0].image, "postgres:16");
    assert_eq!(plan.workspace, "/__w");
    let guest = plan.to_guest("job-shared", 1);
    assert_eq!(guest.image, plan.job_container_image);
    assert_eq!(guest.steps[0].script, "echo run");
    assert_eq!(guest.services[0].network_alias, "svc-0");
    let docker = run_custom(ExecutionBackendKind::Docker, plan.clone());
    let micro = run_custom(ExecutionBackendKind::MicroVm, plan);
    assert_eq!(docker.conclusion, micro.conclusion);
    assert_eq!(docker.exit_code, micro.exit_code);
    assert!(docker.command_file.is_some());
    assert!(micro.command_file.is_none());
    assert_eq!(docker.outputs, micro.outputs);
    assert_eq!(docker.cache, micro.cache);
    assert_eq!(docker.artifacts, micro.artifacts);
    assert_eq!(docker.buildx, micro.buildx);
    assert_eq!(docker.testcontainers, micro.testcontainers);
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
        let plan = microvm_plan("job-net");
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
fn microvm_teardown_kill_failure_fences_and_attempts_network_cleanup() {
    let resources = IsolationResources::for_identity(
        IsolationIdentity::new("job-kill-failure", 1),
        Path::new("/run"),
    );
    let mut fs = MemoryFs::default();
    let docker = PathBuf::from("/var/run/docker.sock");
    let artifacts = PathBuf::from("/microvm");
    let kvm = PathBuf::from("/dev/kvm");
    let mut runner = RecordingCommands {
        fail_kill: Some("permission denied".into()),
        ..RecordingCommands::default()
    };
    let mut api = RecordingFirecracker::default();
    let mut backend = FirecrackerBackend {
        jailer: Some(crate::executor::SpawnedProcess { pid: 7 }),
        ..FirecrackerBackend::default()
    };
    let mut events = Vec::new();
    let error = {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        backend
            .teardown(&resources, &mut world, &mut events)
            .unwrap_err()
    };
    assert!(error.to_string().contains("microvm teardown uncertain"));
    assert!(error.to_string().contains("kill jailer pid 7"));
    assert!(runner.calls.iter().any(|(program, args)| {
        program == "ip" && args.windows(2).any(|window| window == ["netns", "delete"])
    }));
    assert!(
        backend.jailer.is_some(),
        "uncertain jailer cleanup was discarded"
    );
}

#[test]
fn microvm_teardown_network_failure_is_not_reported_clean() {
    let resources = IsolationResources::for_identity(
        IsolationIdentity::new("job-net-failure", 1),
        Path::new("/run"),
    );
    let mut fs = MemoryFs::default();
    let docker = PathBuf::from("/var/run/docker.sock");
    let artifacts = PathBuf::from("/microvm");
    let kvm = PathBuf::from("/dev/kvm");
    let mut runner = RecordingCommands {
        codes: vec![1],
        ..RecordingCommands::default()
    };
    let mut api = RecordingFirecracker::default();
    let mut backend = FirecrackerBackend::default();
    let mut events = Vec::new();
    let error = {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        backend
            .teardown(&resources, &mut world, &mut events)
            .unwrap_err()
    };
    assert!(error.to_string().contains("microvm teardown uncertain"));
    assert!(error.to_string().contains("network teardown"));
    assert!(runner.calls.iter().any(|(program, args)| {
        program == "ip" && args.windows(2).any(|window| window == ["netns", "delete"])
    }));
}

#[test]
fn log_substring_does_not_override_explicit_exit() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let docker = PathBuf::from("/var/run/docker.sock");
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands {
        next: CommandResult {
            code: 0,
            stdout: "this is not a failure cancel timeout".into(),
            stderr: String::new(),
        },
        ..RecordingCommands::default()
    };
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let artifacts = PathBuf::from("/microvm");
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    let mut plan = ValidatedPlan::example_success("job-echo");
    plan.steps[0].script = "echo this is not a failure".into();
    let outcome = crate::execution::run_validated_job(
        &file,
        IsolationIdentity::new("job-echo", 1),
        &plan,
        &mut world,
    )
    .unwrap();
    assert_eq!(outcome.conclusion, "success", "{:?}", outcome.log_lines);
    assert_eq!(outcome.exit_code, 0);
    assert!(
        outcome
            .log_lines
            .iter()
            .any(|line| line.contains("failure")),
        "{:?}",
        outcome.log_lines
    );
    assert_eq!(outcome.outputs, vec![("result".into(), "ok".into())]);
    assert_eq!(outcome.command_file.as_deref(), Some("GITHUB_OUTPUT"));
}

#[test]
fn debian_package_binds_complete_microvm_identity() {
    let assets = include_str!("../../Cargo.toml");
    for path in [
        "release/microvm/firecracker",
        "release/microvm/jailer",
        "release/microvm/velnor-guest-agent",
        "release/microvm/vmlinux",
        "release/microvm/rootfs.ext4",
        "release/microvm/manifest.json",
    ] {
        assert!(assets.contains(path), "cargo-deb assets missing {path}");
    }
    let postinst = include_str!("../../debian/postinst");
    for needle in [
        "vmlinux",
        "rootfs.ext4",
        "microvm_verify_sha256",
        "guest_agent",
    ] {
        assert!(postinst.contains(needle), "postinst missing {needle}");
    }
}

#[test]
fn full_github_visible_plan_preserves_docker_contract_and_guest_projection() {
    let mut plan = ValidatedPlan::example_success("job-full");
    plan.env = vec![("CI".into(), "true".into())];
    plan.workspace = "/__w".into();
    plan.cache = vec!["sha256:cache".into()];
    plan.artifacts = vec![("logs".into(), "/__w/logs".into())];
    plan.annotations = vec!["notice".into()];
    plan.summary = "ok".into();
    plan.buildx = true;
    plan.testcontainers = true;
    let docker = run_custom(ExecutionBackendKind::Docker, plan.clone());
    assert_eq!(docker.cache, plan.cache);
    assert_eq!(docker.artifacts, plan.artifacts);
    assert_eq!(docker.annotations, plan.annotations);
    assert_eq!(docker.summary, plan.summary);
    assert!(docker.buildx);
    assert!(docker.testcontainers);
    let guest = plan.to_guest("job-full", 1);
    assert!(guest.buildx);
    assert!(guest.testcontainers);
    assert_eq!(guest.workspace, "/__w");
    assert!(!guest
        .encode()
        .unwrap()
        .windows(11)
        .any(|w| w == b"docker.sock"));
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

#[test]
fn packaged_conffile_is_the_last_execution_toml_fallback() {
    let dir = std::env::temp_dir().join(format!(
        "velnor-exec-fallback-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let primary = dir.join("execution.toml");
    let packaged = dir.join("packaged").join("execution.toml");
    std::fs::create_dir_all(packaged.parent().unwrap()).unwrap();
    std::fs::write(&packaged, "[execution]\nbackend = \"microvm\"\n").unwrap();

    // Primary missing: the packaged conffile answers.
    let file = load_execution_file_from(&primary, &packaged).unwrap();
    assert_eq!(file.backend(), ExecutionBackendKind::MicroVm);

    // Primary valid: it wins over the packaged conffile.
    std::fs::write(&primary, "[execution]\nbackend = \"docker\"\n").unwrap();
    let file = load_execution_file_from(&primary, &packaged).unwrap();
    assert_eq!(file.backend(), ExecutionBackendKind::Docker);

    // Primary invalid: the parse error propagates instead of silently
    // falling back to the packaged conffile.
    std::fs::write(&primary, "[execution]\nbackend = \"kata\"\n").unwrap();
    let err = load_execution_file_from(&primary, &packaged).unwrap_err();
    assert_eq!(err.field, "[execution] backend");

    // Both missing: fail closed naming both searched paths.
    std::fs::remove_file(&primary).unwrap();
    std::fs::remove_file(&packaged).unwrap();
    let err = load_execution_file_from(&primary, &packaged).unwrap_err();
    assert!(err
        .to_string()
        .contains(primary.display().to_string().as_str()));

    std::fs::remove_dir_all(&dir).ok();
}
