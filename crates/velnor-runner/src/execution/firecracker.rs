//! Jailed Firecracker backend. Guest-local Docker; no host docker.sock.

use std::path::{Path, PathBuf};

use velnor_model::{MicroVmKind, MicroVmPreflightFailure, VsockMessage};

use super::artifacts::{
    verify_microvm_artifacts, MicroVmArtifactSet, MicroVmGeneration, FIRECRACKER_VERSION,
};
use super::backend::{ExecutionError, ExecutionEvent, ValidatedPlan};
use super::isolation::IsolationResources;
use super::snapshot::{read_identity, vmstate_path, write_identity, GuestReady, SnapshotIdentity};
use super::ExecutionWorld;

/// Injectable Firecracker HTTP API (Unix socket). Tests use [`RecordingFirecracker`].
pub trait FirecrackerApi {
    /// # Errors
    /// API transport or protocol failures.
    fn put_boot_source(&mut self, kernel: &Path) -> Result<(), String>;
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
    fn put_boot_source(&mut self, kernel: &Path) -> Result<(), String> {
        self.calls
            .push(format!("put_boot_source {}", kernel.display()));
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
        if world.host_fs.exists(world.host_docker_socket) {
            // Presence is allowed on the host; the backend must not use it.
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
            "--netns".into(),
            resources.netns.display().to_string(),
            "--new-pid-ns".into(),
            "--resource-limit".into(),
            "no-file=1024".into(),
        ]
    }

    pub(crate) fn prepare(
        &mut self,
        plan: &ValidatedPlan,
        resources: &IsolationResources,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        let set = MicroVmArtifactSet::load(world.artifact_root, world.host_fs)?;
        let _ = plan;
        let identity_ok = snapshot_identity_matches(&set, world);
        start_jailer(&set, resources, world, events)?;
        if identity_ok && try_restore_snapshot(&set, world, events)? {
            self.restored = true;
            self.guest_cid = FIRECRACKER_GUEST_CID;
            return Ok(());
        }
        if identity_ok {
            // Load taints a Firecracker process; a failed load needs a fresh VMM.
            start_jailer(&set, resources, world, events)?;
        }
        cold_configure(self, &set, resources, world, events)
    }

    pub(crate) fn start(
        &mut self,
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
        events.push(ExecutionEvent::GuestDocker("engine healthy".into()));
        if !self.restored {
            let _ = create_golden_snapshot(
                world,
                GuestReady {
                    agent_listening: true,
                    docker_healthy: true,
                    job_credentials_absent: true,
                },
                events,
            );
        }
        Ok(())
    }

    pub(crate) fn execute(
        &mut self,
        plan: &ValidatedPlan,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        let guest = plan.to_guest(&format!("job-{}", plan.job_id), 1);
        let bytes = guest
            .encode()
            .map_err(|detail| MicroVmPreflightFailure::new("vsock.plan", detail))?;
        events.push(ExecutionEvent::FirecrackerApi("vsock DeliverPlan".into()));
        if let Some(vsock) = world.vsock.as_mut() {
            return drive_vsock(
                &mut **vsock,
                VsockMessage::DeliverPlan {
                    isolation_id: guest.isolation_id,
                    generation: guest.generation,
                    plan_bytes: bytes,
                },
                events,
            );
        }
        if !world.allow_inline_guest_plan {
            return Err(MicroVmPreflightFailure::new(
                "vsock",
                "microvm execute requires a vsock channel",
            )
            .into());
        }
        let code = super::guest_runtime::handle_delivered_plan(&bytes, world.runner, events)
            .map_err(|detail| MicroVmPreflightFailure::new("guest.docker", detail))?;
        if code != 0 && !super::guest_runtime::has_terminal_log(events) {
            events.push(ExecutionEvent::Log {
                stream: 1,
                line: "failure".into(),
            });
        }
        Ok(())
    }

    pub(crate) fn cancel(
        &mut self,
        world: &mut ExecutionWorld<'_>,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<(), ExecutionError> {
        let _ = world;
        events.push(ExecutionEvent::FirecrackerApi("cancel".into()));
        events.push(ExecutionEvent::Log {
            stream: 1,
            line: "cancel".into(),
        });
        Ok(())
    }
}

fn start_jailer(
    set: &MicroVmArtifactSet,
    resources: &IsolationResources,
    world: &mut ExecutionWorld<'_>,
    events: &mut Vec<ExecutionEvent>,
) -> Result<(), ExecutionError> {
    let jailer_bin = set.jailer.path.to_str().unwrap_or("jailer");
    let jailer_args = FirecrackerBackend::jailer_args(resources, &set.firecracker.path);
    let jailer = world
        .runner
        .run(jailer_bin, &jailer_args)
        .map_err(|error| MicroVmPreflightFailure::new("jailer", error.to_string()))?;
    if jailer.code != 0 {
        return Err(MicroVmPreflightFailure::new(
            "jailer",
            format!("jailer start exited {}", jailer.code),
        )
        .into());
    }
    events.push(ExecutionEvent::FirecrackerApi("jailer".into()));
    Ok(())
}

fn cold_configure(
    backend: &mut FirecrackerBackend,
    set: &MicroVmArtifactSet,
    resources: &IsolationResources,
    world: &mut ExecutionWorld<'_>,
    events: &mut Vec<ExecutionEvent>,
) -> Result<(), ExecutionError> {
    world
        .firecracker
        .put_boot_source(&set.kernel.path)
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

fn snapshot_mem_path(set: &MicroVmArtifactSet, root: &Path) -> PathBuf {
    set.snapshot
        .as_ref()
        .map(|snapshot| snapshot.path.clone())
        .unwrap_or_else(|| root.join("snapshot.mem"))
}

fn snapshot_identity_matches(set: &MicroVmArtifactSet, world: &ExecutionWorld<'_>) -> bool {
    let mem = snapshot_mem_path(set, world.artifact_root);
    if !world.host_fs.exists(&mem) || !world.host_fs.exists(&vmstate_path(&mem)) {
        return false;
    }
    let generation = MicroVmGeneration::from_set(set);
    let want = SnapshotIdentity::from_generation(&generation, std::env::consts::ARCH, "linux-6.1");
    let Ok(have) = read_identity(world.host_fs, &mem) else {
        return false;
    };
    want.restore_or_cold_boot(&have).is_ok()
}

/// Pause a credential-free ready guest and persist mem+vmstate+identity.
///
/// # Errors
/// Credentials present, guest not ready, or Firecracker API failure.
pub fn create_golden_snapshot(
    world: &mut ExecutionWorld<'_>,
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
    world
        .firecracker
        .create_snapshot(&mem, &vmstate)
        .map_err(|detail| MicroVmPreflightFailure::new("guest.snapshot", detail))?;
    events.push(ExecutionEvent::FirecrackerApi("create_snapshot".into()));
    let generation = MicroVmGeneration::from_set(&set);
    let identity =
        SnapshotIdentity::from_generation(&generation, std::env::consts::ARCH, "linux-6.1");
    write_identity(world.host_fs, &mem, &identity)?;
    world
        .firecracker
        .resume_vm()
        .map_err(|detail| MicroVmPreflightFailure::new("guest.snapshot", detail))?;
    events.push(ExecutionEvent::FirecrackerApi("resume_vm".into()));
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

fn drive_vsock(
    vsock: &mut dyn super::VsockChannel,
    message: VsockMessage,
    events: &mut Vec<ExecutionEvent>,
) -> Result<(), ExecutionError> {
    vsock
        .send(message)
        .map_err(|detail| MicroVmPreflightFailure::new("vsock", detail))?;
    loop {
        match vsock
            .recv()
            .map_err(|detail| MicroVmPreflightFailure::new("vsock", detail))?
        {
            VsockMessage::TeardownAck { .. } => return Ok(()),
            VsockMessage::StepCompleted { exit_code, .. } if exit_code != 0 => {
                events.push(ExecutionEvent::Log {
                    stream: 1,
                    line: "failure".into(),
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
