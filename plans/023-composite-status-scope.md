# Plan 023: Scope `success()`/`failure()` to the composite action's own steps

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

Inside a composite action, `if: success()` / `if: failure()` should reflect only
the composite's **own** steps, matching GitHub. Velnor evaluates them against the
**whole job's** conclusions: `with_step_action` clones the single shared
`outcomes`/`conclusions` maps, and `success()`/`failure()` scan all conclusions.
So a job-level step that failed **before** a composite runs makes `success()`
false / `failure()` true for steps *inside* the composite, causing them to skip
or run incorrectly — a parity divergence for any composite action that uses
status functions in its internal step conditions.

## Current state

- `crates/velnor-runner/src/executor.rs`:
  - `with_step_action` clones the shared conclusion maps, excerpt at
    `executor.rs:4531-4535`:
    ```rust
    outputs: self.outputs.clone(),
    action_states: self.action_states.clone(),
    outcomes: self.outcomes.clone(),
    conclusions: self.conclusions.clone(),   // <-- whole-job conclusions, not composite-scoped
    ```
  - `success()`/`failure()` scan all conclusions, excerpt at
    `executor.rs:4955-4965`:
    ```rust
    if expression == "success()" {
        return !self.conclusions.values().any(|o| *o == StepOutcome::Failure);
    }
    if expression == "failure()" {
        return self.conclusions.values().any(|o| *o == StepOutcome::Failure);
    }
    ```
  - A `composite_stack` field already exists and is cloned/tracked
    (`executor.rs:4535` clones it; grep `composite_stack` to see how frames are
    pushed/popped around composite execution).
- GitHub semantics: inside a composite, `success()` is true iff none of the
  composite's own prior steps failed; the job-level status does not leak in.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|------------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                        | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0          |
| Tests     | `cargo nextest run -p velnor-runner composite --locked`         | new tests pass  |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/executor.rs` — introduce a conclusion scope for the
  active composite frame and make `success()`/`failure()` consult it; add tests.

**Out of scope**:
- `always()`/`cancelled()` — `always()` is unconditional; `cancelled()` is plan
  031-adjacent (job-level). Do not change them here.
- Step outputs scoping — unrelated.

## Git workflow

- Branch: `advisor/023-composite-status-scope`
- Commit: `fix(executor): scope success()/failure() to the composite's own steps`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Give each composite frame its own conclusion set

Using the existing `composite_stack`, track conclusions recorded **within** the
active composite frame separately from job-level conclusions. When a composite
frame is active, `success()`/`failure()` must consult only that frame's
conclusions; at job level (no active frame) they consult the job conclusions as
today. Prefer a per-frame `conclusions` collection on the frame struct populated
by `state.apply` while the frame is active, rather than mutating the global map's
meaning.

Read how `state.apply` records conclusions and how frames are entered/exited
(grep `composite_stack`, `with_step_action`, `frame`), and thread the scope so
that evaluation inside a frame sees only that frame.

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Tests

Add `composite_status_ignores_prior_job_failure` in `executor.rs` `#[cfg(test)]`:
construct a job where a step **fails**, then a composite action runs whose first
inner step succeeds and whose second inner step has `if: success()`. Assert the
`if: success()` inner step **runs** (composite-internal success is true despite
the prior job-level failure). Add the mirror for `failure()`: an inner
`if: failure()` step does **not** run when the composite's own steps have all
succeeded, even though a job-level step failed. Model construction on the
existing composite tests (grep `composite` in the test module, e.g.
`target_rust_cache_receives_runtime_env_and_posts_on_failure` at
`executor.rs:12351`).

**Verify**: `cargo nextest run -p velnor-runner composite --locked` → pass;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- Composite-internal `success()` ignores a prior job failure; composite-internal
  `failure()` reflects only the composite's steps.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] Inside a composite, `success()`/`failure()` consult only the composite frame's conclusions
- [ ] At job level, behavior is unchanged
- [ ] `cargo nextest run --workspace --locked` exits 0; new tests pass
- [ ] Only `executor.rs` modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The excerpts don't match (drift).
- Scoping conclusions per frame would require changing `state.apply`'s contract
  in a way that affects non-composite paths — report the surface before
  proceeding.

## Maintenance notes

- Nested composites: ensure the scope is per-frame (a stack), so a composite
  inside a composite sees only its own steps. Test one level; note deeper nesting
  as a follow-up if the frame model only supports one level.
- Reviewer: confirm job-level `success()`/`failure()` is byte-for-byte unchanged
  (regression risk is leaking the new scoping to the job level).
