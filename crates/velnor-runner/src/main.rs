use anyhow::Result;
use std::time::Duration;

const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Tokio's blocking pool is the control plane's pool, and nothing else.
///
/// Every task submitted to it is short and bounded: SQLite admission and
/// retention writes (milliseconds), Docker network sweeps, slot teardown
/// joins, and the read-only Contents-API metadata fetches of action
/// admission. The job body is minutes to hours and runs on its own OS thread
/// (the runner's `run_on_job_execution_thread`), so it can never occupy a
/// thread this pool needs.
///
/// Sizing: one slot process admits one job at a time, so steady-state demand
/// is a handful of threads — the action-admission limiter caps its own
/// concurrency, retention and the Docker sweep are one task each, and the
/// teardown joins are bounded by the number of displaced slots. Sixteen
/// leaves roughly a 2x headroom over that peak while keeping the bound
/// explicit: the default of 512 is not a budget, it is the absence of one,
/// and it is what made fail-closed permits look necessary.
const CONTROL_PLANE_BLOCKING_THREADS: usize = 16;

fn main() -> Result<()> {
    let runtime = build_runtime()?;
    let result = runtime.block_on(velnor_runner::service::execute());
    // Tokio waits forever for a started `spawn_blocking` task when Runtime is
    // dropped. A stuck Docker/curl cleanup must not hold a fully drained
    // systemd daemon (and package upgrade) for hours.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    result
}

fn build_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .max_blocking_threads(CONTROL_PLANE_BLOCKING_THREADS)
        .enable_all()
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_shutdown_does_not_wait_forever_for_blocking_work() {
        let runtime = build_runtime().unwrap();
        runtime.block_on(async {
            tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_secs(30)));
            tokio::time::sleep(Duration::from_millis(10)).await;
        });
        let started = std::time::Instant::now();
        runtime.shutdown_timeout(Duration::from_millis(20));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
