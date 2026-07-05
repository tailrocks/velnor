# Plan 002: Make cache-save staging unique per slot (not per PID) and non-fatal on contention

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> "STOP conditions" item occurs, stop and report — do not improvise. When done,
> update this plan's row in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs`
> On any change to `executor.rs`, compare the "Current state" excerpts to the
> live code before proceeding; mismatch ⇒ STOP.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The daemon runs N slots as **tokio tasks inside one OS process** (one PID). The
`actions/cache` save path names its staging directory with
`std::process::id()`, which is identical for every slot. The cache store is
**daemon-shared** across slots. So two slots saving the same cache key (routine:
matrix legs, re-runs, or two PRs on the same base — the key is a lockfile hash)
derive the **same** staging path and race: one slot's `remove_dir_all(&staging)`
deletes files the other is mid-copy into, causing a spurious `Err` that aborts
the whole job (a job whose steps all passed reports FAILURE), or publishes a
truncated tree (cache corruption). `actions/cache` never fails a job on a save
race; Velnor must not either. This fix makes the staging path unique per save
and downgrades save-contention errors to warnings.

## Current state

- `crates/velnor-runner/src/executor.rs` — cache save is `save_cache_result`
  (a free function). Excerpt at `executor.rs:3251-3312`:
  ```rust
  let cache_dir = cache_store_dir(state)?.join(sanitize_artifact_name(key));
  let staging_name = format!(
      "{}.staging-{}",
      cache_dir.file_name().and_then(|n| n.to_str()).unwrap_or("cache"),
      std::process::id()                       // <-- identical across in-process slots
  );
  let staging_dir = cache_dir.with_file_name(staging_name);
  fs::remove_dir_all(&staging_dir).ok();
  fs::create_dir_all(&staging_dir).with_context(...)?;
  ...
  for (index, path) in cache_paths(paths).into_iter().enumerate() {
      ...
      let target = staging_dir.join(index.to_string());
      fs::create_dir_all(&target).with_context(...)?;
      copy_cache_source(&source, &target)?;    // <-- Err here aborts the whole job (see plan 009)
      saved += 1;
  }
  ...
  fs::remove_dir_all(&cache_dir).ok();
  fs::rename(&staging_dir, &cache_dir).with_context(...)?;   // <-- races a concurrent restore
  ```
- Slots are in-process tokio tasks: `runner.rs:672` spawns each slot; job
  execution runs on `tokio::task::spawn_blocking` (`runner.rs:2261`) — same PID.
- `JobExecutionState` carries a slot/job identity. Confirm what unique value is
  available on `state` for the staging suffix: look for a slot index, job id, or
  daemon id field on `JobExecutionState` (grep `struct JobExecutionState`). If a
  unique per-job id already exists there, use it; otherwise use a process-global
  atomic counter (see Step 1).

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| One test  | `cargo nextest run -p velnor-runner cache_stag --locked`        | new test passes |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/executor.rs` — `save_cache_result` staging naming
  and the save-contention error handling, plus a unit test.

**Out of scope**:
- `cache_store_dir` repo/trust scoping — that is plan 006; do not change the
  store root here.
- The cache **restore** path (`restore_cache_paths`) — not modified here.

## Git workflow

- Branch: `advisor/002-cache-staging-pid-collision`
- Commit: `fix(cache): stage cache saves per-slot, tolerate save contention`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Give the staging directory a process-unique suffix

Replace `std::process::id()` in the `staging_name` with a value that is unique
across concurrent in-process saves. Preferred: if `JobExecutionState` exposes a
unique per-job/slot identifier, incorporate it. Otherwise add a module-level
monotonic counter and combine it with the PID:

```rust
use std::sync::atomic::{AtomicU64, Ordering};
static CACHE_STAGING_SEQ: AtomicU64 = AtomicU64::new(0);
// ...
let unique = CACHE_STAGING_SEQ.fetch_add(1, Ordering::Relaxed);
let staging_name = format!(
    "{}.staging-{}-{}",
    cache_dir.file_name().and_then(|n| n.to_str()).unwrap_or("cache"),
    std::process::id(),
    unique
);
```

The staging directory must be unique even when two slots save the same key
concurrently.

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Do not fail the job on save contention

Change the two error-propagating operations that can lose a race — the
per-entry `copy_cache_source(&source, &target)?` inside the loop, and the final
`fs::rename(&staging_dir, &cache_dir)?` — so a failure is logged into the cache
step's stderr text and returns a **successful** `cache_save_step_result` (exit
code 0, with a warning message), matching `actions/cache` behavior. Keep the
staging cleanup (`fs::remove_dir_all(&staging_dir).ok()`) on the failure path so
no partial staging dir leaks. Do not swallow the error silently — surface it in
the step's stderr string so it appears in logs.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 3: Unit test — two saves of the same key don't collide

Add a test `cache_staging_dirs_are_unique_per_save` in the `executor.rs`
`#[cfg(test)]` module. Since staging naming is deterministic given the counter,
assert that two calls to the staging-name construction (extract it into a small
helper `fn cache_staging_name(cache_file_name: &str) -> String` if that makes it
testable) produce **different** names. If extracting a helper is cleaner, do so
and call it from `save_cache_result`. Keep the helper private.

**Verify**: `cargo nextest run -p velnor-runner cache_stag --locked` → passes;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- New test asserting staging names are unique across two consecutive
  constructions (the collision the PID caused).
- If you add a save-contention integration-style test, model directory setup on
  existing cache tests in `executor.rs` (grep for `save_cache_result` /
  `restore_cache` in the test module).
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` exits 0
- [ ] `grep -n "staging-{}" crates/velnor-runner/src/executor.rs` shows the suffix now includes a unique counter/id, not only `process::id()`
- [ ] `save_cache_result` returns Ok (exit 0) when a save-time copy/rename loses a race, with the failure surfaced in stderr text
- [ ] `cargo nextest run --workspace --locked` exits 0; new uniqueness test passes
- [ ] Only `executor.rs` modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The `save_cache_result` excerpt doesn't match (drift).
- You cannot determine a safe unique identifier and the atomic-counter approach
  conflicts with an existing counter of the same name.
- Making the copy/rename non-fatal would require changing `copy_cache_source`'s
  signature or a caller outside `save_cache_result` — report instead.

## Maintenance notes

- Plan 006 will re-scope `cache_store_dir` by repo/trust; the staging directory
  lives next to `cache_dir` via `with_file_name`, so it inherits that scoping
  automatically — no rework needed here.
- Plan 009 fixes the broader "step Err aborts the job loop" issue; Step 2 here
  is a targeted fix for the cache-save path specifically and remains correct
  even after plan 009 lands.
- Reviewer: confirm the failure path still cleans up the staging dir (no leak).
