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
| Velnor inventory | `/root/velnor_inventory` | current source, PR diffs, historical jobs | source/runtime distinctions, measured bottlenecks, compatible fixes | bounded inventory delivered; 30-day metadata collected; obligation mapping and detailed job evidence ongoing |
| Jackin inventory | `/root/jackin_inventory` | current source, PR diffs, desktop/Swift runs | product graph, tool boundaries, duplicate-work proof | tool-boundary patch written; independent actual-Mise validation ongoing |
| Parallax inventory | `/root/parallax_inventory` | current source, PR diffs, failed/scheduled runs | actual language graph, prerequisites, scheduler outcomes | bounded inventory delivered; 30-day metadata collected; obligation mapping and detailed job evidence ongoing |
| Timing collection/tooling | `/root/parallax_inventory` (reused) | raw paginated run/job/attempt data | reproducible JSONL/CSV and ranked cohorts | committed and pushed f8ac97b1; independent review, 16 timing tests, fmt and clippy pass |
| Typed validation stages | `/root/velnor_inventory` (reused) | scan and opaque runtime command contract | exact scoped commands, visible stages, regression tests | inferred shell-classifier draft rejected and stashed; typed constructor design queued |
| Compiler/cache/tools | `/root/velnor_inventory` (reused) | inventory, PR #967, actual quota failure | alternatives with compatibility and trust constraints | PR #967 integrated and pushed f0fb1c01; cold CI observed; warm comparison pending |
| Swift/Docker/artifacts | next available agent | product inventory + primary sources | explicit producer/consumer contracts and experiments | queued |
| Relevance/scheduling | next available agent | events, gates, transitive inputs | scenario matrix and fail-closed checks | queued |
| Independent review | `/root/jackin_inventory` | hypotheses, diff, raw measurements | recorded findings before acceptance | collector PASS; consumer pin reviews delivered; Parallax source2 HOLD; gate replay PASS with admission gap; policy identity patch under independent review |
| Integration/final checks | parent | reviewed units | small signed commits, regular pushes, exact-SHA CI | active |

Dependency order: inventories → ranked baseline → independently challenged
hypothesis → implementation → focused checks → controlled CI → independent
results review → commit/push → rerank. Independent research runs concurrently;
timing experiments must account for shared runner contention.

Current bounded queue (2026-09-20, after `112e6acc`):

- Parent committed and pushed telemetry schema 2, complete cache-key evidence
  checks, separate host declarations, regenerated action, and executable
  regression fixtures as `112e6acc`. Independent source-action matrix: 24/24;
  isolated generator report/parity tests: 8/8; actionlint passes. The exact
  committed binary builds and generation check passes with the candidate-render
  notice. Real CI acceptance remains pending.
  Exact all-target Clippy found three overlong regression-test functions;
  assertion-preserving helper extraction landed as `ebc05ab0` (same tree as
  local signed commit `7397441a`). Independent strict Clippy and the parent's
  six focused tests passed. Clean binary reports exact revision `ebc05ab0`.
  Policy
  run `35503026531` failed after 15 minutes waiting for the candidate product.
  PR #968's refreshed API response reports `mergeable=false` and
  `mergeable_state=dirty`; reconcile upstream before expecting a fresh PR run.
- Velnor agent independently reviews upstream integration, required-gate
  preservation, and the controlled rerun mechanism. It now owns the collector's
  reused-result/fresh-attempt distinction. Completed MBX classification and
  test refactoring are committed; timing-category repair remains separate.
- Jackin agent owns transitive Mise task/tool resolution, lock/platform identity,
  structured task execution semantics, and typed installation planning. Prior
  independent findings must be resolved before this unit lands. Disabling broad
  task auto-install depends on that model and real tool availability checks.
  Phase 1 passed 14 focused tests and library Clippy, but runner-name inference
  still misclassifies Windows ARM/custom runners. Replace guessed architecture
  with explicit runner identity before acceptance. That agent independently
  reviews the publication rollback boundary while implementation is paused.
- Parallax agent owns the upstream Bash 3 publication-rollback repair and then
  the main policy/candidate dependency cycle. The first rollback candidate was
  rejected: `if ( verifier )` suppresses `errexit`, allowing an early failing
  command followed by success to return zero. Keep that negative case.
  The replacement background verifier preserves failure status in five parent
  Bash 3 probes and 43 independently executed package tests. Cancellation and
  Bash 5 execution remain open; a simpler foreground process is under review.
- Source identity repair has a bounded patch and eight focused fixtures,
  including staged paths and a 20,000-path pipe test. Parent checks standalone
  coherence with the local candidate binding before integrating it. A local
  binding is never CI artifact provenance.
- Upstream main advanced through `e717de39` to `9307861d` (package publication
  transactions, cache quota handling and shell-fragment scoping). Parent's
  detached integration preserves campaign changes and the declared `325719f1`
  runtime pin. Against `e717de39`, all-target Clippy passed; full nextest found
  1,937 passes and one Bash 3 rollback failure. The merge remains uncommitted
  until the regression is corrected and reviewed.
- Actual `e717de39` main jobs wait on policy; policy waits for a candidate
  emitted by one of those jobs and searches PR runs only. Runs `35499213737`
  and `35499885181` failed acquisition; preview `35499213813` failed
  generated-tree policy. This blocks correct default-branch cache seeding,
  as well as release verification. Repair the graph and run identity contract.
- Early candidate production, immutable consumer runtime distribution, typed
  visible stages, selected-but-unadmitted gate rejection, cache transport/reuse,
  desktop product reuse, Docker input closure, and dispatcher child-result
  propagation remain required work. They are dependencies, not waived scope.

Latest completed successful PR run `35493166478` (`df9fb272`) had 68 recorded
jobs: 614 seconds trigger to final completion, 2,813 seconds aggregate execution.
Largest job: generator, 574 seconds. Separate policy run `35493165389` failed.
The previous `21799655` PR run `35492702575` passed in 596 seconds, with 3,018
seconds aggregate execution; its separate policy run also failed.

Historical collector rerun `35491248265`, attempt 2, succeeded: 40-second job,
18-second checks. Only the collector and two gates freshly executed; seventeen
successful results retain earlier timestamps despite new IDs and attempt 2
labels. This is a bounded capability probe, not a new full-workflow sample.

Rust-order candidate run `35492230871` passed in 604 seconds (aggregate
2,734 seconds), versus fixture run `35491248265` at 592 seconds (aggregate
2,705 seconds). Single observations are not matched repeated measurements.
The collector check interval rose from roughly 14 to 21 seconds; this remains
an unresolved possible regression. Figures use raw job completion timestamps,
never workflow `updated_at`. None establishes a speedup or completed plateau.

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
- Existing attested e94 runtime promoted through the generator in all three
  repositories. Exact-runtime checks pass; see
  [consumer experiment](experiments/CONSUMER-RUNTIME-001.md) and
  [independent review](reviews/consumer-upgrade.md). Parallax PR #112 and Jackin
  PR #1007 are pushed. Real CI exposes failures; no performance acceptance.
- Velnor main advanced to `325719f1e05d3d46322c9fd3eeb9ad545e175638` during
  execution. Integrated its package consumer rendering fix in merge
  `58447892`, preserving the campaign pin and regenerating ownership in a
  clean detached integration checkout. The old renderer passed its own check but
  source-generator CI correctly rejected revision 53 versus 54. Commit c0a46790
  promoted the attested 325719 runtime and repaired that mismatch. Latest f8ac97b1
  PR run 35483490582 passes all selected units; separate policy 35483489289 fails
  candidate/pin closure validation. The rejected stage draft remains stashed.

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

## Current integration checkpoint

Main advanced to `89f82dd8b287f46a3cf4c0920f341f6ca6c736db` (PR #969).
Merge `266dd76e` retains campaign gate/transport fixes and integrates per-member
cache routing. Combined generator passed 1,782 library tests, all-target Clippy,
actionlint and generation check. Exact pushed CI remains pending.

The f0fb1c01 PR run `35484350008` failed documentation lint; its Rust and Docker
jobs passed. Documentation repaired in `724ee71`. Separate policy run
`35484349032` timed out after 15 minutes awaiting its candidate artifact. This
is distinct from the earlier candidate/pin closure failure. Both remain evidence;
neither failed run is a successful speed baseline.

Publication currently uses the authenticated Git-object API after SSH signing
failed. Exact tree/parent checks, non-force ref updates, and preserved local
commits keep this reviewable; see [access evidence](access-limitations.md).
