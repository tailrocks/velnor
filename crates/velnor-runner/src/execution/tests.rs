use super::*;
use crate::executor::CommandResult;
use std::path::{Path, PathBuf};
use velnor_model::{ExecutionBackendKind, ExecutionFile};

#[test]
fn each_guest_step_summary_is_captured_before_the_next_step_writes() {
    let mut runner = RecordingCommands {
        results: vec![
            CommandResult {
                code: 0,
                stdout: "step-one-summary\n".into(),
                stderr: String::new(),
            },
            CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
            CommandResult {
                code: 0,
                stdout: "step-two-overwrites-with-\x3e\n".into(),
                stderr: String::new(),
            },
            CommandResult {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
        ],
        ..RecordingCommands::default()
    };
    let files = vec!["GITHUB_STEP_SUMMARY".to_string()];
    let mut events = Vec::new();
    let mut first = crate::script_step::StepCommandState::default();
    super::guest_runtime::apply_step_command_files(
        "job",
        &mut runner,
        &mut events,
        false,
        &files,
        &mut first,
    )
    .unwrap();
    assert_eq!(first.summary, "step-one-summary\n");
    let mut second = crate::script_step::StepCommandState::default();
    super::guest_runtime::apply_step_command_files(
        "job",
        &mut runner,
        &mut events,
        false,
        &files,
        &mut second,
    )
    .unwrap();
    assert_eq!(second.summary, "step-two-overwrites-with->\n");
    let captured: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ExecutionEvent::CommandFile { path, bytes } if path == "GITHUB_STEP_SUMMARY" => {
                Some(bytes.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        captured,
        [
            b"step-one-summary\n".to_vec(),
            b"step-two-overwrites-with->\n".to_vec()
        ]
    );
}

#[test]
fn checkout_guest_inputs_carry_token_and_flags() {
    let plan = crate::checkout::CheckoutPlan {
        step_id: "co".into(),
        display_name: "checkout".into(),
        clone_url: "https://github.com/tailrocks/velnor".into(),
        version: Some("refs/pull/1/merge".into()),
        destination: PathBuf::from("/__w/repo"),
        token: Some("secret-token".into()),
        fetch_depth: Some(1),
        fetch_tags: true,
        persist_credentials: false,
        clean: true,
        lfs: false,
        condition: None,
        continue_on_error: false,
        timeout_minutes: None,
    };
    let inputs =
        super::backend::executable_inputs(&crate::executor::ExecutableStep::Checkout(plan));
    let get = |name: &str| {
        inputs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| panic!("missing input {name}"))
    };
    assert_eq!(get("token"), "secret-token");
    assert_eq!(get("fetch_tags"), "1");
    assert_eq!(get("persist_credentials"), "0");
    assert_eq!(get("clean"), "1");
    assert_eq!(get("fetch_depth"), "1");
}

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

fn socket_for(kind: ExecutionBackendKind) -> PathBuf {
    match kind {
        ExecutionBackendKind::Docker => PathBuf::from("/var/run/docker.sock"),
        ExecutionBackendKind::MicroVm => PathBuf::from(MICROVM_NO_HOST_DOCKER_SOCKET),
    }
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
        isolation_root: artifacts,
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
fn microvm_world_naming_host_docker_socket_fails_closed() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let mut runner = RecordingCommands::default();
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let docker = socket_for(ExecutionBackendKind::Docker);
    fs.write(&docker, b"socket").unwrap();
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    let error = preflight_selected(&file, &mut world).unwrap_err();
    assert!(
        matches!(error, ExecutionError::HostDockerForbidden),
        "{error}"
    );
    assert!(runner.calls.is_empty());
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
    assert!(
        !runner.contains("backend != Some(velnor_model::ExecutionBackendKind::MicroVm)"),
        "missing execution.toml must not be treated as docker for host Docker prune"
    );
    let leftover = include_str!("../leftover_disk.rs");
    let cache = include_str!("../cache.rs");
    assert!(
        leftover.contains("permits_host_docker_maintenance"),
        "disk-pressure reclaim must gate host Docker on selected backend"
    );
    assert!(
        leftover.contains("live_job_ids_for_reclaim"),
        "cache-GC live-job listing must not default missing selection to docker"
    );
    assert!(
        cache.contains("live_job_ids_for_reclaim"),
        "cache leftover listing must use the backend-gated live-job helper"
    );
    assert!(
        runner.contains("maybe_startup_host_docker_reclaim"),
        "startup prune must skip host Docker unless docker is selected"
    );
    assert!(
        runner.contains("doctor_host_docker_reclaim"),
        "doctor reclaim must skip host Docker unless docker is selected"
    );
    assert!(
        runner.contains("reclaim_production_if_hard_pressure_for"),
        "disk-pressure path must use the gated leftover reclaim"
    );
}

#[test]
fn docker_backend_does_not_boot_firecracker() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let docker = socket_for(ExecutionBackendKind::Docker);
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
fn docker_backend_execute_runs_network_service_and_job_containers() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let docker = socket_for(ExecutionBackendKind::Docker);
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands {
        next: CommandResult {
            code: 0,
            stdout: "*** ok ***".into(),
            stderr: String::new(),
        },
        ..RecordingCommands::default()
    };
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let artifacts = PathBuf::from("/microvm");
    let plan = ValidatedPlan::example_success("job-real-docker");
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        crate::execution::run_validated_job(
            &file,
            IsolationIdentity::new("job-real-docker", 1),
            &plan,
            &mut world,
        )
        .unwrap();
    }
    assert!(
        runner.calls.iter().any(|(program, args)| {
            program == "docker" && args.windows(2).any(|w| w == ["network", "create"])
        }),
        "docker execute must create the job network, got {:?}",
        runner.calls
    );
    assert!(
        runner.calls.iter().any(|(program, args)| {
            program == "docker" && args.contains(&"postgres:16".to_string())
        }),
        "docker execute must run the service container, got {:?}",
        runner.calls
    );
    assert!(
        runner.calls.iter().any(|(program, args)| {
            program == "docker"
                && args.contains(&"velnor/job-ubuntu:26.04".to_string())
                && args.contains(&"run".to_string())
        }),
        "docker execute must run the job container, got {:?}",
        runner.calls
    );
    assert!(api.calls.is_empty());
}

#[test]
fn docker_backend_cancel_runs_docker_rm_force() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let docker = socket_for(ExecutionBackendKind::Docker);
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
    let plan = ValidatedPlan::example_success("job-cancel-docker");
    {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        let mut session = open_session(
            &file,
            IsolationIdentity::new("job-cancel-docker", 1),
            &mut world,
        )
        .unwrap();
        session.reserve(&mut world).unwrap();
        session.prepare(&plan, &mut world).unwrap();
        session.start(&mut world).unwrap();
        session.cancel(&mut world).unwrap();
    }
    assert!(
        runner.calls.iter().any(|(program, args)| {
            program == "docker"
                && args.contains(&"rm".to_string())
                && args.contains(&"--force".to_string())
                && args.iter().any(|arg| arg.contains("job-cancel-docker"))
        }),
        "docker cancel must rm --force the job container, got {:?}",
        runner.calls
    );
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
    assert_eq!(docker_out.command_file, micro_out.command_file);
    assert!(docker_out.command_file.is_some());

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
    let docker = socket_for(kind);
    if kind == ExecutionBackendKind::Docker {
        fs.write(&docker, b"socket").unwrap();
    }
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
fn session_lifecycle_inspect_and_wrong_phase_match_across_backends() {
    for kind in [ExecutionBackendKind::Docker, ExecutionBackendKind::MicroVm] {
        let toml = match kind {
            ExecutionBackendKind::Docker => "[execution]\nbackend = \"docker\"\n",
            ExecutionBackendKind::MicroVm => "[execution]\nbackend = \"microvm\"\n",
        };
        let file = ExecutionFile::parse_toml(toml).unwrap();
        let mut fs = MemoryFs::default();
        let docker = socket_for(kind);
        if kind == ExecutionBackendKind::Docker {
            fs.write(&docker, b"socket").unwrap();
        }
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
        let mut vsock = LoopbackVsock::with_ready("job-lifecycle", 1);
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        if kind == ExecutionBackendKind::MicroVm {
            world.allow_inline_guest_plan = false;
            world.vsock = Some(&mut vsock);
        }
        let mut session = open_session(
            &file,
            IsolationIdentity::new("job-lifecycle", 1),
            &mut world,
        )
        .unwrap();
        assert_eq!(session.inspect(), BackendPhase::Preflighted, "{kind}");
        let plan = ValidatedPlan::example_success("job-lifecycle");
        let err = session.execute(&plan, &mut world).unwrap_err();
        assert!(
            matches!(
                err,
                ExecutionError::WrongPhase {
                    required: BackendPhase::Started,
                    actual: BackendPhase::Preflighted
                }
            ),
            "{kind}: {err}"
        );
        assert_eq!(session.inspect(), BackendPhase::Preflighted, "{kind}");
        session.reserve(&mut world).unwrap();
        assert_eq!(session.inspect(), BackendPhase::Reserved, "{kind}");
        session.prepare(&plan, &mut world).unwrap();
        assert_eq!(session.inspect(), BackendPhase::Prepared, "{kind}");
        let err = session.collect().unwrap_err();
        assert!(
            matches!(err, ExecutionError::CollectBeforeStop),
            "{kind}: {err}"
        );
        session.start(&mut world).unwrap();
        assert_eq!(session.inspect(), BackendPhase::Started, "{kind}");
        session.execute(&plan, &mut world).unwrap();
        assert_eq!(session.inspect(), BackendPhase::Stopped, "{kind}");
        let outcome = session.collect().unwrap();
        assert!(!outcome.cleaned, "{kind}");
        let torn = session.teardown(&mut world).unwrap();
        assert!(torn.cleaned, "{kind}");
        assert_eq!(session.inspect(), BackendPhase::TornDown, "{kind}");
        match kind {
            ExecutionBackendKind::Docker => assert!(session.used_host_docker(), "{kind}"),
            ExecutionBackendKind::MicroVm => {
                assert!(!session.used_host_docker(), "{kind}");
                assert!(session.used_firecracker(), "{kind}");
            }
        }
    }
}

#[test]
fn collect_while_live_is_forbidden_and_snapshot_mismatch_fails_closed() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"docker\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let docker = socket_for(ExecutionBackendKind::Docker);
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
    let docker = socket_for(kind);
    if kind == ExecutionBackendKind::Docker {
        fs.write(&docker, b"socket").unwrap();
    }
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
        Path::new(MICROVM_ISOLATION_ROOT),
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
            .prepare(&ValidatedPlan::example_success("job-restore"), &mut world)
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
        .prepare(&ValidatedPlan::example_success("job-current"), &mut world)
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
    assert!(
        !executor_is_proven_at(
            &dir,
            ExecutionBackendKind::MicroVm,
            Path::new("/no-docker.sock"),
            &dir
        ),
        "generation without jailed guest-docker probe must not advertise"
    );
    crate::node::prove::write_microvm_executor_ok(
        &dir,
        &generation.clone().with_jailed_guest_docker_probe(),
    )
    .unwrap();
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
    let docker = socket_for(ExecutionBackendKind::Docker);
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
    assert!(outcome.step_summaries.is_empty());
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
        let plan = ValidatedPlan::example_success("job-vsock");
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
fn microvm_result_bridge_collects_command_files_outputs_logs_and_teardown() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
    let mut vsock = LoopbackVsock::with_ready("job-bridge", 1)
        .with_step_completions([("run".into(), true), ("executed".into(), false)]);
    let outcome = {
        let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
        world.allow_inline_guest_plan = false;
        world.vsock = Some(&mut vsock);
        let mut session =
            open_session(&file, IsolationIdentity::new("job-bridge", 1), &mut world).unwrap();
        session.reserve(&mut world).unwrap();
        let mut plan = ValidatedPlan::example_success("job-bridge");
        plan.buildx = false;
        plan.testcontainers = false;
        plan.cache.clear();
        plan.artifacts.clear();
        plan.annotations.clear();
        plan.summary.clear();
        let mut executed_step = plan.steps[0].clone();
        executed_step.id = "executed".into();
        plan.steps.push(executed_step);
        session.prepare(&plan, &mut world).unwrap();
        session.start(&mut world).unwrap();
        session.execute(&plan, &mut world).unwrap();
        session.collect().unwrap()
    };
    assert_eq!(outcome.conclusion, "success");
    assert_eq!(outcome.executed_physical_actions, Some(1));
    assert!(
        outcome
            .command_file_bytes
            .iter()
            .any(|(path, bytes)| path == "GITHUB_ENV" && !bytes.is_empty()),
        "{:?}",
        outcome.command_file_bytes
    );
    assert_eq!(
        outcome.environment_url.as_deref(),
        Some("https://example.test/env")
    );
    assert!(outcome
        .outputs
        .iter()
        .any(|(name, value)| name == "result" && value == "ok"));
    assert!(outcome
        .log_lines
        .iter()
        .any(|line| line.contains("result-bridge")));
}

#[test]
fn microvm_rejects_replayed_step_frames() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
    let mut vsock = LoopbackVsock::with_ready("job-replay-step", 1)
        .with_step_completions([("run".into(), false), ("run".into(), false)]);
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    world.allow_inline_guest_plan = false;
    world.vsock = Some(&mut vsock);
    let mut session = open_session(
        &file,
        IsolationIdentity::new("job-replay-step", 1),
        &mut world,
    )
    .unwrap();
    session.reserve(&mut world).unwrap();
    let plan = ValidatedPlan::example_success("job-replay-step");
    session.prepare(&plan, &mut world).unwrap();
    session.start(&mut world).unwrap();
    let error = session.execute(&plan, &mut world).unwrap_err();
    assert!(
        error.to_string().contains("duplicate step start"),
        "{error}"
    );
}

#[test]
fn microvm_rejects_frames_after_job_completion() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
    let mut vsock = LoopbackVsock::with_ready("job-terminal-replay", 1)
        .with_post_completion_frames([velnor_model::VsockMessage::StepStarted {
            step_id: "run".into(),
        }]);
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    world.allow_inline_guest_plan = false;
    world.vsock = Some(&mut vsock);
    let mut session = open_session(
        &file,
        IsolationIdentity::new("job-terminal-replay", 1),
        &mut world,
    )
    .unwrap();
    session.reserve(&mut world).unwrap();
    let plan = ValidatedPlan::example_success("job-terminal-replay");
    session.prepare(&plan, &mut world).unwrap();
    session.start(&mut world).unwrap();
    let error = session.execute(&plan, &mut world).unwrap_err();
    assert!(
        error.to_string().contains("after terminal completion"),
        "{error}"
    );
}

#[test]
fn microvm_execute_without_vsock_fails_closed() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
    let mut plan = ValidatedPlan::example_success("job-unsupported");
    plan.buildx = true;
    let error = session.prepare(&plan, &mut world).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("field 'guest.buildx'"), "{text}");
    assert!(text.contains("received 'true'"), "{text}");
    assert!(
        text.contains("accepted 'false until native guest buildx execution is implemented'"),
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
    let plan = ValidatedPlan::example_success("job-ack");
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands::default();
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let plan = ValidatedPlan::example_success("job-replay");
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
    assert_eq!(docker.command_file, micro.command_file);
    assert!(docker.command_file.is_some());
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
fn microvm_teardown_kill_failure_fences_and_attempts_network_cleanup() {
    let resources = IsolationResources::for_identity(
        IsolationIdentity::new("job-kill-failure", 1),
        Path::new(MICROVM_ISOLATION_ROOT),
    );
    let mut fs = MemoryFs::default();
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
        Path::new(MICROVM_ISOLATION_ROOT),
    );
    let mut fs = MemoryFs::default();
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
    let docker = socket_for(ExecutionBackendKind::MicroVm);
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
        "/var/lib/velnor/microvm",
    ] {
        assert!(postinst.contains(needle), "postinst missing {needle}");
    }
    let daemon = include_str!("../../debian/velnor-daemon.service");
    let template = include_str!("../../debian/velnor-daemon@.service");
    for unit in [daemon, template] {
        assert!(
            !unit.contains("Requires=docker.service"),
            "microvm pools must start without docker.service"
        );
        assert!(
            !unit.contains("--require-docker-socket"),
            "docker socket requirement is derived from execution.toml"
        );
    }
    let cargo = include_str!("../../Cargo.toml");
    assert!(
        cargo.contains("recommends = \"docker.io | docker-ce\""),
        "host docker is recommended for the docker backend, not a hard package depend"
    );
    let release = include_str!("../../../../.github/workflows/release.yml");
    assert!(
        release.contains("stage --root crates/velnor-runner/release/microvm --arch \"$pin_arch\""),
        "release staging must verify source-manifest checksums for the package arch"
    );
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
fn synthetic_microvm_probe_runs_guest_docker_without_host_socket() {
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = socket_for(ExecutionBackendKind::MicroVm);
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands {
        next: CommandResult {
            code: 0,
            stdout: "Docker version 29.7.2".into(),
            stderr: String::new(),
        },
        ..RecordingCommands::default()
    };
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let mut vsock = LoopbackVsock::with_ready("velnor-probe", 1);
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    world.allow_inline_guest_plan = false;
    world.vsock = Some(&mut vsock);
    crate::execution::run_synthetic_microvm_probe_with(&mut world, "velnor-probe").unwrap();
    assert!(runner
        .calls
        .iter()
        .any(|(program, _)| program.contains("jailer") || program == "jailer"));
    assert!(!runner.calls.iter().any(|(program, args)| {
        program == "docker" && args.iter().any(|arg| arg.contains("docker.sock"))
    }));
}

#[test]
fn synthetic_microvm_probe_fails_closed_when_guest_docker_unhealthy() {
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = socket_for(ExecutionBackendKind::MicroVm);
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands::default();
    let mut api = RecordingFirecracker::default();
    let kvm = PathBuf::from("/dev/kvm");
    let mut vsock = LoopbackVsock::with_unhealthy_docker("velnor-probe", 1);
    let mut world = world(&mut fs, &mut runner, &mut api, &kvm, &artifacts, &docker);
    world.allow_inline_guest_plan = false;
    world.vsock = Some(&mut vsock);
    let err =
        crate::execution::run_synthetic_microvm_probe_with(&mut world, "velnor-probe").unwrap_err();
    assert!(err.to_string().contains("guest Docker"), "{err}");
    assert!(!runner.calls.iter().any(|(program, args)| {
        program == "docker" && args.iter().any(|arg| arg.contains("docker.sock"))
    }));
}

#[test]
fn jailer_failure_does_not_touch_host_docker_socket() {
    let file = ExecutionFile::parse_toml("[execution]\nbackend = \"microvm\"\n").unwrap();
    let mut fs = MemoryFs::default();
    let artifacts = PathBuf::from("/microvm");
    seed_microvm_world(&mut fs, &artifacts);
    let docker = socket_for(ExecutionBackendKind::MicroVm);
    fs.write(&docker, b"socket").unwrap();
    let mut runner = RecordingCommands {
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
    assert!(api.calls.is_empty() || !api.calls.iter().any(|c| c.contains("docker")));
}

#[test]
fn fixture_parity_yaml_keeps_lanes_choice() {
    let yaml = include_str!("../../../../docs/fixture-backend-parity.yml");
    assert!(yaml.contains("lanes:"), "{yaml}");
    assert!(yaml.contains("options: [velnor, github, both]"), "{yaml}");
    assert!(yaml.contains("GITHUB_OUTPUT"), "{yaml}");
    assert!(yaml.contains("GITHUB_ENV"), "{yaml}");
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
