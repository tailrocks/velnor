# A3 streak record — three consecutive FULL green bootstrap main runs

Owner: A3 owner. Authority: `plans/bastion-three-provider-ci/work-plan.md` STEP A3 +
`checklist.md` A3 line + `spec.md` §2 (all read from `origin/main`, post-#912).
Main: `58a94b12d0139c303ddf1dfdd57a36260c39fa02` (merge PR #912).
Baseline shape: `/tmp/m3-watch.md` (run 35240541658 @231253b3).
Out-of-scope proof: `/tmp/main-fail-diag.md` FAILURE 2 + A3-scope verdict.

## 1. BOOTSTRAP SCOPE DECLARATION (fixed before execution, per spec §2 "expected result set is fixed before execution")

Label: **bootstrap** (github-hosted recovery/bootstrap set per §0.6 — never triple-qualified).

### 1a. IN-SCOPE expected results — every one must be present and green (success), else the run fails A3

Workflow: `CI / Main` (`ci-main.yml`) on branch `main`, FULL scope only
(push runs = planner FULL with 17 SELECTED_UNITS; dispatch runs = explicit `scope=full`):

| # | Expected result (spec §2 identity: unit_id + provider) | CI job |
|---|---|---|
| 1 | Control / Planning | `Control / Planning` |
| 2 | Policy | `Policy` |
| 3–19 | 17 × github-hosted unit legs: `bun-velnor`, `docker`, `docs`, `opentofu`, `rust-policy`, `rust-production-topology`, `rust-unit-collector`, `rust-velnor-bench`, `rust-velnor-client`, `rust-velnor-control`, `rust-velnor-model`, `rust-velnor-render`, `rust-velnor-runner`, `rust-velnor-tools`, `rust-velnor-workflow`, `rust-velnor-workflow-contract`, `rust-velnorctl` — each `unit / github-hosted` | `… / GitHub …` legs |
| 20 | DCO (PR-level required check on the merged head) | DCO app (`dco-2`), `completed/success` |

Result identity per spec §2: `repository_id + source_sha + run_id + run_attempt + plan_digest
+ unit_id + provider + platform + command/profile/features/fixture_digest` — recorded per run below.

### 1b. EXCLUDED (labeled, with evidence — never silently skipped-and-green per §0.6)

- **Preview workflow incl. `Guest payload` — OUT OF SCOPE.** Proven in `/tmp/main-fail-diag.md`
  FAILURE 2 + A3-scope verdict: A3 gates strictly on the ci-main workflow's unit×provider
  expected-result set; `Preview`/`guest-payload` appears nowhere in the A3 step text, the A3
  checklist acceptance line, spec §2, or `evidence.md`. (Main now also carries fix `6be652b5`.)
- **velnor legs + dependent rollups — OUT, pre-bastion environmental.** `bun/docker/docs/opentofu/velnor`,
  `Control / Prepare Cargo`, and rollups `ci-required` + `Control / Required` (pure mirrors of the
  admission red) are excluded while the bastion has no Velnor execution capacity. They RUN (never
  skipped) and fail closed with the admission signature: rejection before workflow execution,
  zero declared workflow commands executed (baseline text `/tmp/m3-watch.md` §3; exact log text
  re-cited per streak run since `915ed072` now names the failing check). Structural skips
  (cross-matrix non-selected legs + 13 `Rust · velnor` prepare-cargo guard-cascade skips) are by
  design, not executed-set skips (`/tmp/m3-watch.md` §3).
- **Runtime-products / Nightly / Maintenance / release workflows** — not ci-main; not in the A3 set.

### 1c. Quoted lines satisfied

- Work-plan A3.1: "Declare the bootstrap provider scope explicitly in the ledger (per §0.6:
  hosted recovery/bootstrap set or existing healthy capacity — labeled bootstrap, never
  triple-qualified)." → §1 above; this file is the ledger record.
- Work-plan A3.2: "Obtain three consecutive FULL green `main` runs with zero manual reruns and
  zero hidden source failures: `gh run list --repo tailrocks/velnor --branch main --workflow
  <ci-main> --limit 5`, then `gh run view <id> --repo tailrocks/velnor` per run including
  attempts." → §2; consecutive by completion time, all `run_attempt: 1`.
- Work-plan A3.3: "For each run record: run URL, source SHA, plan digest, complete expected-result
  set per spec §2 identity, per-unit outcomes, and per-job timings. Every expected result is
  present and green; skipped/cancelled/missing entries fail the gate." → per-run records in §2.
- Work-plan A3.4: "Keep real required checks (including DCO) enforced; no protection bypass."
  → DCO `completed/success` (app `dco-2`) on PR #912 head `b3ff4626`; `main` branch protection
  reads 404 (unprotected) so there is nothing to bypass; A3 merges nothing.
- Checklist A3: "Done when: three consecutive FULL green `main` runs, zero manual reruns, zero
  hidden source failures, bootstrap provider scope explicitly declared (labeled bootstrap, never
  triple-qualified); each run has URL, source SHA, plan digest, complete expected-result set,
  per-job timings; required checks (incl. DCO) enforced, no protection bypass." → this file.
- Spec §2: "Result identity is defined as: `repository_id + source_sha + run_id + run_attempt +
  plan_digest + unit_id + provider + platform + command/profile/features/fixture_digest`" and
  "The expected result set is fixed before execution." → §1a fixed before completions; per-run
  identity values in §2.
- Spec §0.6: "An expected local job is never silently skipped and called green; bootstrap
  qualification is reported separately." → velnor legs run-and-fail environmentally, recorded
  separately as out-of-scope observations in each per-run record.

### 1d. Streak boundaries

- Streak-breaker (pre-streak, in-scope failure — streak starts after it): run `35237563515`
  (@`ec399527`, push): `Policy` FAILURE + all unit legs skipped + `ci-required`/`Required` red.
- Run `35240541658` (@`231253b3`) is the green-shape BASELINE (`/tmp/m3-watch.md`), not a streak
  member: the streak is three NEW runs on current main `58a94b12` (post-#912 tree).
- Legitimate triggers only: the #912-merge push run + `workflow_dispatch` with `scope=full`
  (`ci-main.yml` `on.workflow_dispatch` exists with `scope` default `full`; dispatch is an
  explicitly allowed trigger). No empty/test commits, no reruns.

## 2. PER-RUN RECORDS

(consecutive by completion time; no intervening in-scope failure; all `run_attempt: 1`)

### Run 1 — IN-SCOPE GREEN (push from real campaign merge #912)

- URL: https://github.com/tailrocks/velnor/actions/runs/35242831003
- Head SHA: `58a94b12d0139c303ddf1dfdd57a36260c39fa02` — Event: `push` — Attempt: 1
- Created 2026-09-17T15:50:24Z, completed 2026-09-17T15:59:47Z (9m23s). Run-level conclusion:
  `failure` — red confined to the §1b-excluded velnor-admission set (proven below).
- Plan digest: `e4e3afe95955da1d` (Planning log `plan_digest=...`); Planning log `scope=full`;
  SELECTED_UNITS = 17 units × [github-hosted, velnor] (same digest as baseline).
- In-scope expected set (§1a) — 20/20 present and green:
  - `Control / Planning`: success (15:50:28–15:50:40Z)
  - `Policy`: success (15:50:28–15:50:47Z); runtime fast-path line
    `pin ec3995277f82473777f18969e58ea76f63e54cfd shares the base closure and renders the
    tree`, `Generated files are current`, zero runtime `falling through to the candidate path`
  - 17/17 github-hosted legs success (all `… / GitHub · hosted`): bun-velnor 15:50:49–15:51:21,
    docker 15:50:49–15:59:17, docs 15:50:50–15:51:11, opentofu 15:50:50–15:51:06,
    rust-policy 15:50:50–15:51:17, rust-production-topology 15:50:50–15:51:47,
    rust-unit-collector 15:50:50–15:51:30, rust-velnor-bench 15:50:49–15:51:40,
    rust-velnor-client 15:50:50–15:51:31, rust-velnor-control 15:50:50–15:51:26,
    rust-velnor-model 15:50:50–15:51:29, rust-velnor-render 15:50:50–15:51:32,
    rust-velnor-runner 15:50:50–15:59:35, rust-velnor-tools 15:50:50–15:51:45,
    rust-velnor-workflow 15:50:50–15:54:40, rust-velnor-workflow-contract 15:50:49–15:51:17,
    rust-velnorctl 15:50:50–15:52:02
  - DCO: `success` (app `dco-2`) on PR #912 head `b3ff4626`; merge commit carries no DCO
    check-run (DCO is PR-scoped); `main` unprotected (404) → no bypass possible, A3 merged nothing
- Out-of-scope (§1b) observation: 5 velnor jobs (bun, docker·trusted, docs, opentofu,
  prepare-cargo) each failed with the admission signature (`Velnor rejected this job before
  workflow execution` / `phase: operational_store` / `reason: operational store rejected the
  sanitized admission row` / `effect: no declared workflow command was executed`; sole step
  `Velnor rejected job (operational_store)=failure`, zero declared commands executed);
  `ci-required` single runtime verdict `expected CI job velnor-bun-velnor did not pass:
  failure`, zero runtime `was skipped`; `Control / Required` pure mirror (`exit 1`).
  49 structural skips (cross-matrix non-selection + 13 guard-cascade), zero executed-set skips.
- Job-set diff vs baseline run 35240541658 (`/tmp/m3-watch.md`): EMPTY (75/75 identical
  conclusions per job name; m3-watch's "73/47" was a 2-job miscount of Prepare-Cargo
  cross-matrix skips — shape identical).

### Run 2 — IN-SCOPE GREEN (workflow_dispatch scope=full)

- URL: https://github.com/tailrocks/velnor/actions/runs/35243228417
- Head SHA: `58a94b12d0139c303ddf1dfdd57a36260c39fa02` — Event: `workflow_dispatch`
  (explicit `-f scope=full`) — Attempt: 1
- Created 2026-09-17T15:54:11Z, completed 2026-09-17T16:02:30Z (8m19s). Run-level conclusion:
  `failure` — red confined to the §1b-excluded velnor-admission set (proven below).
- Plan digest: `e4e3afe95955da1d` (Planning log `plan_digest=...`).
- In-scope expected set (§1a) — 20/20 present and green:
  - `Control / Planning`: success (15:54:17–15:54:27Z); `Policy`: success (15:54:17–15:54:42Z)
  - 17/17 github-hosted legs success: bun-velnor 15:54:47–15:55:14, docker 15:54:45–16:02:17,
    docs 15:54:45–15:54:58, opentofu 15:54:46–15:55:00, rust-policy 15:54:51–15:55:27,
    rust-production-topology 15:54:46–15:55:53, rust-unit-collector 15:54:45–15:55:24,
    rust-velnor-bench 15:54:46–15:56:30, rust-velnor-client 15:54:47–15:55:27,
    rust-velnor-control 15:54:45–15:55:41, rust-velnor-model 15:54:45–15:55:14,
    rust-velnor-render 15:54:45–15:55:25, rust-velnor-runner 15:54:46–16:02:09,
    rust-velnor-tools 15:54:46–15:55:32, rust-velnor-workflow 15:54:47–15:57:50,
    rust-velnor-workflow-contract 15:54:46–15:55:21, rust-velnorctl 15:54:46–15:57:00
  - DCO: same head as Run 1 → `success` (app `dco-2`) on `b3ff4626`; no bypass, nothing merged
- Out-of-scope (§1b) observation: identical to Run 1 — 5/5 velnor jobs admission signature
  (both marker lines grepped per job log; sole step `Velnor rejected job
  (operational_store)=failure`); `ci-required` single verdict `velnor-bun-velnor did not pass:
  failure` (16:02:21Z), zero runtime `was skipped`; 49 structural skips.
- Job-set diff vs baseline run 35240541658: EMPTY (IDENTICAL).

### Run 3 — IN-SCOPE GREEN (workflow_dispatch scope=full)

- URL: https://github.com/tailrocks/velnor/actions/runs/35243257869
- Head SHA: `58a94b12d0139c303ddf1dfdd57a36260c39fa02` — Event: `workflow_dispatch`
  (explicit `-f scope=full`) — Attempt: 1
- Created 2026-09-17T15:54:29Z, completed 2026-09-17T16:04:29Z (10m0s). Run-level conclusion:
  `failure` — red confined to the §1b-excluded velnor-admission set (proven below).
- Plan digest: `e4e3afe95955da1d` (Planning log `plan_digest=...`).
- In-scope expected set (§1a) — 20/20 present and green:
  - `Control / Planning`: success (15:54:34–15:54:45Z); `Policy`: success (15:54:35–15:54:57Z)
  - 17/17 github-hosted legs success: bun-velnor 15:55:01–15:55:30, docker 15:55:16–16:01:47,
    docs 15:55:00–15:55:14, opentofu 15:55:16–15:55:32, rust-policy 15:55:55–15:56:21,
    rust-production-topology 15:55:16–15:56:40, rust-unit-collector 15:55:32–15:56:12,
    rust-velnor-bench 15:55:34–15:56:35, rust-velnor-client 15:55:55–15:56:21,
    rust-velnor-control 15:55:34–15:56:21, rust-velnor-model 15:55:29–15:56:15,
    rust-velnor-render 15:55:26–15:55:53, rust-velnor-runner 15:55:29–16:04:16,
    rust-velnor-tools 15:55:27–15:56:35, rust-velnor-workflow 15:55:42–15:59:37,
    rust-velnor-workflow-contract 15:55:23–15:56:00, rust-velnorctl 15:55:02–15:56:30
  - DCO: same head as Run 1 → `success` (app `dco-2`) on `b3ff4626`; no bypass, nothing merged
- Out-of-scope (§1b) observation: identical to Run 1 — 5/5 velnor jobs admission signature
  (both marker lines grepped per job log; sole step `Velnor rejected job
  (operational_store)=failure`); `ci-required` single verdict `velnor-bun-velnor did not pass:
  failure` (16:04:20Z), zero runtime `was skipped`; 49 structural skips.
- Job-set diff vs baseline run 35240541658: EMPTY (IDENTICAL).

### Streak consecutiveness proof

- Completion order: Run 1 (15:59:47Z) → Run 2 (16:02:30Z) → Run 3 (16:04:29Z).
- `gh run list --branch main --workflow ci-main.yml` shows no other ci-main main-branch run
  completing between them (next-older is baseline 35240541658 @15:29Z; next-newer: none).
  The pre-streak in-scope failure 35237563515 (Policy red @ec399527) predates all three.
- Reruns: all three `run_attempt: 1`; zero failed-job reruns, zero manual reruns.
- Triggers: 1 × push from real campaign merge (#912) + 2 × workflow_dispatch `scope=full`
  (workflow defines `on.workflow_dispatch`; explicitly legitimate per task). No empty/test commits.

## 3. VERDICT — A3-PASS

Three consecutive FULL green bootstrap-main runs within the declared §1 scope (20/20 in-scope
results green per run: Planning + Policy + 17 github-hosted legs + DCO), zero reruns
(all `run_attempt: 1`), zero hidden source failures (all red confined to the labeled
out-of-scope velnor-admission set with per-job log proof; Preview/rollups excluded per
evidence). Evidence complete: URLs, SHAs (all `58a94b12`), plan digests (all
`e4e3afe95955da1d`), per-unit outcomes, per-job timings, DCO state — all in §2 above.
Checklist A3 acceptance satisfied; no fixes needed; A3 merged nothing.
