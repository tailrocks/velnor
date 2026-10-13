# Plan 011: Never abandon an acquired job — complete it to GitHub on panic and setup errors

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/runner.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

Once `acquire_job` succeeds, GitHub considers this runner the owner of the job;
it must receive a completion call or the job hangs in the UI until the
server-side timeout (hours), surfacing as "runner lost communication." Two paths
abandon an acquired job with no completion:

1. **Panic in job execution.** When the `spawn_blocking(execute_script_job)`
   task returns `Err(JoinError)` (a panic — the 13k-line executor is
   panic-capable via indexing/`unwrap`), the arm aborts renewal and returns
   `Err` **without** calling `complete_run_service_job`. The sibling
   execution-error arm does complete correctly, so the fix is to mirror it.

2. **Post-acquire setup errors.** After `acquire_job` succeeds, parsing the job
   message and building the client use `?`, so any error abandons the already-
   acquired job with no completion.

This plan guarantees an acquired job is always completed to GitHub (as `Failed`
with an infrastructure category) before the runner gives up.

## Current state

- `crates/velnor-runner/src/runner.rs` — the panic arm, excerpt at
  `runner.rs:2278-2286`:
  ```rust
  .await;
  cancellation.abort();
  let job_result = match job_result {
      Ok(job_result) => job_result,
      Err(join_error) => {
          renewal.abort();
          return Err(join_error).context("join Docker job execution task");   // <-- no completion sent
      }
  };
  ```
- The **correct** sibling arm that DOES complete (execution error, not panic),
  excerpt at `runner.rs:2312-2334`:
  ```rust
  Err(error) => {
      if canceled.load(Ordering::SeqCst) {
          ScriptJobResult { result: TaskResult::Canceled, ... }
      } else {
          let infrastructure_failure_category = infrastructure_failure_category(&error).map(ToOwned::to_owned);
          let completion = complete_run_service_job(
              &run_service_job.client, &run_service_job.run_service_url, &job,
              TaskResult::Failed, BTreeMap::new(), Vec::new(), None,
              run_service_job.billing_owner_id.clone(),
              infrastructure_failure_category, true,
          ) ...
      }
  }
  ```
- The post-acquire setup path (parse + client build) that `?`-propagates after
  acquire: grep `acquire_job` in `runner.rs` and read the ~30 lines after the
  successful acquire (around `runner.rs:2130-2145`), where the job value is
  deserialized (`serde_json::from_value::<AgentJobRequestMessage>`) and the
  client is built — both use `?`.
- `complete_run_service_job` signature is visible in the sibling arm above;
  `run_service_job` carries `client`, `run_service_url`, `billing_owner_id`.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| Tests     | `cargo nextest run -p velnor-runner complete --locked`          | pass            |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/runner.rs` — the panic (`JoinError`) arm and the
  post-acquire setup error paths.

**Out of scope**:
- Token refresh during completion — plan 013.
- The executor panics themselves — this plan makes them non-abandoning, it does
  not chase individual `unwrap`s.

## Git workflow

- Branch: `advisor/011-never-abandon-acquired-job`
- Commit: `fix(runner): complete acquired job to GitHub on panic and setup errors`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Complete the job on a panic (`JoinError`)

In the `Err(join_error)` arm (`runner.rs:2282`), before returning, call
`complete_run_service_job(..., TaskResult::Failed, ..., infrastructure_failure_category:
Some("...")/None, true)` exactly as the sibling arm does, using
`run_service_job.client` / `run_service_url` / `billing_owner_id`. Then abort
renewal and return. Guard against double-completion: this arm returns
immediately, so the later completion path is not reached — verify that by
reading the control flow. If a panic message is available from the `JoinError`,
pass an appropriate infra category; otherwise `None`.

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Complete the job on post-acquire setup errors

For the parse/client-build steps that run **after** a successful `acquire_job`,
replace bare `?` with error handling that first completes the acquired job as
`Failed` (infra category) and then returns/continues. Extract the `plan_id` /
`job_id` needed for completion from the raw acquired job value (they are present
in the acquire response even if full deserialization fails — if they are not
reachably present, complete using whatever identifiers `run_service_job` already
holds from the acquire call). If completion itself needs identifiers that are
only obtainable by parsing (and parsing is what failed), STOP and report — the
acquire response likely already carries them; confirm before assuming.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 3: Test the panic path completes

Add a test in `runner.rs` `#[cfg(test)]`. The full slot loop is hard to unit
test (see plan 018 for the harness), so at minimum extract the "on abandon,
build the completion request" logic into a small pure helper and test that a
panic/setup-failure maps to a `TaskResult::Failed` completion with an infra
category. Model assertions after existing completion/classification tests in
`runner.rs`/`protocol.rs` (grep `complete_job` / `TaskResult::Failed` in tests).
If no pure seam is extractable without a broad refactor, add the regression to
plan 018's harness scope and note it here.

**Verify**: `cargo nextest run -p velnor-runner complete --locked` → pass;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- A pure-helper test asserting panic/setup failure → `Failed` completion with
  infra category, OR (if no seam) an entry in plan 018's harness test list.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] The `JoinError` (panic) arm calls `complete_run_service_job(... Failed ...)` before returning
- [ ] Post-acquire parse/client-build errors complete the acquired job before returning
- [ ] No double-completion (each abandon path returns immediately after completing)
- [ ] `cargo nextest run --workspace --locked` exits 0
- [ ] Only `runner.rs` modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The panic arm or sibling completion arm doesn't match the excerpts (drift).
- Completion after a parse failure needs identifiers only obtainable by the
  parse that failed — report (confirm the acquire response carries plan/job id).
- Adding completion risks a double-complete on some path — report the control
  flow rather than guessing.

## Maintenance notes

- Plan 013 makes completion token-refresh-aware; a panic-path completion that
  hits an expired token benefits from that. Sequence 013 after this if both are
  planned.
- Plan 018 (broker/run-service HTTP harness) enables an end-to-end test that a
  panicking job still sends a `Failed` completion — add that case there.
- Reviewer: the invariant is "acquired ⇒ completed exactly once." Check every
  early return between acquire and the normal completion for a missing
  completion call.
