# Control-plane live validation — 2026-08-24 (partial: blocked at scenario dispatch)

Fixture `tailrocks/velnor-actions-fixture`, ref `campaign/control-plane-corpus` @ `46f71d3`, PR #90.
Evidence sanitized: no rendered GitHub HTML, no tokens, log excerpts ≤20 lines.

## Hygiene (per leaf + README dispatch cleanup rules)

- Cancelled runs older than PR #90's own runs: **none existed** — the only
  pending/in-progress runs were PR #90's own (`CI` 32702825087,
  `compat public unmerged` 32702824642); both completed green. Cancelled IDs: none.
- Runner registrations matching validation prefix `velnor-cp-queue-validation`: **zero**;
  deleted: **zero**; remaining after: zero (nothing to prove beyond absence).
  Fleet seen: `velnor-fixture-slot-1`, `velnor-fixture-slot-2` (online, idle) — untouched.

## PR #90 checks

- Verdict: **green** — 13 SUCCESS / 12 SKIPPED / 0 FAILED; `mergeable=MERGEABLE`,
  `mergeStateStatus=CLEAN`. Fixture baseline passes unchanged.

## Run B: compat lane=both on campaign branch

- Run `32703106587`: **conclusion success** (~80s), all jobs green on both lanes:
  - `compat (app-a, Velnor, velnor-trusted, velnor-target-mvp, true)` success
  - `compat (app-a, GitHub, ubuntu-26.04, false)` success
  - same pair for app-b; `compare-results` → `aggregate-needs: success`;
    `compat-required` success.
- Lane split proof: Velnor and GitHub job pairs both executed in one run.
  URL: https://github.com/tailrocks/velnor-actions-fixture/actions/runs/32703106587

## Scenarios C–E: NOT RUN — blocked pre-merge

Dispatch of `control-plane.yml` is impossible until PR #90 merges:

```
gh workflow run control-plane.yml --ref campaign/control-plane-corpus
→ HTTP 404: workflow control-plane.yml not found on the default branch
POST /actions/workflows/control-plane.yml/dispatches → HTTP 404
GET /actions/workflows → 14 registered workflows, none matches "control"
```

GitHub registers dispatchable workflows from the default branch only. The
workflow file exists solely on the campaign branch. Merging PR #90 is an
operator merge decision and was out of scope for this run ("NO commits anywhere").

Consequences (explicitly unproven, not converted into success):

- success succeeds — unproven
- failure fails exactly at `controlled-failure` with logs — unproven
- hold cancel via GitHub reports `cancelled`; no orphaned validation runner — unproven
- queue isolated to dedicated `velnor-cp-queue-validation` instance — unproven
  (note: no runner currently carries that label either)
- concurrent interval overlap proof from aggregator — unproven
- artifacts distinct + bounded — unproven
- cache cold/warm/unchanged marker reuse — unproven
- load bounded activity + guaranteed teardown — unproven

## Resume path

After operator merges PR #90 to main: rerun hygiene, then dispatch all eight
scenarios one at a time on `main`, defaults for `hold_seconds`(30)/`artifact_count`(3),
capture IDs, monitor ≤60s cycles, produce full CANCEL-PROOF and per-scenario quotes.

---

# Continuation 2026-08-24 (post-merge, step 4 executed)

Operator authorized the merge; the resume path above was executed in full.
Evidence sanitized: no rendered GitHub HTML, no tokens, quotes ≤20 lines.

## Merge

- Method **squash** (repo default accepted). Merge commit:
  `799178c1b53ddb5d1db5ddfc46d41c6284e1b72b`.
- Subject: `feat(ci): add control-plane corpus and align compat to sole lane selector`
- Trailers preserved verbatim: DCO `Signed-off-by: Alexey Zhokhov
  <alexey@zhokhov.com>.` + `Co-authored-by: Codex <codex@openai.com>.` +
  `Plan 063 step 3.` (verified via GET /commits/main message body).
- Local fixture clone fast-forwarded clean: `main` `415cadf` → `799178c` =
  `origin/main`; working tree clean. No other edits or commits anywhere.

## Hygiene before dispatching

- Zero pending/in-progress fixture runs found → zero cancellations needed.
- Zero registrations matching validation prefix `velnor-cp-queue-validation`
  existed → zero deletions; fleet = `velnor-fixture-slot-1/-2` (untouched).

## Runs (all on `main` @ 799178c, defaults hold_seconds=30 artifact_count=3 lane=velnor)

| run id | scenario | conclusion | one-word proof |
|---|---|---|---|
| 32703865136 | success | success | aggregator verdict=match |
| 32704019420 | failure | failure | named-step error marker |
| 32704204228 | hold | cancelled | mid-flight gh cancel |
| 32704312250 | concurrent | success | overlap_seconds=20 |
| 32704438283 | artifacts | success | count=3 distinct=3 |
| 32704574052 | cache cold | success | CP_CACHE_HIT=false |
| 32704719858 | cache warm | success | CP_CACHE_HIT=true |
| 32704848711 | load | success | teardown complete=true |
| 32705006580 | queue | cancelled | isolation-unproven |

## Per-scenario proof quotes

- failure — failed exactly at its named step (`controlled-failure`):
  `##[error]CP_MARKER scenario=failure phase=controlled-failure lane=Velnor expected=true`
  followed by `exit 1`. Step conclusions: controlled-failure=failure;
  job conclusion=failure = requested terminal state; aggregator green.
  Observation recorded verbatim below (expression rendering quirk).
- concurrent — both slots overlapped fully:
  slot1 `start_epoch=1787558569 end_epoch=1787558589`;
  slot2 `start_epoch=1787558569 end_epoch=1787558589`;
  aggregator: `CP_OVERLAP lane=Velnor overlap_seconds=20`.
- artifacts — `CP_ARTIFACT_LIST=cp-artifact-Velnor-1,cp-artifact-Velnor-2,cp-artifact-Velnor-3`,
  `CP_ARTIFACTS_VERIFIED count=3 distinct=3`; API shows bounded upload
  (`cp-artifacts-Velnor` 568 bytes; each file ≤256 bytes enforced).
- cache — cold run 32704574052: `Cache not found for input keys:
  cp-control-plane-Linux-fixed-v1`, `CP_CACHE_HIT=false`. Unchanged rerun
  32704719858: `Cache restored from key: cp-control-plane-Linux-fixed-v1`,
  `CP_CACHE_HIT=true`, post step "not saving cache" — cold/warm/unchanged reuse proven.
- load — `phase=ready`, `CP_LOAD_NPROC=4`, declared tolerances
  (cpu_seconds=10 pct50, memory_mib=256, disk_mib=64), `phase=done`, then
  guaranteed `phase=teardown complete=true`.

## Cancel-proof (hold)

Run 32704204228 cancelled via `gh run cancel` while `scenario-hold` was
in_progress (mid-sleep); final `status=completed conclusion=cancelled`.
Post-cancel runner listing: only `velnor-fixture-slot-1`, `velnor-fixture-slot-2`;
**zero** orphaned registrations prefixed `velnor-cp-queue-validation` or any
other validation name; nothing deleted because nothing stale existed.

## Queue verdict: ISOLATION-UNPROVEN

- Dispatched 08:11:02Z; `validate-inputs` completed hosted; `scenario-queue`
  stayed `queued` past 08:16:42Z — beyond the ~4min diagnosis window.
- Throughout the window `GET /actions/runners` listed only slot-1/slot-2 with
  labels `[fixture, self-hosted, velnor, velnor-target-mvp]`. **No runner
  carries `velnor-cp-queue-validation`; no JIT registration appeared under any
  queue/validation name.**
- The job requires `runs-on: [self-hosted, velnor-cp-queue-validation]`. A
  dedicated validation instance must be registered before this proof can exist.
  Run cancelled (conclusion=cancelled) to restore clean state; final fleet
  unchanged; zero orphans. Not faked by relabeling shared slots.

## Observations (recorded verbatim, no fixes applied here)

1. On the Velnor lane, env `OUTCOME: ${{ steps.controlled-failure.outcome }}`
   rendered literally (`${{ steps.controlled-failure.outcome }}`) in the
   follow-up step of the failure scenario, so its mismatch guard exited 1.
   Hyphenated step-id dot-path evaluation needs a both-lane check before
   blaming either side. Job-level requested terminal state still held.
2. Cold cache miss rendered empty `CP_CACHE_PRIMARY_KEY` output on the Velnor
   lane; the hit path proves key handling works. Recorded verbatim.

## Unproven / next blockers

- queue ISOLATION-UNPROVEN — needs a dedicated validation instance registered
  with exactly label `velnor-cp-queue-validation` (operator/daemon action).
- GitHub-leg control-plane coverage optional/future (defaults ran Velnor only).
