# CI performance campaign

Status: active; no performance improvement accepted yet. Started 2026-09-20.

This directory is the canonical record for the cross-repository campaign.
Historical work remains in [the September 16 cache plan](../2026-09-16-ci-workflow-and-cache-plan.md)
and [the September 17 behavior ledger](../2026-09-17-pr994-behavior-ledger.md).
Their claims require revalidation against current revisions.

## Latest measured checkpoint

Release metadata staging (`57e7cafc`) and rolling-release recovery
(`27bfb54b`) passed exact-head PR and policy CI. Three latest full PR
observations are retained below; they are not matched performance treatments.

| Source | PR run | Trigger to required result | Aggregate job execution | Runner | Generator |
| --- | --- | ---: | ---: | ---: | ---: |
| `57e7cafc` | 35510807365 | 443s | 1,266s | 413s | 182s |
| `413458df` | 35511219049 | 541s | 1,290s | 466s | 175s |
| `27bfb54b` | 35511815559 | 559s | 1,359s | 525s | 184s |

Each observation includes 68 job records: 20 executed and 48 skipped.
Generator coverage increased from 1,947 to 1,952 passing tests; runner coverage
retains 2,516 passing tests, five existing skips and one repeated leaky-test
result. No introduced-flake absence or compiler-cache reuse is established.
Raw timestamps, compressed logs, collector JSONL/CSV and summaries are in
`observations/velnor-<run>-*`. Completion uses job timestamps, never `updated_at`.
Runner jobs remain the longest observed component. These differing revisions,
uncontrolled hardware/contention and small samples support no speedup or
plateau claim. Substantive optimization iteration credit remains zero.

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

Current ownership checkpoint (2026-09-20, after `27bfb54b`):

- Parent: preserve and publish measured evidence, integrate independently
  reviewed units, run deterministic checks. Shared Git operations stay serial.
- Velnor agent: telemetry phase repair in `/tmp/velnor-telemetry-413458`.
  Separate candidate preparation/publication and explicit cache save; distinguish
  skipped/failed work and unobserved post-job MBX save. Parent review found
  false durations from unconditional markers and incorrect save-action semantics.
- Parallax agent: independent rolling-release review, then clean consumer runtime
  convergence. Larger pending consumer migration remains isolated and unaccepted.
- Jackin agent: full PR #962 ARM comparison, independent candidate-contract
  review, then typed transitive Mise/Rust tool boundaries for Jackin.
- Candidate contract draft is held: malformed generated YAML, missing local
  candidate binding, and trusted pinned-policy permission incompatibility.
  Diagnostic patch/logs are preserved separately; no draft was published.
- Contract and telemetry edits now use separate real Git worktrees after an
  overlapping scratch-checkout draft was detected and rejected. Parent integrates
  only scoped patches; no synthetic-checkout whole-file replacement.
- Dependency chain: trusted runtime/contract rollout → candidate graph → exact
  consumer regeneration → controlled CI cohorts. Local source identity remains
  a separate prerequisite; no artifact or policy check is waived.
- Typed stages, Mise tool selection, relevance, scheduled child outcomes,
  native ARM production and Swift/product reuse remain required work.

Prior checkpoint (2026-09-20, after `56017bac`):

- Telemetry schema 2 and its assertion-preserving test helpers are published
  (`112e6acc`, `ebc05ab0`). Exact committed generation check passed. Compiler
  reuse, telemetry phase attribution, and real CI acceptance remain separate.
- Collector attempt freshness is published as `56017bac`: mixed reused/fresh
  results withhold full-workflow timing while preserving marked partial
  observations. Independent raw replay, 48 package tests, and strict Clippy
  passed. Historical attempt 2 remains a bounded rerun, not a warm benchmark.
- Parent integrates upstream `9e5c0eb2`, including existing package transaction,
  MBX quota, and sccache grouping fixes. Independent review approves exact
  published runtime `4fa7a3a8`; manifest and macOS binary provenance are retained.
  Generated workflows, actionlint, strict Clippy, formatting, and all 1,942
  tests pass. Final independent review and exact committed rebuild precede CI.
- Parallax agent repairs publication verification with a foreground Bash child.
  Conditional subshell and background alternatives were rejected. Independent
  review approves process isolation, exit handling, cancellation, and lock
  fencing. Full checks found fixture lint issues and a forbidden repository
  literal; repaired helpers retain every assertion and pass the full suite.
  This agent then resumes the
  dedicated candidate bootstrap producer and main policy dependency graph.
- Jackin agent repairs transitive Mise tool closure and installation. Independent
  review found task-local `key@version` incompatible with Velnor runner's bare
  lock-key contract, missing single-table task references, and mismatched runner
  platforms. Generic integration remains held until those contracts are sound.
- Velnor agent independently investigates actual upstream preview failures:
  missing ARM cross-linker and dirty source identity. PRs #960/#962 are discovery
  leads; their actual diffs must be checked before reusing any implementation.
- Source identity has a bounded candidate patch and focused fixtures, including
  staged paths and a 20,000-path pipe test. Standalone coherence with local
  candidate binding remains unverified. Local identity is not CI provenance.
- PR #968 remains merge-conflicted until this integration lands. Policy runs
  `35503026531` and `35505010001` failed after waiting fifteen minutes for absent
  candidate products. No successful full candidate validation is inferred.
- Immutable consumer runtime distribution, typed visible stages, required-gate
  admission, cache transport/reuse, desktop products, Docker input closure, and
  dispatcher child-result propagation remain required work, not waived scope.

See [upstream integration review](reviews/upstream-9e5-integration-review.md).

Latest published repair `fecc59e9` passes fresh Linux PR run `35508236735`
and policy run `35508235601`. Generator: 1,943 tests passed, none skipped.
PR trigger-to-required: 458 seconds; aggregate execution: 1,210 seconds.
Runner remains largest at 413 seconds, generator 186 seconds. One observation
is not performance acceptance. The preceding failed run remains below.

Jackin branch advanced to `f4054488` with the attested `4fa7a3a8` runtime and
current main. PR `35505323064` and policy `35505321838` pass, but the PR executes
only planning and gates; expensive jobs are skipped. It is not full build proof.
The existing desktop edits were preserved during local fast-forward.

Latest pushed integration `3a7da430` has an exact clean build and generation
check, but PR run `35506628393` failed. Its generator job received SIGTERM during
new cancellation fixtures; required gates failed. The fixture's external process
signal parser matches a documented Ubuntu procps defect. The typed `rustix`
repair preserves cancellation coverage and passes 1,943 local tests; independent
review and new Linux CI remain required. See
[signal ownership experiment](experiments/V-ROLLBACK-SIGNAL-001.md).

The failed PR took 577 seconds (1,368 aggregate execution seconds). Runner crate
job: 546 seconds, with 506-second checks and no recorded crate downloads. Its
compiler/cache counters remain under investigation. These failed-run numbers do
not establish a speedup. Separate policy run `35506627126` failed acquisition
after 582 seconds when the PR finished without publishing its candidate; it did
not consume the full fifteen-minute deadline.

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

## Historical integration checkpoint (266dd76e)

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
