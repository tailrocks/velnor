# Plan 030 (design/spike): Add a regression/watch mode to `velnor-tools lane-compare`

> **Executor instructions**: This is a **design/spike + build** plan for dev
> tooling (not the production runner). Follow step by step; run each verification.
> STOP ⇒ report. Update `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-tools/src/main.rs crates/velnor-tools/src/lane_compare.rs`

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: LOW
- **Depends on**: none
- **Category**: direction
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

`lane-compare` diffs the GitHub-hosted and Velnor lanes of **one run**. The
master-plan wants a scheduled regression canary (P5.2: "scheduled `lanes=both`
canary ... alert on regression — Velnor slower than GitHub on any job class, or a
parity diff") and P4.1 wants the diff as a repeatable, CI-runnable subcommand.
Now that three repos run Velnor-default in production, a Velnor slowdown or parity
drift is only caught by a human eyeballing a run. A `lane-compare` mode that pulls
the last N both-lane runs, compares per-job-class timing + parity against a
baseline, and exits non-zero on regression turns the one-shot tool into a
CI-schedulable guard — closing the P5 "continuously proven" loop.

## Current state (evidence)

- `crates/velnor-tools/src/main.rs:65` — `lane-compare` is documented as diffing
  "one run" via the GitHub API.
- `crates/velnor-tools/src/lane_compare.rs` — the comparator; grep for
  `watch|continuous|regression|alert|interval|baseline` returns nothing (no such
  mode exists).
- The tool already has a GitHub HTTP client and per-run diff logic to reuse
  (`main.rs` `github_http_client`/`fetch_github_file`; `lane_compare.rs`).

## Deliverables

1. A short design note (in the PR or `docs/`) defining: what "regression" means
   (per-job-class wall-time delta beyond a threshold; any parity diff), how the
   baseline is stored (a committed JSON baseline file vs computed from the last N
   runs), and the alert surface (non-zero exit + a report artifact a scheduled
   GitHub-hosted workflow can act on).
2. A new subcommand/flags: `lane-compare --watch --workflow <wf>
   --regress-threshold <pct> [--since <n-runs>]` that pulls recent both-lane runs,
   compares per-job-class timing + parity against the baseline, prints a report,
   and exits non-zero on regression.

## Commands you will need

| Purpose      | Command                                                          | Expected            |
|--------------|------------------------------------------------------------------|---------------------|
| Format       | `cargo fmt --all --check`                                        | exit 0              |
| Lint         | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0              |
| Help         | `cargo run -p velnor-tools -- lane-compare --help`             | shows new flags     |
| Tests        | `cargo nextest run -p velnor-tools lane_compare --locked`      | pass                |

## Scope

**In scope**:
- `crates/velnor-tools/src/main.rs` — new flags on the `lane-compare` subcommand.
- `crates/velnor-tools/src/lane_compare.rs` — the regression comparison + baseline
  logic + report.
- Tests for the regression decision (pure function over timing/parity inputs).

**Out of scope**:
- The scheduled GitHub-hosted workflow that runs this — a follow-up (this plan
  delivers the CLI it will call).
- Changing the runner — this is tooling only.

## Git workflow

- Branch: `advisor/030-lane-compare-regression-mode`
- Commit: `feat(tools): lane-compare regression mode (--watch/--regress-threshold)`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Define the regression decision as a pure function

Implement `fn is_regression(baseline: &LaneStats, current: &LaneStats, threshold_pct:
f64) -> RegressionVerdict` comparing per-job-class Velnor-vs-GitHub wall time and
parity. Keep it pure (inputs = parsed stats) so it is unit-testable without
network. Decide baseline storage in the design note (committed baseline JSON is
simplest and reviewable).

**Verify**: `cargo nextest run -p velnor-tools lane_compare --locked` → the
decision tests pass.

### Step 2: Wire the CLI + report

Add the `--watch`/`--regress-threshold`/`--since`/`--workflow` flags, pull recent
both-lane runs via the existing GitHub client, compute per-job-class stats, run
`is_regression`, print a report, and set a non-zero exit code on regression. Reuse
the existing per-run diff for the parity portion.

**Verify**: `cargo run -p velnor-tools -- lane-compare --help` shows the new
flags; `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 3: Design note

Document the regression definition, baseline strategy, and the intended scheduled
usage (a weekly GitHub-hosted workflow calling this subcommand and paging on a
non-zero exit).

**Verify**: the note exists (PR description or `docs/`).

## Test plan

- `is_regression` tests: within-threshold (no regression), Velnor slower beyond
  threshold (regression), parity diff present (regression), Velnor faster (no
  regression).
- Model after the existing `lane_compare.rs` tests (it has ~9 tests — grep
  `#[test]` there).
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `lane-compare` has `--watch`/`--regress-threshold`/`--since`/`--workflow` flags
- [ ] A pure `is_regression` decision exists and is unit-tested (timing + parity cases)
- [ ] Regression sets a non-zero exit and prints a report
- [ ] A design note defines regression, baseline, and scheduled usage
- [ ] `cargo fmt`/`clippy`/`nextest` all green
- [ ] Only `velnor-tools` files (+ optional docs) modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The GitHub API shape for "recent both-lane runs" is unclear from the existing
  client — report; scope to a baseline-file comparison first.
- Per-job-class timing is not extractable from the data `lane-compare` already
  fetches — report what additional API calls are needed.

## Maintenance notes

- The scheduled workflow that pages on regression is a follow-up; keep the CLI's
  exit-code contract stable for it.
- Reviewer: focus on the `is_regression` thresholds and the parity-diff → failure
  mapping; a too-sensitive threshold pages constantly, a too-loose one misses
  drift.
