use anyhow::Result;
use std::time::Duration;

const RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> Result<()> {
    let runtime = build_runtime()?;
    let result = runtime.block_on(velnor_runner::scaffold::execute());
    // Tokio waits forever for a started `spawn_blocking` task when Runtime is
    // dropped. A stuck Docker/curl cleanup must not hold a fully drained
    // systemd daemon (and package upgrade) for hours.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    result
}

fn build_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
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
