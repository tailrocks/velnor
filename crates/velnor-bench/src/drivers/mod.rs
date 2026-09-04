//! Workload drivers.
//!
//! A driver turns one declared scenario into real work and returns real
//! observations. There is deliberately no driver that simulates a job: a
//! scenario whose driver cannot run here is reported as unrun, never as a
//! synthesised number.

pub mod cargo;
pub mod docker;

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

    /// Cleanup after the last iteration. Failures are reported, not fatal.
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
    workload.prepare(context)?;
    let mut observations = Vec::with_capacity(context.iterations);
    for _ in 0..context.iterations {
        observations.push(workload.iterate(context)?);
    }
    workload.teardown(context)?;
    Ok(observations)
}
