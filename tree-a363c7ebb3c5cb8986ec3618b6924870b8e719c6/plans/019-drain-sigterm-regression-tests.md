# Plan 019: Regression-test the drain / SIGTERM lifecycle (the upgrade-kills-jobs incident)

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done. **Tests** plan.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/runner.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW
- **Depends on**: none
- **Category**: tests
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The fix for the worst named incident — an apt upgrade's unit restart killing 7
in-flight jobs — is a graceful-drain flag that makes busy slots finish their job
before exiting and idle slots exit at the next poll boundary. That fix has **zero
regression coverage**: the only writer of the drain flag is the signal handler,
and no test sets it. If someone inverts a `!draining()` guard, drops a drain
branch, or changes the idle-exit logic, in-flight jobs get killed on the next
daemon restart/upgrade and CI stays green. This plan makes the drain decision
testable and pins its behavior.

## Current state

- `crates/velnor-runner/src/runner.rs`:
  - `static DRAINING: AtomicBool` at `runner.rs:513`; the incident is documented
    inline around `runner.rs:508-513`.
  - `draining()` reads it; it gates lifecycle branches at approximately
    `runner.rs:717, 757, 885, 950, 1572` (idle-slot deregister-and-exit vs
    busy-slot finish-first). Read each to see the exact decision.
  - The only writer is the SIGTERM/SIGINT handler (`runner.rs:540`,
    `DRAINING.store(true, ...)`).
- `nextest` runs each test in its own process, so a global `AtomicBool` is safe
  to drive in a test without cross-test interference (confirm the repo uses
  nextest — it does, per `ci.yml`).
- Existing pure-decision tests to model after: the `idle_health_*` /
  `supervised_retry_delay_*` tests in `runner.rs` `#[cfg(test)]` already model
  per-poll decisions as pure functions.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|------------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                        | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0          |
| Tests     | `cargo nextest run -p velnor-runner drain --locked`             | new tests pass  |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/runner.rs` — extract the drain decision into a pure
  helper (preferred) and unit-test it; or add a `#[cfg(test)]` setter for the
  global and assert the branch outcomes.

**Out of scope**:
- Changing drain behavior — this plan only pins existing behavior.
- End-to-end signal delivery — cannot be unit-tested reliably; covered by the
  live fixture.

## Git workflow

- Branch: `advisor/019-drain-sigterm-regression-tests`
- Commit: `test(runner): pin graceful-drain slot decisions (idle exits, busy finishes)`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Extract the drain decision into a pure function

Introduce a helper such as:

```rust
enum SlotAction { Continue, DeregisterAndExit, FinishJobThenExit }
fn slot_action_on_poll(draining: bool, busy: bool) -> SlotAction { ... }
```

Replace the inline `draining()` checks at the gated sites with calls to this
helper (behavior-preserving — read each current branch and encode it exactly).
If some sites have additional conditions beyond `draining`/`busy`, keep those at
the call site and only extract the drain/busy decision. If extraction would
change control flow in a way you cannot prove equivalent, fall back to Step 1b.

**Step 1b (fallback)**: add a `#[cfg(test)] fn set_draining(v: bool)` that writes
the global, and test the real branch outcomes directly.

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Unit-test the truth table

Add `drain_slot_decisions` in `runner.rs` `#[cfg(test)]`, modeled on the
`idle_health_*` tests:
- `slot_action_on_poll(false, _)` ⇒ `Continue` (not draining: keep working).
- `slot_action_on_poll(true, false)` ⇒ `DeregisterAndExit` (draining + idle).
- `slot_action_on_poll(true, true)` ⇒ `FinishJobThenExit` (draining + busy: the
  incident fix — a busy slot must NOT be killed).

If you used Step 1b, assert the equivalent outcomes by setting the global and
checking the branch each gated site takes.

**Verify**: `cargo nextest run -p velnor-runner drain --locked` → passes;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- The three-case truth table above (the load-bearing case is draining+busy ⇒
  finish, not kill).
- Model after `idle_health_*` pure-decision tests in `runner.rs`.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] The drain/busy decision is testable (pure helper or `#[cfg(test)]` setter) with behavior unchanged
- [ ] Tests assert: not-draining ⇒ continue; draining+idle ⇒ deregister+exit; draining+busy ⇒ finish-then-exit
- [ ] `cargo nextest run --workspace --locked` exits 0; new tests pass
- [ ] Only `runner.rs` modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The drain flag / gated sites don't match the excerpt (drift).
- Extracting the helper would change control flow in a way you can't prove
  equivalent — use the Step 1b fallback instead of risking a behavior change.

## Maintenance notes

- If plan 018's harness lands, add an end-to-end drain test there (a busy slot
  receiving SIGTERM completes its job before exiting).
- Reviewer: the extraction must be provably behavior-preserving; diff each gated
  site against its original branch.
