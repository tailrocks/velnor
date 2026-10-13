//! Real Rust builds, executed on the host with no container and no runner.
//!
//! This is the weakest driver in the crate and it is labelled as such
//! everywhere: it observes [`Stage::FirstUserCommand`] and nothing else, so a
//! result it produces can never be read as a claim about Velnor's job startup.
//! It exists because the bash script this crate replaces measured exactly this,
//! and that coverage must not be lost when the script is deleted.
//!
//! Every scenario works in its own detached `git worktree` under the scratch
//! root. The subject checkout is never mutated: another agent may be editing
//! it, and a benchmark that edits the tree it measures is not reproducible
//! anyway.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context as _, Result};

use crate::{
    drivers::{Context, Workload},
    gittrace::{self, GitCounters, GitEvidence, GitTrace},
    record::{Observation, Resources},
    scenario::Scenario,
    stage::Stage,
    sys::{tree_bytes, Invocation, Runner},
};

/// How the workspace is prepared before each measured iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Workspace {
    /// One worktree, reused across iterations.
    Reused,
    /// A new worktree at the same commit for every iteration.
    FreshEachIteration,
    /// One worktree per concurrent job.
    PerConcurrentJob,
}

/// How the target directory is prepared before each measured iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetDir {
    /// Removed before every iteration: a genuinely cold build.
    Cold,
    /// Retained and warmed once before measurement begins.
    Warm,
    /// One target directory per concurrent job, each warmed once.
    PerConcurrentJob,
}

/// Change applied to the workspace immediately before the measured command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mutation {
    None,
    /// Re-run an identical command first, so the measurement is pure no-op cost.
    PrimeIdenticalRun,
    /// Append a line to the first tracked Rust source file.
    AppendToFirstSource,
    /// Touch the workspace manifest without changing its content.
    TouchManifest,
    /// Touch a build script, forcing its rerun and downstream invalidation.
    TouchBuildScript,
    /// Rewrite `Cargo.lock` through a real `cargo update`.
    UpdateLockfile,
}

/// The measured cargo invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CargoCommand {
    /// A fixed subcommand over the whole workspace.
    Workspace {
        subcommand: &'static str,
        extra: &'static [&'static str],
    },
    /// A package resolved from `cargo metadata` at prepare time.
    ResolvedPackage { kind: PackageKind },
    /// Alternate between the default and the full feature set.
    AlternatingFeatures,
}

/// How a package is picked out of the resolved dependency graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageKind {
    /// A `*-sys` package, which links native code through a build script.
    NativeSys,
    /// A proc-macro package, which is compiled for the host and invalidates
    /// every downstream crate.
    ProcMacro,
}

/// A fully declarative plan for one Rust scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    workspace: Workspace,
    target: TargetDir,
    mutation: Mutation,
    command: CargoCommand,
}

const CHECK: CargoCommand = CargoCommand::Workspace {
    subcommand: "check",
    extra: &["--workspace", "--all-targets", "--locked"],
};

/// Ambient variables that can silently turn an ordinary Cargo measurement into
/// a wrapper, cross-target, offline, or otherwise operator-specific run. The
/// benchmark sets its own target directory and terminal/incremental policy;
/// these overrides are removed so a caller's shell cannot change the workload
/// without the record saying so.
const AMBIENT_CARGO_ENV_TO_REMOVE: &[&str] = &[
    "CARGO_BUILD_JOBS",
    "CARGO_BUILD_RUSTC",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_TARGET",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_NET_OFFLINE",
    "RUSTC",
    "RUSTC_BOOTSTRAP",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTDOC",
    "RUSTDOCFLAGS",
    "RUSTFLAGS",
];

static NEXT_SCRATCH_OWNER_ID: AtomicU64 = AtomicU64::new(1);

fn plan_for(id: &str) -> Option<Plan> {
    let plan = match id {
        "rust/cold" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Cold,
            mutation: Mutation::None,
            command: CHECK,
        },
        "rust/warm" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::None,
            command: CHECK,
        },
        "rust/noop" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::PrimeIdenticalRun,
            command: CHECK,
        },
        "rust/fresh-worktree-same-commit" => Plan {
            workspace: Workspace::FreshEachIteration,
            target: TargetDir::Warm,
            mutation: Mutation::None,
            command: CHECK,
        },
        "rust/small-source-edit" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::AppendToFirstSource,
            command: CHECK,
        },
        "rust/manifest-touch" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::TouchManifest,
            command: CHECK,
        },
        "rust/lockfile-update" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::UpdateLockfile,
            command: CHECK,
        },
        "rust/build-script-change" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::TouchBuildScript,
            command: CHECK,
        },
        "rust/feature-set-change" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::None,
            command: CargoCommand::AlternatingFeatures,
        },
        "rust/native-sys" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Cold,
            mutation: Mutation::None,
            command: CargoCommand::ResolvedPackage {
                kind: PackageKind::NativeSys,
            },
        },
        "rust/proc-macro" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Cold,
            mutation: Mutation::None,
            command: CargoCommand::ResolvedPackage {
                kind: PackageKind::ProcMacro,
            },
        },
        "rust/cargo-check" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::None,
            command: CHECK,
        },
        "rust/cargo-build" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::None,
            command: CargoCommand::Workspace {
                subcommand: "build",
                extra: &["--workspace", "--all-targets", "--locked"],
            },
        },
        "rust/nextest" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::None,
            command: CargoCommand::Workspace {
                subcommand: "nextest",
                extra: &["run", "--workspace", "--locked"],
            },
        },
        "rust/clippy" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::None,
            command: CargoCommand::Workspace {
                subcommand: "clippy",
                extra: &["--workspace", "--all-targets", "--locked"],
            },
        },
        "rust/doc" => Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Warm,
            mutation: Mutation::None,
            command: CargoCommand::Workspace {
                subcommand: "doc",
                extra: &["--workspace", "--no-deps", "--locked"],
            },
        },
        "rust/concurrent-jobs" => Plan {
            workspace: Workspace::PerConcurrentJob,
            target: TargetDir::PerConcurrentJob,
            mutation: Mutation::None,
            command: CHECK,
        },
        _ => return None,
    };
    Some(plan)
}

pub(super) fn build(scenario: &Scenario) -> Result<Box<dyn Workload>> {
    let plan = plan_for(scenario.id).ok_or_else(|| {
        anyhow::anyhow!(
            "no cargo workload is implemented for {}; \
             it is declared in the matrix and reported as unrun",
            scenario.id
        )
    })?;
    Ok(Box::new(CargoWorkload {
        plan,
        scenario: scenario.id,
        scratch: ScratchOwner::new(),
        package: None,
        iteration: 0,
        notes: vec![
            "cargo-direct driver: measured on the host with no container and no runner, so \
             this record describes the build only and is not a claim about Velnor job latency"
                .to_owned(),
        ],
    }))
}

#[derive(Debug, Default)]
struct ScratchOwner {
    id: u64,
    nonce: u128,
    root: Option<PathBuf>,
    worktrees: Vec<OwnedWorktree>,
    targets: Vec<PathBuf>,
    traces: Vec<PathBuf>,
}

impl ScratchOwner {
    fn new() -> Self {
        Self {
            id: NEXT_SCRATCH_OWNER_ID.fetch_add(1, Ordering::Relaxed),
            nonce: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos()),
            ..Self::default()
        }
    }

    fn scenario_root(&self, work_root: &Path, scenario: &str) -> PathBuf {
        work_root.join(format!(
            "{}-{}-{}-{}",
            scenario.replace('/', "_"),
            std::process::id(),
            self.nonce,
            self.id
        ))
    }

    fn register_worktree(&mut self, path: PathBuf) -> usize {
        let index = self.worktrees.len();
        self.worktrees.push(OwnedWorktree {
            path,
            state: WorktreeState::Pending,
        });
        index
    }

    fn mark_worktree_registered(&mut self, index: usize) {
        self.worktrees[index].state = WorktreeState::Registered;
    }
}

#[derive(Debug)]
struct OwnedWorktree {
    path: PathBuf,
    state: WorktreeState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorktreeState {
    /// `git worktree add` may have created Git metadata before failing.
    Pending,
    /// Git owns a registered worktree at `path`.
    Registered,
    /// Git cleanup succeeded; only the owned filesystem path may remain.
    Removed,
}

struct CargoWorkload {
    plan: Plan,
    scenario: &'static str,
    scratch: ScratchOwner,
    package: Option<String>,
    iteration: u64,
    notes: Vec<String>,
}

impl CargoWorkload {
    fn add_worktree(&mut self, context: &mut Context, root: &Path, name: &str) -> Result<PathBuf> {
        let path = root.join(name);
        let ownership = self.scratch.register_worktree(path.clone());
        let invocation = context.runner.run(
            "git",
            &[
                "-C".to_owned(),
                context.velnor_repo.display().to_string(),
                "worktree".to_owned(),
                "add".to_owned(),
                "--detach".to_owned(),
                path.display().to_string(),
                "HEAD".to_owned(),
            ],
        );
        let invocation = invocation.context("git worktree add")?;
        if !invocation.ok() {
            bail!("git worktree add failed: {}", invocation.stderr.trim());
        }
        self.scratch.mark_worktree_registered(ownership);
        Ok(path)
    }

    fn cargo_args(&self, iteration: u64) -> Vec<String> {
        match &self.plan.command {
            CargoCommand::Workspace { subcommand, extra } => {
                let mut args = vec![(*subcommand).to_owned()];
                args.extend(extra.iter().map(|value| (*value).to_owned()));
                args
            }
            CargoCommand::ResolvedPackage { .. } => {
                let package = self.package.clone().unwrap_or_default();
                vec![
                    "build".to_owned(),
                    "--locked".to_owned(),
                    "-p".to_owned(),
                    package,
                ]
            }
            CargoCommand::AlternatingFeatures => {
                let mut args = vec![
                    "check".to_owned(),
                    "--workspace".to_owned(),
                    "--all-targets".to_owned(),
                    "--locked".to_owned(),
                ];
                if iteration.is_multiple_of(2) {
                    args.push("--all-features".to_owned());
                }
                args
            }
        }
    }

    fn run_cargo(
        &self,
        context: &mut Context,
        workspace: &Path,
        target: &Path,
        args: &[String],
        trace_file: Option<&Path>,
    ) -> Result<u64> {
        let env = cargo_env(target, trace_file);
        let started = Instant::now();
        let invocation = context
            .runner
            .exec_without(
                "cargo",
                args,
                Some(workspace),
                &env,
                AMBIENT_CARGO_ENV_TO_REMOVE,
            )
            .context("spawning cargo")?;
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if !invocation.ok() {
            bail!(
                "cargo {} failed with status {}: {}",
                args.join(" "),
                invocation.code,
                invocation
                    .stderr
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        }
        Ok(elapsed)
    }

    fn apply_mutation(&self, context: &mut Context, workspace: &Path) -> Result<()> {
        match self.plan.mutation {
            Mutation::None | Mutation::PrimeIdenticalRun => Ok(()),
            Mutation::AppendToFirstSource => {
                let file = first_tracked_source(context, workspace)?;
                let path = workspace.join(file);
                let mut text = std::fs::read_to_string(&path)?;
                text.push_str("\n// velnor-bench source edit\n");
                std::fs::write(&path, text)?;
                Ok(())
            }
            Mutation::TouchManifest => touch(&workspace.join("Cargo.toml")),
            Mutation::TouchBuildScript => {
                let script = find_build_script(workspace).ok_or_else(|| {
                    anyhow::anyhow!(
                        "no build.rs exists in this workspace, so the build-script-change \
                         scenario has nothing to invalidate"
                    )
                })?;
                touch(&script)
            }
            Mutation::UpdateLockfile => {
                let lockfile = workspace.join("Cargo.lock");
                let before = std::fs::read(&lockfile).with_context(|| {
                    format!("reading {} before cargo update", lockfile.display())
                })?;
                let invocation = context
                    .runner
                    .exec_without(
                        "cargo",
                        &["update", "--workspace"],
                        Some(workspace),
                        &[("CARGO_TERM_COLOR".to_owned(), "never".to_owned())],
                        AMBIENT_CARGO_ENV_TO_REMOVE,
                    )
                    .context("cargo update")?;
                if !invocation.ok() {
                    bail!("cargo update failed: {}", invocation.stderr.trim());
                }
                let after = std::fs::read(&lockfile).with_context(|| {
                    format!("reading {} after cargo update", lockfile.display())
                })?;
                if !lockfile_bytes_changed(&before, &after) {
                    bail!(
                        "cargo update succeeded but {} was unchanged; refusing to measure a no-op as dependency invalidation",
                        lockfile.display()
                    );
                }
                Ok(())
            }
        }
    }

    /// Undo the mutation so the next iteration starts from the same state.
    fn restore(&self, context: &mut Context, workspace: &Path) -> Result<()> {
        if matches!(
            self.plan.mutation,
            Mutation::AppendToFirstSource | Mutation::UpdateLockfile
        ) {
            let invocation = context
                .runner
                .exec("git", &["checkout", "--", "."], Some(workspace), &[])
                .context("restore Cargo workload mutation")?;
            if !invocation.ok() {
                bail!(
                    "restoring Cargo workload mutation failed with exit code {}: {}",
                    invocation.code,
                    invocation.stderr.trim()
                );
            }
        }
        Ok(())
    }
}

impl Workload for CargoWorkload {
    fn prepare(&mut self, context: &mut Context) -> Result<()> {
        if self.scratch.root.is_some()
            || !self.scratch.worktrees.is_empty()
            || !self.scratch.targets.is_empty()
            || !self.scratch.traces.is_empty()
        {
            bail!("Cargo workload still owns scratch; teardown is required before prepare");
        }
        let root = self
            .scratch
            .scenario_root(&context.work_root, self.scenario);
        // Register before creating the directory so a partial prepare remains
        // recoverable when directory creation fails.
        self.scratch.root = Some(root.clone());
        std::fs::create_dir_all(&root)
            .with_context(|| format!("creating Cargo workload root {}", root.display()))?;

        let worktree_count = match self.plan.workspace {
            Workspace::PerConcurrentJob => context.concurrency.max(1),
            Workspace::Reused | Workspace::FreshEachIteration => 1,
        };
        for index in 0..worktree_count {
            self.add_worktree(context, &root, &format!("workspace-{index}"))?;
        }
        let target_count = match self.plan.target {
            TargetDir::PerConcurrentJob => context.concurrency.max(1),
            TargetDir::Cold | TargetDir::Warm => 1,
        };
        for index in 0..target_count {
            let target = root.join(format!("target-{index}"));
            // Register before creating the directory so a partial prepare can
            // retry or report ownership of this path.
            self.scratch.targets.push(target.clone());
            std::fs::create_dir_all(&target)
                .with_context(|| format!("creating Cargo target {}", target.display()))?;
        }

        if let CargoCommand::ResolvedPackage { kind } = self.plan.command {
            let workspace = self.scratch.worktrees[0].path.clone();
            let package = resolve_package(context, &workspace, kind)?;
            self.notes
                .push(format!("resolved package for this scenario: {package}"));
            self.package = Some(package);
        }

        // Warm the target directory outside the measurement where the plan
        // calls for it. A warm scenario measured from cold is not warm.
        if matches!(
            self.plan.target,
            TargetDir::Warm | TargetDir::PerConcurrentJob
        ) {
            let args = self.cargo_args(0);
            for index in 0..self.scratch.targets.len() {
                let workspace = self.scratch.worktrees[index.min(self.scratch.worktrees.len() - 1)]
                    .path
                    .clone();
                let target = self.scratch.targets[index].clone();
                self.run_cargo(context, &workspace, &target, &args, None)
                    .context("warm-up run")?;
            }
        }
        Ok(())
    }

    fn iterate(&mut self, context: &mut Context) -> Result<Observation> {
        self.iteration += 1;
        let root = self
            .scratch
            .root
            .clone()
            .context("Cargo workload was not prepared")?;

        if self.plan.workspace == Workspace::FreshEachIteration {
            let name = format!("workspace-fresh-{}", self.iteration);
            self.add_worktree(context, &root, &name)?;
        }
        let workspace = self
            .scratch
            .worktrees
            .last()
            .map(|worktree| worktree.path.clone())
            .context("Cargo workload has no owned worktree")?;
        let target = self
            .scratch
            .targets
            .first()
            .cloned()
            .context("Cargo workload has no owned target")?;

        if self.plan.target == TargetDir::Cold && target.exists() {
            std::fs::remove_dir_all(&target)?;
            std::fs::create_dir_all(&target)?;
        }

        self.apply_mutation(context, &workspace)?;
        let args = self.cargo_args(self.iteration);
        if self.plan.mutation == Mutation::PrimeIdenticalRun {
            self.run_cargo(context, &workspace, &target, &args, None)
                .context("priming run")?;
        }

        // Setup and priming are not the measured user command. Reset the
        // process/resource census and establish the disk baseline immediately
        // before the command; snapshot all observation inputs before restore.
        context.runner.reset();
        let trace_files = self.prepare_trace_files(&context.work_root)?;
        let disk_before = tree_bytes(&root);
        let started = Instant::now();
        let command_ms = if self.plan.workspace == Workspace::PerConcurrentJob {
            self.run_concurrent(context, &args, &trace_files)?
        } else {
            self.run_cargo(context, &workspace, &target, &args, Some(&trace_files[0]))?
        };
        let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let usage = context.runner.rusage();
        let disk_after = tree_bytes(&root);
        let process_count = context.runner.process_count() as u64;
        let docker_invocations = context.runner.count_of("docker") as u64;
        let git = read_trace_files(&trace_files).context("reading Git trace evidence")?;

        let observation = Observation {
            total_ms,
            stages_ms: BTreeMap::from([(Stage::FirstUserCommand, command_ms)]),
            checkout_phases_ms: BTreeMap::new(),
            resources: Resources {
                cpu_user_us: usage.user_us,
                cpu_system_us: usage.system_us,
                max_rss_bytes: usage.max_rss_bytes,
                block_input_ops: usage.block_input_ops,
                block_output_ops: usage.block_output_ops,
                disk_bytes_delta: i64::try_from(disk_after).unwrap_or(i64::MAX)
                    - i64::try_from(disk_before).unwrap_or(0),
                process_count,
                docker_invocations,
                bytes_downloaded: git.received_bytes(),
                ..Resources::default()
            },
            git,
        };
        self.restore(context, &workspace)?;
        Ok(observation)
    }

    fn teardown(&mut self, context: &mut Context) -> Result<()> {
        // Leave the shared repository exactly as it was found.
        let mut failures = Vec::new();
        self.cleanup_worktrees(context, &mut failures);
        cleanup_owned_paths(
            &mut self.scratch.targets,
            "target",
            remove_owned_directory,
            &mut failures,
        );
        cleanup_owned_paths(
            &mut self.scratch.traces,
            "trace file",
            remove_owned_file,
            &mut failures,
        );
        self.cleanup_root(&mut failures);
        if !failures.is_empty() {
            bail!("Cargo workload teardown failed: {}", failures.join("; "));
        }
        Ok(())
    }

    fn notes(&self) -> Vec<String> {
        self.notes.clone()
    }
}

impl CargoWorkload {
    fn prepare_trace_files(&mut self, work_root: &Path) -> Result<Vec<PathBuf>> {
        // Git resolves GIT_TRACE2_EVENT relative to each worker's workspace.
        // Use one absolute root so every worker writes to the path we later
        // read, regardless of the caller's relative --work-root spelling.
        let work_root = std::fs::canonicalize(work_root)
            .with_context(|| format!("resolving Cargo trace root {}", work_root.display()))?;
        let worker_count = if self.plan.workspace == Workspace::PerConcurrentJob {
            self.scratch.worktrees.len()
        } else {
            1
        };
        if worker_count == 0 {
            bail!("Cargo workload has no workers for Git trace evidence");
        }

        let mut trace_files = Vec::with_capacity(worker_count);
        for worker_index in 0..worker_count {
            let trace_file = trace_file_path_for_worker(
                &work_root,
                self.scenario,
                self.scratch.nonce,
                self.scratch.id,
                self.iteration,
                worker_index,
            );
            // Register before clearing or allowing Git tracing to create the
            // file. A partial setup remains owned by teardown.
            self.scratch.traces.push(trace_file.clone());
            clear_trace_file(&trace_file)?;
            trace_files.push(trace_file);
        }
        Ok(trace_files)
    }

    fn cleanup_worktrees(&mut self, context: &mut Context, failures: &mut Vec<String>) {
        let owned = std::mem::take(&mut self.scratch.worktrees);
        let mut remaining = Vec::with_capacity(owned.len());
        for mut worktree in owned {
            let mut git_removed = worktree.state == WorktreeState::Removed;
            if !git_removed {
                match remove_git_worktree(context, &worktree.path) {
                    Ok(()) => {
                        worktree.state = WorktreeState::Removed;
                        git_removed = true;
                    }
                    Err(error) => failures.push(format!(
                        "remove worktree {} failed: {error:#}",
                        worktree.path.display()
                    )),
                }
            }

            let filesystem_removed = match remove_owned_directory(&worktree.path) {
                Ok(()) => true,
                Err(error) => {
                    failures.push(format!(
                        "remove worktree path {} failed: {error:#}",
                        worktree.path.display()
                    ));
                    false
                }
            };
            if !(git_removed && filesystem_removed) {
                remaining.push(worktree);
            }
        }
        self.scratch.worktrees = remaining;
    }

    fn cleanup_root(&mut self, failures: &mut Vec<String>) {
        let Some(root) = self.scratch.root.clone() else {
            return;
        };
        if !self.scratch.worktrees.is_empty()
            || !self.scratch.targets.is_empty()
            || !self.scratch.traces.is_empty()
        {
            failures.push(format!(
                "retain Cargo workload root {} while owned child resources remain",
                root.display()
            ));
            return;
        }
        match remove_owned_directory(&root) {
            Ok(()) => self.scratch.root = None,
            Err(error) => failures.push(format!(
                "remove Cargo workload root {} failed: {error:#}",
                root.display()
            )),
        }
    }

    /// Concurrent jobs really do run concurrently: one thread per worktree,
    /// each with its own target directory.
    fn run_concurrent(
        &self,
        context: &mut Context,
        args: &[String],
        trace_files: &[PathBuf],
    ) -> Result<u64> {
        if self.scratch.worktrees.len() != self.scratch.targets.len()
            || self.scratch.worktrees.len() != trace_files.len()
        {
            bail!("concurrent Cargo workers, targets, and Git traces must have equal ownership");
        }
        if trace_files.is_empty() {
            bail!("concurrent Cargo workload has no Git trace files");
        }

        let pairs: Vec<(PathBuf, PathBuf, PathBuf)> = self
            .scratch
            .worktrees
            .iter()
            .map(|worktree| worktree.path.clone())
            .zip(self.scratch.targets.iter().cloned())
            .zip(trace_files.iter().cloned())
            .map(|((workspace, target), trace_file)| (workspace, target, trace_file))
            .collect();
        let started = Instant::now();
        let worker_results: Vec<Result<Runner, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = pairs
                .iter()
                .map(|(workspace, target, trace_file)| {
                    scope.spawn(move || {
                        let mut runner = Runner::new();
                        let env = cargo_env(target, Some(trace_file));
                        let outcome = match runner.exec_without(
                            "cargo",
                            args,
                            Some(workspace),
                            &env,
                            AMBIENT_CARGO_ENV_TO_REMOVE,
                        ) {
                            Ok(invocation) if invocation.ok() => Ok(()),
                            Ok(invocation) => Err(format!(
                                "cargo exited {} in {}",
                                invocation.code,
                                workspace.display()
                            )),
                            Err(error) => Err(format!("cargo could not be spawned: {error}")),
                        };
                        match outcome {
                            Ok(()) => Ok(runner),
                            Err(error) => Err(error),
                        }
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("thread panicked".to_owned()))
                })
                .collect()
        });
        let mut failures = Vec::new();
        for result in worker_results {
            match result {
                Ok(worker) => context.runner.merge(worker),
                Err(error) => failures.push(error),
            }
        }
        if !failures.is_empty() {
            bail!("concurrent jobs failed: {}", failures.join("; "));
        }
        Ok(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX))
    }
}

fn remove_git_worktree(context: &mut Context, path: &Path) -> Result<()> {
    let invocation = context
        .runner
        .run(
            "git",
            &[
                "-C".to_owned(),
                context.velnor_repo.display().to_string(),
                "worktree".to_owned(),
                "remove".to_owned(),
                "--force".to_owned(),
                path.display().to_string(),
            ],
        )
        .context("git worktree remove")?;
    if invocation.ok() || git_worktree_not_found(invocation) {
        return Ok(());
    }
    bail!(
        "git worktree remove exited {}: {}",
        invocation.code,
        invocation.stderr.trim()
    );
}

fn git_worktree_not_found(invocation: &Invocation) -> bool {
    let output = format!("{}\n{}", invocation.stdout, invocation.stderr).to_ascii_lowercase();
    ["not a working tree", "does not exist"]
        .iter()
        .any(|message| output.contains(message))
}

fn cleanup_owned_paths(
    paths: &mut Vec<PathBuf>,
    kind: &str,
    remove: fn(&Path) -> Result<()>,
    failures: &mut Vec<String>,
) {
    let owned = std::mem::take(paths);
    let mut remaining = Vec::with_capacity(owned.len());
    for path in owned {
        match remove(&path) {
            Ok(()) => {}
            Err(error) => {
                failures.push(format!(
                    "remove Cargo {kind} {} failed: {error:#}",
                    path.display()
                ));
                remaining.push(path);
            }
        }
    }
    *paths = remaining;
}

fn remove_owned_directory(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_owned_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn trace_file_path_for_worker(
    work_root: &Path,
    scenario: &str,
    owner_nonce: u128,
    owner_id: u64,
    iteration: u64,
    worker_index: usize,
) -> PathBuf {
    work_root.join(format!(
        "{}-git-trace-{}-{}-{owner_id}-{iteration}-worker-{worker_index}.jsonl",
        std::process::id(),
        owner_nonce,
        scenario.replace('/', "_")
    ))
}

fn cargo_env(target: &Path, trace_file: Option<&Path>) -> Vec<(String, String)> {
    let mut env = vec![
        ("CARGO_TARGET_DIR".to_owned(), target.display().to_string()),
        ("CARGO_TERM_COLOR".to_owned(), "never".to_owned()),
        ("CARGO_INCREMENTAL".to_owned(), "0".to_owned()),
    ];
    if let Some(trace_file) = trace_file {
        env.extend(gittrace::trace_env(trace_file));
    }
    env
}

fn read_trace_files(paths: &[PathBuf]) -> Result<GitEvidence> {
    if paths.is_empty() {
        bail!("no Git trace files were registered for the measured command");
    }
    let mut counters = GitCounters::default();
    let mut successful = true;
    for path in paths {
        let trace = match GitTrace::from_event_file(path) {
            Ok(trace) => trace,
            Err(gittrace::TraceError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Err(anyhow::anyhow!(
                    "worker Git trace {} is missing; refusing to infer no Git process without an authoritative measured-window child-exec census",
                    path.display()
                ));
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("reading worker Git trace {}", path.display())));
            }
        };
        successful &= trace.successful;
        counters.merge(trace.counters);
    }
    Ok(GitEvidence::Observed {
        counters,
        successful,
    })
}

fn clear_trace_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("removing stale git trace {}", path.display()))
        }
    }
}

fn touch(path: &Path) -> Result<()> {
    let text = std::fs::read(path)?;
    std::fs::write(path, text)?;
    Ok(())
}

fn lockfile_bytes_changed(before: &[u8], after: &[u8]) -> bool {
    before != after
}

fn first_tracked_source(context: &mut Context, workspace: &Path) -> Result<String> {
    let listing = context
        .runner
        .exec("git", &["ls-files", "*.rs"], Some(workspace), &[])
        .context("git ls-files")?;
    let mut files: Vec<&str> = listing.stdout.lines().filter(|l| !l.is_empty()).collect();
    files.sort_unstable();
    files
        .first()
        .map(|file| (*file).to_owned())
        .ok_or_else(|| anyhow::anyhow!("workspace has no tracked Rust source"))
}

fn find_build_script(workspace: &Path) -> Option<PathBuf> {
    let direct = workspace.join("build.rs");
    if direct.is_file() {
        return Some(direct);
    }
    let crates = workspace.join("crates");
    let entries = std::fs::read_dir(crates).ok()?;
    entries
        .flatten()
        .map(|entry| entry.path().join("build.rs"))
        .find(|path| path.is_file())
}

/// Pick a package out of the resolved graph. Nothing here guesses from a name
/// alone for proc macros: `cargo metadata` states the target kind.
fn resolve_package(context: &mut Context, workspace: &Path, kind: PackageKind) -> Result<String> {
    let metadata = context
        .runner
        .exec_without(
            "cargo",
            &["metadata", "--format-version", "1", "--locked"],
            Some(workspace),
            &[("CARGO_TERM_COLOR".to_owned(), "never".to_owned())],
            AMBIENT_CARGO_ENV_TO_REMOVE,
        )
        .context("cargo metadata")?;
    if !metadata.ok() {
        bail!("cargo metadata failed: {}", metadata.stderr.trim());
    }
    let value: serde_json::Value = serde_json::from_str(&metadata.stdout)?;
    select_package(&value, kind).ok_or_else(|| {
        anyhow::anyhow!(
            "the dependency graph contains no {} package, so this scenario has no subject",
            match kind {
                PackageKind::NativeSys => "-sys",
                PackageKind::ProcMacro => "proc-macro",
            }
        )
    })
}

fn select_package(metadata: &serde_json::Value, kind: PackageKind) -> Option<String> {
    let packages = metadata.get("packages")?.as_array()?;
    let mut names: Vec<String> = packages
        .iter()
        .filter(|package| match kind {
            PackageKind::NativeSys => package
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| name.ends_with("-sys")),
            PackageKind::ProcMacro => package
                .get("targets")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|targets| {
                    targets.iter().any(|target| {
                        target
                            .get("kind")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|kinds| {
                                kinds.iter().any(|kind| kind.as_str() == Some("proc-macro"))
                            })
                    })
                }),
        })
        .filter_map(|package| {
            package
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    names.sort_unstable();
    names.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

    fn test_path(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "velnor-bench-cargo-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create test path");
        path
    }

    fn test_workload(plan: Plan, scenario: &'static str) -> CargoWorkload {
        CargoWorkload {
            plan,
            scenario,
            scratch: ScratchOwner::new(),
            package: None,
            iteration: 0,
            notes: Vec::new(),
        }
    }

    fn test_context(work_root: PathBuf, velnor_repo: PathBuf, iterations: usize) -> Context {
        Context {
            work_root,
            velnor_repo,
            job_image: String::new(),
            iterations,
            concurrency: 1,
            runner: Runner::new(),
        }
    }

    fn git_success(runner: &mut Runner, args: Vec<String>) {
        let invocation = runner.run("git", &args).expect("run git test command");
        assert!(
            invocation.ok(),
            "git {} failed: {}",
            args.join(" "),
            invocation.stderr.trim()
        );
    }

    fn test_git_repo(parent: &Path) -> PathBuf {
        let repo = parent.join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("create test repository");
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"scratch-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write test manifest");
        std::fs::write(repo.join("src/lib.rs"), "pub fn fixture() {}\n")
            .expect("write test source");

        let mut runner = Runner::new();
        let repo_arg = repo.display().to_string();
        git_success(
            &mut runner,
            vec!["init".to_owned(), "-q".to_owned(), repo_arg.clone()],
        );
        git_success(
            &mut runner,
            vec![
                "-C".to_owned(),
                repo_arg.clone(),
                "config".to_owned(),
                "user.name".to_owned(),
                "Velnor test".to_owned(),
            ],
        );
        git_success(
            &mut runner,
            vec![
                "-C".to_owned(),
                repo_arg.clone(),
                "config".to_owned(),
                "user.email".to_owned(),
                "velnor-test@example.invalid".to_owned(),
            ],
        );
        git_success(
            &mut runner,
            vec![
                "-C".to_owned(),
                repo_arg.clone(),
                "add".to_owned(),
                ".".to_owned(),
            ],
        );
        git_success(
            &mut runner,
            vec![
                "-C".to_owned(),
                repo_arg,
                "commit".to_owned(),
                "-qm".to_owned(),
                "initial fixture".to_owned(),
            ],
        );
        repo
    }

    fn complete_trace(sid: &str, bytes: u64) -> String {
        format!(
            "{{\"event\":\"version\",\"sid\":\"{sid}\",\"thread\":\"main\",\"time\":\"2026-09-05T00:00:00.000001Z\",\"evt\":\"4\",\"exe\":\"2.50.1\"}}\n{{\"event\":\"data\",\"sid\":\"{sid}\",\"thread\":\"main\",\"time\":\"2026-09-05T00:00:00.000002Z\",\"key\":\"bytes-received\",\"value\":{bytes}}}\n{{\"event\":\"exit\",\"sid\":\"{sid}\",\"thread\":\"main\",\"time\":\"2026-09-05T00:00:00.000003Z\",\"code\":0}}\n{{\"event\":\"atexit\",\"sid\":\"{sid}\",\"thread\":\"main\",\"time\":\"2026-09-05T00:00:00.000004Z\",\"code\":0}}\n"
        )
    }

    fn test_cargo_workspace(parent: &Path, name: &str) -> PathBuf {
        let workspace = parent.join(name);
        std::fs::create_dir_all(workspace.join("src")).expect("create Cargo workspace");
        std::fs::write(
            workspace.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n"
            ),
        )
        .expect("write Cargo manifest");
        std::fs::write(workspace.join("src/lib.rs"), "pub fn worker() {}\n")
            .expect("write Cargo source");
        std::fs::write(
            workspace.join("build.rs"),
            "fn main() {\n    let status = std::process::Command::new(\"git\")\n        .arg(\"--version\")\n        .status()\n        .expect(\"run git\");\n    assert!(status.success());\n}\n",
        )
        .expect("write Cargo build script");
        workspace
    }

    #[test]
    fn every_rust_scenario_with_a_cargo_fallback_has_a_plan() {
        for scenario in crate::scenario::MATRIX {
            if scenario.fallback == Some(crate::scenario::Driver::CargoDirect) {
                assert!(
                    plan_for(scenario.id).is_some(),
                    "{} has no cargo plan",
                    scenario.id
                );
                assert!(build(scenario).is_ok(), "{}", scenario.id);
            }
        }
    }

    #[test]
    fn the_plans_cover_what_the_deleted_bash_script_measured() {
        // Every scenario the removed scripts/benchmark/benchmark.sh ran, mapped
        // onto the declarative matrix. A gap here means coverage was lost.
        for (bash_name, id) in [
            ("cold_check", "rust/cold"),
            ("warm_check", "rust/warm"),
            ("noop", "rust/noop"),
            ("fresh_worktree_separate_target_cold", "rust/cold"),
            ("fresh_worktree_separate_target_warm", "rust/warm"),
            (
                "cross_worktree_shared_target_reuse",
                "rust/fresh-worktree-same-commit",
            ),
            ("small_source_edit", "rust/small-source-edit"),
            ("dependency_manifest_touch", "rust/manifest-touch"),
            ("dependency_graph_change", "rust/lockfile-update"),
            ("build", "rust/cargo-build"),
            ("warm_build", "rust/cargo-build"),
            ("nextest", "rust/nextest"),
            ("clippy", "rust/clippy"),
            ("native_sys_build", "rust/native-sys"),
            ("parallel_independent_jobs", "rust/concurrent-jobs"),
        ] {
            assert!(
                plan_for(id).is_some(),
                "bash scenario {bash_name} lost its replacement {id}"
            );
        }
    }

    #[test]
    fn a_non_rust_scenario_is_refused_not_faked() {
        let scenario = crate::scenario::find("docker/existing-image").expect("scenario");
        let error = build(scenario).map(|_| ()).expect_err("must refuse");
        assert!(error.to_string().contains("reported as unrun"), "{error}");
    }

    #[test]
    fn cold_plans_remove_the_target_and_warm_plans_do_not() {
        assert_eq!(plan_for("rust/cold").unwrap().target, TargetDir::Cold);
        assert_eq!(plan_for("rust/warm").unwrap().target, TargetDir::Warm);
        assert_eq!(
            plan_for("rust/concurrent-jobs").unwrap().workspace,
            Workspace::PerConcurrentJob
        );
    }

    #[test]
    fn cargo_measurements_remove_ambient_workload_overrides() {
        for variable in [
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_TARGET",
            "CARGO_NET_OFFLINE",
            "RUSTFLAGS",
        ] {
            assert!(
                AMBIENT_CARGO_ENV_TO_REMOVE.contains(&variable),
                "{variable} must not silently alter a cargo-direct measurement"
            );
        }
        assert!(
            !AMBIENT_CARGO_ENV_TO_REMOVE.contains(&"CARGO_TARGET_DIR"),
            "the explicit per-sample target directory must remain set"
        );
    }

    #[test]
    fn drivers_run_tears_down_owned_scratch_when_prepare_fails() {
        let work_root = test_path("prepare-failure");
        let repo = test_git_repo(&work_root);
        let plan = Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Cold,
            mutation: Mutation::None,
            command: CargoCommand::ResolvedPackage {
                kind: PackageKind::NativeSys,
            },
        };
        let mut workload = test_workload(plan, "test/prepare-failure");
        let expected_root = workload
            .scratch
            .scenario_root(&work_root, workload.scenario);
        let mut context = test_context(work_root.clone(), repo, 1);

        let error = crate::drivers::run(&mut workload, &mut context)
            .expect_err("metadata without a native sys package must fail preparation");

        assert!(!error.to_string().contains("workload teardown also failed"));
        assert!(workload.scratch.worktrees.is_empty());
        assert!(workload.scratch.targets.is_empty());
        assert!(workload.scratch.traces.is_empty());
        assert!(workload.scratch.root.is_none());
        assert!(!expected_root.exists(), "prepare scratch must be removed");
        assert!(
            context.runner.invocations().iter().any(|invocation| {
                invocation.program == "git" && invocation.args.contains(&"remove".to_owned())
            }),
            "drivers::run must invoke Cargo teardown after prepare failure"
        );

        let _ = std::fs::remove_dir_all(work_root);
    }

    #[test]
    fn drivers_run_preserves_prepare_failure_when_cleanup_also_fails() {
        let work_root = test_path("prepare-and-cleanup-failure");
        let mut workload = test_workload(plan_for("rust/cold").expect("cold plan"), "rust/cold");
        let mut context = test_context(work_root.clone(), work_root.join("not-a-repository"), 1);

        let error = crate::drivers::run(&mut workload, &mut context)
            .expect_err("worktree creation from a non-repository must fail");
        let text = format!("{error:#}");

        assert!(text.contains("git worktree add failed"), "{text}");
        assert!(text.contains("workload teardown also failed"));
        assert!(text.contains("remove worktree"));
        assert_eq!(workload.scratch.worktrees.len(), 1);
        assert!(workload.scratch.root.is_some());

        let _ = std::fs::remove_dir_all(work_root);
    }

    #[test]
    fn drivers_run_tears_down_owned_scratch_when_iteration_fails() {
        let work_root = test_path("iteration-failure");
        let repo = test_git_repo(&work_root);
        let scenario = "test/iteration-failure";
        let plan = Plan {
            workspace: Workspace::Reused,
            target: TargetDir::Cold,
            mutation: Mutation::None,
            command: CargoCommand::Workspace {
                subcommand: "velnor-no-such-cargo-subcommand",
                extra: &[],
            },
        };
        let mut workload = test_workload(plan, scenario);
        let expected_root = workload.scratch.scenario_root(&work_root, scenario);
        let expected_trace = trace_file_path_for_worker(
            &work_root,
            scenario,
            workload.scratch.nonce,
            workload.scratch.id,
            1,
            0,
        );
        std::fs::write(&expected_trace, b"stale trace").expect("write stale trace");
        let mut context = test_context(work_root.clone(), repo, 1);

        let error = crate::drivers::run(&mut workload, &mut context)
            .expect_err("unknown Cargo subcommand must fail iteration");

        assert!(error
            .to_string()
            .contains("cargo velnor-no-such-cargo-subcommand"));
        assert!(!error.to_string().contains("workload teardown also failed"));
        assert!(workload.scratch.worktrees.is_empty());
        assert!(workload.scratch.targets.is_empty());
        assert!(workload.scratch.traces.is_empty());
        assert!(workload.scratch.root.is_none());
        assert!(!expected_root.exists(), "iteration scratch must be removed");
        assert!(!expected_trace.exists(), "owned trace must be removed");

        let _ = std::fs::remove_dir_all(work_root);
    }

    #[test]
    fn teardown_attempts_all_cleanup_and_retains_failed_ownership() {
        let work_root = test_path("cleanup-failures");
        let root = work_root.join("scratch");
        std::fs::create_dir_all(&root).expect("create scratch root");
        let target = root.join("target-file");
        std::fs::write(&target, b"not a directory").expect("create target collision");
        let trace = root.join("trace-directory");
        std::fs::create_dir_all(&trace).expect("create trace collision");

        let mut workload = test_workload(plan_for("rust/cold").expect("cold plan"), "rust/cold");
        workload.scratch.root = Some(root.clone());
        workload.scratch.targets.push(target.clone());
        workload.scratch.traces.push(trace.clone());
        let mut context = test_context(work_root.clone(), work_root.join("not-a-repository"), 1);

        let error = workload
            .teardown(&mut context)
            .expect_err("invalid owned paths must be reported");
        let text = format!("{error:#}");

        assert!(text.contains("remove Cargo target"));
        assert!(text.contains("remove Cargo trace file"));
        assert!(text.contains("retain Cargo workload root"));
        assert_eq!(workload.scratch.targets, vec![target]);
        assert_eq!(workload.scratch.traces, vec![trace]);
        assert_eq!(workload.scratch.root, Some(root.clone()));
        assert!(root.exists());

        let _ = std::fs::remove_dir_all(work_root);
    }

    #[test]
    fn teardown_retains_root_when_trace_cleanup_fails() {
        let work_root = test_path("trace-cleanup-failure");
        let root = work_root.join("scratch");
        let trace = root.join("trace-directory");
        std::fs::create_dir_all(&trace).expect("create trace collision");

        let mut workload = test_workload(plan_for("rust/cold").expect("cold plan"), "rust/cold");
        workload.scratch.root = Some(root.clone());
        workload.scratch.traces.push(trace.clone());
        let mut context = test_context(work_root.clone(), work_root.join("not-a-repository"), 1);

        let error = workload
            .teardown(&mut context)
            .expect_err("invalid trace path must be reported");
        assert!(error.to_string().contains("remove Cargo trace file"));
        assert!(trace.exists());
        assert_eq!(workload.scratch.root, Some(root));

        let _ = std::fs::remove_dir_all(work_root);
    }

    #[test]
    fn teardown_treats_missing_owned_paths_as_idempotent() {
        let work_root = test_path("missing-resources");
        let repo = test_git_repo(&work_root);
        let root = work_root.join("missing-root");
        let mut workload = test_workload(plan_for("rust/cold").expect("cold plan"), "rust/cold");
        workload.scratch.worktrees.push(OwnedWorktree {
            path: root.join("missing-worktree"),
            state: WorktreeState::Pending,
        });
        workload.scratch.root = Some(root.clone());
        workload.scratch.targets.push(root.join("target"));
        workload.scratch.traces.push(root.join("trace.jsonl"));
        let mut context = test_context(work_root.clone(), repo, 1);

        workload
            .teardown(&mut context)
            .expect("missing owned resources are already clean");

        assert!(workload.scratch.worktrees.is_empty());
        assert!(workload.scratch.targets.is_empty());
        assert!(workload.scratch.traces.is_empty());
        assert!(workload.scratch.root.is_none());
        let _ = std::fs::remove_dir_all(work_root);
    }

    #[test]
    fn teardown_surfaces_worktree_removal_failures() {
        let mut workload = CargoWorkload {
            plan: plan_for("rust/cold").expect("cold plan"),
            scenario: "rust/cold",
            scratch: ScratchOwner {
                worktrees: vec![OwnedWorktree {
                    path: std::env::temp_dir().join("velnor-bench-unregistered-worktree"),
                    state: WorktreeState::Pending,
                }],
                ..ScratchOwner::new()
            },
            package: None,
            iteration: 0,
            notes: Vec::new(),
        };
        let mut context = Context {
            work_root: std::env::temp_dir(),
            velnor_repo: std::env::temp_dir(),
            job_image: String::new(),
            iterations: 1,
            concurrency: 1,
            runner: Runner::new(),
        };

        let error = workload
            .teardown(&mut context)
            .expect_err("an unregistered worktree must fail cleanup");
        assert!(error.to_string().contains("Cargo workload teardown failed"));
        assert!(error.to_string().contains("remove worktree"));
        assert_eq!(workload.scratch.worktrees.len(), 1);
        assert_eq!(workload.scratch.worktrees[0].state, WorktreeState::Pending);
    }

    #[test]
    fn concurrent_workers_write_and_merge_isolated_trace_files() {
        let root = test_path("concurrent-workers");
        let first_workspace = test_cargo_workspace(&root, "workspace-0");
        let second_workspace = test_cargo_workspace(&root, "workspace-1");
        let first_target = root.join("target-0");
        let second_target = root.join("target-1");
        let scratch = ScratchOwner::new();
        let trace_files = vec![
            trace_file_path_for_worker(&root, "test/concurrent", scratch.nonce, scratch.id, 1, 0),
            trace_file_path_for_worker(&root, "test/concurrent", scratch.nonce, scratch.id, 1, 1),
        ];
        let mut harness = CargoWorkload {
            plan: plan_for("rust/concurrent-jobs").expect("concurrent plan"),
            scenario: "rust/concurrent-jobs",
            scratch: ScratchOwner {
                root: Some(root.clone()),
                worktrees: vec![
                    OwnedWorktree {
                        path: first_workspace,
                        state: WorktreeState::Registered,
                    },
                    OwnedWorktree {
                        path: second_workspace,
                        state: WorktreeState::Registered,
                    },
                ],
                targets: vec![first_target, second_target],
                traces: trace_files.clone(),
                ..scratch
            },
            package: None,
            iteration: 0,
            notes: Vec::new(),
        };
        let mut context = Context {
            work_root: std::env::temp_dir(),
            velnor_repo: std::env::temp_dir(),
            job_image: String::new(),
            iterations: 1,
            concurrency: 2,
            runner: Runner::new(),
        };

        harness
            .run_concurrent(&mut context, &["check".to_owned()], &trace_files)
            .expect("cargo workers");

        assert_eq!(context.runner.process_count(), 2);
        assert_eq!(context.runner.count_of("cargo"), 2);
        let first = GitCounters::from_event_file(&trace_files[0]).expect("first worker trace");
        let second = GitCounters::from_event_file(&trace_files[1]).expect("second worker trace");
        assert!(first.processes > 0);
        assert!(second.processes > 0);
        let GitEvidence::Observed {
            counters: merged,
            successful,
        } = read_trace_files(&trace_files).expect("merge worker traces")
        else {
            panic!("worker traces must be observed");
        };
        assert!(successful);
        assert_eq!(
            merged.processes,
            first.processes.saturating_add(second.processes)
        );
        assert_eq!(
            merged.received_bytes,
            first.received_bytes.saturating_add(second.received_bytes)
        );
        assert!(trace_files.iter().all(|path| path.exists()));

        // The fixture workspaces are ordinary directories, so mark the Git
        // portion already removed and exercise the same owned-path teardown
        // used by the workload after the worker evidence is consumed.
        for worktree in &mut harness.scratch.worktrees {
            worktree.state = WorktreeState::Removed;
        }
        harness
            .teardown(&mut context)
            .expect("owned worker cleanup");
        assert!(harness.scratch.worktrees.is_empty());
        assert!(harness.scratch.targets.is_empty());
        assert!(harness.scratch.traces.is_empty());
        assert!(harness.scratch.root.is_none());
        assert!(trace_files.iter().all(|path| !path.exists()));
        assert!(!root.exists());
    }

    #[test]
    fn concurrent_trace_paths_and_environments_are_worker_specific() {
        let root = std::env::temp_dir();
        let first = trace_file_path_for_worker(&root, "rust/concurrent-jobs", 7, 8, 9, 0);
        let second = trace_file_path_for_worker(&root, "rust/concurrent-jobs", 7, 8, 9, 1);
        assert_ne!(first, second);

        let first_env = cargo_env(&root, Some(&first));
        let second_env = cargo_env(&root, Some(&second));
        assert!(first_env.contains(&("GIT_TRACE2_EVENT".to_owned(), first.display().to_string())));
        assert!(second_env.contains(&("GIT_TRACE2_EVENT".to_owned(), second.display().to_string())));
        assert_ne!(first_env, second_env);
    }

    #[test]
    fn concurrent_trace_merge_isolated_and_strict() {
        let root = test_path("concurrent-trace-merge");
        std::fs::create_dir_all(&root).expect("create trace root");
        let first = root.join("worker-0.jsonl");
        let second = root.join("worker-1.jsonl");
        let ambient = root.join("ambient.jsonl");
        std::fs::write(&first, complete_trace("worker-0", 11)).expect("write first trace");
        std::fs::write(&second, complete_trace("worker-1", 22)).expect("write second trace");
        std::fs::write(&ambient, complete_trace("ambient", 1000)).expect("write ambient trace");

        let GitEvidence::Observed {
            counters,
            successful,
        } = read_trace_files(&[first.clone(), second.clone()]).expect("merge traces")
        else {
            panic!("complete worker traces must be observed");
        };
        assert!(successful);
        assert_eq!(counters.received_bytes, 33);
        assert_eq!(counters.processes, 2);

        let no_git = root.join("no-git.jsonl");
        let error = read_trace_files(std::slice::from_ref(&no_git))
            .expect_err("missing child evidence must fail closed");
        assert!(error.to_string().contains("authoritative"), "{error:#}");

        let malformed = root.join("malformed.jsonl");
        std::fs::write(&malformed, "not json\n").expect("write malformed trace");
        let error = read_trace_files(&[first, malformed]).expect_err("malformed worker fails");
        assert!(error.to_string().contains("malformed"), "{error:#}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn feature_set_change_alternates_the_feature_flags() {
        let workload = build(crate::scenario::find("rust/feature-set-change").unwrap()).unwrap();
        let _ = workload;
        let plan = plan_for("rust/feature-set-change").unwrap();
        assert_eq!(plan.command, CargoCommand::AlternatingFeatures);

        let harness = CargoWorkload {
            plan,
            scenario: "rust/feature-set-change",
            scratch: ScratchOwner::new(),
            package: None,
            iteration: 0,
            notes: Vec::new(),
        };
        assert!(!harness.cargo_args(1).contains(&"--all-features".to_owned()));
        assert!(harness.cargo_args(2).contains(&"--all-features".to_owned()));
    }

    #[test]
    fn proc_macro_selection_uses_the_target_kind_not_the_name() {
        let metadata = serde_json::json!({
            "packages": [
                {"name": "zzz-not-a-macro", "targets": [{"kind": ["lib"]}]},
                {"name": "syn-like", "targets": [{"kind": ["proc-macro"]}]},
                {"name": "openssl-sys", "targets": [{"kind": ["lib"]}]}
            ]
        });
        assert_eq!(
            select_package(&metadata, PackageKind::ProcMacro).as_deref(),
            Some("syn-like")
        );
        assert_eq!(
            select_package(&metadata, PackageKind::NativeSys).as_deref(),
            Some("openssl-sys")
        );
    }

    #[test]
    fn selection_returns_nothing_when_the_graph_has_no_such_package() {
        let metadata = serde_json::json!({"packages": []});
        assert_eq!(select_package(&metadata, PackageKind::ProcMacro), None);
        assert_eq!(select_package(&metadata, PackageKind::NativeSys), None);
    }

    #[test]
    fn touch_preserves_content() {
        let path = std::env::temp_dir().join(format!("velnor-bench-touch-{}", std::process::id()));
        std::fs::write(&path, b"contents").expect("write");
        touch(&path).expect("touch");
        assert_eq!(std::fs::read(&path).expect("read"), b"contents");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn git_trace_is_outside_disk_measurement_but_counters_are_preserved() {
        let work_root = std::env::temp_dir().join(format!(
            "velnor-bench-cargo-trace-root-{}",
            std::process::id()
        ));
        let measured_root = work_root.join("rust_cold");
        std::fs::create_dir_all(&measured_root).expect("create measured root");

        let disk_before = tree_bytes(&measured_root);
        let trace_file = trace_file_path_for_worker(&work_root, "rust/cold", 1, 1, 1, 0);
        std::fs::write(&trace_file, complete_trace("cold-worker", 123)).expect("write trace");

        assert_eq!(tree_bytes(&measured_root), disk_before);
        let git = GitCounters::from_event_file(&trace_file).expect("read trace");
        assert_eq!(git.received_bytes, 123);

        let _ = std::fs::remove_dir_all(work_root);
    }

    #[test]
    fn scratch_owner_roots_are_unique_for_same_scenario() {
        let work_root = std::env::temp_dir();
        let first = ScratchOwner::new().scenario_root(&work_root, "rust/cold");
        let second = ScratchOwner::new().scenario_root(&work_root, "rust/cold");
        assert_ne!(first, second);
    }

    #[test]
    fn teardown_removes_owned_targets_traces_and_root() {
        let root = std::env::temp_dir().join(format!(
            "velnor-bench-cargo-owned-root-{}",
            std::process::id()
        ));
        let target = root.join("target");
        let trace = root.join("trace.jsonl");
        std::fs::create_dir_all(&target).expect("create target");
        std::fs::write(&trace, b"trace").expect("create trace");

        let mut workload = CargoWorkload {
            plan: plan_for("rust/cold").expect("cold plan"),
            scenario: "rust/cold",
            scratch: ScratchOwner {
                root: Some(root.clone()),
                targets: vec![target],
                traces: vec![trace],
                ..ScratchOwner::new()
            },
            package: None,
            iteration: 0,
            notes: Vec::new(),
        };
        let mut context = Context {
            work_root: std::env::temp_dir(),
            velnor_repo: std::env::temp_dir(),
            job_image: String::new(),
            iterations: 1,
            concurrency: 1,
            runner: Runner::new(),
        };

        workload.teardown(&mut context).expect("teardown");
        assert!(!root.exists());
        assert!(workload.scratch.root.is_none());
        assert!(workload.scratch.targets.is_empty());
        assert!(workload.scratch.traces.is_empty());
    }

    #[test]
    fn teardown_keeps_failed_owned_path_for_retry_and_cleans_siblings() {
        let root = std::env::temp_dir().join(format!(
            "velnor-bench-cargo-failed-root-{}",
            std::process::id()
        ));
        let failed_target = root.join("failed-target");
        let clean_target = root.join("clean-target");
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(&failed_target, b"not a directory").expect("create file target");
        std::fs::create_dir_all(&clean_target).expect("create clean target");

        let mut workload = CargoWorkload {
            plan: plan_for("rust/cold").expect("cold plan"),
            scenario: "rust/cold",
            scratch: ScratchOwner {
                root: Some(root.clone()),
                targets: vec![failed_target.clone(), clean_target.clone()],
                ..ScratchOwner::new()
            },
            package: None,
            iteration: 0,
            notes: Vec::new(),
        };
        let mut context = Context {
            work_root: std::env::temp_dir(),
            velnor_repo: std::env::temp_dir(),
            job_image: String::new(),
            iterations: 1,
            concurrency: 1,
            runner: Runner::new(),
        };

        let error = workload.teardown(&mut context).expect_err("failed cleanup");
        assert!(error.to_string().contains("failed-target"));
        assert!(failed_target.exists());
        assert!(!clean_target.exists());
        assert!(workload.scratch.root.is_some());
        assert_eq!(workload.scratch.targets, vec![failed_target.clone()]);

        std::fs::remove_dir_all(root).expect("remove test root");
    }

    #[test]
    fn lockfile_change_requires_different_bytes() {
        assert!(!lockfile_bytes_changed(b"lockfile", b"lockfile"));
        assert!(lockfile_bytes_changed(b"lockfile", b"changed"));
    }
}
