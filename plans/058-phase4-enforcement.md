# Plan 058: Phase 4 — estate-wide enforcement, docs reconcile, required checks

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
> This plan closes LAST. Its concern inventory, canonical signatures, and
> enforcement work start while blocked estate deliveries are repaired; final
> default-branch verification runs only after plans 047–057 report DONE.
>
> **Drift check (run first)**: `git log --oneline -1` in the velnor repo —
> planned at `48b04ad`. This plan is mostly cross-repo verification; drift in
> velnor docs is expected and handled in step 3.

## Status

- **Priority**: P2 (Phase 4 of VELNOR_PROJECTS_SETUP.md §8)
- **Effort**: L
- **Risk**: LOW (verification + docs); required-checks flip is operator-gated
- **Depends on**: plans/046 (audit-ci), plans/047–057 (all estate PRs merged)
- **Category**: docs / dx
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Standardization without enforcement decays. Phase 4 closes the loop:
common and required concerns mechanically converged across all 13 repos,
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

**In scope**: a machine-readable per-repository concern inventory; canonical
workflow signatures/templates; Rust `audit-ci` rules and tests for
`missing-required`, `canonical-drift`, `non-applicable`, and `repo-specific`;
all estate workflow changes needed to add missing required concerns or converge
shared implementations; velnor repo docs (`VELNOR_PROJECTS_SETUP.md` status marks,
`docs/master-plan.md`, `docs/mission.md`, `AGENTS.md` log entry,
`plans/README.md`, and `docs/prompt.md`); the audit-ci estate list file; a new
`docs/ci-performance-report-<date>.md` (campaign results collected from the
verified workflow runs); per-repo `.github` and supporting configuration/code
needed to converge an applicable concern or add a required one.
**Out of scope**: new runner features; new estate structure; changing the
standard itself.

## Git workflow

Velnor branch `velnor-estate-standard`; `docs:`/`chore:` commits,
`git commit -s`; estate one-liners on `velnor-estate-standard` branches per repo; no
pushes without operator instruction.

## Steps

### Step 0: Inventory concerns and define canonical signatures

For every repository classify lane selection, checkout, tool setup, Rust CI,
integration/services, Cargo/cache, Docker/build, artifacts, docs/Pages,
preview, release, Renovate, and workflow safety as `required`, `applicable`,
`non-applicable`, or `repo-specific`, with evidence. Select one canonical
implementation for each common concern from `VELNOR_PROJECTS_SETUP.md` §2/§9
and the newest contract-conformant estate implementation. Record permitted
repository data parameters separately from structural YAML.

**Verify**: all 13 repositories and every listed concern have an evidence-backed
classification; no absent workflow is implicitly treated as non-applicable.

### Step 1: Converge and enforce the estate

Add every missing required concern. Converge every common/applicable concern on
the canonical filename, job ids, lane matrix, step order, action refs/inputs,
environment, cache keys, timeouts, concurrency, permissions, writer gate,
artifacts, and aggregator. Keep product-specific commands, paths, packages,
targets, secrets, and justified resource limits as explicit parameters. Do not
add no-op jobs for non-applicable concerns.

Extend `velnor-tools audit-ci` in Rust to consume the inventory and fail on
`missing-required` and `canonical-drift`, while reporting documented
`non-applicable` and `repo-specific`. Add unit/fixture tests that prove an
omitted required concern fails and repository data parameters do not create
false drift.

**Verify**: focused Rust tests pass; generated comparison output shows the
canonical signature used by each applicable concern in each repository.

### Step 2: Estate audit sweep

Run audit-ci over all 13; fix trivial findings (one-liners) on per-repo
branches; larger findings → reopen the repo's plan row as BLOCKED with the
finding attached.

**Verify**: program-branch and delivered-default-branch sweeps exit 0 with zero
unexplained `missing-required` or `canonical-drift` findings.

### Step 3: Estate performance campaign

Operator-gated: the §2.11 protocol (fleet healthy → cold/warm1/warm2 ×
lanes=both per workload workflow, three rounds — the method of
`docs/ci-performance-report-2026-06-11.md`). Collect per-repo tables +
`compare` budget verdicts; write `docs/ci-performance-report-<date>.md`
(model the 2026-06-11 report's structure, including the FINAL NUMBERS
section style). Every budget miss becomes a named follow-up in the report.

**Verify**: report committed; every workload workflow has three-round data
or a named blocker.

### Step 4: Docs reconcile

Sweep `docs/master-plan.md`, `docs/mission.md`, `docs/roadmap.md`,
`docs/prompt.md`, `plans/README.md`, and the root `README.md` for statements contradicting Velnor-default-everywhere /
the strict cache contract; fix each (minimal edits); add the AGENTS.md
direction-log entry summarizing Phase-4 completion + the operator's recorded
answers to §12 decisions 1/7/8. Mark `VELNOR_PROJECTS_SETUP.md` §8 phases
with completion dates.

**Verify**: `rg -n "github.*default" docs plans README.md` reviewed — no stale
direction statements; AGENTS log entry present.

### Step 5: Required checks + parity cadence (operator decisions applied)

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
- [ ] Every repository/concern classified with evidence
- [ ] Every missing required concern added; every common concern uses one canonical implementation
- [ ] `audit-ci` tests and estate output prove missing-required and canonical-drift enforcement
- [ ] Performance report committed with three-round data
- [ ] Docs/AGENTS reconciled; §12 decisions 1/7/8 recorded
- [x] Required-check checklist delivered to operator
- [ ] plans/README.md final statuses updated for the whole 033–058 set

## STOP conditions

- Any repo's Velnor lane went red since its migration (fleet drift) → STOP;
  stability work outranks enforcement (P1.9 doctrine).
- The operator has not answered §12 decisions → finish every independent step,
  record the exact requested decision, keep the goal active, and resume step 5
  after the answer. Do not mark this plan DONE from a handoff alone.

## Maintenance notes

- Re-run the estate sweep + a one-round campaign quarterly (or wire
  audit-ci into each repo's CI as a policy job — natural follow-up).
- The §2.11 budgets get re-baselined from the step-2 report — update
  `VELNOR_PROJECTS_SETUP.md` §2.11 numbers with measured medians.
