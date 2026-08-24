# Plan 063 — criterion 5+7 live fixture proof (2026-08-24)

Verifier session. Target: `tailrocks/velnor-actions-fixture`, ref
`plan-063-fixture-control-plane` (=46f71d3 "feat(ci): add control-plane corpus
and align compat to sole lane selector").

## Hygiene (pre-dispatch)

- Pending/in-progress runs in fixture repo at start: **0** (30 most recent runs
  all `completed`). Nothing to cancel.
- Runner registrations found: `velnor-fixture-slot-1` (id 5534) and
  `velnor-fixture-slot-2` (id 5535), both **online, idle**, labels
  `[fixture, self-hosted, velnor, velnor-target-mvp]`. These match the
  protected class (`velnor-target-mvp` online fleet). No stale validation-owned
  registrations existed → **deleted none**.
- Clean-state proof: zero non-completed runs + two healthy online idle runners,
  verified immediately before dispatch.

## Dispatches

| workflow | run | conclusion | duration |
|---|---|---|---|
| compat.yml (inputs lane=both) | 32700101089 | **success** | 85s (07:07:12Z → 07:08:37Z), all 15 jobs green |
| control-plane.yml | **NOT DISPATCHED — blocked** | n/a | n/a |

## control-plane.yml dispatch blocker (exact evidence)

GitHub resolves `{workflow_id}` for the dispatch endpoint against workflows
registered on the **default branch only**; a workflow file that exists solely
on a feature branch is invisible to the API.

1. `gh workflow run control-plane.yml --ref plan-063-fixture-control-plane`
   → `HTTP 404: workflow control-plane.yml not found on the default branch`
2. Direct REST attempt
   `POST /repos/tailrocks/velnor-actions-fixture/actions/workflows/control-plane.yml/dispatches -f ref=plan-063-fixture-control-plane`
   → `{"message":"Not Found","status":"404"}`
3. `GET /actions/workflows` registry lists 15 default-branch workflows;
   control-plane is absent.
4. Branch state: `main...plan-063-fixture-control-plane` = ahead 1 / behind 0.
   The file lands on main only when the branch merges (operator merge gate:
   `ci-required` + DCO). This verifier writes no repository files and does not
   merge.

### Additional plan-model correction

The mandate assumed "its 8 scenarios run as matrix jobs inside this one run."
The actual file takes ONE scenario per dispatch via choice input
`scenario ∈ {success, failure, hold, queue, concurrent, artifacts, cache,
load}`; every other scenario job is skipped, and the aggregator asserts exactly
one expected terminal state per input. Full proof of all 8 scenarios requires
**8 separate dispatches** (each optionally lane=both, which doubles the matrix
into Velnor+GitHub legs). Scenario `queue` additionally requires a dedicated
runner carrying label `velnor-cp-queue-validation` that no other runner may
carry (README contract); no such registration existed during this session.

Unblocking sequence for the operator:

1. Merge `plan-063-fixture-control-plane` into main (gates apply).
2. Re-dispatch control-plane.yml once per scenario × lane selector from main or
   a post-merge ref; capture fresh run IDs; aggregator job's
   `CP_VERDICT=match` step summary is the pass signal (failure scenario's
   controlled failure at `controlled-failure` is the requested terminal state).

## Evidence files (sanitized)

- `compat-run-32700101089.json` — run metadata (id, ref, sha, timing, URL).
- `appa-velnor.log` — job 97349755858 `compat (app-a, Velnor, …)` success log.
- `appa-github.log` — job 97349755885 `compat (app-a, GitHub, …)` success log.
- `compare-results.log` — job 97349967613 dual-lane compare-results success log.
- Sanitization: ANSI escapes stripped; patterns redacted
  (`gh[pousr]_*`, `github_pat_*`, `AKIA…`, `Bearer …`, `x-access-token:`).
  Post-redaction scan: 0 credential-shaped matches across all three logs;
  only benign runner banner lines ("Secret source: Actions") remain.

No failed steps occurred → no failure logs to capture (mandate item satisfied
vacuously; recorded here).

## Cleanup

- Post-run re-check: 0 pending/in-progress runs attributable to this session
  (single dispatch completed).
- Registrations unchanged (2 protected online runners; none created/deleted by
  verifier).
- Workflow artifacts: concurrent/artifacts scenarios never ran (blocked), so no
  artifact junk created. Compat run produced no persistent junk beyond
  naturally expiring run artifacts.

## Gates impact

- Criterion 7 (fresh run IDs + conclusions recorded): **partial**. compat.yml
  dual-lane proof recorded with fresh run ID 32700101089 = success on
  46f71d3. control-plane corpus has **zero** live execution evidence.
- Criterion 5 (live fixture proof of control-plane corpus): **BLOCKED** pending
  merge of the corpus branch to its default branch. Not an outage — a GitHub
  platform constraint plus unmet merge prerequisite.
- Plan 063 DONE: **blocked** until the operator merges the branch and the 8
  scenario dispatches are executed with captured conclusions.
