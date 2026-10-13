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
    time::Instant,
};

use anyhow::{bail, Context as _, Result};

use crate::{
    drivers::{Context, Workload},
    gittrace::{self, GitCounters},
    record::{Observation, Resources},
    scenario::Scenario,
    stage::Stage,
    sys::{tree_bytes, Runner},
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
        worktrees: Vec::new(),
        targets: Vec::new(),
        package: None,
        iteration: 0,
        notes: vec![
            "cargo-direct driver: measured on the host with no container and no runner, so \
             this record describes the build only and is not a claim about Velnor job latency"
                .to_owned(),
        ],
    }))
}

struct CargoWorkload {
    plan: Plan,
    scenario: &'static str,
    worktrees: Vec<PathBuf>,
    targets: Vec<PathBuf>,
    package: Option<String>,
    iteration: u64,
    notes: Vec<String>,
}

impl CargoWorkload {
    fn add_worktree(&mut self, context: &mut Context, root: &Path, name: &str) -> Result<PathBuf> {
        let path = root.join(name);
        let invocation = context
            .runner
            .run(
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
            )
            .context("git worktree add")?;
        if !invocation.ok() {
            bail!("git worktree add failed: {}", invocation.stderr.trim());
        }
        self.worktrees.push(path.clone());
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
        let mut env = vec![
            ("CARGO_TARGET_DIR".to_owned(), target.display().to_string()),
            ("CARGO_TERM_COLOR".to_owned(), "never".to_owned()),
            ("CARGO_INCREMENTAL".to_owned(), "0".to_owned()),
        ];
        if let Some(trace_file) = trace_file {
            env.extend(gittrace::trace_env(trace_file));
        }
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
        let root = context.work_root.join(self.scenario.replace('/', "_"));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir_all(&root)?;

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
            std::fs::create_dir_all(&target)?;
            self.targets.push(target);
        }

        if let CargoCommand::ResolvedPackage { kind } = self.plan.command {
            let workspace = self.worktrees[0].clone();
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
            for index in 0..self.targets.len() {
                let workspace = self.worktrees[index.min(self.worktrees.len() - 1)].clone();
                let target = self.targets[index].clone();
                self.run_cargo(context, &workspace, &target, &args, None)
                    .context("warm-up run")?;
            }
        }
        Ok(())
    }

    fn iterate(&mut self, context: &mut Context) -> Result<Observation> {
        self.iteration += 1;
        let root = context.work_root.join(self.scenario.replace('/', "_"));

        if self.plan.workspace == Workspace::FreshEachIteration {
            let name = format!("workspace-fresh-{}", self.iteration);
            self.add_worktree(context, &root, &name)?;
        }
        let workspace = self
            .worktrees
            .last()
            .cloned()
            .expect("prepare created at least one worktree");
        let target = self.targets[0].clone();

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
        let trace_file = trace_file_path(&context.work_root, self.scenario, self.iteration);
        clear_trace_file(&trace_file)?;
        let disk_before = tree_bytes(&root);
        let started = Instant::now();
        let command_ms = if self.plan.workspace == Workspace::PerConcurrentJob {
            self.run_concurrent(context, &args)?
        } else {
            self.run_cargo(context, &workspace, &target, &args, Some(&trace_file))?
        };
        let total_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let usage = context.runner.rusage();
        let disk_after = tree_bytes(&root);
        let process_count = context.runner.process_count() as u64;
        let docker_invocations = context.runner.count_of("docker") as u64;
        let git = GitCounters::from_event_file(&trace_file).unwrap_or_default();

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
                bytes_downloaded: git.received_bytes,
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
        for path in std::mem::take(&mut self.worktrees) {
            let result = context.runner.run(
                "git",
                &[
                    "-C".to_owned(),
                    context.velnor_repo.display().to_string(),
                    "worktree".to_owned(),
                    "remove".to_owned(),
                    "--force".to_owned(),
                    path.display().to_string(),
                ],
            );
            match result {
                Ok(invocation) if invocation.ok() => {}
                Ok(invocation) => failures.push(format!(
                    "remove worktree {} exited {}: {}",
                    path.display(),
                    invocation.code,
                    invocation.stderr.trim()
                )),
                Err(error) => failures.push(format!(
                    "remove worktree {} could not be executed: {error}",
                    path.display()
                )),
            }
        }
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
    /// Concurrent jobs really do run concurrently: one thread per worktree,
    /// each with its own target directory.
    fn run_concurrent(&self, context: &mut Context, args: &[String]) -> Result<u64> {
        let pairs: Vec<(PathBuf, PathBuf)> = self
            .worktrees
            .iter()
            .cloned()
            .zip(self.targets.iter().cloned())
            .collect();
        let started = Instant::now();
        let worker_results: Vec<Result<Runner, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = pairs
                .iter()
                .map(|(workspace, target)| {
                    scope.spawn(move || {
                        let mut runner = Runner::new();
                        let outcome = match runner.exec_without(
                            "cargo",
                            args,
                            Some(workspace),
                            &[
                                ("CARGO_TARGET_DIR".to_owned(), target.display().to_string()),
                                ("CARGO_TERM_COLOR".to_owned(), "never".to_owned()),
                                ("CARGO_INCREMENTAL".to_owned(), "0".to_owned()),
                            ],
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

fn trace_file_path(work_root: &Path, scenario: &str, iteration: u64) -> PathBuf {
    work_root.join(format!(
        "{}-git-trace-{iteration}.jsonl",
        scenario.replace('/', "_")
    ))
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
    fn teardown_surfaces_worktree_removal_failures() {
        let mut workload = CargoWorkload {
            plan: plan_for("rust/cold").expect("cold plan"),
            scenario: "rust/cold",
            worktrees: vec![std::env::temp_dir().join("velnor-bench-unregistered-worktree")],
            targets: Vec::new(),
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
    }

    #[test]
    fn concurrent_workers_are_recorded_by_the_parent_runner() {
        let worktree = std::env::temp_dir();
        let harness = CargoWorkload {
            plan: plan_for("rust/concurrent-jobs").expect("concurrent plan"),
            scenario: "rust/concurrent-jobs",
            worktrees: vec![worktree.clone(), worktree.clone()],
            targets: vec![worktree.clone(), worktree],
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
            .run_concurrent(&mut context, &["--version".to_owned()])
            .expect("cargo workers");

        assert_eq!(context.runner.process_count(), 2);
        assert_eq!(context.runner.count_of("cargo"), 2);
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
            worktrees: Vec::new(),
            targets: Vec::new(),
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
        let trace_file = trace_file_path(&work_root, "rust/cold", 1);
        std::fs::write(
            &trace_file,
            br#"{"event":"version"}
{"event":"data","key":"bytes-received","value":123}"#,
        )
        .expect("write trace");

        assert_eq!(tree_bytes(&measured_root), disk_before);
        let git = GitCounters::from_event_file(&trace_file).expect("read trace");
        assert_eq!(git.received_bytes, 123);

        let _ = std::fs::remove_dir_all(work_root);
    }

    #[test]
    fn lockfile_change_requires_different_bytes() {
        assert!(!lockfile_bytes_changed(b"lockfile", b"lockfile"));
        assert!(lockfile_bytes_changed(b"lockfile", b"changed"));
    }
}
