# CI performance campaign

Status: active; no performance improvement accepted yet. Started 2026-09-20.

This directory is the canonical record for the cross-repository campaign.
Historical work remains in [the September 16 cache plan](../2026-09-16-ci-workflow-and-cache-plan.md)
and [the September 17 behavior ledger](../2026-09-17-pr994-behavior-ledger.md).
Their claims require revalidation against current revisions.

## Completion contract

- Generator/runtime fixes, regenerated Velnor, Jackin and Parallax consumers.
- Retain required platforms, features, tests, packaging, trust and provenance.
- At least 100 distinct measured, independently checked substantive experiments.
- At least five comparable baseline/candidate observations for important claims;
  ten fresh held-out final repetitions for each primary warm cohort, retaining failures.
- Ten consecutive distinct controlled non-improving experiments per affected
  recurring workflow after its last accepted improvement.
- Final deterministic checks, independent review of pushed revisions, and real CI.
- Target: baseline duration divided by ten, separately for equivalent cohorts.
  Targets and estimates are not achievements.

## Ownership and dependency graph

Actual concurrency limit: parent plus three agents. Queue work; reuse agents.
Only parent performs shared branch/index/commit operations.

| Work | Owner | Inputs | Output / acceptance | State |
| --- | --- | --- | --- | --- |
| Velnor inventory | `/root/velnor_inventory` | current source, PR diffs, historical jobs | source/runtime distinctions, measured bottlenecks, compatible fixes | running |
| Jackin inventory | `/root/jackin_inventory` | current source, PR diffs, desktop/Swift runs | product graph, tool boundaries, duplicate-work proof | running |
| Parallax inventory | `/root/parallax_inventory` | current source, PR diffs, failed/scheduled runs | actual language graph, prerequisites, scheduler outcomes | running |
| Timing collection/tooling | next available agent | raw paginated run/job/attempt data | reproducible JSONL/CSV and ranked cohorts | queued |
| Compiler/cache/tools | next available agent | inventory + cache statistics | alternatives with compatibility and trust constraints | queued |
| Swift/Docker/artifacts | next available agent | product inventory + primary sources | explicit producer/consumer contracts and experiments | queued |
| Relevance/scheduling | next available agent | events, gates, transitive inputs | scenario matrix and fail-closed checks | queued |
| Independent review | agent other than author | hypotheses, diff, raw measurements | recorded findings before acceptance | queued |
| Integration/final checks | parent | reviewed units | small signed commits, regular pushes, exact-SHA CI | active |

Dependency order: inventories → ranked baseline → independently challenged
hypothesis → implementation → focused checks → controlled CI → independent
results review → commit/push → rerank. Independent research runs concurrently;
timing experiments must account for shared runner contention.

## Initial revisions and access

- Velnor initial local main: `1048337062ea625fada1b4f7c07f2feed75f60c7`.
- Refreshed Velnor remote main: `e94b48406c4ed206fce2bbf39b788264e72cf39c`.
  Clean local main fast-forwarded before creating `codex/ci-performance-campaign`.
- Jackin and Parallax revisions: inventory pending.
- `rtk` 0.49.0 available. Missing `gh` installed using Homebrew: 2.101.0.
- `gh auth status`: existing account token invalid; `gh api user`: HTTP 401,
  `Requires authentication`. Public repository/run/job/PR reads nevertheless work.
- Authenticated GitHub connector repository lookup works and reports Velnor
  push/admin permissions. SSH fetch and `ls-remote` work. Push untested.
- Connector run/job/artifact list wrappers expose first page only; they cannot
  establish complete inventories. Use paginated REST where accessible.

## Evidence and experiment rules

Preserve raw timestamps, run attempt, source/configuration/runtime revisions,
event, runner, toolchain, features, cache state, result and direct URLs.
Never use `updated_at` as execution completion. Keep wall latency separate from
aggregate job execution; pre-start delay is not necessarily runner queue time.
Unknown fields stay unknown. Failed/canceled runs are not successful baselines.

Each experiment records ID, hypothesis/root cause, alternatives/primary sources,
controlled inputs, baseline/candidate SHAs, run IDs/attempts, measurements,
coverage, independent reviewer, verdict, resulting commit and next question.
Documentation, unchanged reruns, and untested suggestions do not count.

Substantive completed iteration count: **0**. Plateau counts: **0**.
