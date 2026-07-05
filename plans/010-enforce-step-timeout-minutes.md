# Plan 010: Enforce `timeout-minutes` so a hung step can't wedge a slot forever

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs crates/velnor-runner/src/job_message.rs crates/velnor-runner/src/script_step.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

`timeout-minutes` is **parsed but never enforced.** The step exec loop blocks on
`child.wait()` with no deadline, and the whole job runs on a `spawn_blocking`
thread. A step that hangs (infinite loop, wedged network call, deadlocked test)
blocks `docker exec` indefinitely: GitHub would kill it at `timeout-minutes`
(default job cap 360 minutes) and mark the job failed, but Velnor holds the slot
**forever**, never completing or reporting, silently shrinking fleet capacity.
The systemd `TimeoutStopSec` is 3h (`10800s`) — matching "the estate's largest
timeout-minutes" — which only bounds *drain*, not a live hang. This plan enforces
per-step and job-level timeouts by racing the blocking exec against a deadline
and killing the container/exec on expiry, mapping a timeout to a failed step.

## Current state

- `crates/velnor-runner/src/job_message.rs` — the field is deserialized and then
  unused. Excerpt at `job_message.rs:222-223`:
  ```rust
  #[serde(default, rename = "TimeoutInMinutes", alias = "timeoutInMinutes")]
  pub timeout_in_minutes: Option<Value>,
  ```
  `rg "timeout_in_minutes|TimeoutInMinutes|timeout-minutes" crates/velnor-runner/src`
  shows **only** these two lines — no reader anywhere.
- Step-level `timeout-minutes` is **not parsed at all** in `script_step.rs`
  (the `ScriptStep` struct has no timeout field — confirm by reading it).
- The blocking exec, excerpt at `executor.rs:157-179`:
  ```rust
  for (stream, line) in receiver {   // drains stdout/stderr lines
      ...
      on_output(stream, &line);
  }
  let status = child.wait()          // <-- blocks with no deadline
      .with_context(|| format!("wait for {program} {}", args.join(" ")))?;
  ```
  Here `child` is a `std::process::Child` (the `docker exec` process). The job
  runs inside `tokio::task::spawn_blocking` (`runner.rs:2261`).
- GitHub semantics: a step exceeding `timeout-minutes` is **cancelled/killed**
  and the step conclusion is `failure` (the job then fails unless
  continue-on-error). The default job `timeout-minutes` cap is 360.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| Tests     | `cargo nextest run -p velnor-runner timeout --locked`           | new tests pass  |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/job_message.rs` — expose the parsed job timeout to
  the executor (it's already deserialized).
- `crates/velnor-runner/src/script_step.rs` — add a `timeout_minutes` field to
  `ScriptStep` and populate it from the step definition (find where `ScriptStep`
  is constructed from the job message; the step-level `TimeoutInMinutes` lives on
  the same step struct as `condition`/`continue_on_error` — `job_message.rs:218-223`).
- `crates/velnor-runner/src/executor.rs` — enforce the deadline around the
  blocking exec and kill on expiry; map timeout → failed step result.

**Out of scope**:
- Job-level wall-clock cancellation from the UI — that's the cancellation path
  (separate). This plan enforces the workflow-declared `timeout-minutes`.
- Changing the default job cap behavior beyond applying 360 when unspecified.

## Git workflow

- Branch: `advisor/010-enforce-step-timeout-minutes`
- Commit: `fix(executor): enforce step and job timeout-minutes with container kill`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Parse the effective per-step timeout

Add `timeout_minutes: Option<u64>` to `ScriptStep` (`script_step.rs`) and to any
other executable step type that should honor it (checkout/native run in the same
loop). Populate it from the step's `TimeoutInMinutes` when the plan/steps are
built from the job message. Convert the `Option<Value>` (which may be a number
or an expression string) using the same numeric-coercion the codebase uses for
`continue_on_error` (grep how `continue_on_error: Option<Value>` is coerced to a
bool — mirror that for a number). Compute the **effective** timeout for a step
as: step `timeout-minutes` if set, else the job `timeout_in_minutes`, else the
default 360.

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Enforce the deadline around the blocking exec

Wrap the `child.wait()` region so the exec is bounded by the effective timeout.
Because this runs on a blocking thread with a `std::process::Child`, implement a
watchdog: spawn a thread that, after the deadline, kills the exec — kill the
`child` **and** issue a `docker kill`/`docker stop` on the exec's process inside
the container if `child.kill()` alone doesn't stop the in-container process
(a `docker exec` child dying does not always stop the in-container process; the
robust kill is to stop/kill the container or the exec). Prefer the mechanism the
cancellation path already uses to kill a running container (grep for how the
cancellation flag kills the container — `runner.rs` around 2695 kills the
container; reuse that helper if it is reachable, or replicate a `docker kill`).
On timeout, return a `StepExecutionResult` with a nonzero exit code and a
message like `##[error]The operation was canceled.` / timeout note in stderr,
so the step is recorded as failed (this composes with plan 009's failed-step
handling).

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 3: Tests

Add tests in `executor.rs` `#[cfg(test)]`:
- `effective_timeout_prefers_step_over_job_over_default`: a pure-function test
  of the effective-timeout computation (step set → step; step unset, job set →
  job; both unset → 360).
- `timed_out_step_returns_failure_not_hang`: using the scripted `CommandRunner`
  (or a fake exec that sleeps), a step with a very small timeout returns a
  failed `StepExecutionResult` rather than blocking. If the test harness cannot
  simulate a hang cheaply (real sleep = slow test), make the effective-timeout
  helper and the "map timeout to failed result" mapping pure and test those
  directly, and note in the PR that end-to-end kill is covered by the live
  fixture, not a unit test.

**Verify**: `cargo nextest run -p velnor-runner timeout --locked` → new tests
pass; `cargo nextest run --workspace --locked` → all pass.

## Test plan

- Effective-timeout selection (pure function).
- Timeout → failed step mapping.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] `ScriptStep` (and sibling step types in the loop) carry an effective timeout; job default 360 applied when unset
- [ ] A step exceeding its timeout is killed (container/exec) and recorded as a failed step, not an infinite `child.wait()`
- [ ] `rg "timeout_in_minutes" crates/velnor-runner/src` shows the field is now **read**, not just parsed
- [ ] `cargo nextest run --workspace --locked` exits 0; new tests pass
- [ ] Only the three in-scope files modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- Any excerpt doesn't match (drift).
- `child.kill()` does not stop the in-container process and no reusable
  container-kill helper exists — report; killing the container is the correct
  answer but must reuse the cancellation path's mechanism, not a new ad-hoc one.
- The `Option<Value>` timeout can be an expression that needs full expression
  evaluation to resolve — report; scope this plan to numeric literals and defer
  expression-valued timeouts.

## Maintenance notes

- Composes with plan 009: the timeout returns a failed `StepExecutionResult`,
  which plan 009's handling routes through the normal loop (honoring
  continue-on-error and later conditions).
- Composes with plan 011: a timeout must still let the job **complete** to
  GitHub (as failed), not abandon it.
- Reviewer: verify the watchdog thread is always joined/cleaned up (no thread
  leak per step) and that a fast-finishing step cancels its watchdog.
