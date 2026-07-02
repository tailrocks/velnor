# Plan 031 (investigate): Decide and enforce upload-artifact semantics when the remote upload fails

> **Executor instructions**: This is an **investigate-then-fix** plan. Step 1 is
> a decision that needs the intended semantics confirmed; do not change behavior
> until Step 1 concludes. STOP ⇒ report. Update `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: MED
- **Depends on**: 009 (if the decision is to fail the step, route it through the
  failed-step handling)
- **Category**: bug (investigate)
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

`upload-artifact` reports **SUCCESS even when the GitHub Results Service upload
fails** — a failed `upload_artifact_blocking` is logged as "best-effort ... failed"
and swallowed, and the step still returns exit code 0; only the local copy is
required for success. If the artifact never reaches GitHub's store, a downstream
GitHub-hosted `download-artifact` then fails to find it — a "Velnor says SUCCESS,
GitHub would say FAILURE" case that silently breaks cross-host artifact hand-off.
This may be an intentional local-first tradeoff, but it is **not** in the
documented tradeoff list, so the intended semantics must be confirmed before
changing behavior — hence an investigate plan (MED confidence).

## Current state

- `crates/velnor-runner/src/executor.rs` — the swallowed remote-upload failure,
  excerpt at `executor.rs:3416-3442`:
  - A failed `upload_artifact_blocking` is logged "Best-effort ... failed" and
    swallowed; the step returns `exit_code: 0` (only the local copy is required).
  - Read the exact lines to see whether a warning is surfaced in the step log or
    only on daemon stderr.
- Contrast: on a dual-lane setup, a GitHub-hosted job may `download-artifact`
  something a Velnor job `upload-artifact`ed — so the remote copy matters for
  cross-host workflows.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|------------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                        | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0          |
| Tests     | `cargo nextest run -p velnor-runner artifact --locked`         | pass            |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/executor.rs` — the upload-artifact remote-failure
  handling, per the Step 1 decision.
- Docs: if the decision is "local-first is intentional," document it in the
  tradeoff list (`docs/`), so it stops being an undocumented divergence.

**Out of scope**:
- The in-memory zip OOM (that is the separate P3.3 streaming-upload item).
- The upload transport itself.

## Git workflow

- Branch: `advisor/031-upload-artifact-remote-failure`
- Commit: `fix(artifact): surface/propagate remote upload failure per confirmed semantics`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Confirm the intended semantics (DECISION)

Determine whether cross-host artifact hand-off (a GitHub-hosted job downloading a
Velnor-uploaded artifact) is a supported scenario in the estate. Check the fixture
and the three production repos' workflows for `download-artifact` consuming
another job's artifact across lanes. Write the finding + a recommendation in the
PR:
- If cross-host download **is** used → the remote upload failing must **fail the
  step** (GitHub parity). Proceed to Step 2A.
- If artifacts are always consumed same-host (Velnor→Velnor) → local-first is
  acceptable, but the remote failure must at least surface a `::warning::` in the
  step log (not only daemon stderr), and the behavior must be **documented**.
  Proceed to Step 2B.

If you cannot determine the scenario, STOP and report — this is the crux
decision.

**Verify**: the decision + evidence are written down.

### Step 2A: Propagate the failure (if cross-host download is used)

Make a remote-upload failure return a nonzero step result (respecting
`continue-on-error`), routed through plan 009's failed-step handling so it records
correctly and later conditions evaluate. Keep the local copy as-is.

**Verify**: `cargo nextest run -p velnor-runner artifact --locked` → pass.

### Step 2B: Surface a warning + document (if local-first is intentional)

Emit a `::warning::` into the **step log** (visible in the GitHub UI) when the
remote upload fails, instead of only daemon stderr, and add the local-first
behavior to the documented tradeoff list in `docs/`.

**Verify**: a warning appears in the step output on remote-upload failure; the
tradeoff is documented.

### Step 3: Test

Add a test in `executor.rs` `#[cfg(test)]` matching the chosen path: either the
step fails on remote-upload error (2A) or a warning is emitted while the step
succeeds (2B). Use the scripted runner / fake upload to force the failure (grep
`upload_artifact` in tests for the pattern).

**Verify**: `cargo nextest run --workspace --locked` → all pass.

## Test plan

- The chosen-path test (fail-on-remote-error, or warn-and-succeed).
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] The intended semantics are confirmed and recorded (with evidence) in the PR
- [ ] Behavior matches the decision: either remote-upload failure fails the step (2A), or it emits a step-log warning and is documented (2B)
- [ ] `cargo fmt`/`clippy`/`nextest` all green
- [ ] Only `executor.rs` (+ docs if 2B) modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- You cannot determine whether cross-host artifact download is used in the estate
  — STOP and ask; the decision drives everything.
- Failing the step (2A) would break a purely-local workflow that legitimately
  doesn't need the remote copy — report the tension before changing.

## Maintenance notes

- This interacts with the P3.3 streaming-upload work (large artifacts): whichever
  path is chosen, streaming upload must preserve it.
- Reviewer: confirm the decision is evidence-based (fixture/estate usage), not
  assumed.
