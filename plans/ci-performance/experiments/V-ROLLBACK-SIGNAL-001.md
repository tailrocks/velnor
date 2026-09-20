# V-ROLLBACK-SIGNAL-001 — cancellation fixture signal ownership

Status: repair published; local checks and one exact-head Linux CI pass.
Independent result review and repeated outcomes remain pending.
No performance acceptance or completed iteration credit. Preserve failed attempt
and eventual repetitions together.

- Repository: Velnor; generator package tests in PR CI.
- Baseline/source: `3a7da4304c7ed9b899d65611a68bc51254e15ad9`.
- Effective merge: `219ae9162538788e583cb22f9303b9a4ab0d9a6e`.
- Run/attempt: [35506628393/1](https://github.com/tailrocks/velnor/actions/runs/35506628393).
- Job: [106067477910](https://github.com/tailrocks/velnor/actions/runs/35506628393/job/106067477910).
- Runtime pin: `4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d`; schema-2 revision 64.
- Runner: GitHub-hosted Ubuntu 24.04.5, image 20260907.300.1, x64.
- Candidate: typed signaling repair in this commit; owner `/root/velnor_inventory`.

## Hypothesis and enabling boundary

The cancellation fixture delegates numeric process identity through an external
command parser. A documented procps parser defect can broaden the signal target.
The test should own and verify its process group, then invoke a typed signaling
API. A correct production rollback boundary does not make its test harness safe.

[Upstream issue](https://gitlab.com/procps-ng/procps/-/issues/354) and
[versioned implementation](https://gitlab.com/procps-ng/procps/-/raw/v4.0.4/src/kill.c)
support the mechanism. The failed job does not record its actual target PID or
installed procps version; specific-run attribution remains an inference.

## Alternatives and intervention

1. Existing workspace `rustix` process API, with child/group identity checks:
   removes command parsing and rejects broadcast/shared-group targets.
2. External utility with explicit operand delimiter: smaller intervention,
   retains utility/version behavior and a string boundary.
3. Shell builtin with explicit operand delimiter: retains shell execution and
   a string boundary.

Choose the typed API. Preserve child TERM and group TERM coverage, lock fencing,
transaction cleanup, failure status, and single rollback assertions. Validate raw
PID values before constructing the library type. Preserve bounded TERM/KILL
cleanup without signaling the caller's group.

## Evidence and controls

- macOS baseline: 1,942 tests passed; this did not prove Linux signal safety.
- Real PR: generator exited 143 after a runner shutdown message; 869 passing
  tests were logged before interruption. Required gates failed correctly.
- Entire failed PR: 577 seconds elapsed, 1,368 seconds aggregate execution;
  slowest successful job was the runner crate at 546 seconds. Exclude from
  successful full-coverage speedup comparisons.
- Parent safe source-derived parser replay: eight cases, no signals sent;
  source and results retained in `observations/procps-parser-parent-probe.json`.
  This replay is not execution of the Ubuntu binary.
- Candidate: 45 package tests, strict all-target Clippy, formatting, and all
  1,943 generator tests pass (21 binaries; 38.759 seconds local execution).
- Independent reviewer `/root/jackin_inventory`: PASS; independently ran 46
  unit and four integration package tests. Confirmed invalid PID rejection,
  owned group validation, bounded cleanup, and preserved lifecycle assertions.
- Exact pushed SHA, Linux CI, and repeated outcomes remain pending. No failed
  observation may be dropped from the cohort.

Next question: does the owned-process implementation retain every cancellation
assertion and pass actual Linux CI without runner termination?

## First fresh Linux result

Published [fecc59e9](https://github.com/tailrocks/velnor/commit/fecc59e9e40568d92b6d828a6515ad3c68c81438),
with effective merge `4ef5c402e2842e9392b505acfeaf9b829cd50e81`.
[PR run 35508236735](https://github.com/tailrocks/velnor/actions/runs/35508236735)
and [policy run 35508235601](https://github.com/tailrocks/velnor/actions/runs/35508235601)
succeeded. The generator ran 1,943 tests, all passing, none skipped; the
cancellation fixture and invalid process-group fixture passed explicitly.

Raw job completion gives 458 seconds trigger-to-required, 1,210 aggregate
execution seconds, 20 executed jobs and 48 configured skipped jobs. Runner:
413 seconds; generator: 186 seconds. These are one observation, not a speedup
claim. The previous full run failed and cannot be its successful baseline.

The runner retained 2,516 passing tests, five skipped tests, and the same
pre-existing leaky `guest_docker_probe_kills_hung_process_at_deadline` test
observed in run 35506628393. No test selection was reduced by this repair.
Generator preparation (three seconds) and upload (six seconds) still appear
as cleanup in schema-2 telemetry; phase attribution remains a separate defect.
Raw JSON, logs, collector JSONL/CSV and validation limits are retained under
`observations/velnor-35508236735-*`.

## Independent Linux confirmation (2026-09-20)

Reviewer replayed the exact pushed generator log
`/tmp/velnor-signal-generator.log`. Run `35508236735`, attempt 1, completed
successfully with 68 jobs: 20 executed, 48 configured skips, and no skipped
generator tests. All 1,943 generator tests passed. The cancellation-specific
checks `process_group_validation_rejects_broadcast_and_shared_groups` and
`rolling_verifier_cancellation_retains_lock_without_release` passed. This is
a correctness PASS for the owned-process signal boundary and preserved test
coverage. It is one fresh observation; it supplies no speedup or critical-path
claim. PR973 must retain this `fecc59e9` foreground verifier and typed signal
implementation when rebased; its pre-`fecc59e9` file otherwise removes that
coverage and regresses the cancellation boundary.
