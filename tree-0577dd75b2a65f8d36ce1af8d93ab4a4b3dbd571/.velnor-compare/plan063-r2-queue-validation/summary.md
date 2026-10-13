# Plan 063 live proof — control-plane corpus + queue isolation (r2)

Date: 2026-08-24 · Repo: `tailrocks/velnor-actions-fixture` · Trigger: operator-authorized campaign leaf.

## PR stage

- Branch `plan-063-fixture-control-plane` (commit `46f71d3`) had tree identical to
  post-#90 main (`56332b7a`), so PR #91 carried zero file delta; merged anyway per
  operator instruction to land the branch through required checks.
- PR: #91 https://github.com/tailrocks/velnor-actions-fixture/pull/91
- Required gates: `ci-required` pass, `DCO` pass; all applicable checks pass
  (skips = classified non-applicable lanes). Full rollup: pr91.json.
- Merge: squash, merge commit `932d97d9dbcc9ba0bff3e377663a02485279d232`
  (repo allows squash only).
- Post-merge: `.github/workflows/control-plane.yml` present on main; registry id
  341041473, state active (workflow-registry.json).

## Ephemeral queue runner

- actions/runner v2.336.0 (latest at run time), osx-arm64 tarball sha256
  `8e8839c49b7060b6b2154f4931f815df330c27f167d53ef2239ee3dfce28b079`.
- User-space config under `/var/folders/.../opencode/cp-runner`, no sudo,
  `--ephemeral --unattended`, labels: self-hosted, macOS, ARM64,
  `velnor-cp-queue-validation`. Registration ID **5540**, name
  `cp-queue-validation-154447`.
- Baseline fleet before: slots 5538/5539 (`velnor-fixture-slot-1/2`) online.
  Fleet after job: back to exactly the 2 protected slots (runner self-removed).

## Scenario matrix (workflow_dispatch on main, default lane = velnor)

| scenario | run ID | run conclusion | aggregator CP_VERDICT | duration |
|---|---|---|---|---|
| success | 32707922584 | success | match | 27s |
| failure | 32708290245 | failure (controlled) | match | 26s |
| hold | 32708557335 | success | match | 51s |
| queue | 32708401801 | success | match | 30s |
| concurrent | 32707955260 | success | match | 48s |
| artifacts | 32708604967 | success | match | 44s |
| cache | 32708657716 | success | match | 35s |
| load | 32707978393 | success | match | 61s |

Notes:
- failure scenario's `scenario-failure` job fails by design; aggregator exit 0 with
  `CP_VERDICT=match` is the pass signal.
- Queue job executed on runner name `cp-queue-validation-154447`
  (`CP_RUNNER_NAME=` in queue-scenario-job.log) — label affinity to the isolated
  instance proven; no other runner carries the label.
- First dispatch burst fired all 8 simultaneously; 5 pending runs (failure, hold,
  queue, artifacts, cache) were cancelled externally within ~10–25 s of creation
  while slots were saturated (zero jobs materialized). Re-dispatched one-at-a-time:
  every scenario completed normally. Burst-contention artifact, not a corpus defect;
  canceller identity not attributable from available APIs (org audit log 404).

## Evidence files

- `<scenario>-aggregator.log` — one passing representative per scenario (all 8),
  each containing `CP_VERDICT=match`.
- `failure-scenario-job.log` — the failed-step log (controlled failure markers).
- `queue-scenario-job.log` — ephemeral-runner pickup proof.
- `runs.json`, `pr91.json`, `workflow-registry.json` — machine metadata.

Sanitized: secret scan negative (no tokens/PATs/private keys/credentialed URLs);
logs contain runner diagnostics only.

## Cleanup

- No leftover non-completed runs from this session (verified post-matrix).
- Ephemeral runner deregistered (ephemeral auto-remove after its single job);
  registration list reduced to protected slots 5538/5539.
- Local runner dir, tarball, and scratch clones deleted after capture.
