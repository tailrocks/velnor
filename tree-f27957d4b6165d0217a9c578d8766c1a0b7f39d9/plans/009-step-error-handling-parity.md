# Plan 009: Treat step-logic failures as failed steps, not loop-aborting errors

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs crates/velnor-runner/src/checkout.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The step-execution loop treats a step returning `Err` as a fatal error: it
stores the error and **breaks the loop**. But `actions/checkout` and native
action adapters return `Err` on ordinary *step-logic* failures (a bad git ref,
auth failure, transient network error, an artifact copy failure) — not just on
runner-infrastructure faults. So a failed checkout or native action **aborts the
whole job loop** instead of being recorded as a normal failed step. Consequences
vs GitHub: (a) `continue-on-error: true` is ignored (it's only honored on the
`Ok` branch); (b) later `if: always()` / `if: failure()` cleanup/notify steps
never run; (c) no timeline step log is emitted for the failing step, so the UI
shows no error detail. Script (`run:`) steps already do the right thing —
returning `Ok(exit_code != 0)` — so the two failure channels are inconsistent.
This plan converts expected step-logic failures into `Ok(result with nonzero
exit_code)` so the loop records them, honors continue-on-error, evaluates later
conditions, and emits step logs — reserving `Err` for genuine infra faults.

## Current state

- `crates/velnor-runner/src/executor.rs` — the step loop's error handling,
  excerpt at `executor.rs:1122-1160`:
  ```rust
                  if failed && step.continue_on_error() {
                      result.failure_ignored = true;       // <-- continue-on-error handled ONLY in Ok arm
                  }
                  ... // emit step log, state.apply, results.push
              }
              Err(error) => {
                  step_error = Some(error);
                  break;                                    // <-- BUG: any Err aborts the loop
              }
          }
  ```
- The checkout adapter bails on nonzero git exit, excerpt at
  `checkout.rs:525-531`:
  ```rust
  if result.code != 0 {
      bail!("git {} failed with code {}: {}", format_git_args(args), result.code, result.stderr);
  }
  ```
- Native adapters similarly `bail!`/`?` on logic failures (e.g. artifact copy in
  `executor.rs` around 3357-3388; native dispatch around 1563-1569).
- The correct model (script steps) returns a `StepExecutionResult` with a
  nonzero `exit_code`; grep `StepExecutionResult` for the shape. The loop's `Ok`
  arm already computes `failed`, honors `continue_on_error`, applies state, and
  emits the step log.
- There is an existing test proving script-step failure keeps the outcome and
  runs later steps: `continue_on_error_keeps_failure_outcome_but_runs_later_steps`
  at `executor.rs:9444` — model the new checkout/native tests after it.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| Tests     | `cargo nextest run -p velnor-runner continue_on_error --locked` | pass            |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/executor.rs` — convert expected step-logic failures
  from the checkout/native-adapter execution paths into `Ok(nonzero result)`;
  keep `Err` for infra faults; add tests.
- `crates/velnor-runner/src/checkout.rs` — **only** if the cleanest boundary is
  to return a typed step result from checkout rather than `bail!`. Prefer
  adapting at the executor call site if that is less invasive.

**Out of scope**:
- The cache-save `Err` specifically — plan 002 already makes that non-fatal;
  don't double-fix it.
- Panic handling (`JoinError`) — plan 011.
- The distinction of *which* infra errors abort — keep the current infra-abort
  behavior for genuine faults (Docker daemon gone, container start failure).

## Git workflow

- Branch: `advisor/009-step-error-handling-parity`
- Commit: `fix(executor): record checkout/native-action failures as failed steps, not job aborts`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Classify step-logic failures at the execution boundary

Identify where the loop calls the per-step executor for checkout and native
actions (the arms that can return `Err` for a non-infra reason). For a
**step-logic** failure (nonzero git exit, adapter logic failure), produce an
`Ok(StepExecutionResult { exit_code: <nonzero>, ... })` with the failure text in
`stderr`, instead of propagating `Err`. Reserve `Err` for genuine runner-infra
faults (container/daemon problems, missing pipes) so those still abort.

The cleanest implementation is usually at the executor call site: wrap the
checkout/native call, and map its "expected failure" error into a failed
`StepExecutionResult`. Use a discriminator — e.g. checkout's git-exit failure is
expected; a Docker-exec transport error is infra. If the current error type
cannot distinguish these, either (a) have checkout/native return a
`StepExecutionResult` directly for the git-exit case, or (b) STOP and report
that a typed error is needed first (plan 012 introduces one).

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Ensure the failed step flows through the normal `Ok` path

Confirm that a converted failure now: sets `failed`, honors
`step.continue_on_error()` (sets `failure_ignored`), calls `state.apply(...)`
(so `steps.<id>.outcome`/`conclusion` and `success()`/`failure()` reflect it),
and emits a timeline step log. This is automatic if the failure returns through
the `Ok` arm — verify by reading, don't add parallel logic.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 3: Tests

Add tests in `executor.rs` `#[cfg(test)]`, modeled on
`continue_on_error_keeps_failure_outcome_but_runs_later_steps` (`executor.rs:9444`):
- `failed_checkout_runs_always_step`: a checkout step that fails (git nonzero,
  via the scripted `CommandRunner`) is followed by a step with
  `if: always()` — assert the always-step **runs** and the job records the
  checkout as failed (not that the loop aborted).
- `failed_native_action_honors_continue_on_error`: a native action step with
  `continue-on-error: true` that fails does not abort; a later unconditional
  step still runs; the failing step's outcome is failure with
  `failure_ignored == true`.
- Assert a step log **is** emitted for the failing step (no silent
  disappearance).

Use `RecordingRunner`/the scripted-runner pattern already in the test module
(grep `RecordingRunner` / `CommandRunner` in `executor.rs` tests).

**Verify**: `cargo nextest run -p velnor-runner continue_on_error --locked` →
pass; `cargo nextest run --workspace --locked` → all pass.

## Test plan

- The two tests above plus a step-log-emitted assertion.
- Model after `continue_on_error_keeps_failure_outcome_but_runs_later_steps`.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] A failing checkout/native action no longer breaks the step loop; later `always()`/`failure()` steps run
- [ ] `continue-on-error: true` is honored for checkout/native-action failures
- [ ] A timeline step log is emitted for the failed step
- [ ] Genuine infra faults (container/daemon) still abort (unchanged)
- [ ] `cargo nextest run --workspace --locked` exits 0; new tests pass
- [ ] Only `executor.rs` (and `checkout.rs` if required) modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The step-loop excerpt doesn't match (drift).
- The error type cannot distinguish step-logic failures from infra faults, and
  distinguishing them cleanly requires plan 012's typed error — report and
  sequence after 012.
- Converting the failure would swallow a genuine infra fault (you can't tell
  which is which) — report rather than risk masking real crashes.

## Maintenance notes

- Plan 012 (typed GitHub/exec errors) makes the infra-vs-logic discrimination
  robust; if 012 lands first, use its error variants as the discriminator here.
- Reviewer: the key risk is masking a real infrastructure crash as a "failed
  step." Scrutinize the classification: only expected step-logic failures
  (git nonzero exit, adapter validation/logic) become `Ok(nonzero)`; transport
  and container faults still `Err`.
