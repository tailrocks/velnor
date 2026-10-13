//! Workload drivers.
//!
//! A driver turns one declared scenario into real work and returns real
//! observations. There is deliberately no driver that simulates a job: a
//! scenario whose driver cannot run here is reported as unrun, never as a
//! synthesised number.

pub mod cargo;
pub mod docker;
mod isolated_docker;

use std::path::PathBuf;

use anyhow::Result;

use crate::{
    record::Observation,
    scenario::{Driver, Scenario},
    sys::Runner,
};

/// Everything a driver needs from the invocation.
#[derive(Debug)]
pub struct Context {
    /// Scratch root; drivers create and remove their own subtrees under it.
    pub work_root: PathBuf,
    /// Velnor checkout used as the Rust workload subject.
    pub velnor_repo: PathBuf,
    /// Image used for container scenarios.
    pub job_image: String,
    /// Measured iterations per scenario.
    pub iterations: usize,
    /// Concurrency for the concurrent-jobs scenario.
    pub concurrency: usize,
    /// Shared counting process runner.
    pub runner: Runner,
}

impl Context {
    /// Directory reserved for one scenario, recreated empty.
    ///
    /// # Errors
    /// The directory could not be recreated.
    pub fn scenario_dir(&self, scenario: &Scenario) -> Result<PathBuf> {
        let dir = self.work_root.join(scenario.id.replace('/', "_"));
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

/// A driver able to execute one scenario.
pub trait Workload {
    /// Work done once before the measured iterations, never recorded.
    ///
    /// # Errors
    /// Preparation failed; the scenario must then be reported as unrun.
    fn prepare(&mut self, context: &mut Context) -> Result<()> {
        let _ = context;
        Ok(())
    }

    /// One measured iteration.
    ///
    /// # Errors
    /// The workload failed; a failed iteration is never summarised.
    fn iterate(&mut self, context: &mut Context) -> Result<Observation>;

    /// Cleanup after preparation or iteration, including failures. Cleanup
    /// failures are reported alongside the primary workload error.
    fn teardown(&mut self, context: &mut Context) -> Result<()> {
        let _ = context;
        Ok(())
    }

    /// Anything a reader must know to interpret this driver's numbers.
    fn notes(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Build the driver for a scenario, or explain why none exists here.
///
/// # Errors
/// No driver is implemented for this scenario and driver combination.
pub fn build(scenario: &Scenario, driver: Driver) -> Result<Box<dyn Workload>> {
    match driver {
        Driver::VelnorJob => anyhow::bail!(
            "{}: the velnor-job driver needs a registered runner and dispatch credentials; \
             no such runner is configured, and this harness will not simulate one",
            scenario.id
        ),
        Driver::DockerDirect => docker::build(scenario),
        Driver::CargoDirect => cargo::build(scenario),
    }
}

/// Run a scenario's measured iterations.
///
/// # Errors
/// Preparation or any iteration failed.
pub fn run(workload: &mut dyn Workload, context: &mut Context) -> Result<Vec<Observation>> {
    if let Err(error) = workload.prepare(context) {
        return finish_with_teardown(workload, context, Err(error));
    }

    let result = (|| {
        let mut observations = Vec::with_capacity(context.iterations);
        for _ in 0..context.iterations {
            observations.push(workload.iterate(context)?);
        }
        Ok(observations)
    })();
    finish_with_teardown(workload, context, result)
}

fn finish_with_teardown<T>(
    workload: &mut dyn Workload,
    context: &mut Context,
    result: Result<T>,
) -> Result<T> {
    match (result, workload.teardown(context)) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(teardown_error)) => {
            Err(error.context(format!("workload teardown also failed: {teardown_error:#}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CleanupProbe {
        fail_prepare: bool,
        teardown_calls: usize,
    }

    impl Workload for CleanupProbe {
        fn prepare(&mut self, _context: &mut Context) -> Result<()> {
            if self.fail_prepare {
                anyhow::bail!("prepare failure");
            }
            Ok(())
        }

        fn iterate(&mut self, _context: &mut Context) -> Result<Observation> {
            anyhow::bail!("iteration failure");
        }

        fn teardown(&mut self, _context: &mut Context) -> Result<()> {
            self.teardown_calls += 1;
            Ok(())
        }
    }

    fn context() -> Context {
        Context {
            work_root: std::env::temp_dir(),
            velnor_repo: std::env::temp_dir(),
            job_image: String::new(),
            iterations: 1,
            concurrency: 1,
            runner: Runner::new(),
        }
    }

    #[test]
    fn teardown_runs_when_preparation_fails() {
        let mut workload = CleanupProbe {
            fail_prepare: true,
            teardown_calls: 0,
        };
        let error = run(&mut workload, &mut context()).expect_err("prepare must fail");
        assert_eq!(error.to_string(), "prepare failure");
        assert_eq!(workload.teardown_calls, 1);
    }

    #[test]
    fn teardown_runs_when_an_iteration_fails() {
        let mut workload = CleanupProbe {
            fail_prepare: false,
            teardown_calls: 0,
        };
        let error = run(&mut workload, &mut context()).expect_err("iteration must fail");
        assert_eq!(error.to_string(), "iteration failure");
        assert_eq!(workload.teardown_calls, 1);
    }
}
