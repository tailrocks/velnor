//! Deterministic local proof for issue 408's idle resource-scaling gate.

#![cfg(unix)]

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

fn scratch(slots: u32) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "velnor-408-idle-scale-{}-{slots}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create measurement directory");
    std::fs::write(
        path.join("execution.toml"),
        "[execution]\nbackend = \"docker\"\n",
    )
    .expect("write execution configuration");
    path
}

fn spawn_controller(state_dir: &Path, slots: u32) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_velnor-runner"));
    command
        .args([
            "controller",
            "--state-dir",
            state_dir.to_str().expect("state directory is utf-8"),
            "--scope",
            "idle-scale",
            "--desired-ready",
            &slots.to_string(),
            "--surge",
            "0",
            "--once",
        ])
        .env_remove("GITHUB_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Own the controller and its deliberately persistent slot children as a
    // single test process group; no measurement may leak into the next one.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    command.spawn().expect("spawn idle controller")
}

fn wait_for_metrics(path: &Path) -> Value {
    for _ in 0..100 {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(value) = serde_json::from_slice(&bytes) {
                return value;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("controller did not publish metrics: {}", path.display());
}

fn number(metrics: &Value, path: &[&str]) -> u64 {
    path.iter()
        .try_fold(metrics, |value, key| value.get(*key))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing numeric metric {path:?}: {metrics}"))
}

fn cpu_us(metrics: &Value) -> u64 {
    [
        "journal",
        "filesystem",
        "github",
        "broker",
        "child_supervision",
    ]
    .into_iter()
    .map(|phase| {
        number(metrics, &["cpu", phase, "user_us"])
            .saturating_add(number(metrics, &["cpu", phase, "system_us"]))
    })
    .sum()
}

fn stop_process_group(child: &mut Child) {
    let pgid = -(child.id() as libc::pid_t);
    unsafe {
        assert_eq!(libc::kill(pgid, libc::SIGKILL), 0, "kill measurement group");
    }
    let _ = child.wait();
}

#[derive(Debug)]
struct Measurement {
    slots: u32,
    slot_processes: u64,
    job_processes: u64,
    waiter_processes: u64,
    reconcile_p95_ms: u64,
    controller_cpu_us: u64,
    journal_transactions: u64,
    wal_bytes: u64,
}

#[test]
fn idle_resource_scaling_from_one_to_sixteen_slots_is_bounded() {
    let mut measurements = Vec::new();
    for slots in [1, 2, 4, 8, 16] {
        let state_dir = scratch(slots);
        let metrics_path = state_dir.join("controller-metrics.json");
        let mut child = spawn_controller(&state_dir, slots);
        let metrics = wait_for_metrics(&metrics_path);
        let measurement = Measurement {
            slots,
            slot_processes: number(&metrics, &["slot_processes"]),
            job_processes: number(&metrics, &["job_processes"]),
            waiter_processes: number(&metrics, &["waiter_processes"]),
            reconcile_p95_ms: number(&metrics, &["reconcile_duration_ms", "p95"]),
            controller_cpu_us: cpu_us(&metrics),
            journal_transactions: number(&metrics, &["journal", "transactions"]),
            wal_bytes: number(&metrics, &["journal", "wal_bytes"]),
        };
        stop_process_group(&mut child);
        std::fs::remove_dir_all(&state_dir).expect("remove measurement directory");

        assert_eq!(measurement.slots, slots);
        assert_eq!(measurement.slot_processes, u64::from(slots));
        assert_eq!(measurement.job_processes, 0, "idle jobs: {measurement:?}");
        assert_eq!(
            measurement.waiter_processes, 0,
            "idle waiters: {measurement:?}"
        );
        assert!(
            measurement.reconcile_p95_ms > 0,
            "controller must publish a non-zero reconcile duration: {measurement:?}"
        );
        assert!(
            measurement.journal_transactions > 0,
            "controller must publish journal telemetry: {measurement:?}"
        );
        assert!(
            measurement.wal_bytes <= 4 * 1024 * 1024,
            "startup WAL must remain bounded: {measurement:?}"
        );
        println!(
            "idle_scaling slots={} slot_processes={} job_processes={} waiter_processes={} reconcile_p95_ms={} controller_cpu_us={} journal_transactions={} wal_bytes={}",
            measurement.slots,
            measurement.slot_processes,
            measurement.job_processes,
            measurement.waiter_processes,
            measurement.reconcile_p95_ms,
            measurement.controller_cpu_us,
            measurement.journal_transactions,
            measurement.wal_bytes,
        );
        measurements.push(measurement);
    }

    let baseline = measurements.first().expect("one-slot measurement");
    let largest = measurements.last().expect("sixteen-slot measurement");
    // The first cycle includes deterministic slot-process creation.  CPU
    // attribution is the steady control-resource gate; duration remains in
    // the exact report for diagnosing startup work separately.
    assert!(
        largest.controller_cpu_us <= baseline.controller_cpu_us.saturating_mul(2) + 1_000,
        "controller CPU exceeded 2x: {measurements:#?}"
    );

    println!("idle scaling measurements: {measurements:#?}");
}
