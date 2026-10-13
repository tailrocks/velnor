# Plan 059: Velnor host baseline cleanup — clean test bed before dual-lane verification

> **Executor instructions**: This plan is OPERATOR-SUPERVISED — every
> destructive step needs the operator watching or explicitly delegating.
> Follow step by step; record every number; STOP conditions binding. Update
> the status row in `plans/README.md` when done.
>
> **Drift check (run first)**: this plan touches the LIVE Velnor host
> (Sentry), not the repo. Confirm with the operator which host(s) run the
> fleet before touching anything. Velnor repo planned at `48b04ad`.

## Status

- **Priority**: P0 (gate 7 of VELNOR_PROJECTS_SETUP.md §0 — no verification
  campaign on a polluted host)
- **Effort**: M (mostly careful ops)
- **Risk**: HIGH (live server; deletion)
- **Depends on**: none to START the inventory; the reclaim step prefers
  plans/035 (fail-closed identity) + 037 (GC tooling) landed so cleaned
  state STAYS clean — run inventory now, destructive pass ideally after
  035/037, and always before the first estate verification campaign (V-C
  timings of plans 047–057).
- **Category**: stability / ops
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

Every §2.11 verification (cold/warm/no-change timings, rerun-idempotency,
both-lane parity) runs on the current Velnor host. Live evidence
(2026-07-18, `docs/storage-and-disk-pressure-2026-07-18.md`): root XFS **84%
used**, ~**432 GB** physical in persistent Cargo target trees (largely under
the `unknown-repository/unknown-workflow` collapse), ~**158 GB** in the two
largest BuildKit stores. On that host, "cold" runs aren't cold, disk
pressure skews timings, and stale runner registrations steal queued jobs.
The program's measurements are only trustworthy from a recorded clean
baseline. Manual deletion is NOT the product fix (035–037 are) — this is the
one-time test-bed reset the setup doc's §8 Phase 0.5 mandates.

## Current state

- Host: the Sentry fleet host (operator provides SSH access + the config
  dirs; daemons run via systemd per `docs/runner-usage.md` production
  section).
- Stores live under the daemon work roots as `_velnor_cargo`, `_velnor_mise`,
  `_velnor_sccache`, `_velnor_targets/<trust>/<repo>/<workflow>/<job>`,
  `_velnor_caches`, `_velnor_artifacts` (see
  `docs/storage-and-disk-pressure-2026-07-18.md` "Live Sentry evidence" for
  the observed paths and sizes).
- Existing tooling: `velnor-runner cache du` (logical sizes),
  `velnor-runner cache gc --dry-run` (plan only — destructive GC is a stub
  until plan 037), `velnor-runner doctor` (fleet health), daemon startup
  pruner for stale `velnor-job-*` containers + `velnor-net-*` networks.
- AGENTS.md hard rules that bind here: cancel stale runs + delete stale
  runner registrations before dispatching verification runs; never modify
  the fixture to work around gaps; never broad `docker system prune`
  (storage doc: owned-resource deletion only).

## Commands you will need (on the host; adjust config-dir paths per operator)

| Purpose | Command | Expected |
|---------|---------|----------|
| Disk | `df -h /` and `df -h` per mount | recorded |
| Store sizes | `velnor-runner cache du` (per daemon config dir) + `du -sh <workroot>/_velnor_*` | recorded |
| Docker usage | `docker system df -v` | recorded |
| Builders | `docker buildx ls` | recorded |
| Runner registrations | `gh api repos/<owner>/<repo>/actions/runners` (or org equivalent) per fleet scope | recorded |
| Stale runs | `gh run list --repo <r> --json databaseId,status --jq '.[] | select(.status != "completed") | .databaseId'` | recorded |

## Scope

**In scope**: the Velnor host's velnor-owned state (stores, velnor-labeled
docker resources, velnor builders, runner registrations, stale queued runs);
a baseline report committed to the velnor repo as
`docs/host-baseline-<YYYY-MM-DD>.md` (on branch `velnor-estate-standard`).
**Out of scope**: ANY non-velnor data on the host (other services, system
packages); the durable fixes (035–037); fleet reconfiguration (039's
runbook).

## Git workflow

Velnor repo, branch `velnor-estate-standard` (the shared program branch);
`docs:` commit for the baseline report, `git commit -s`.

## Steps

### Step 1: Inventory (read-only — run anytime)

Capture ALL commands from the table above into a scratch file. Break down
`_velnor_targets` by first two path levels (`du -sh */* | sort -rh | head -40`)
— separate `unknown-repository` trees from legitimate `<repo>` trees. List
BuildKit stores per builder with sizes. Snapshot systemd unit states +
daemon versions (`velnor-runner --version`).

**Verify**: every table row has a number; nothing was modified
(`docker system df` unchanged on re-run).

### Step 2: Fleet quiesce (operator window)

Cancel all pending/in-progress runs across the estate repos (AGENTS rule
command); drain daemons (SIGTERM → wait for graceful drain per the
drain/TimeoutStopSec behavior); confirm `doctor` shows no busy slots; delete
OFFLINE/stale runner registrations via `gh api` (keep healthy registered
runners of daemons you are about to restart — simplest: after daemons are
stopped, delete ALL of this fleet's registrations; they re-register on
start).

**Verify**: zero non-completed runs on estate repos; zero velnor runner
registrations while daemons stopped.

### Step 3: Docker cleanup (owned resources only)

With daemons stopped: remove leftover `velnor-job-*` containers and
`velnor-net-*` networks (the daemon startup pruner's scope — doing it
manually is the same owned set); remove velnor job images ONLY if the
operator wants an image-pull-cold baseline (record the choice); prune the
velnor-owned buildx builder's cache to zero
(`docker buildx prune --builder <name> --all` for THAT builder) — if
builders are unnamed/shared, ask the operator which are velnor's; NEVER
`docker system prune`.

**Verify**: `docker ps -a | grep velnor` → none; `docker network ls | grep
velnor-net` → none; builder cache size ~0 in `docker buildx du`.

### Step 4: Store reclamation (supervised; prefer post-037 tooling)

If plan 037 landed: `velnor-runner cache gc --yes` with the agreed policy
(TTL + budgets) + manual removal of the `unknown-repository` trees (037's
GC can select them once 035 stops new writes). If 037 NOT landed: manual,
operator-confirmed `rm -rf` of exactly: (a) every
`_velnor_targets/*/unknown-repository` tree, (b) target buckets for
repos/workflows that no longer exist (cross-check the estate list),
(c) `_velnor_caches` scopes older than 30 days. KEEP: `_velnor_cargo`,
`_velnor_mise`, `_velnor_sccache` (bounded, legitimately warm — unless the
operator wants a fully-cold baseline pass first; record the choice either
way). Each deletion: log path + size to the report.

**Verify**: `df -h /` target ≤ 60% used (or the operator's number);
`du -sh _velnor_targets` shows only legitimate `<trust>/<repo>` trees.

### Step 5: Restart + baseline report

Start daemons; `doctor` green (all slots online, registry reconciled);
dispatch one fixture `compat.yml` `lanes=both` smoke — green. Write
`docs/host-baseline-<date>.md`: before/after tables (disk, per-class store
bytes, docker, builders), what was deleted and why, what was kept and why,
the cold-vs-warm choice, fleet state, and the rule that V-C campaign results
reference THIS baseline. Commit on `velnor-estate-standard`.

**Verify**: report committed; doctor green; fixture smoke run URL recorded.

## Test plan

The fixture both-lane smoke (step 5) is the functional test; the report's
before/after deltas are the deliverable. Re-run a mini inventory after the
first estate campaign to confirm steady-state (no runaway growth — that
would reopen 036/037).

## Done criteria

- [ ] Baseline report committed with full before/after numbers
- [ ] Disk ≤ agreed threshold; zero `unknown-repository` trees remain
- [ ] Zero stale runners/runs/containers/networks; doctor green
- [ ] Fixture both-lane smoke green post-cleanup
- [ ] Nothing non-velnor touched (operator sign-off line in the report)

## STOP conditions

- Any path's ownership is ambiguous (not clearly velnor's) → STOP, ask the
  operator; never guess on a live server.
- Daemons fail to drain within the systemd window → STOP; that is a runner
  bug (drain regression) to file before killing anything.
- Post-cleanup doctor NOT green or the fixture smoke fails → STOP; the
  program's verification campaigns are blocked until the fleet is healthy
  (stability outranks schedule — P1.9 doctrine).
- Deleting `unknown-repository` trees while pre-035 daemons still RUN would
  refill them → either land 035 first or schedule cleanup immediately before
  the campaign window; if neither is possible, STOP and say so.

## Maintenance notes

- This is one-time. If the host re-pollutes after 035–037 are live, that is
  a product regression (reopen those plans), not a reason to re-run manual
  cleanup.
- The baseline report is the denominator for every §2.11 claim in plans
  047–058 — link it from the campaign report (058 step 2).
