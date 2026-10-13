//! Differential replay of the two checkout strategies this program shipped.
//!
//! The runner's checkout is not reachable from a separate process: it has no
//! binary entry point, and the only driver that observes it is `velnor-job`,
//! which needs a registered runner. So the checkout claim cannot be reproduced
//! through the scenario matrix on a host without one — and it is not put in the
//! matrix here, because a matrix scenario is a claim about the product and this
//! is a claim about two sequences of `git` commands.
//!
//! What this module does instead is replay, against a real repository, the
//! exact argv each revision's checkout builds:
//!
//! * [`Strategy::MirrorAllRefs`] — `2858e92`. The mirror fetches
//!   `+refs/*:refs/*` at full depth on every job while holding the store's
//!   exclusive lock across the network, and the workspace then *fetches from
//!   the mirror*, which writes a second copy of the objects.
//! * [`Strategy::WantedRefLinked`] — `22c1b95`. The mirror fetches only the
//!   wanted revision, pins it under `refs/velnor`, takes the exclusive lock
//!   only to create the store, and returns without touching the network when
//!   the commit is already present. The workspace hard-links the object files
//!   instead of fetching them.
//!
//! Nothing here is simulated: every phase runs the real `git` process, or the
//! real filesystem operation, that the corresponding revision runs. What it is
//! *not* is an execution of `crates/velnor-runner/src/checkout.rs`, and every
//! record this module produces says so.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context as _, Result};

use crate::{
    env::EnvironmentIdentity,
    gittrace::{self, GitCounters},
    stage::CheckoutPhase,
    stats::{Summary, TooFewSamples},
    sys::{tree_bytes, Runner},
};

/// Stable discriminator for a replay record.
pub const REPLAY_SCHEMA: &str = "velnor.bench.checkout-replay.v1";

/// Which revision's checkout argv is being replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// `2858e92`: mirror fetches every ref, workspace fetches from the mirror.
    MirrorAllRefs,
    /// `22c1b95`: mirror fetches the wanted revision, workspace hard-links.
    WantedRefLinked,
}

impl Strategy {
    /// Both strategies, baseline first.
    pub const ALL: [Self; 2] = [Self::MirrorAllRefs, Self::WantedRefLinked];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MirrorAllRefs => "mirror-all-refs",
            Self::WantedRefLinked => "wanted-ref-linked",
        }
    }

    /// The revision whose checkout this strategy reproduces.
    #[must_use]
    pub const fn revision(self) -> &'static str {
        match self {
            Self::MirrorAllRefs => "2858e92df0eb78df4f1a6fe2ad4cbf86f1d56355",
            Self::WantedRefLinked => "22c1b95e3d61a4416336e04dd317c83b2aaed19c",
        }
    }
}

/// A synthetic origin repository: one branch plus a wall of pull refs, which is
/// the shape that made `+refs/*:refs/*` expensive on a real GitHub repository.
#[derive(Debug, Clone)]
pub struct Fixture {
    /// Bare repository the mirror fetches from.
    pub origin: PathBuf,
    /// Commit the job asks for.
    pub sha: String,
    /// Refs under `refs/pull/*` the origin advertises.
    pub pull_refs: usize,
    /// Bytes of tracked content on the wanted commit.
    pub content_bytes: u64,
    /// Bytes reachable only from `refs/pull/*` — the objects a job that asked
    /// for one commit has no use for, and the whole cost the ref wall imposes.
    pub pull_ref_bytes: u64,
}

/// One replayed checkout: the cold mirror plus `legs - 1` further workspaces on
/// the same commit, which is the N-way matrix shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Replay {
    /// Phase timings of the first (cold) leg.
    pub cold_phases_ms: BTreeMap<CheckoutPhase, u64>,
    /// Wall milliseconds of the cold leg, mirror creation included.
    pub cold_total_ms: u64,
    /// On-disk bytes of the mirror after the cold fetch.
    pub mirror_bytes: u64,
    /// Wall milliseconds of every further leg, summed.
    pub further_legs_ms: u64,
    /// Object bytes each further leg had to copy rather than share.
    pub further_legs_object_bytes: u64,
    /// `git fetch` processes that contacted the origin across every leg.
    pub origin_fetches: u64,
    /// trace2 counters summed over every git process in the replay.
    pub git: GitCounters,
}

/// Build the synthetic origin.
///
/// # Errors
/// Any git command failed, or the content could not be written.
pub fn build_fixture(
    root: &Path,
    runner: &mut Runner,
    pull_refs: usize,
    blob_bytes: usize,
    blobs: usize,
    pull_blob_bytes: usize,
) -> Result<Fixture> {
    let work = root.join("origin-work");
    let origin = root.join("origin.git");
    std::fs::create_dir_all(&work).with_context(|| format!("create {}", work.display()))?;
    git(runner, &["init", "-q", "-b", "main", str_of(&work)?])?;
    git(
        runner,
        &[
            "-C",
            str_of(&work)?,
            "config",
            "user.email",
            "bench@velnor.invalid",
        ],
    )?;
    git(
        runner,
        &["-C", str_of(&work)?, "config", "user.name", "velnor-bench"],
    )?;

    let mut content_bytes = 0_u64;
    for index in 0..blobs {
        // Distinct, incompressible-ish content so the pack is not a rounding
        // error: a repository of identical blobs would deltify to nothing and
        // the byte counts would say nothing about a real checkout.
        let path = work.join(format!("blob-{index:04}.bin"));
        let body = filler(index, blob_bytes);
        content_bytes += body.len() as u64;
        std::fs::write(&path, &body).with_context(|| format!("write {}", path.display()))?;
    }
    git(runner, &["-C", str_of(&work)?, "add", "."])?;
    git(
        runner,
        &["-C", str_of(&work)?, "commit", "-q", "-m", "fixture"],
    )?;

    git(runner, &["init", "-q", "--bare", str_of(&origin)?])?;
    // GitHub answers a fetch for a bare object id; the fixture must too, or the
    // wanted-ref strategy would be measured on its fallback path.
    git(
        runner,
        &[
            "-C",
            str_of(&origin)?,
            "config",
            "uploadpack.allowAnySHA1InWant",
            "true",
        ],
    )?;
    git(
        runner,
        &["-C", str_of(&work)?, "push", "-q", str_of(&origin)?, "main"],
    )?;

    let sha = capture(runner, &["-C", str_of(&work)?, "rev-parse", "HEAD"])?;

    // The pull refs: each is a distinct commit carrying its own blob, because
    // the whole cost `+refs/*:refs/*` pays is the objects reachable only from
    // refs the job did not ask for. A ref wall of empty commits would make the
    // two strategies look identical for a reason that has nothing to do with
    // either of them. A ref whose object the origin does not hold would be a
    // broken advertisement rather than a cost, so the commits are pushed, never
    // `update-ref`d in.
    let mut refspecs = Vec::with_capacity(pull_refs);
    let mut pull_ref_bytes = 0_u64;
    for index in 0..pull_refs {
        let body = filler(blobs + index + 1, pull_blob_bytes);
        pull_ref_bytes += body.len() as u64;
        std::fs::write(work.join("pr.bin"), &body)
            .with_context(|| "write pull payload".to_owned())?;
        std::fs::write(work.join("pr.txt"), format!("pull {index}\n"))
            .with_context(|| "write pull marker".to_owned())?;
        git(runner, &["-C", str_of(&work)?, "add", "pr.bin", "pr.txt"])?;
        git(
            runner,
            &[
                "-C",
                str_of(&work)?,
                "commit",
                "-q",
                "-m",
                &format!("pull {index}"),
            ],
        )?;
        let head = capture(runner, &["-C", str_of(&work)?, "rev-parse", "HEAD"])?;
        refspecs.push(format!("{head}:refs/pull/{index}/head"));
        git(
            runner,
            &["-C", str_of(&work)?, "reset", "-q", "--hard", "HEAD~1"],
        )?;
    }
    if !refspecs.is_empty() {
        let mut push = vec![
            "-C",
            str_of(&work)?,
            "push",
            "-q",
            "--force",
            str_of(&origin)?,
        ];
        push.extend(refspecs.iter().map(String::as_str));
        git(runner, &push)?;
    }
    let _ = git(runner, &["-C", str_of(&origin)?, "gc", "-q", "--prune=now"]);

    Ok(Fixture {
        origin,
        sha,
        pull_refs,
        content_bytes,
        pull_ref_bytes,
    })
}

/// Deterministic filler that does not deltify to nothing.
fn filler(seed: usize, len: usize) -> Vec<u8> {
    let mut state = (seed as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(1);
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Replay one strategy end to end: a fresh mirror store, one cold workspace,
/// then `legs - 1` further workspaces on the same commit.
///
/// # Errors
/// Any git command failed, or a filesystem operation failed.
pub fn replay(
    strategy: Strategy,
    fixture: &Fixture,
    root: &Path,
    runner: &mut Runner,
    legs: usize,
) -> Result<Replay> {
    if legs == 0 {
        bail!("a replay needs at least one workspace leg");
    }
    let store = root.join("mirrors");
    let mirror = store.join("fixture.git");
    let lock_path = store.join("fixture.lock");
    std::fs::create_dir_all(&store).with_context(|| format!("create {}", store.display()))?;
    let trace = root.join("git-trace.jsonl");
    let _ = std::fs::remove_file(&trace);
    let env = gittrace::trace_env(&trace).to_vec();

    let mut phases: BTreeMap<CheckoutPhase, u64> = BTreeMap::new();
    let mut origin_fetches = 0_u64;
    let started = Instant::now();

    // Phase 1: the store lock. Both revisions open the same lock file; only the
    // mode differs, and the mode is the whole point of the phase.
    let lock_started = Instant::now();
    let lock = lock_file(&lock_path)?;
    let lock_ms = millis(lock_started);

    // Phase 2: mirror preparation and fetch.
    let mirror_started = Instant::now();
    if !mirror.join("HEAD").exists() {
        git(runner, &["init", "-q", "--bare", str_of(&mirror)?])?;
    }
    match strategy {
        Strategy::MirrorAllRefs => {
            git_env(
                runner,
                &[
                    "-C",
                    str_of(&mirror)?,
                    "-c",
                    "protocol.version=2",
                    "fetch",
                    str_of(&fixture.origin)?,
                    "+refs/*:refs/*",
                ],
                &env,
            )?;
            origin_fetches += 1;
        }
        Strategy::WantedRefLinked => {
            git_env(
                runner,
                &[
                    "-C",
                    str_of(&mirror)?,
                    "-c",
                    "protocol.version=2",
                    "-c",
                    "gc.auto=0",
                    "fetch",
                    "--no-tags",
                    "--force",
                    str_of(&fixture.origin)?,
                    &fixture.sha,
                ],
                &env,
            )?;
            origin_fetches += 1;
            git(
                runner,
                &[
                    "-C",
                    str_of(&mirror)?,
                    "update-ref",
                    &format!("refs/velnor/wanted/{}", fixture.sha),
                    &fixture.sha,
                ],
            )?;
        }
    }
    let mirror_ms = millis(mirror_started);
    let mirror_bytes = tree_bytes(&mirror);
    drop(lock);

    // Phases 3-5: the first workspace.
    let cold = workspace_leg(strategy, fixture, &mirror, &root.join("ws-0"), runner, &env)?;
    phases.insert(CheckoutPhase::MirrorLockWait, lock_ms);
    phases.insert(CheckoutPhase::MirrorFetch, mirror_ms);
    for (phase, value) in &cold.phases {
        phases.insert(*phase, *value);
    }
    let cold_total_ms = millis(started);

    // Further legs: the N-way matrix on one commit.
    let further_started = Instant::now();
    let mut further_object_bytes = 0_u64;
    for leg in 1..legs {
        let lock = lock_file(&lock_path)?;
        match strategy {
            Strategy::MirrorAllRefs => {
                // The baseline refetches unconditionally; there is no
                // already-present short circuit to take.
                git_env(
                    runner,
                    &[
                        "-C",
                        str_of(&mirror)?,
                        "-c",
                        "protocol.version=2",
                        "fetch",
                        str_of(&fixture.origin)?,
                        "+refs/*:refs/*",
                    ],
                    &env,
                )?;
                origin_fetches += 1;
            }
            Strategy::WantedRefLinked => {
                let present = runner
                    .run(
                        "git",
                        &[
                            "-C".to_owned(),
                            str_of(&mirror)?.to_owned(),
                            "cat-file".to_owned(),
                            "-e".to_owned(),
                            format!("{}^{{commit}}", fixture.sha),
                        ],
                    )
                    .is_ok_and(crate::sys::Invocation::ok);
                if !present {
                    bail!("the wanted commit vanished from the mirror between legs");
                }
            }
        }
        drop(lock);
        let leg_result = workspace_leg(
            strategy,
            fixture,
            &mirror,
            &root.join(format!("ws-{leg}")),
            runner,
            &env,
        )?;
        further_object_bytes += leg_result.object_bytes_copied;
    }
    let further_legs_ms = millis(further_started);

    let git_counters = GitCounters::from_event_file(&trace).unwrap_or_default();

    Ok(Replay {
        cold_phases_ms: phases,
        cold_total_ms,
        mirror_bytes,
        further_legs_ms,
        further_legs_object_bytes: further_object_bytes,
        origin_fetches,
        git: git_counters,
    })
}

struct LegResult {
    phases: BTreeMap<CheckoutPhase, u64>,
    /// Bytes the leg had to write into its own object store. Zero when every
    /// object is a hard link to the mirror's.
    object_bytes_copied: u64,
}

fn workspace_leg(
    strategy: Strategy,
    fixture: &Fixture,
    mirror: &Path,
    destination: &Path,
    runner: &mut Runner,
    env: &[(String, String)],
) -> Result<LegResult> {
    std::fs::create_dir_all(destination)
        .with_context(|| format!("create {}", destination.display()))?;
    git(runner, &["init", "-q", str_of(destination)?])?;
    let git_dir = destination.join(".git");

    let fetch_started = Instant::now();
    let object_bytes_copied = match strategy {
        Strategy::MirrorAllRefs => {
            git(
                runner,
                &[
                    "-C",
                    str_of(destination)?,
                    "remote",
                    "add",
                    "origin",
                    str_of(&fixture.origin)?,
                ],
            )?;
            git_env(
                runner,
                &[
                    "-C",
                    str_of(destination)?,
                    "-c",
                    "protocol.version=2",
                    "fetch",
                    "--prune",
                    "--tags",
                    str_of(mirror)?,
                    &fixture.sha,
                    "+refs/heads/*:refs/remotes/origin/*",
                    "+refs/tags/*:refs/tags/*",
                ],
                env,
            )?;
            // Everything under the workspace's own object store is a second
            // copy of bytes the mirror already holds.
            tree_bytes(&git_dir.join("objects"))
        }
        Strategy::WantedRefLinked => {
            let copied = link_objects(&mirror.join("objects"), &git_dir.join("objects"))?;
            std::fs::write(
                git_dir.join("FETCH_HEAD"),
                format!(
                    "{}\t\tcommit '{}' of {}\n",
                    fixture.sha,
                    fixture.sha,
                    fixture.origin.display()
                ),
            )
            .with_context(|| "write FETCH_HEAD".to_owned())?;
            copied
        }
    };
    let fetch_ms = millis(fetch_started);

    let checkout_started = Instant::now();
    git(
        runner,
        &[
            "-C",
            str_of(destination)?,
            "checkout",
            "--force",
            "FETCH_HEAD",
        ],
    )?;
    let checkout_ms = millis(checkout_started);

    let mtime_started = Instant::now();
    normalize_mtimes(destination, runner)?;
    let mtime_ms = millis(mtime_started);

    Ok(LegResult {
        phases: BTreeMap::from([
            (CheckoutPhase::WorkspaceFetch, fetch_ms),
            (CheckoutPhase::WorkspaceCheckout, checkout_ms),
            (CheckoutPhase::MtimeNormalization, mtime_ms),
        ]),
        object_bytes_copied,
    })
}

/// Hard-link every object file, returning the bytes that had to be *copied*
/// because a link was impossible. This is `crates/velnor-runner/src/checkout.rs`'s
/// object materialisation, down to skipping `info/` and mid-write `tmp_` names.
///
/// # Errors
/// A directory could not be created, or a fallback copy failed.
pub fn link_objects(source: &Path, destination: &Path) -> Result<u64> {
    let mut copied = 0_u64;
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(source.join(&relative)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let child = relative.join(&name);
            if file_type.is_dir() {
                if name != "info" {
                    pending.push(child);
                }
                continue;
            }
            if !file_type.is_file() || name.to_string_lossy().starts_with("tmp_") {
                continue;
            }
            let target = destination.join(&child);
            if target.exists() {
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            let origin = source.join(&child);
            if std::fs::hard_link(&origin, &target).is_err() {
                let bytes = std::fs::copy(&origin, &target)
                    .with_context(|| format!("copy {}", origin.display()))?;
                copied += bytes;
            }
        }
    }
    Ok(copied)
}

/// The mtime pin both revisions perform after checkout, unchanged between them.
fn normalize_mtimes(destination: &Path, runner: &mut Runner) -> Result<()> {
    let stamp = capture(
        runner,
        &["-C", str_of(destination)?, "log", "-1", "--format=%ct"],
    )?;
    let Ok(seconds) = stamp.trim().parse::<u64>() else {
        return Ok(());
    };
    let commit_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds);
    let mut pending = vec![destination.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if entry.file_name() != ".git" {
                    pending.push(entry.path());
                }
            } else if file_type.is_file()
                && let Ok(file) = std::fs::File::options().append(true).open(entry.path())
            {
                let _ = file.set_modified(commit_time);
            }
        }
        if let Ok(handle) = std::fs::File::open(&dir) {
            let _ = handle.set_modified(commit_time);
        }
    }
    Ok(())
}

fn lock_file(path: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open lock {}", path.display()))?;
    file.lock()
        .with_context(|| format!("lock {}", path.display()))?;
    Ok(file)
}

fn millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn str_of(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("path {} is not utf-8", path.display()))
}

fn git(runner: &mut Runner, args: &[&str]) -> Result<()> {
    git_env(runner, args, &[])
}

fn git_env(runner: &mut Runner, args: &[&str], env: &[(String, String)]) -> Result<()> {
    let invocation = runner
        .exec("git", args, None, env)
        .with_context(|| format!("spawn git {}", args.join(" ")))?;
    if !invocation.ok() {
        bail!(
            "git {} exited {}: {}",
            args.join(" "),
            invocation.code,
            invocation.stderr.trim()
        );
    }
    Ok(())
}

fn capture(runner: &mut Runner, args: &[&str]) -> Result<String> {
    runner
        .capture("git", args)
        .map_err(|reason| anyhow::anyhow!(reason))
}

/// Identity of the synthetic repository a replay ran against, so a number can
/// never be read without the shape that produced it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FixtureIdentity {
    pub pull_refs: usize,
    pub content_bytes: u64,
    pub pull_ref_bytes: u64,
    pub commit: String,
}

/// Distribution summaries over a strategy's replays.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplaySummaries {
    pub cold_total_ms: Summary,
    pub cold_phases_ms: BTreeMap<CheckoutPhase, Summary>,
    pub mirror_bytes: Summary,
    pub further_legs_ms: Summary,
    pub further_legs_object_bytes: Summary,
    pub origin_fetches: Summary,
    pub received_bytes: Summary,
}

impl ReplaySummaries {
    /// Summarise a sample of replays.
    ///
    /// # Errors
    /// Fewer replays than [`crate::stats::MIN_SAMPLES`].
    pub fn new(replays: &[Replay]) -> Result<Self, TooFewSamples> {
        let field = |extract: fn(&Replay) -> u64| -> Result<Summary, TooFewSamples> {
            Summary::new(&replays.iter().map(extract).collect::<Vec<u64>>())
        };
        let mut cold_phases_ms = BTreeMap::new();
        for phase in CheckoutPhase::ALL {
            if replays
                .iter()
                .all(|replay| replay.cold_phases_ms.contains_key(&phase))
            {
                let values: Vec<u64> = replays
                    .iter()
                    .map(|replay| replay.cold_phases_ms[&phase])
                    .collect();
                cold_phases_ms.insert(phase, Summary::new(&values)?);
            }
        }
        Ok(Self {
            cold_total_ms: field(|replay| replay.cold_total_ms)?,
            cold_phases_ms,
            mirror_bytes: field(|replay| replay.mirror_bytes)?,
            further_legs_ms: field(|replay| replay.further_legs_ms)?,
            further_legs_object_bytes: field(|replay| replay.further_legs_object_bytes)?,
            origin_fetches: field(|replay| replay.origin_fetches)?,
            received_bytes: field(|replay| replay.git.received_bytes)?,
        })
    }
}

/// One strategy's replay result, in the same NDJSON shape as a bench record.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReplayRecord {
    pub schema: String,
    pub run_id: String,
    pub recorded_at_unix_ms: u64,
    pub strategy: Strategy,
    /// The revision whose checkout argv this record replays.
    pub revision: String,
    pub environment: EnvironmentIdentity,
    pub fixture: FixtureIdentity,
    /// Workspaces per replay: one cold leg plus `legs - 1` matrix legs.
    pub legs: usize,
    pub replays: Vec<Replay>,
    pub summaries: ReplaySummaries,
    pub notes: Vec<String>,
}

impl ReplayRecord {
    /// Serialise as one NDJSON line.
    ///
    /// # Errors
    /// Serialisation failure.
    pub fn to_ndjson(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
}

/// The caveat every replay record carries. A reader must not mistake this for
/// an execution of the runner's checkout.
#[must_use]
pub fn caveats() -> Vec<String> {
    vec![
        "this record replays the git argv of the named revision against a synthetic \
         repository; it is not an execution of crates/velnor-runner/src/checkout.rs, \
         which has no entry point outside a job"
            .to_owned(),
        "the origin is a local path, so mirror-fetch time excludes wide-area latency \
         and TLS; the ref and byte counts are transport-independent and the wall times \
         are a lower bound"
            .to_owned(),
        "crates/velnor-runner/src/checkout.rs emits no tracing span per checkout phase \
         and sets no GIT_TRACE2_EVENT on its git children, so these phases cannot yet \
         be read out of a real job's trace.jsonl"
            .to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "velnor-bench-replay-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch");
        dir
    }

    #[test]
    fn strategies_name_the_revision_they_reproduce() {
        for strategy in Strategy::ALL {
            assert_eq!(strategy.revision().len(), 40, "{}", strategy.as_str());
        }
        assert_ne!(
            Strategy::MirrorAllRefs.revision(),
            Strategy::WantedRefLinked.revision()
        );
    }

    #[test]
    fn filler_is_deterministic_and_sized_exactly() {
        assert_eq!(filler(7, 1000).len(), 1000);
        assert_eq!(filler(7, 1000), filler(7, 1000));
        assert_ne!(filler(7, 1000), filler(8, 1000));
    }

    #[test]
    fn a_replay_needs_at_least_one_leg() {
        let dir = scratch("legs");
        let mut runner = Runner::new();
        let fixture = Fixture {
            origin: dir.join("origin.git"),
            sha: "0".repeat(40),
            pull_refs: 0,
            content_bytes: 0,
            pull_ref_bytes: 0,
        };
        assert!(replay(Strategy::WantedRefLinked, &fixture, &dir, &mut runner, 0).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hard_linking_copies_no_bytes_and_shares_the_inode() {
        let dir = scratch("link");
        let source = dir.join("src");
        let destination = dir.join("dst");
        std::fs::create_dir_all(source.join("ab")).expect("create");
        std::fs::create_dir_all(source.join("info")).expect("create");
        std::fs::write(source.join("ab").join("cdef"), b"object bytes").expect("write");
        std::fs::write(source.join("ab").join("tmp_half"), b"mid-write").expect("write");
        std::fs::write(source.join("info").join("alternates"), b"elsewhere").expect("write");

        let copied = link_objects(&source, &destination).expect("link");

        assert_eq!(copied, 0, "a same-filesystem link must copy no bytes");
        assert!(destination.join("ab").join("cdef").exists());
        assert!(
            !destination.join("ab").join("tmp_half").exists(),
            "a mid-write object must never be linked"
        );
        assert!(
            !destination.join("info").exists(),
            "info/ holds regenerable caches and an alternates pointer"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_strategies_check_out_the_same_tree_and_the_linked_one_copies_nothing() {
        let dir = scratch("replay");
        let mut runner = Runner::new();
        if runner.capture("git", &["--version"]).is_err() {
            // No git here; the replay is reported as unrun, never faked.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let fixture =
            build_fixture(&dir, &mut runner, 8, 4096, 4, 4096).expect("build the synthetic origin");
        assert_eq!(fixture.sha.len(), 40);
        assert!(
            fixture.pull_ref_bytes > 0,
            "the ref wall must carry objects, not just names"
        );

        let mut trees = Vec::new();
        for strategy in Strategy::ALL {
            let root = dir.join(strategy.as_str());
            std::fs::create_dir_all(&root).expect("create");
            let result =
                replay(strategy, &fixture, &root, &mut runner, 3).expect("replay the strategy");
            assert!(result
                .cold_phases_ms
                .contains_key(&CheckoutPhase::MirrorFetch));
            assert!(result
                .cold_phases_ms
                .contains_key(&CheckoutPhase::WorkspaceCheckout));
            if strategy == Strategy::WantedRefLinked {
                assert_eq!(
                    result.further_legs_object_bytes, 0,
                    "a linked workspace copies no object bytes"
                );
                assert_eq!(
                    result.origin_fetches, 1,
                    "further legs on one commit must not touch the origin"
                );
            } else {
                assert_eq!(
                    result.origin_fetches, 3,
                    "the baseline refetches the origin on every leg"
                );
            }
            trees.push(
                runner
                    .capture(
                        "git",
                        &[
                            "-C",
                            root.join("ws-0").to_str().expect("utf-8"),
                            "rev-parse",
                            "HEAD^{tree}",
                        ],
                    )
                    .expect("resolve tree"),
            );
        }
        assert_eq!(
            trees[0], trees[1],
            "the two strategies must agree on the tree"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
