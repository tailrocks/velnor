# Plan 063 r3 hardening — post-merge proof

PR #94 (squash-merged): `e7af34d74c67b63bb18d32984739e1efd680da11`
Branch: `plan-063-review-hardening` → `main`, tailrocks/velnor-actions-fixture.

## Changes shipped
1. `control-plane.yml`: new hosted `queue-guard` watchdog (ubuntu-26.04,
   `timeout-minutes: 5`, `actions: write`) bounds the otherwise unbounded QUEUED
   wait of `scenario-queue`: polls up to 210s for the job to leave `queued`;
   on bound emits `::error::CP_MARKER ... reason=queue-still-queued`,
   cancels the run, exits 1. `scenario-queue` timeout reduced 30 → 10 minutes.
2. Aggregator failure-scenario acceptance now log-based: downloads each
   `scenario-failure` job log and requires exactly one `::error::CP_MARKER`
   line, carrying `phase=controlled-failure ... expected=true`, plus the
   terminal notice.
3. `audit_workflow_surface.py`: structural YAML-key parsing for job-level
   `timeout-minutes` and block/flow-style `concurrency.group` (replaces
   comment-prone substring checks).
4. README: ephemeral-runner prerequisite + guarded fail-fast + log-only
   acceptance documented.

## Gate results (local)
- `mise run check` green both pre-rebase and post-rebase (#93 conflict):
  actionlint ok; workflow-surface audit ok; l2-closure valid=3;
  l2-test 7/7 passed. Audit helper negative/positive sanity checks passed
  (comment fakes rejected, real keys accepted).
- PR checks: 10 passed / 0 failed; `ci-required` SUCCESS, `DCO` SUCCESS.

## Post-merge dispatch proof — scenario=queue, NO runners registered
- Run ID: **32712041469** (workflow_dispatch on main, lanes=velnor default)
  https://github.com/tailrocks/velnor-actions-fixture/actions/runs/32712041469
- Pre-dispatch cleanup confirmed: zero non-completed control-plane runs; zero
  runners carrying `velnor-cp-queue-validation`.
- Exact observed behavior (guarded fail-fast, bounded ~4.5 min wall):
  - `validate-inputs`: success.
  - `queue-guard`: started 09:31:58Z, polled job status every 15s (observed
    `absent` — scenario-queue never created a runner assignment), hit the 210s
    bound at 09:35:34Z, emitted
    `##[error]CP_MARKER scenario=queue phase=mismatch reason=queue-still-queued prerequisite=one-dedicated-velnor-cp-queue-validation-runner`,
    echoed `CP_MISMATCH reason=queue-still-queued`, POSTed run cancellation,
    exited 1 → job cancelled by the run cancel it requested.
  - `scenario-queue`: stayed queued the whole window → `cancelled`. Never ran.
  - All other scenario jobs: skipped as designed.
  - `aggregator` (`if: always()`): reported
    `CP_OBSERVED scenario-queue=cancelled`, failed with
    `CP_MISMATCH job=scenario-queue observed=cancelled expected=success`, exit 1.
  - Run conclusion: **cancelled** (guard-requested), not hung. Zero unbounded
    queued state; zero leftover non-completed runs after completion.

## Files (sanitized; no HTML captured)
- `run-metadata.json` — run id/status/conclusion/SHAs
- `jobs.json` — per-job status/conclusion/timestamps
- `queue-guard.log` — full guard job log (ANSI stripped; GH_TOKEN redacted to *** by GitHub)
- `aggregator.log` — full aggregator log (ANSI stripped; GH_TOKEN redacted to *** by GitHub)
