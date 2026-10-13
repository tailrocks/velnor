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
| Velnor inventory | `/root/velnor_inventory` | current source, PR diffs, historical jobs | source/runtime distinctions, measured bottlenecks, compatible fixes | bounded inventory delivered; full history pending |
| Jackin inventory | `/root/jackin_inventory` | current source, PR diffs, desktop/Swift runs | product graph, tool boundaries, duplicate-work proof | running |
| Parallax inventory | `/root/parallax_inventory` | current source, PR diffs, failed/scheduled runs | actual language graph, prerequisites, scheduler outcomes | bounded inventory delivered; full history pending |
| Timing collection/tooling | `/root/parallax_inventory` (reused) | raw paginated run/job/attempt data | reproducible JSONL/CSV and ranked cohorts | design and independent challenge |
| Typed validation stages | `/root/velnor_inventory` (reused) | scan and opaque runtime command contract | exact scoped commands, visible stages, regression tests | design and independent challenge |
| Compiler/cache/tools | next available agent | inventory + cache statistics | alternatives with compatibility and trust constraints | queued |
| Swift/Docker/artifacts | next available agent | product inventory + primary sources | explicit producer/consumer contracts and experiments | queued |
| Relevance/scheduling | next available agent | events, gates, transitive inputs | scenario matrix and fail-closed checks | queued |
| Independent review | `/root/jackin_inventory` | hypotheses, diff, raw measurements | recorded findings before acceptance | first stage and collector designs queued |
| Integration/final checks | parent | reviewed units | small signed commits, regular pushes, exact-SHA CI | active |

Dependency order: inventories → ranked baseline → independently challenged
hypothesis → implementation → focused checks → controlled CI → independent
results review → commit/push → rerank. Independent research runs concurrently;
timing experiments must account for shared runner contention.

## Initial revisions and access

- Velnor initial local main: `1048337062ea625fada1b4f7c07f2feed75f60c7`.
- Refreshed Velnor remote main: `e94b48406c4ed206fce2bbf39b788264e72cf39c`.
  Clean local main fast-forwarded before creating `codex/ci-performance-campaign`.
- Jackin remote main: `41796158b1e45535ae4e74d5ff048cb5bb4e0488`.
  Initial local `3b1e1fc0` fast-forwarded cleanly.
- Parallax remote/local main: `6a12bf47a816b63e848b563aaa45ef9694159c79`.
- Each consumer uses one new `codex/ci-performance-campaign` working branch;
  initial branches were clean main, so feature branches isolate review from main.
- `rtk` 0.49.0 available. Missing `gh` installed using Homebrew: 2.101.0.
- `gh auth status`: existing account token invalid; `gh api user`: HTTP 401,
  `Requires authentication`. Public repository/run/job/PR reads nevertheless work.
- Authenticated GitHub connector repository lookup works and reports Velnor
  push/admin permissions. SSH fetch, `ls-remote` and campaign push work.
- Connector run/job/artifact list wrappers expose first page only; they cannot
  establish complete inventories. Use paginated REST where accessible.
- Generic authenticated `github_fetch` supports explicit pagination; verified
  after CLI public REST hit HTTP 403. See [baseline](baseline.md).
- Velnor draft [PR #968](https://github.com/tailrocks/velnor/pull/968) tracks the
  branch. Documentation-only source `c17c6ad09c4eb9617fbf8372914f50b9195a67ec`
  triggered PR run `35480462500` and policy run `35480462376`. These form a
  negative relevance diagnostic; conclusions pending.
- Current generator build (`--locked --no-default-features`) and dry-run scans
  succeeded for all three checkouts. These are local checks, not CI acceptance.

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
