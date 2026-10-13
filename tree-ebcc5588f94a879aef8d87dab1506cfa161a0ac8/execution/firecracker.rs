//! Jailed Firecracker backend. Guest-local Docker; no host docker.sock.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use velnor_model::{
    derive_execution_nonce, JobConclusion, MicroVmKind, MicroVmPreflightFailure, VsockMessage,
};

use super::artifacts::{
    hex_sha256, verify_microvm_artifacts, MicroVmArtifactSet, MicroVmGeneration,
    FIRECRACKER_VERSION,
};
use super::backend::{ExecutionError, ExecutionEvent, ValidatedPlan};
use super::guest_runtime::validate_guest_plan;
use super::isolation::{IsolationIdentity, IsolationResources};
use super::net::{setup_net_invocations, teardown_net_invocations};
use super::snapshot::{read_identity, vmstate_path, write_identity, GuestReady, SnapshotIdentity};
use super::ExecutionWorld;
use crate::executor::SpawnedProcess;

/// Injectable Firecracker HTTP API (Unix socket). Tests use [`RecordingFirecracker`].
pub trait FirecrackerApi {
    /// # Errors
    /// API transport or protocol failures.
    fn put_boot_source(&mut self, kernel: &Path, boot_args: &str) -> Result<(), String>;
    /// # Errors
    /// API transport or protocol failures.
    fn put_drive(&mut self, drive_id: &str, path: &Path, read_only: bool) -> Result<(), String>;
    /// # Errors
    /// API transport or protocol failures.
    fn put_network_interface(&mut self, iface_id: &str, tap: &str) -> Result<(), String>;
    /// # Errors
    /// API transport or protocol failures.
    fn put_vsock(&mut self, guest_cid: u32, uds: &Path) -> Result<(), String>;
    /// # Errors
    /// API transport or protocol failures.
    fn instance_start(&mut self) -> Result<(), String>;
    /// Pause a running VM. Required before [`FirecrackerApi::create_snapshot`].
    ///
    /// # Errors
    /// API transport or protocol failures.
    fn pause_vm(&mut self) -> Result<(), String>;
    /// Resume a paused VM.
    ///
    /// # Errors
    /// API transport or protocol failures.
    fn resume_vm(&mut self) -> Result<(), String>;
    /// # Errors
    /// API transport or protocol failures.
    fn create_snapshot(&mut self, mem: &Path, vmstate: &Path) -> Result<(), String>;
    /// # Errors
    /// Snapshot format/version mismatch must not restore permissively.
    fn load_snapshot(
        &mut self,
        mem: &Path,
        vmstate: &Path,
        expected_version: &str,
    ) -> Result<(), String>;
}

/// Records API calls for contract tests.
#[derive(Debug, Default)]
pub struct RecordingFirecracker {
    pub calls: Vec<String>,
    pub fail_load_snapshot: bool,
}

impl FirecrackerApi for RecordingFirecracker {
    fn put_boot_source(&mut self, kernel: &Path, boot_args: &str) -> Result<(), String> {
        self.calls.push(format!(
            "put_boot_source {} args={boot_args}",
            kernel.display()
        ));
        Ok(())
    }

    fn put_drive(&mut self, drive_id: &str, path: &Path, read_only: bool) -> Result<(), String> {
        self.calls.push(format!(
            "put_drive {drive_id} {} ro={read_only}",
            path.display()
        ));
        Ok(())
    }

    fn put_network_interface(&mut self, iface_id: &str, tap: &str) -> Result<(), String> {
        self.calls
            .push(format!("put_network_interface {iface_id} {tap}"));
        Ok(())
    }

    fn put_vsock(&mut self, guest_cid: u32, uds: &Path) -> Result<(), String> {
        self.calls
            .push(format!("put_vsock cid={guest_cid} {}", uds.display()));
        Ok(())
    }

    fn instance_start(&mut self) -> Result<(), String> {
        self.calls.push("instance_start".into());
        Ok(())
    }

    fn pause_vm(&mut self) -> Result<(), String> {
        self.calls.push("pause_vm".into());
        Ok(())
    }

    fn resume_vm(&mut self) -> Result<(), String> {
        self.calls.push("resume_vm".into());
        Ok(())
    }

    fn create_snapshot(&mut self, mem: &Path, vmstate: &Path) -> Result<(), String> {
        self.calls.push(format!(
            "create_snapshot {} {}",
            mem.display(),
            vmstate.display()
        ));
        Ok(())
    }

    fn load_snapshot(
        &mut self,
        mem: &Path,
        vmstate: &Path,
        expected_version: &str,
    ) -> Result<(), String> {
        if self.fail_load_snapshot || expected_version != FIRECRACKER_VERSION {
            return Err(format!(
                "snapshot version {expected_version} mismatch (pinned {FIRECRACKER_VERSION}); cold boot required"
            ));
        }
        self.calls.push(format!(
            "load_snapshot {} {}",
            mem.display(),
            vmstate.display()
        ));
        Ok(())
    }
}

/// Firecracker guest CID used for vsock.
pub const FIRECRACKER_GUEST_CID: u32 = 3;

/// One jailed Firecracker process per GitHub job.
#[derive(Debug, Default)]
pub struct FirecrackerBackend {
    pub guest_cid: u32,
    pub started: bool,
    pub restored: bool,
    pub jailer: Option<SpawnedProcess>,
    pub(crate) execution: Option<GuestExecutionState>,
    pub(crate) session_challenge: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuestExecutionState {
    pub(crate) job_id: String,
    pub(crate) isolation_id: String,
    pub(crate) generation: u64,
    pub(crate) execution_nonce: String,
    pub(crate) plan_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GuestHandshake {
    ready: GuestReady,
    session_challenge: String,
}

impl FirecrackerBackend {
    /// # Errors
    /// Missing KVM, artifacts, jailer, or Firecracker VMM selection.
    pub fn preflight(world: &mut ExecutionWorld<'_>) -> Result<(), ExecutionError> {
        if MicroVmKind::Firecracker.activate_production().is_err() {
            return Err(MicroVmPreflightFailure::new(
                "vmm",
                "Firecracker is not the production microVM",
            )
            .into());
        }
        if !world.host_fs.exists(world.kvm) {
            return Err(MicroVmPreflightFailure::new(
                "kvm",
                format!("{} is missing or unusable", world.kvm.display()),
            )
            .into());
        }
        if super::isolation::is_host_docker_control_socket(world.host_docker_socket) {
            return Err(ExecutionError::HostDockerForbidden);
        }
        let set = MicroVmArtifactSet::load(world.artifact_root, world.host_fs)?;
        verify_microvm_artifacts(&set, world.host_fs)?;
        let jailer = world
            .runner
            .run(
                set.jailer.path.to_str().unwrap_or("jailer"),
                &["--version".into()],
            )
            .map_err(|error| MicroVmPreflightFailure::new("jailer", error.to_string()))?;
        if jailer.code != 0 {
            return Err(MicroVmPreflightFailure::new(
                "jailer",
                format!("jailer --version exited {}", jailer.code),
            )
            .into());
        }
        Ok(())
    }

    /// Jailer argv for one isolation ID. Extra args after `--` go to Firecracker.
    #[must_use]
    pub fn jailer_args(resources: &IsolationResources, exec_file: &Path) -> Vec<String> {
        vec![
            "--id".into(),
            resources.identity.as_jailer_id(),
            "--exec-file".into(),
            exec_file.display().to_string(),
            "--uid".into(),
            "123".into(),
            "--gid".into(),
            "100".into(),
            "--cgroup-version".into(),
            "2".into(),
            "--chroot-base-dir".into(),
            resources.chroot_base.display().to_string(),
            "--netns".into(),
            resources.netns.display().to_string(),
            "--new-pid-ns".into(),
            "--resource-limit".into(),
            "no-file=1024".into(),
            "--".into(),
            "--api-sock".into(),
            "/run/firecracker.socket".into(),
        ]
    }

    pub(crate) fn prepare(
        &mut self,
        plan: &ValidatedPlan,
        resources: &IsolationResources,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        if plan.job_id != resources.identity.id {
            return Err(MicroVmPreflightFailure::new(
                "guest.plan.job_id",
                format!("{} != expected {}", plan.job_id, resources.identity.id),
            )
            .into());
        }
        let guest = plan.to_guest(&resources.identity.id, resources.identity.generation);
        validate_guest_plan(&guest)
            .map_err(|detail| MicroVmPreflightFailure::new("guest.plan", detail))?;
        let _ = guest_boot_args(&resources.identity)?;
        let set = MicroVmArtifactSet::load(world.artifact_root, world.host_fs)?;
        let identity_ok = snapshot_identity_matches(&set, world, &resources.identity);
        setup_guest_net(resources, world, events)?;
        self.jailer = Some(spawn_jailer(&set, resources, world, events)?);
        if identity_ok && try_restore_snapshot(&set, world, events)? {
            self.restored = true;
            self.guest_cid = FIRECRACKER_GUEST_CID;
            return Ok(());
        }
        if identity_ok {
            // Load taints a Firecracker process; a failed load needs a fresh VMM.
            if let Some(previous) = self.jailer.clone() {
                world.runner.kill(&previous).map_err(|error| {
                    ExecutionError::Isolation(format!(
                        "kill jailer pid {} after snapshot failure: {error:#}",
                        previous.pid
                    ))
                })?;
                self.jailer = None;
            }
            self.jailer = Some(spawn_jailer(&set, resources, world, events)?);
        }
        cold_configure(self, &set, resources, world, events)
    }

    pub(crate) fn start(
        &mut self,
        isolation: &IsolationIdentity,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        if !self.restored {
            world
                .firecracker
                .instance_start()
                .map_err(|detail| MicroVmPreflightFailure::new("firecracker.api", detail))?;
            events.push(ExecutionEvent::FirecrackerApi("instance_start".into()));
        }
        self.started = true;
        events.push(ExecutionEvent::GuestDocker("guest started".into()));
        if world.allow_inline_guest_plan {
            return Ok(());
        }

        // A restored VM is already running because snapshot/load uses
        // resume_vm=true. Both cold boot and restore must complete a fresh
        // control handshake before a plan can be delivered.
        let handshake = establish_guest_session(world, isolation, self.restored)?;
        if self.restored {
            self.session_challenge = Some(handshake.session_challenge);
        } else {
            // The guest leaves the first session before the VM is paused. The
            // snapshot therefore contains an agent waiting in accept(), not a
            // plan-read loop. Resume it through a second, real handshake.
            prepare_guest_snapshot(world)?;
            // Errors here leave the guest resumed (see create_golden_snapshot);
            // the snapshot itself is an optimization, so its failure is an
            // event, not a job failure.
            create_golden_snapshot(world, isolation, handshake.ready, events)?;
            let rebootstrap = establish_guest_session(world, isolation, false)?;
            self.session_challenge = Some(rebootstrap.session_challenge);
        }
        Ok(())
    }

    pub(crate) fn execute(
        &mut self,
        plan: &ValidatedPlan,
        isolation: &IsolationIdentity,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        validate_plan_identity(plan, isolation)?;
        if self.execution.is_some() {
            return Err(MicroVmPreflightFailure::new(
                "guest.execution_nonce",
                "one execution is already bound to this microVM session; replay rejected",
            )
            .into());
        }
        let guest = plan.to_guest(&isolation.id, isolation.generation);
        let bytes = guest
            .encode()
            .map_err(|detail| MicroVmPreflightFailure::new("vsock.plan", detail))?;
        let execution_nonce = if world.vsock.is_some() {
            let session_challenge = self.session_challenge.as_deref().ok_or_else(|| {
                MicroVmPreflightFailure::new(
                    "guest.ready.session_challenge",
                    "missing guest-issued session challenge; fresh GuestReady required",
                )
            })?;
            derive_execution_nonce(
                session_challenge,
                &guest.job_id,
                &guest.isolation_id,
                guest.generation,
                &hex_sha256(&bytes),
            )
        } else {
            uuid::Uuid::new_v4().to_string()
        };
        let state = GuestExecutionState {
            job_id: guest.job_id.clone(),
            isolation_id: guest.isolation_id.clone(),
            generation: guest.generation,
            execution_nonce,
            plan_sha256: hex_sha256(&bytes),
        };
        self.execution = Some(state.clone());
        events.push(ExecutionEvent::FirecrackerApi("vsock DeliverPlan".into()));
        if let Some(vsock) = world.vsock.as_mut() {
            return drive_vsock(
                &mut **vsock,
                VsockMessage::DeliverPlan {
                    job_id: guest.job_id.clone(),
                    isolation_id: guest.isolation_id.clone(),
                    generation: guest.generation,
                    execution_nonce: state.execution_nonce.clone(),
                    plan_sha256: state.plan_sha256.clone(),
                    plan_bytes: bytes,
                },
                &guest,
                &state,
                events,
                Duration::from_millis(plan.timeout_ms.max(1)),
            );
        }
        if !world.allow_inline_guest_plan {
            return Err(MicroVmPreflightFailure::new(
                "vsock",
                "microvm execute requires a vsock channel",
            )
            .into());
        }
        super::guest_runtime::handle_delivered_plan(&bytes, world.runner, events)
            .map_err(|detail| MicroVmPreflightFailure::new("guest.docker", detail))?;
        Ok(())
    }

    pub(crate) fn cancel(
        &mut self,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        events.push(ExecutionEvent::FirecrackerApi("cancel".into()));
        events.push(ExecutionEvent::JobCompleted {
            conclusion: JobConclusion::Cancelled,
            exit_code: 1,
        });
        self.stop_jailer(world, events)?;
        Ok(())
    }

    pub(crate) fn teardown(
        &mut self,
        resources: &IsolationResources,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        let mut failures = Vec::new();
        if let Err(error) = self.stop_jailer(world, events) {
            failures.push(error.to_string());
        }
        for (program, args) in teardown_net_invocations(resources) {
            match world.runner.run(&program, &args) {
                Ok(result) if result.code == 0 => {}
                Ok(result) => failures.push(format!(
                    "network teardown {program} {} exited {}",
                    args.join(" "),
                    result.code
                )),
                Err(error) => failures.push(format!(
                    "network teardown {program} {}: {error:#}",
                    args.join(" ")
                )),
            }
        }
        events.push(ExecutionEvent::FirecrackerApi("teardown_net".into()));
        if failures.is_empty() {
            Ok(())
        } else {
            Err(ExecutionError::Isolation(format!(
                "microvm teardown uncertain: {}",
                failures.join("; ")
            )))
        }
    }

    fn stop_jailer(
        &mut self,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        if let Some(jailer) = self.jailer.clone() {
            world.runner.kill(&jailer).map_err(|error| {
                ExecutionError::Isolation(format!("kill jailer pid {}: {error:#}", jailer.pid))
            })?;
            self.jailer = None;
            events.push(ExecutionEvent::FirecrackerApi(format!(
                "kill jailer pid {}",
                jailer.pid
            )));
        }
        Ok(())
    }
}

fn setup_guest_net(
    resources: &IsolationResources,
    world: &mut ExecutionWorld<'_>,
    events: &mut Vec<ExecutionEvent>,
) -> Result<(), ExecutionError> {
    for (program, args) in setup_net_invocations(resources) {
        let result = world
            .runner
            .run(&program, &args)
            .map_err(|error| MicroVmPreflightFailure::new("guest.net", error.to_string()))?;
        if result.code != 0 {
            return Err(MicroVmPreflightFailure::new(
                "guest.net",
                format!("{program} exited {}: {}", result.code, result.stderr),
            )
            .into());
        }
    }
    events.push(ExecutionEvent::FirecrackerApi("setup_net".into()));
    Ok(())
}

fn validate_plan_identity(
    plan: &ValidatedPlan,
    isolation: &IsolationIdentity,
) -> Result<(), ExecutionError> {
    if plan.job_id != isolation.id {
        return Err(MicroVmPreflightFailure::new(
            "guest.plan.job_id",
            format!("{} != expected {}", plan.job_id, isolation.id),
        )
        .into());
    }
    Ok(())
}

fn send_guest_identity(
    world: &mut ExecutionWorld<'_>,
    identity: &IsolationIdentity,
    restored: bool,
) -> Result<(), ExecutionError> {
    let Some(vsock) = world.vsock.as_mut() else {
        return Err(MicroVmPreflightFailure::new(
            "guest.identity",
            "production microVM requires a vsock control channel",
        )
        .into());
    };
    vsock
        .send(VsockMessage::GuestIdentity {
            isolation_id: identity.id.clone(),
            generation: identity.generation,
            restored,
        })
        .map_err(|detail| MicroVmPreflightFailure::new("guest.identity", detail).into())
}

fn establish_guest_session(
    world: &mut ExecutionWorld<'_>,
    expected: &IsolationIdentity,
    restored: bool,
) -> Result<GuestHandshake, ExecutionError> {
    send_guest_identity(world, expected, restored)?;
    let ready = receive_guest_ready(world, expected)?;
    let session_challenge = ready.session_challenge.clone();
    Ok(GuestHandshake {
        ready: ready.ready,
        session_challenge,
    })
}

fn receive_guest_ready(
    world: &mut ExecutionWorld<'_>,
    expected: &IsolationIdentity,
) -> Result<ReadyMessage, ExecutionError> {
    let Some(vsock) = world.vsock.as_mut() else {
        return Err(MicroVmPreflightFailure::new(
            "guest.ready",
            "production microVM requires a guest readiness handshake over vsock",
        )
        .into());
    };
    vsock.set_idle_timeout(Duration::from_secs(30));
    let message = vsock.recv().map_err(|detail| {
        MicroVmPreflightFailure::new(
            "guest.ready",
            format!("readiness handshake failed: {detail}"),
        )
    })?;
    let VsockMessage::GuestReady {
        isolation_id,
        generation,
        docker_healthy,
        job_credentials_absent,
        session_challenge,
    } = message
    else {
        return Err(MicroVmPreflightFailure::new(
            "guest.ready",
            "expected GuestReady as the first guest message",
        )
        .into());
    };
    if isolation_id != expected.id {
        return Err(MicroVmPreflightFailure::new(
            "guest.ready.isolation_id",
            format!("{isolation_id} != expected {}", expected.id),
        )
        .into());
    }
    if generation != expected.generation {
        return Err(MicroVmPreflightFailure::new(
            "guest.ready.generation",
            format!("{generation} != expected {}", expected.generation),
        )
        .into());
    }
    if uuid::Uuid::parse_str(&session_challenge).is_err() {
        return Err(MicroVmPreflightFailure::new(
            "guest.ready.session_challenge",
            format!("received {session_challenge:?}; accepted a guest-generated UUID challenge"),
        )
        .into());
    }
    let ready = GuestReady {
        agent_listening: true,
        docker_healthy,
        job_credentials_absent,
    };
    ready.credential_free_or_err()?;
    Ok(ReadyMessage {
        ready,
        session_challenge,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadyMessage {
    ready: GuestReady,
    session_challenge: String,
}

fn prepare_guest_snapshot(world: &mut ExecutionWorld<'_>) -> Result<(), ExecutionError> {
    let Some(vsock) = world.vsock.as_mut() else {
        return Err(MicroVmPreflightFailure::new(
            "guest.snapshot",
            "production microVM requires a vsock control channel before snapshot",
        )
        .into());
    };
    vsock
        .send(VsockMessage::PrepareSnapshot)
        .map_err(|detail| MicroVmPreflightFailure::new("guest.snapshot", detail))?;
    match vsock.recv() {
        Ok(VsockMessage::SnapshotReady) => {
            vsock.reset();
            Ok(())
        }
        Ok(other) => Err(MicroVmPreflightFailure::new(
            "guest.snapshot",
            format!("expected SnapshotReady, received {other:?}"),
        )
        .into()),
        Err(detail) => Err(MicroVmPreflightFailure::new(
            "guest.snapshot",
            format!("pre-snapshot handshake failed: {detail}"),
        )
        .into()),
    }
}

fn spawn_jailer(
    set: &MicroVmArtifactSet,
    resources: &IsolationResources,
    world: &mut ExecutionWorld<'_>,
    events: &mut Vec<ExecutionEvent>,
) -> Result<SpawnedProcess, ExecutionError> {
    let jailer_bin = set.jailer.path.to_str().unwrap_or("jailer");
    let jailer_args = FirecrackerBackend::jailer_args(resources, &set.firecracker.path);
    let spawned = world
        .runner
        .spawn(jailer_bin, &jailer_args)
        .map_err(|error| MicroVmPreflightFailure::new("jailer", error.to_string()))?;
    events.push(ExecutionEvent::FirecrackerApi(format!(
        "jailer pid {}",
        spawned.pid
    )));
    Ok(spawned)
}

fn cold_configure(
    backend: &mut FirecrackerBackend,
    set: &MicroVmArtifactSet,
    resources: &IsolationResources,
    world: &mut ExecutionWorld<'_>,
    events: &mut Vec<ExecutionEvent>,
) -> Result<(), ExecutionError> {
    let boot_args = guest_boot_args(&resources.identity)?;
    world
        .firecracker
        .put_boot_source(&set.kernel.path, &boot_args)
        .map_err(|detail| MicroVmPreflightFailure::new("firecracker.api", detail))?;
    events.push(ExecutionEvent::FirecrackerApi("put_boot_source".into()));
    world
        .firecracker
        .put_drive("rootfs", &set.rootfs.path, true)
        .map_err(|detail| MicroVmPreflightFailure::new("firecracker.api", detail))?;
    world
        .firecracker
        .put_drive("scratch", &resources.writable_disk, false)
        .map_err(|detail| MicroVmPreflightFailure::new("firecracker.api", detail))?;
    world
        .firecracker
        .put_network_interface("eth0", &resources.tap)
        .map_err(|detail| MicroVmPreflightFailure::new("firecracker.api", detail))?;
    backend.guest_cid = FIRECRACKER_GUEST_CID;
    world
        .firecracker
        .put_vsock(backend.guest_cid, &resources.vsock)
        .map_err(|detail| MicroVmPreflightFailure::new("firecracker.api", detail))?;
    events.push(ExecutionEvent::FirecrackerApi("devices".into()));
    Ok(())
}

fn guest_boot_args(identity: &IsolationIdentity) -> Result<String, ExecutionError> {
    if identity.id.is_empty()
        || !identity
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(MicroVmPreflightFailure::new(
            "guest.identity",
            format!(
                "received {:?}; accepted ASCII letters, digits, '-' or '_' in isolation_id",
                identity.id
            ),
        )
        .into());
    }
    if identity.generation == 0 {
        return Err(MicroVmPreflightFailure::new(
            "guest.identity.generation",
            "received 0; accepted a non-zero generation",
        )
        .into());
    }
    Ok(format!(
        "console=ttyS0 reboot=k panic=1 pci=off velnor.isolation_id={} velnor.isolation_generation={}",
        identity.id, identity.generation
    ))
}

fn snapshot_mem_path(set: &MicroVmArtifactSet, root: &Path) -> PathBuf {
    set.snapshot
        .as_ref()
        .map(|snapshot| snapshot.path.clone())
        .unwrap_or_else(|| root.join("snapshot.mem"))
}

fn snapshot_identity_matches(
    set: &MicroVmArtifactSet,
    world: &ExecutionWorld<'_>,
    isolation: &IsolationIdentity,
) -> bool {
    let mem = snapshot_mem_path(set, world.artifact_root);
    if !world.host_fs.exists(&mem) || !world.host_fs.exists(&vmstate_path(&mem)) {
        return false;
    }
    let generation = MicroVmGeneration::from_set(set);
    let want = SnapshotIdentity::from_generation(
        &generation,
        std::env::consts::ARCH,
        "linux-6.1",
        isolation,
    );
    let Ok(have) = read_identity(world.host_fs, &mem) else {
        return false;
    };
    want.restore_or_cold_boot(&have).is_ok()
}

/// Pause a credential-free ready guest and persist mem+vmstate+identity.
///
/// Every path out of here leaves the guest resumed: a failed snapshot is an
/// observability event, never a paused VM that a later execute would wait on
/// until the vsock timeout. Only a failed resume (or a failure before the
/// pause) is an error.
///
/// # Errors
/// Credentials present, artifact load, or Firecracker API failure.
pub fn create_golden_snapshot(
    world: &mut ExecutionWorld<'_>,
    isolation: &IsolationIdentity,
    ready: GuestReady,
    events: &mut Vec<ExecutionEvent>,
) -> Result<(), ExecutionError> {
    ready.credential_free_or_err()?;
    let set = MicroVmArtifactSet::load(world.artifact_root, world.host_fs)?;
    let mem = world.artifact_root.join("snapshot.mem");
    let vmstate = vmstate_path(&mem);
    world
        .firecracker
        .pause_vm()
        .map_err(|detail| MicroVmPreflightFailure::new("guest.snapshot", detail))?;
    events.push(ExecutionEvent::FirecrackerApi("pause_vm".into()));
    let persisted = persist_snapshot(world, &set, &mem, &vmstate, isolation, events);
    if let Err(failure) = persisted {
        world
            .firecracker
            .resume_vm()
            .map_err(|resume_detail| {
                MicroVmPreflightFailure::new(
                    "guest.snapshot",
                    format!("{failure}; resume also failed: {resume_detail}"),
                )
            })
            .map_err(ExecutionError::from)?;
        events.push(ExecutionEvent::FirecrackerApi("resume_vm".into()));
        events.push(ExecutionEvent::FirecrackerApi(format!(
            "golden snapshot failed (guest resumed): {failure}"
        )));
        return Ok(());
    }
    world
        .firecracker
        .resume_vm()
        .map_err(|detail| MicroVmPreflightFailure::new("guest.snapshot", detail))?;
    events.push(ExecutionEvent::FirecrackerApi("resume_vm".into()));
    Ok(())
}

fn persist_snapshot(
    world: &mut ExecutionWorld<'_>,
    set: &MicroVmArtifactSet,
    mem: &Path,
    vmstate: &Path,
    isolation: &IsolationIdentity,
    events: &mut Vec<ExecutionEvent>,
) -> Result<(), ExecutionError> {
    world
        .firecracker
        .create_snapshot(mem, vmstate)
        .map_err(|detail| MicroVmPreflightFailure::new("guest.snapshot", detail))?;
    events.push(ExecutionEvent::FirecrackerApi("create_snapshot".into()));
    let generation = MicroVmGeneration::from_set(set);
    let identity = SnapshotIdentity::from_generation(
        &generation,
        std::env::consts::ARCH,
        "linux-6.1",
        isolation,
    );
    write_identity(world.host_fs, mem, &identity)?;
    Ok(())
}

fn try_restore_snapshot(
    set: &MicroVmArtifactSet,
    world: &mut ExecutionWorld<'_>,
    events: &mut Vec<ExecutionEvent>,
) -> Result<bool, ExecutionError> {
    let mem = snapshot_mem_path(set, world.artifact_root);
    let vmstate = vmstate_path(&mem);
    match restore_or_cold_boot(world.firecracker, &mem, &vmstate, FIRECRACKER_VERSION) {
        Ok(_) => {
            events.push(ExecutionEvent::FirecrackerApi("snapshot_restore".into()));
            events.push(ExecutionEvent::FirecrackerApi("devices".into()));
            Ok(true)
        }
        Err(_) => {
            events.push(ExecutionEvent::FirecrackerApi(
                "snapshot load failed; cold boot".into(),
            ));
            Ok(false)
        }
    }
}

/// Drive the vsock session to its terminal message.
///
/// The plan's declared timeout bounds the whole session: a guest that hangs
/// (or never answers) fails the job at the deadline instead of stalling the
/// slot until the channel's idle timeout.
fn drive_vsock(
    vsock: &mut dyn super::VsockChannel,
    message: VsockMessage,
    expected: &velnor_model::GuestJobPlan,
    state: &GuestExecutionState,
    events: &mut Vec<ExecutionEvent>,
    timeout: Duration,
) -> Result<(), ExecutionError> {
    let VsockMessage::DeliverPlan {
        job_id,
        isolation_id,
        generation,
        execution_nonce,
        plan_sha256,
        plan_bytes,
        ..
    } = &message
    else {
        return Err(MicroVmPreflightFailure::new(
            "vsock.plan",
            "host attempted to send a non-plan message",
        )
        .into());
    };
    if job_id != &expected.job_id
        || isolation_id != &expected.isolation_id
        || *generation != expected.generation
        || job_id != &state.job_id
        || isolation_id != &state.isolation_id
        || *generation != state.generation
        || execution_nonce != &state.execution_nonce
        || plan_sha256 != &state.plan_sha256
        || plan_sha256 != &hex_sha256(plan_bytes)
    {
        return Err(MicroVmPreflightFailure::new(
            "vsock.plan",
            "delivery identity does not match the expected job, isolation, and generation",
        )
        .into());
    }
    let deadline = std::time::Instant::now() + timeout;
    for cache in &expected.cache {
        if cache.bytes.is_empty() {
            continue;
        }
        vsock
            .send(VsockMessage::ImportBlob {
                digest_sha256: cache.digest.clone(),
                bytes: cache.bytes.clone(),
            })
            .map_err(|detail| MicroVmPreflightFailure::new("vsock.import_blob", detail))?;
    }
    vsock
        .send(message)
        .map_err(|detail| MicroVmPreflightFailure::new("vsock", detail))?;
    let mut completed = false;
    let expected_step_ids: BTreeSet<String> =
        expected.steps.iter().map(|step| step.id.clone()).collect();
    let mut started_step_ids = BTreeSet::new();
    let mut completed_step_ids = BTreeSet::new();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(MicroVmPreflightFailure::new(
                "vsock.timeout",
                format!(
                    "job exceeded the plan timeout of {} ms",
                    timeout.as_millis()
                ),
            )
            .into());
        }
        vsock.set_idle_timeout(remaining);
        let received = match vsock.recv() {
            Ok(message) => message,
            Err(detail) => {
                if std::time::Instant::now() >= deadline {
                    return Err(MicroVmPreflightFailure::new(
                        "vsock.timeout",
                        format!(
                            "job exceeded the plan timeout of {} ms (last error: {detail})",
                            timeout.as_millis()
                        ),
                    )
                    .into());
                }
                return Err(MicroVmPreflightFailure::new("vsock", detail).into());
            }
        };
        if completed && !matches!(received, VsockMessage::TeardownAck { .. }) {
            return Err(MicroVmPreflightFailure::new(
                "vsock.job_completed",
                "frame arrived after terminal completion (replay)",
            )
            .into());
        }
        match received {
            VsockMessage::TeardownAck {
                job_id,
                isolation_id,
                generation,
                execution_nonce,
                plan_sha256,
            } => {
                if !completed {
                    return Err(MicroVmPreflightFailure::new(
                        "vsock.teardown_ack",
                        "acknowledgement arrived before JobCompleted",
                    )
                    .into());
                }
                if job_id != expected.job_id
                    || isolation_id != expected.isolation_id
                    || generation != expected.generation
                    || job_id != state.job_id
                    || isolation_id != state.isolation_id
                    || generation != state.generation
                    || execution_nonce != state.execution_nonce
                    || plan_sha256 != state.plan_sha256
                {
                    return Err(MicroVmPreflightFailure::new(
                        "vsock.teardown_ack",
                        "acknowledgement identity does not match the expected job, isolation, and generation",
                    )
                    .into());
                }
                return Ok(());
            }
            VsockMessage::JobCompleted {
                conclusion,
                exit_code,
            } => {
                if completed {
                    return Err(MicroVmPreflightFailure::new(
                        "vsock.job_completed",
                        "duplicate completion message (replay)",
                    )
                    .into());
                }
                if conclusion == JobConclusion::Success {
                    let missing_step_ids = expected_step_ids
                        .difference(&completed_step_ids)
                        .map(String::as_str)
                        .collect::<Vec<_>>();
                    if !missing_step_ids.is_empty() {
                        return Err(MicroVmPreflightFailure::new(
                            "vsock.job_completed",
                            format!(
                                "successful completion missing StepCompleted for delivered plan step(s): {}",
                                missing_step_ids.join(", ")
                            ),
                        )
                        .into());
                    }
                }
                completed = true;
                events.push(ExecutionEvent::JobCompleted {
                    conclusion,
                    exit_code,
                });
            }
            VsockMessage::CommandFile { path, bytes } => {
                events.push(ExecutionEvent::CommandFile { path, bytes });
            }
            VsockMessage::StepStarted { step_id } => {
                if !expected_step_ids.contains(&step_id) {
                    return Err(MicroVmPreflightFailure::new(
                        "vsock.step_started",
                        "step identity is not present in the delivered plan",
                    )
                    .into());
                }
                if !started_step_ids.insert(step_id.clone()) {
                    return Err(MicroVmPreflightFailure::new(
                        "vsock.step_started",
                        "duplicate step start message (replay)",
                    )
                    .into());
                }
                events.push(ExecutionEvent::StepStarted { step_id });
            }
            VsockMessage::StepCompleted {
                step_id,
                exit_code,
                skipped,
            } => {
                if !started_step_ids.contains(&step_id) {
                    return Err(MicroVmPreflightFailure::new(
                        "vsock.step_completed",
                        "step completion arrived without a matching step start",
                    )
                    .into());
                }
                if !completed_step_ids.insert(step_id.clone()) {
                    return Err(MicroVmPreflightFailure::new(
                        "vsock.step_completed",
                        "duplicate step completion message (replay)",
                    )
                    .into());
                }
                events.push(ExecutionEvent::StepCompleted {
                    step_id,
                    exit_code,
                    skipped,
                });
            }
            VsockMessage::ResultExport {
                digest_sha256,
                bytes,
            } => {
                let actual = crate::execution::hex_sha256(&bytes);
                if actual != digest_sha256 {
                    return Err(MicroVmPreflightFailure::new(
                        "vsock.result_export",
                        format!("digest {digest_sha256} != sha256(bytes) {actual}"),
                    )
                    .into());
                }
                events.push(ExecutionEvent::ResultExport {
                    digest_sha256,
                    bytes,
                });
            }
            VsockMessage::Stdio { stream, bytes } => {
                events.push(ExecutionEvent::Log {
                    stream,
                    line: String::from_utf8_lossy(&bytes).into_owned(),
                });
            }
            other => events.push(ExecutionEvent::GuestDocker(format!("{other:?}"))),
        }
    }
}

/// Snapshot restore that refuses a version mismatch (cold boot instead).
///
/// # Errors
/// Version mismatch or API failure.
pub fn restore_or_cold_boot(
    api: &mut dyn FirecrackerApi,
    mem: &Path,
    vmstate: &Path,
    snapshot_version: &str,
) -> Result<PathBuf, MicroVmPreflightFailure> {
    match api.load_snapshot(mem, vmstate, snapshot_version) {
        Ok(()) => Ok(mem.to_path_buf()),
        Err(detail) => Err(MicroVmPreflightFailure::new(
            "guest.snapshot",
            format!("{detail}; using verified cold boot"),
        )),
    }
}
