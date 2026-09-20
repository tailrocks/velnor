# CI performance campaign

Status: active; no performance improvement accepted yet. Started 2026-09-20.

This directory is the canonical record for the cross-repository campaign.
Historical work remains in [the September 16 cache plan](../2026-09-16-ci-workflow-and-cache-plan.md)
and [the September 17 behavior ledger](../2026-09-17-pr994-behavior-ledger.md).
Their claims require revalidation against current revisions.

## Current measured checkpoint (8e1262d7 / 07bc3e23)

Parallax PR #119 at `8e1262d75cdf8b091f479ef4639b139041e47eff`
passed full CI `35525629941` and policy `35525629889`. All 24 jobs executed.
Required-result latency was 756s; final control completion was 762s;
aggregate job execution was 3,666s. Server took 723s, CLI 562s.
Actual checkout `1ba04c383691f97b8cb49bb72af6c74949a1e686` has the same
Git tree as the pushed head, with current main and that head as parents.

This is one cold-MBX observation, with exact Rustup, Cargo and mold restores.
The action explicitly logged `No mbx cache found` for server and CLI;
the older schema-1 reporter incorrectly labeled both as prefix restores.
Neither job obtained a compiler cache hit. Directory import was not exercised.
Post-MBX steps took 48s and 46s respectively; their internal work is not
observable from the logs and is excluded from the older phase report.
Raw timestamps, logs, normalized rows and this correction are retained under
`observations/parallax-35525629941-*`. No speedup is accepted from this sample.

Velnor evidence commits through `07bc3e23` are pushed. Exact-head PR
`35526876220` and policy `35526874804` passed. Environment partitioning,
Swift condition composition and same-repository PR cache writing remain
unaccepted local candidates with independent review findings under repair.
Parent integrates serially; Velnor agent owns environment partitioning,
Jackin agent reviews that work and repairs condition composition, and Parallax
agent owns the cache-writer repair. Older ownership notes below are historical.

## Latest measured checkpoint

Release metadata staging (`57e7cafc`) and rolling-release recovery
(`27bfb54b`) passed exact-head PR and policy CI. Latest full PR
observations are retained below; they are not matched performance treatments.

| Source | PR run | Trigger to required result | Aggregate job execution | Runner | Generator |
| --- | --- | ---: | ---: | ---: | ---: |
| `57e7cafc` | 35510807365 | 443s | 1,266s | 413s | 182s |
| `413458df` | 35511219049 | 541s | 1,290s | 466s | 175s |
| `27bfb54b` | 35511815559 | 559s | 1,359s | 525s | 184s |
| `32733948` | 35513481769 | 546s | 1,324s | 518s | 170s |
| `2da37b63` | 35517547611 | 539s | 1,320s | 475s | 186s |
| `17318e52` | 35519321056 / 1 | 568s | 1,263s | 520s | 178s |
| `17318e52` | 35519321056 / 2 | 543s | 1,338s | 514s | 169s |

Each observation includes 68 job records: 20 executed and 48 skipped.
Generator coverage increased from 1,947 to 1,955 passing tests; runner coverage
increased from 2,516 to 2,522 passing tests, with five existing skips and one repeated leaky-test
result. No introduced-flake absence or compiler-cache reuse is established.
Raw timestamps, compressed logs, collector JSONL/CSV and summaries are in
`observations/velnor-<run>-*`. Completion uses job timestamps, never `updated_at`.
Runner jobs remain the longest observed component. These differing revisions,
uncontrolled hardware/contention and small samples support no speedup or
plateau claim. Substantive optimization iteration credit remains zero.

The unchanged `17318e52` full rerun passed with 20 executed and 48 skipped jobs.
Attempt 2 has separate raw attempt metadata, job timestamps, logs and normalized
outputs; attempt 1 is retained. These two observations are not a noise estimate
sufficient for a speedup claim. The PR was merged between attempts, cache
contents may differ, and new telemetry correctness CI overlapped the latter part
of attempt 2. Runner reports still show 1,879/1,807 operations not looked up and
131 bypasses; there were no recorded crate downloads. Schema 2 phase categories
remain historical evidence and must not be treated as schema 4 measurements.

## Current integration checkpoint

Commit `7864f9d` retains unowned rolling tags and passes the independently
replayed regression tests. Its exact-head policy run `35515212764` failed:
candidate acquisition waited 900 seconds, and no same-head PR run existed.
Policy, actionlint and full coverage did not execute at that head. Preserve
this failed attempt; the preceding successful runs cannot substitute for it.

Git transport reports main `386a5b63`, while the sampled PR REST response still
reported base `9e5c0eb2` and a merge conflict. Integration includes the newer
main history, its release-leg seed/pin-fetch fix and typed runner failures.
The runtime pin advances to upstream `38dbf85e` only with independently checked
product identity and provenance. This is compatibility repair, not a speedup.

Integration `2da37b63` passed PR `35517547611` and policy `35517545736`.
The PR executed reusable workflows at merge revision `74e1b66d`; raw run
metadata identifies source head `2da37b63`. Required-gate latency was 539s;
the final control job completed at 545s. Policy executed for 258s separately.
The runner remains longest, with 1,879/1,807 MBX operations not looked up and
131 bypasses in each check/test report. Successful CI does not prove reuse.

PR #968 was merged as `845d4740`; its remote campaign branch was deleted.
The merge tree is byte-identical to `17318e52`. Further work uses one successor
branch, `codex/ci-performance-next`, based on that merge.

Current ownership: parent integrates reviewed code and retains exact-head CI;
Jackin agent implements the same-repository PR cache writer; Velnor agent
prepares Parallax's directory-transport runtime upgrade; Parallax agent reviews
complete package input selection. Typed Rust ownership and the portable
telemetry clock have passed independent review.

Parallax UI guard/input selection is published as `43d18415` in PR #118, based
on current main `e28a88ec`. It retains runtime `048a7bda`, GitHub-only execution,
scan exclusions and declared UI prerequisites. Policy run `35521049485` passed.
PR run `35521049612` failed its CLI job before checks: MBX 1.11.1 exceeded quota
while importing a 1,347,231,023-byte compressed cache. This attempt is retained;
local validation does not replace successful full CI. Isolated Rust consumers
still build their own UI products, so no build-once or speedup claim is made.
Upstream #117 cache safeguards were integrated and pushed as `60cf651b`,
retaining the UI guard, whole-package watch and both sets of contract assertions.
The structural runtime transport upgrade remains a separate candidate.

## September 20 follow-up checkpoint

Parallax `60cf651b` passed PR [35522592113](https://github.com/tailrocks/parallax/actions/runs/35522592113)
and policy [35522590702](https://github.com/tailrocks/parallax/actions/runs/35522590702).
Its required result completed 841 seconds after trigger; aggregate execution
was 2,028 seconds. Server took 787 seconds, CLI 201 seconds, UI 111 seconds.
The provisional 10× targets are 84.1 seconds end-to-required, 78.7 seconds
server and 20.1 seconds CLI. One observation is not a baseline distribution.
Server reported zero MBX hits, 1,654/1,569 operations not looked up and
128/126 bypasses despite a prefix archive restore. Compatible compiler reuse
remains unproven. The original failed quota attempt remains in the ledger.

Velnor telemetry commit `de6e1811` passed PR
[35522199636](https://github.com/tailrocks/velnor/actions/runs/35522199636)
and policy [35522199497](https://github.com/tailrocks/velnor/actions/runs/35522199497).
Required-result latency was 593 seconds; aggregate execution 1,367 seconds.
Runner took 542 seconds; generator 180 seconds. Corresponding provisional
10× targets are 59.3, 54.2 and 18 seconds. Generator schema 4 measured candidate
preparation/publication as 3/7 seconds; post-job cache cost remains explicitly
unobserved. Raw timestamps, normalized rows and compressed logs are retained.
No speedup comparison is valid across these differing revisions and cache states.

Upstream runner changes from `97bac4c` were integrated without modifying their
source as `3fb38643`. All 2,997 core tests passed, with five existing skips
and one checkout-process leak report. Policy passed; its PR run was cancelled
when the next candidate was pushed. The preceding `49b8e560` policy run was
also cancelled; no successful exact-head PR run exists for that revision.
Retain these attempts rather than substituting adjacent successes.

Typed Mise/Rust ownership is committed as `4f70cf74` after independent review,
1,988 tests and strict Clippy; one APT-process leak report remains investigated.
Complete package-input selection is committed as `c107796` after independent
review, 1,991 tests and strict Clippy. It broadens correctness coverage for
root packages and does not itself establish a performance improvement.

Current ownership: parent integrates, commits, pushes and collects exact-head
evidence; Jackin agent reviews Parallax runtime transport then prepares Jackin
consumer migration; Parallax agent reviews the same-repository PR cache writer;
Velnor agent implements reviewed environment partitions and diagnoses leaked
processes. Consumers and runtime candidates depend on reviewed generator source.
Substantive iteration and plateau credit remain zero.

## Reviewed timing and source identity follow-up

`4f70cf74` passed [PR CI 35523884300](https://github.com/tailrocks/velnor/actions/runs/35523884300)
and [policy 35523882838](https://github.com/tailrocks/velnor/actions/runs/35523882838).
Independent raw-timestamp review confirmed 451 seconds to `ci-required`,
1,455 seconds aggregate execution, 408 seconds runner and 233 seconds generator.
Generator checks rose to 169 seconds while runner checks fell to 376 seconds;
source changes and compiler misses preclude causal attribution. See
[the independent timing audit](reviews/fresh-timing-review.md).

Collector rows conservatively leave source identity unknown when raw API
metadata cannot prove it. Supplemental checkout logs and immutable commit
responses now identify the actual source used by the sampled jobs:

| Run | Effective checkout | PR head | Equal source trees |
| --- | --- | --- | --- |
| 35522199636 | `134a993fe4f91e24dfb7fe47daa763cdb022acc1` | `de6e1811` | yes |
| 35522592113 | `ce04e0c0933922ec83e2bb43cb210f241623681d` | `60cf651b` | yes |
| 35523884300 | `8a9041da6b6c31caf39850fd0f458679576a06e6` | `4f70cf74` | yes |

The supplemental JSON retains exact parents, tree IDs, log lines and digests.
Tree equality does not erase the merge SHA or prove compatibility for inputs
derived from Git metadata. Normalized API-only rows remain unchanged.

The proposed same-repository PR cache writer remains held for ShellCheck,
local-export versus durable-save reporting, mode coupling and bounded timing
repairs. Its [independent review](reviews/mbx-pr-writer-review.md) records all
findings; no unreviewed writer has been integrated.

## Identical-input repetition and API interruption

Run [35524310993](https://github.com/tailrocks/velnor/actions/runs/35524310993)
passed twice at head `ce75f7c3`. Both generator checkout logs prove effective
merge `8955c765dd607b54a9f0f8fdfe440ca4b95b5a88`; Git transport independently
verified its parents and tree, which matches the source head.

| Attempt | Required result from attempt start | Aggregate execution | Runner | Generator |
| --- | ---: | ---: | ---: | ---: |
| 1 | 542s | 1,558s | 517s | 226s |
| 2 | 559s | 1,622s | 532s | 220s |

Attempt 2 reports `created_at` one second after `run_started_at`; the table
uses the latter and retains both raw values without interpreting that skew as queue time.

These are identical-input observations, not proof of a fully warm compiler
cache. Both restored the same older MBX prefix archive and reported the same
high unconsulted/bypass counts. Rustup and mold were exact hits; Cargo used a
prefix restore; no origin downloads were reported. The sample size of two
does not establish a stable noise distribution, tail estimate or speedup.
The rerun counts as a repetition within the same experiment, not an iteration.

Parallax #118 merged upstream as `3a657a54` with the validated `60cf651b` tree.
Reviewed directory-transport runtime candidate `1aeb47e` was integrated with
that history as `8e1262d7`, preserving the identical candidate tree, and pushed
in [PR #119](https://github.com/tailrocks/parallax/pull/119). Its policy run
`35525629889` passed; full CI `35525629941` was still running at last observation.

At 17:26:50 UTC, GitHub rejected that run poll with HTTP 403 for exhausted core
API quota. One bounded retry confirmed remaining 0 of 5,000 and reset
17:39:16 UTC. The generic rate endpoint contradicted the actual response, so
the failed resource's headers govern retries. API observation is temporarily
unavailable; local implementation, tests and Git transport continue. Exact
operations and limitations are retained in `observations/ci-campaign-api-blocker.json`.
No unseen CI result or campaign completion is claimed.

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
