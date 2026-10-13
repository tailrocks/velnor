# Plan 058: Phase 4 — estate-wide enforcement, docs reconcile, required checks

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
> This plan runs LAST — after all estate plans (047–057) report DONE.
>
> **Drift check (run first)**: `git log --oneline -1` in the velnor repo —
> planned at `48b04ad`. This plan is mostly cross-repo verification; drift in
> velnor docs is expected and handled in step 3.

## Status

- **Priority**: P2 (Phase 4 of VELNOR_PROJECTS_SETUP.md §8)
- **Effort**: M
- **Risk**: LOW (verification + docs); required-checks flip is operator-gated
- **Depends on**: plans/046 (audit-ci), plans/047–057 (all estate PRs merged)
- **Category**: docs / dx
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Standardization without enforcement decays. Phase 4 closes the loop:
audit-ci green across all 13 repos, direction docs reconciled to
"Velnor default everywhere", and the required-check strategy decided and
applied. It also produces the first full-estate §2.11 performance report on
the standardized surface — the baseline every regression is measured against.

## Current state

- `VELNOR_PROJECTS_SETUP.md` §8 Phase 4 + §11 metrics + §12 open decisions
  (1: required checks; 7: GitHub-lane GHA sccache; 8: parity cadence) —
  decisions 1/7/8 need operator answers recorded.
- `docs/master-plan.md`, `docs/mission.md`, `AGENTS.md` direction log — may
  still carry pre-standard phrasing (e.g. old jackin-family GitHub-default
  exception; the setup doc's Policy override note says it supersedes them).
- audit-ci estate file: created by plan 046 step 3 (13 clone paths).

## Commands you will need

| Purpose | Command (velnor repo root) | Expected |
|---------|---------------------------|----------|
| Estate audit | `cargo run -p velnor-tools -- audit-ci --estate <file> --json` | zero ERRORs |
| Perf audit per repo | `cargo run -p velnor-tools -- audit-ci --repo-path <r> --perf-log <warm-run log>` | pass |
| Compare | `cargo run -p velnor-tools -- compare --run-id <both-run> --class <X>` | budgets met |

## Scope

**In scope**: velnor repo docs (`VELNOR_PROJECTS_SETUP.md` status marks,
`docs/master-plan.md`, `docs/mission.md`, `AGENTS.md` log entry,
`prompts/README.md` if stale); the audit-ci estate list file; a new
`docs/ci-performance-report-<date>.md` (campaign results — operator-supplied
run data); per-repo `.github` ONLY if an audit finding requires a trivial
fix (one-line, else file back to the repo's plan).
**Out of scope**: new runner features; new estate structure; changing the
standard itself.

## Git workflow

Velnor branch `velnor-estate-standard`; `docs:`/`chore:` commits,
`git commit -s`; estate one-liners on `velnor-estate-standard` branches per repo; no
pushes without operator instruction.

## Steps

### Step 1: Estate audit sweep

Run audit-ci over all 13; fix trivial findings (one-liners) on per-repo
branches; larger findings → reopen the repo's plan row as BLOCKED with the
finding attached.

**Verify**: sweep exits 0 (or every non-zero maps to a reopened row).

### Step 2: Estate performance campaign

Operator-gated: the §2.11 protocol (fleet healthy → cold/warm1/warm2 ×
lanes=both per workload workflow, three rounds — the method of
`docs/ci-performance-report-2026-06-11.md`). Collect per-repo tables +
`compare` budget verdicts; write `docs/ci-performance-report-<date>.md`
(model the 2026-06-11 report's structure, including the FINAL NUMBERS
section style). Every budget miss becomes a named follow-up in the report.

**Verify**: report committed; every workload workflow has three-round data
or a named blocker.

### Step 3: Docs reconcile

Sweep `docs/master-plan.md`, `docs/mission.md`, `docs/roadmap.md`,
`prompts/README.md` for statements contradicting Velnor-default-everywhere /
the strict cache contract; fix each (minimal edits); add the AGENTS.md
direction-log entry summarizing Phase-4 completion + the operator's recorded
answers to §12 decisions 1/7/8. Mark `VELNOR_PROJECTS_SETUP.md` §8 phases
with completion dates.

**Verify**: `grep -rn "github.*default" docs/ prompts/` reviewed — no stale
direction statements; AGENTS log entry present.

### Step 4: Required checks + parity cadence (operator decisions applied)

Apply decision §12.1 (which lane is required) via branch-protection settings
(operator does clicks/API; you produce the exact per-repo checklist with
job names from the merged workflows). Apply §12.8: if weekly `both` chosen,
add the `schedule → both` matrix arm to the canonical expression — that is a
STANDARD CHANGE: fixture first (041 pattern), then estate PRs; scope it as a
follow-up plan, not inline.

**Verify**: checklist delivered; decision recorded; any standard change
spawned as a new plan file, not hacked in.

## Test plan

The estate sweep IS the test. Campaign report completeness per step 2.

## Done criteria

- [ ] audit-ci estate sweep zero-ERROR (or all mapped to reopened rows)
- [ ] Performance report committed with three-round data
- [ ] Docs/AGENTS reconciled; §12 decisions 1/7/8 recorded
- [ ] Required-check checklist delivered to operator
- [ ] plans/README.md final statuses updated for the whole 033–058 set

## STOP conditions

- Any repo's Velnor lane went red since its migration (fleet drift) → STOP;
  stability work outranks enforcement (P1.9 doctrine).
- The operator has not answered §12 decisions → deliver steps 1–3, STOP
  before step 4.

## Maintenance notes

- Re-run the estate sweep + a one-round campaign quarterly (or wire
  audit-ci into each repo's CI as a policy job — natural follow-up).
- The §2.11 budgets get re-baselined from the step-2 report — update
  `VELNOR_PROJECTS_SETUP.md` §2.11 numbers with measured medians.
