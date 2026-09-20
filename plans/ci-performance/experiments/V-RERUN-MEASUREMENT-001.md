# V-RERUN-MEASUREMENT-001: repeat the actual PR dependency graph

Status: one bounded collector rerun completed; full-graph repetitions not
started. No completed optimization iteration or accepted performance claim.

## Capability probe: attempt 2

Run `35491248265` attempt 2 started at `2026-09-20T09:45:10Z` and
completed successfully. Collector job `106058435636` ran 09:45:15–09:45:55;
its checks took 18 seconds. Checkout logs prove merge tree
`91f2fc0e8727041087bdd08f807d062a719d5d01`, retaining the original treatment.
The collector reported one compiler hit, zero misses, 104 operations not
looked up and five bypasses. Its MBX archive was absent; schema 1's reported
prefix hit was incorrect. This is not a warm compiler-cache sample.

The paginated REST response contains 68 jobs, all labeled attempt 2 and with
new job IDs. **Seventeen successful jobs retain timestamps before this
attempt started.** Only the collector and two required gates freshly executed.
Thirty-five skipped records have inverted synthetic timestamps. Matching
attempt numbers and new IDs therefore cannot establish fresh full coverage.
The measurement collector needs an explicit reused-result/freshness boundary;
do not feed this attempt into whole-workflow speedup comparisons.

Raw run, complete single-page job response, analysis and collector log are
retained under `observations/velnor-35491248265-attempt2-*`. Exact request
timestamp is not available in the parent record; do not substitute the
original run creation time. The Velnor agent independently confirmed the
raw totals: 17 reused successes account for 2,665 seconds; three fresh successes
account for 46 seconds. Parent reproduced the collector's incorrect 2,711-second
aggregate. A structural freshness-model repair is now in implementation.

## Independent review

The route is valid as a controlled rerun mechanism, with two hard boundaries.
GitHub documents `POST /repos/{owner}/{repo}/actions/jobs/{job_id}/rerun` as
rerunning the selected job and its dependent jobs. It returns `201` and needs
Actions repository write permission; read-only REST access cannot trigger the
measurement. The operation is therefore recorded as an explicit CI mutation,
with request time and response before any timing claim.

The API does not promise that a selected job is a whole workflow. The local
dependency graph proves that planning is an ancestor of all 38 jobs in the
two inspected `ci-pr.yml` revisions, but the actual rerun must still prove
its new run ID, attempt, head SHA, workflow path, referenced reusable workflow
SHAs, and complete paginated job set. A rerun retains the original workflow
revision and source treatment; it is not a run of the current working tree by
assumption. Any job that is absent, skipped unexpectedly, or has a different
workflow/source identity is censored, not silently merged with the original.

Verdict: use this route for alternating baseline/candidate samples only after
the exact planning job and active runner pool are recorded. It cannot by
itself provide a cold MBX observation: PR runs restore-only under the pinned
action's save policy, and a same-SHA rerun shares the normal cache namespace.
Use isolated cache keys or a documented warm-prefix condition when the
treatment requires it. Keep the ten held-out final repetitions separate.

## Available route

The connector exposes job reruns, although it has no whole-run or dispatch
operation. GitHub's [job rerun API](https://docs.github.com/en/rest/actions/workflow-runs#re-run-a-job-from-a-workflow-run)
reruns the selected job and its dependents. Parsing the actual `ci-pr.yml`
at `bb94bac9731329b99201127210829cec5c11a425` and
`95432856618609a3a1b908cc7ae533b898d4c590` found 38 top-level jobs in each;
every job is planning or transitively depends on planning. This suggests a
planning rerun can repeat this entire PR graph. It does not prove the actual
new attempt includes every reusable child or uses the expected revision.

## Probe and acceptance conditions

1. Wait for active pool work to finish. Record the requested planning job ID,
   original run/attempt, request time and response before triggering one rerun.
2. Retain attempt-specific run and fully paginated job records. Verify all
   expected executed obligations and skipped irrelevant variants against the
   original graph; do not combine old job attempts into a fresh observation.
3. Inspect actual checkout logs and referenced reusable workflow revisions.
   A rerun executes the original treatment, not the current branch by assumption.
   Record event source, effective checkout, runtime and configuration separately.
4. Record the new attempt's observed start and final required result. Original
   run `created_at` predates the rerun and cannot measure rerun trigger latency.
5. Retain failures, cancellations, retries and missing evidence. Do not count
   reused earlier job results as fresh full coverage.

If the route proves complete, alternate at least five baseline/candidate
observations sequentially. The first pair uses the successful runs
`35491248265` and `35492230871`. Hardware/image/toolchain/cache states must be
recorded for every attempt. MBX archive absence is compiler-cold with other
tool/Cargo layers warm; it is not a genuinely cold machine. PR-only repetitions
cannot seed the MBX namespace under the pinned action's save policy.

The candidate adds three generator regression tests and changes generator
source; those differences confound generator-job comparisons and must remain
visible. Other Rust product source and validation command sets are unchanged.
Measure exact command intervals and compiler counters as well as complete
workflow latency and aggregate execution. A 21-versus-14-second collector
check observation is an unresolved possible regression, not noise by fiat.

These are candidate-selection samples, not the ten held-out final repetitions.
This mechanism also does not replace normal source/lockfile/toolchain changes,
fork restrictions, manual/scheduled events or packaging validation.
