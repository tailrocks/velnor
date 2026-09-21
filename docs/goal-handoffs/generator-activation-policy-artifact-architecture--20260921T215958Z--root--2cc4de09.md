# GOAL: Redesign Velnor generator activation, policy validation, and CI artifact dependency architecture

**Handoff ID:** `generator-activation-policy-artifact-architecture--20260921T215958Z--root--2cc4de09`  
**Created / last updated UTC:** 2026-09-21T21:59:58Z / 2026-09-21T22:11:00Z  
**Original goal status:** `PAUSED_BY_USER`  
**Handoff status:** `BLOCKED`  
**Runtime stop:** verified: `update_goal(status=paused)` returned the original objective and `paused` at 2026-09-21T21:59:40Z. This expresses user pause; it does not cancel already-created GitHub runs.  
**Source agent:** `/root`, Codex session `01a0c510-7a18-7680-8954-8a6121240bb9`; `rtk 0.49.0`.  
**Repository:** `tailrocks/velnor`, `git@github.com:tailrocks/velnor.git` (sanitized).  
**Source checkpoint:** local `codex/activation-foundation` `d97645d917c1fb7680af8678c9243286ec30963f`; observed `origin/main` `45ef1ebef769c78f45315e11a798fdaafaef4c4e`.  
**Preservation branch / PR:** `codex/goal-handoff-2cc4de09`; [draft #1064](https://github.com/tailrocks/velnor/pull/1064); final published head is reported in the PR body.  
**Portability:** code checkpoint is remote-portable after publication. The active pinned renderer binary is local-only at the path stored in `/tmp/velnor-active-render-path`; rebuild it from pin `eed474c4a1d9b071fd1b5de00c769c8997398e5a` with the locked toolchain.  
**Resume authorization:** only a later explicit user request. `PAUSED_BY_USER` is not completion or abandonment.

## B. Original goal and success contract

### Recovered verbatim core request

> Redesign and implement Velnor's generator activation, policy validation, and CI artifact dependency architecture. Treat the two reported incidents as architectural bugs, not isolated YAML mistakes or slow-runner problems. Remove the structural conditions that permit them, integrate the redesign, and verify the resulting hosted workflows.

> This is an implementation goal, not a request to stop after research, a design document, a pin bump, or recommendations.

Incidents: Preview run `35623133972` job `106410846252`; Main run `35623133871` job `106410848097`.

### Consolidated explicit contract

The original request requires root-cause-first structural fixes; qualifying candidates separately from trusted active activation; one active-tree invariant for PR/prospective-main/main/Preview/release; authenticated multi-platform active products; typed source/build/binary/tree/run identities; explicit graph producer requirements and event compatibility; no guessed cross-workflow PR lookup or 900-second polling; trusted acceptance outside candidate control; parity on prospective integration; bounded, measured performance where every applicable step/job/critical path above 120 seconds remains a defect; behavioral and mutation regressions; protected integration; hosted main/Preview evidence; and a concise decision record.

User amendments require autonomous investigation, aggressive independent delegation, frequent signed commits/pushes on one working branch where safe, no questions, and after resumption integration of required related goal work plus safe cleanup only after verified integration. Repository rules add: use `rtk`; preserve generated outputs through the active pinned renderer; `actions/runner` is protocol authority; no legacy shims; commits use `-s` and `Co-authored-by: Codex <codex@openai.com>`.

Non-goals during this pause: implementation, repairs, merge/rebase/cherry-pick, remote protection changes, release/publication/deployment, or local cleanup.

## C. Exact interruption state

Last completed original-goal action: commit/push `d97645d9 fix(workflow): cancel superseded candidate qualification`, adding PR-scoped workflow concurrency and regenerated static output/state. Local branch was clean immediately before handoff document creation.

At pause, the current foundation PR was #1044. Candidate and policy hosted checks for `d97645d9` succeeded. CI run `35659890436` was superseded/cancelled by a later remote update. A later observation found remote `origin/codex/activation-foundation` and PR #1044 head had advanced independently to `60bb9326e6303c577bd15e952558f0dc02fd78f2`; local branch remains `d97645d9`. Do **not** force-push or assume ownership of `60bb`; reconcile it after resumption.

Paused workers: `bootstrap_review`, `activation_hosted_plan`, `phase_dag_design`, and `current_gate_watch` stopped with no edits. Handoff auditors were read-only. GitHub runs cannot be retroactively paused by goal state; observed newest external runs at interruption were Policy `35660437293` and CI `35660442225` for remote head `60bb9326`, plus successful candidate run `35660441660`. Do not repair or watch them during pause.

First resumption action: read this file and `AGENTS.md`; create an exclusive worktree from the published preservation branch; fetch/re-observe remote branch/PR #1044, main, rulesets, runs and all inventories; reconcile the unexplained remote `60bb` advance before any product change.

## D. Requirement progress ledger

| ID | Requirement | Status | Evidence | Remaining / dependency |
|---|---|---|---|---|
| R1 | Eliminate candidate-rendered active-tree exception in both active paths | IMPLEMENTED_UNVERIFIED | `4ad4b28e`, `a111a045`; `src/policy.rs`, `src/s2/policy.rs` | Activate renderer and prove current generated consumers; source alone is not active. |
| R2 | Remove guessed main-to-PR candidate acquisition and 900s loop | IMPLEMENTED_UNVERIFIED | `a111a045`; source emitter changes | Current pinned generated `ci-policy.yml`/`ci-main.yml` still contain old path until activation. |
| R3 | Qualify candidate in disposable untrusted lane | HOSTED_PARTIAL | `f7a73dc`, `d4651cf`, `d97645d`; candidate run `35659890240` success | Need independent authority, isolation/provenance/behavioral proof. |
| R4 | Protected authority cannot be candidate replaced/spoofed | BLOCKED | `2bb52c0`; bootstrap audit; ruleset `19573071` | Free org lacks required-workflow control; current required contexts unbound and byte rule only activates after renderer activation. Establish external App/checker or enforced trusted review authority. |
| R5 | Typed product/provenance/readiness and atomic activation | IMPLEMENTED_UNVERIFIED | commits `c7592f0c`..`2bb52c0`; `promote_readiness.rs`, `promote.rs`, activation/setup sources | Fix publisher checksum/tag contract/source-digest gaps; run lifecycle hosted. |
| R6 | Active durable runtime replaces fanout | IMPLEMENTED_UNVERIFIED | `09fb262e`; source runtime setup | Prove exact published manifest, all platforms, cache/revocation/retention and activate output. |
| R7 | Graph required producers/events/platform/trust/deadlines | IN_PROGRESS | `19673f83`, `30a7995c`; `s2/primitives/product_transport.rs` | Full typed EventContext/ArtifactRequirement graph/state machine and mirrored schema-1 work unfinished. |
| R8 | Behavioral/mutation/event matrix | IN_PROGRESS | policy/readiness/contract tests in branch | Add controlled resolver/fake clock, prospective-main, merge identity, cancellation/skip/spoof mutations. |
| R9 | Performance <=120s all paths | VERIFIED_VIOLATION | candidate prior run 217s; runner 426s checks; runtime publication 476s; Docker 788s | Phase-DAG/result identity, product reuse/partitioning and cold/warm measurements remain. |
| R10 | Protected integration/main/Preview evidence | NOT_STARTED | activation absent from main; #1044 unmerged | Must follow R4/R5, reread reviews, merge authorized increments, then hosted evidence. |
| R11 | Independent security/correctness review | PARTIAL | bootstrap and artifact/runner reviews below | Fresh review after combined active transition. |

## E. Change and preservation inventory

### E.1 Discovery scope / coverage

Observed at 2026-09-21T21:59Z: primary clone `/Users/donbeave/Projects/tailrocks/velnor-project/velnor3`, common Git dir `.git`; `git worktree list --porcelain -z`; local refs (`git branch -vv`); stashes; `git ls-remote`; GitHub PR all-state query (100-result page, output truncated); and known session paths `/private/tmp/velnor-*`, `/private/tmp/*worktree`, project sibling `dual-lane-*`, and `.muse/worktrees`. No destructive pruning/fetch/reset occurred. This repository has a large historical worktree estate; many entries are clearly unrelated previous campaigns. The preservation candidate and directly related resources are listed below. The resume agent must rerun complete NUL worktree/ref enumeration before cleanup; lack of full-path enumeration here is a **coverage limitation**, not deletion authority.

Ownership categories: primary W1 and B1 are `GOAL_EXCLUSIVE`; unknown remote update B2 and unrelated/sibling campaigns are `UNKNOWN`/`UNRELATED` and retained. No stash was observed in the pause checkpoint.

### E.2 Worktree / clone ledger

| ID | Path/type/common dir | Branch/HEAD | State / owner | Disposition |
|---|---|---|---|---|
| W1 | `/Users/donbeave/Projects/tailrocks/velnor-project/velnor3` main worktree / `.git` | `codex/activation-foundation` / `d97645d9` | goal coordinator; clean before handoff, then untracked handoff file | KEEP; preservation source. |
| W2 | `/tmp/velnor-active-render-*` detached build worktree (path recorded in `/tmp/velnor-active-render-path`) | `eed474c4` | local active renderer build, nonportable | KEEP until activation work ends; rebuildable. |
| W3 | `.muse/worktrees/subagent-v2-*` linked worktrees discovered in branch output | assorted old feature refs | historical/shared/unknown | REVIEW_SHARED; no cleanup in pause. |
| W4 | `/private/tmp/*` registered historical worktrees, many marked `prunable` | assorted detached/branches | unknown; paths included prior 929/g*/b*/d*/r* campaigns | REVIEW_SHARED; enumerate exact paths/state after resume before any action. |
| C1 | sibling independent project clones such as `../dual-lane-*`, `../velnor-evidence-checkpoint` observed in local branch worktree metadata | assorted | other campaigns / unknown | REVIEW_SHARED; no changes. |

No worker reported a unique dirty worktree, unpublished commit, untracked implementation file, stash, or ignored essential artifact. The sole new artifact is this Markdown handoff.

### E.3 Branch ledger

| ID | Ref/tip | Ownership / relation | Preservation / future action |
|---|---|---|---|
| B1 | local `codex/activation-foundation` `d97645d9` | GOAL_EXCLUSIVE foundation, 33 ahead / 0 behind observed `origin/main` at pre-pause | preserve remotely through handoff branch; later reconcile remote update and #1044. |
| B2 | observed remote `origin/codex/activation-foundation` `60bb9326` | UNKNOWN post-pause advance; PR #1044 head | retain; inspect commit, actor, diff and run results before integrating. |
| B3 | `origin/main` `45ef1ebe` at pre-pause | target | do not assume current; refresh before resume. |
| B4 | remote-only `codex/ci-performance-campaign` `155d6b81`; `codex/ci-performance-next` `970a6dd5` | related performance campaigns, not integrated | retain/review as related evidence; map PRs/checkpoint coverage after resume. |
| B5 | stale goal-related drafts #978/#979/#980 heads on base `97bac4c` | related but stale/conflicting | retain; inspect all comments/diffs; do not merge blindly. |

Other local branches/worktrees are excluded as unrelated unless their diff intersects R1-R10; the complete resume audit must classify them before cleanup. No known goal stash or reflog-only commit is the sole copy of needed work.

### E.4 PR ledger

| ID | PR / state | Head/base observed | Relation / future action |
|---|---|---|---|
| P1 | [#1044](https://github.com/tailrocks/velnor/pull/1044) open, non-draft | remote head `60bb9326`; main recorded `45ef1ebe`, later main has moved | primary original-goal foundation PR. No reviews; one nontechnical usage-limit comment. Do not merge while paused. Re-read all reviews and reconcile head/base. |
| P2 | [#1038](https://github.com/tailrocks/velnor/pull/1038) merged | historical recovery | recovery pin only; not architecture proof. |
| P3 | #978, #979, #980 drafts/stale | base `97bac4c` reported | inspect as overlapping proposals; account as integrate/supersede/reject with evidence. |
| P4 | #1050-#1058, #1063 open (observed all-state query) | varied current bases | related workflow work, ownership unknown. Do not merge/close in pause; reassess overlap. |

### E.5 Future integration map / landing plan

`W1 -> B1(d97645d9) -> preservation branch -> draft handoff PR; B2(remote 60bb) -> P1 #1044 -> main only after reconciliation.`

Future sequential plan: (1) establish independently controlled trusted authority; (2) reconcile/partition P1 versus B2 and related PRs; (3) fix runtime product checksum/tag/provenance contract; (4) validate/merge bootstrap only through actual protection; (5) publish exact multi-platform product; (6) dispatch and verify activation PR; (7) remove staged old generated polling by activation, rerender twice, then run main/Preview gates; (8) finish typed graph/event/resolver and performance DAG; (9) final independent review and target CI. Parallel research is allowed for graph, authority alternatives, performance and audit; target mutations coordinate sequentially.

### E.6 Future cleanup runbook

No cleanup was performed. For W2/W3/W4/C1/B1/B4/B5, first re-observe exact path/ref/head/status/processes. Remove only after: every change is in target or explicitly rejected with durable remote recovery; tracked/untracked/ignored/stash/nested state accounted; no agent/PR/recovery dependency; post-integration checks pass; actual tip/path/common dir match ledger. Use guarded `git worktree remove` for individually named clean linked worktrees, then guarded local branch deletion. Never bulk remove, force remove, prune, drop stashes, or delete remote branches. W1 is retained.

## F. Decisions and findings

* Root causes: PR policy allowed candidate tree while pin defined active tree; main guessed a PR producer at incompatible identity then polled 900s. Candidate self-render is qualification only, never active authority.
* Chosen lifecycle: trusted bootstrap authority -> isolated candidate qualification -> controlled immutable multi-platform publication -> atomic promotion of pin/config/generated tree/ownership.
* Artifact fanout incident: run `35647988900` had one exact same-run artifact consumer fail intermittent intermediary 403 while another succeeded; active consumers should use durable attested runtime, not fanout.
* Authority blocker: GitHub Free org ruleset `19573071` requires names `DCO`, `Policy`, `ci-required`, but names are unbound. Org required workflows returned HTTP 403 upgrade requirement. GitHub Actions App binding alone does not bind workflow source. An external App/checker or enforced trusted review authority is needed before claiming safe bootstrap.
* Runtime blocker: current release identity `closure-version:1` omits workflow/manifest contract; existing release check expects `.sha256` assets which release creation does not publish; old manifest lacks typed identity; exact source digest missing in existing-product verification.
* Performance: runner serializes Clippy/test despite separate phases. Measured `velnor-runner` successful main job: 426s checks/456s total; Clippy 140s and tests 281.5s. Candidate prior path 217s e2e; runtime publish 476s; Docker 788s. A phase DAG requires result identity `(unit, provider, phase, plan_digest, build_identity)` and aggregate rejection of missing/skipped/cancelled phases; splitting alone does not meet 120s.

## G. Verification evidence

* PASS at `d97645d9`: active pinned renderer `--plain --check`; `rtk cargo check -p velnor-workflow -p velnor-runner`; targeted runner manifest and schema-2 policy tests; contract tests earlier. Exact hosted-format correction commit `bb968cc6` used `cd crates/velnor-workflow-contract && mbx fmt --manifest-path Cargo.toml -- --check`.
* Hosted PASS at `d97645d9`: Policy `35659888289`, 52s total / 31s job; candidate `35659890240`, 115s total / 91s job.
* STALE/CANCELLED: CI `35659890436` cancelled by newer synchronized run; prior `35659465206` cancelled; no failed step establishes a product failure.
* FAIL/VIOLATION: historical Preview `35623133972/106410846252` ~17s active-tree failure; Main `35623133871/106410848097` ~920s / ~906s acquisition. Actionlint full availability was blocked by unauthenticated API rate limit; not a pass.
* NOT RUN: hosted activation lifecycle, post-activation deterministic regeneration, all supported consumers, protected main/Preview evidence, complete cold/warm campaign, final security review.

## H. Ordered remaining work

1. **First:** re-observe P1/B1/B2/main and all worktree/branch/PR inventories. Preserve unknown remote `60bb` and do not overwrite it.
2. Establish immutable independent acceptance authority; prove it is enforced for candidate/activation workflow changes.
3. Correct publisher checksum/tag-contract/provenance and typed manifest compatibility; add lifecycle regressions.
4. Complete typed event/artifact graph/state machine in root and s2; remove any fallback where a required producer can skip/fail.
5. Complete mutation/behavioral matrix and independent security review.
6. Design phase result graph, then optimize with measured cold/warm tests without hiding elapsed dependency time.
7. After gates, execute publish/activation lifecycle and protected integration; verify current generated output removes polling and active invariant across consumers.
8. Execute E.6 cleanup only after final integration proof.

## I. Environment / operational recovery

Use repository root, `rtk`, locked Rust toolchain, and active renderer rebuilt from `eed474c4`. Generated static files are controlled by `.github-gen/velnor-workflow.toml`; regenerate using the pinned binary, not manual edits. Main runtime publisher workflow is `ci-runtime-products.yml` (ID `359790572`); activation workflow is absent on pre-bootstrap main. Do not dispatch release/activation during pause.

## J. Fresh-agent resume runbook

```sh
cd /Users/donbeave/Projects/tailrocks/velnor-project/velnor3
git fetch origin --prune=false
git switch codex/goal-handoff-2cc4de09
/goal Read and resume docs/goal-handoffs/generator-activation-policy-artifact-architecture--20260921T215958Z--root--2cc4de09.md
```

Read this document and `AGENTS.md`; recover into an exclusive worktree; compare all live refs/PRs/runs/rulesets to the snapshot; resume the original goal at H.1. The pause ends only with an explicit user resume request. A `/goal Read and resume ...` request authorizes the original goal, not another handoff. Maintain this record, then after verified integration execute only E.6 candidates with fresh safety checks.

## K. Blockers, omissions, independent review

**Blockers:** trusted authority unavailable on current Free-org configuration; publisher contract incompatibility; universal 120s performance violations; activation unexercised. **Coverage limitation:** historical worktree estate is extensive; this document records discovery methods and related known resources but requires fresh complete enumeration before cleanup.

Independent pause reviews: bootstrap reviewer found P1 cannot be considered trusted bootstrap because its active policy does not enforce the newly added workflow-byte rule; phase-DAG reviewer found current aggregation is one result per unit/provider and must be extended before phase parallelism; hosted reviewer found checksum/tag/manifest-source-digest defects. These findings were incorporated. A fresh agent can recover the code and next action without this chat, subject to the explicit inventory refresh and remote-head reconciliation above.

## Pause addendum: final preservation observations

At 2026-09-21T22:05Z, a goal-owned local `gh run watch` process (`35975` parent and `36002` child) for Policy run `35660437293` was terminated; post-kill inspection produced no remaining PID output. The remote run itself was **not cancelled** because its triggering remote-head update was `UNKNOWN` ownership. Record it as still in flight at last observation, not stopped.

Additional related clean separate clone/worktrees found by the verification audit, all retained:

| ID | Path/common dir | Ref/HEAD | Purpose / disposition |
|---|---|---|---|
| W5 | `/private/tmp/velnor-1044-d976`, common `/Users/donbeave/Projects/tailrocks/velnor-project/velnor/.git` | `codex/activation-foundation-fixture-latest` / `60bb9326` | fixture-latest view of remote advance; REVIEW_SHARED. |
| W6 | `/private/tmp/velnor-1044-latest` | `codex/activation-foundation-latest-repair` / `c6979a8a` | variant bootstrap-fixture retirement work; REVIEW_SHARED. |
| W7 | `/private/tmp/velnor-1044-repair` | `codex/activation-foundation-repair` / `82935880` | variant bootstrap-fixture retirement work; REVIEW_SHARED. |

`60bb9326` deletes `crates/velnor-workflow/tests/bootstrap_transport.rs` and changes `selection_plan_handoff.rs` and `verify_set_build_inputs.rs`. It is not in local `d97645d9` and must be diffed/reviewed, not assumed safe. The current generated policy/main workflows still contain `Acquire candidate generator product`, `ci-pr.yml` lookup, `deadline +900`, and `sleep 15`; source removal is staged only. `actionlint 1.7.12` is installed now, but no final local actionlint run occurred.

## Publication receipt

Published preservation branch: `codex/goal-handoff-2cc4de09`. Draft PR: [#1064](https://github.com/tailrocks/velnor/pull/1064). Initial handoff commit: `a965d357cec42c9d5e6833083294489931060c01`. The final branch SHA is reported in the PR body and final response, rather than self-embedded here. Auto-merge was not enabled and no queue/merge action was performed.

## Audit expansion: branch, PR, worktree coverage (2026-09-21T22:09Z)

Independent audits found **369** registered worktrees in W1's common repository: 286 live, 83 missing/prunable, 74 branch-attached, 295 detached; no worktree/index locks. There are no stashes in W1. The separate older clone `/Users/donbeave/Projects/tailrocks/velnor-project/velnor/.git` has 85 live worktrees and 11 untouched stashes. Other discovered related clone common dirs: `/Users/donbeave/Projects/github/velnor/.git` (6 live); `/Users/donbeave/Projects/github/all-repo/tailrocks_velnor/.git` (23 live); `/Users/donbeave/Projects/tailrocks/velnor-project/velnor2/.git` (21 registered, 9 prunable). Discovery did not scan other machines or arbitrary private paths.

Known in-progress/dirty **shared or unknown** resources include numerous historical `/private/tmp/g*/`, `/private/tmp/velnor-*` fixtures and `/Users/donbeave/Projects/tailrocks/velnor-evidence-checkpoint` (587 modifications). Do not touch them. Specific unresolved operations: `/private/tmp/g1-pr955-merge.9aAKSZ` and `/private/tmp/velnor-pr953-954-merged` have `MERGE_HEAD`; `/private/tmp/velnor-latest-macos-policy` and `/private/tmp/velnor-rust-cache` have `REBASE_HEAD`; an old common clone has ambiguous stale `REBASE_HEAD`. These remain `REVIEW_SHARED`.

Additional W8: `/private/tmp/velnor-1044-lifecycle`, detached `d4651cfb`, clean, retained. No local-only worker implementation from this session is unpreserved; broader historical local-only branches are listed by the independent PR audit and must be classified, not removed.

Relevant server branch/PR map: `codex/ci-performance-campaign@155d6b81` has post-merged-#968 WIP; `codex/ci-performance-next@970a6dd5` is #978 draft; `fix/ci-validation-contract@9bbf4a4e` is #979 draft; `refactor/holla-parity@ab2f12fa` is #980 draft. #978/#979/#980 are stale and overlap 107/82/142 files pairwise. Keep their useful evidence/tests but do not merge intact. Focused open PRs #1050/#1052/#1054/#1055/#1056/#1057/#1058/#1063 overlap R1-R10 in the files/counts reported by the branch audit; re-evaluate each after live head refresh. #1063 is a selective port from dirty/stale #962, not blanket merge authority.

At audit end, remote candidate run `35660441660` had passed. Policy `35660437293` remained in progress; CI `35660442225` remained in progress and `velnor-tools` had failed. This is an observed pause-time state, not a repair task. The local watcher for Policy was stopped by coordinator; remote runs were retained.


### Independent handoff review disposition

Reviewer `/root/handoff_review` (read-only, after #1064 publication) verified durable code checkpoint and draft PR, but found the handoff does not meet the user-required exhaustive inventory standard. Therefore status is `BLOCKED`, not `READY`. Required administrative recovery before a READY handoff: enumerate/classify every one of the 369 registered W1 worktrees, each related worktree in the other discovered clone common directories, every local/remote related branch/ref and stash, and every related PR with exact head/base/check/review/disposition; provide a cleanup row per candidate; refresh #1064 checks/head after final publication; replace partial original-goal quotation with the complete recoverable request or a durable approved transcript reference.

Corrections accepted from review: W1 now denotes the handoff branch checkout, while B1 preserves local source `d97645d9`; exact known renderer worktrees are `/private/tmp/velnor-active-render-new.4dhvov` at `eed474c4` and `/private/tmp/velnor-active-render.KWIEiB` at `6737cdb3`, both retained. P4 was overbroad: #1051/#1053 were merged; current related open PR details require the promised per-PR refresh. #1064 itself is the pause artifact, not an original-goal implementation PR.

The original engineering goal remains paused. This administrative block does not authorize implementation or cleanup; it names the exact preservation-record work a later handoff/resumption coordinator must complete before claiming exhaustive inventory coverage.

# Audit repair record — 2026-09-22T00:00:00Z

## A. Source register and instruction hierarchy

| Source ID | Type / accessibility | Authority and recovered content |
|---|---|---|
| U1 | Original user engineering prompt in this session; FULL | Original goal. It specifies incidents, target lifecycle, identity/trust/event/graph/performance/test/integration requirements. Its operative requirements are atomized in G-001..G-026 below because the conversation itself is not a durable artifact for a future agent. |
| U2 | User amendment, session; FULL | Continue autonomously; never ask or wait; resolve uncertainty through agents/evidence; commit/push frequently on one branch. Remains binding after resume. |
| U3 | User amendment, session; FULL | Commit small verified increments and push regularly; minimize branches. Remains binding after resume. |
| U4 | User pause/handoff request, session; FULL | Supersedes execution only: pause original goal, preserve/publish handoff, no merge/cleanup. Remains binding until explicit resume. |
| U5 | User handoff-repair audit request, session; FULL | Authorizes only bounded audit/document repair/preservation publication. It does not resume G. |
| R1 | Repository `AGENTS.md`; FULL | Tooling/commit/generated-output/runner-protocol rules. Applies to resumed engineering and this documentation commit. |
| E1 | [Architecture decision record](../../plans/2026-09-22-generator-activation-architecture.md); SECONDARY_ONLY | Agent evidence/decisions, not user authorization. |
| E2 | PR #1044, runs and Git state; FULL at audit time | Evidence of checkpoint implementation, not proof of completion. |

Instruction order: U4/U5 pause restrictions supersede U1-U3 execution requirements now. U1-U3 resume only after explicit later user `/goal Read and resume ...`; R1 remains applicable. E1/E2 are evidence, never authorization. No secret-bearing content is recorded.

## B. Original-goal requirement matrix

`Coverage` measures this document, not implementation success. States point to D/H/T evidence.

| Requirement ID | Source | Operative requirement | Handoff section | State/evidence | Remaining task / acceptance | Coverage |
|---|---|---|---|---|---|---|
| G-001 | U1 | Treat both incidents as architecture defects; root-cause before fix. | F, T-002 | Root causes recorded. | T-002 proves old states rejected. | COVERED |
| G-002 | U1 | Preserve trusted bootstrap authority separate from candidate and active product. | D:R4, F, T-003 | BLOCKED by unbound check authority. | External independently controlled authority enforced. | COVERED |
| G-003 | U1 | One active-tree invariant for PR, prospective integration, main, Preview, release. | D:R1, T-004 | source implemented, unactivated. | Active renderer hosted proof all contexts. | COVERED |
| G-004 | U1 | Qualify candidate in isolated disposable staging; candidate output cannot activate itself. | D:R3, T-005 | hosted partial. | isolation + behavioral qualification proof. | COVERED |
| G-005 | U1 | Publish immutable authenticated all-platform product before atomic activation; failed publication preserves old active state. | D:R5/R6, T-006 | implementation unverified. | lifecycle tests and hosted publication. | COVERED |
| G-006 | U1 | Extend existing promote transaction with readiness, revocation/expiry/rollback/concurrency/manifest visibility. | D:R5, T-006 | local readiness only. | controlled lifecycle regressions plus hosted evidence. | COVERED |
| G-007 | U1 | Distinguish source, build inputs, binary digest, tree, policy authority, audited tree, run/attempt. | D:R5/R7, T-007 | partial typed identity. | manifest/graph completion and mutations. | COVERED |
| G-008 | U1 | Model explicit producers, requirements, event contexts, platform/trust/deadline; reject cycles/impossible/unsatisfied edges. | D:R7, T-007 | partial. | graph validator in both paths. | COVERED |
| G-009 | U1 | No guessed producer/latest/branch lookup; terminal no-producer fails immediately; remove 900-second polling. | D:R2, T-004/T-007 | source staged; generated active workflow still polls. | activation then negative resolver tests. | COVERED |
| G-010 | U1 | Candidate code never runs privileged; trusted verdict rejects missing/failed/cancelled/unexpected skips and spoofing. | D:R3/R4/R8, T-003/T-005/T-008 | blocked/partial. | authority and verdict tests. | COVERED |
| G-011 | U1 | Prospective integration parity: PR head/merge/squash/merge-group identity not interchangeable. | D:R8, T-008 | not complete. | event matrix and base-fresh proof. | COVERED |
| G-012 | U1 | Use merge_group if available, otherwise enforce fresh integration proof. | T-008 | NOT RUN. | capability check + enforcement test. | COVERED |
| G-013 | U1 | Measure all path components; any applicable step/job/critical path >120s is defect, including queue/dependency wait. | D:R9, F, T-009 | verified violations. | matched cold/warm optimization evidence. | COVERED |
| G-014 | U1 | Do not hide time by staging/optional skips; preserve obligations/security/platforms. | F, T-009 | design constraint. | aggregate all phase obligations. | COVERED |
| G-015 | U1 | Candidate/active products reused only under exact compatible identity; no arbitrary PR executable trusted. | D:R5/R6, T-006/T-007 | partial. | exact provenance/identity tests. | COVERED |
| G-016 | U1 | Required regressions: incidents, publication failure, successful lifecycle, identity/event contexts, artifact attacks, publisher races/revocation/rollback. | D:R8, T-008 | partial. | executable suite, fake clock/state service. | COVERED |
| G-017 | U1 | Mutation tests remove invariant/event/provenance/needs and must fail. | D:R8, T-008 | not complete. | mutation suite. | COVERED |
| G-018 | U1 | Migrate every active consumer, delete permissive/polling/conflicting docs; no permanent legacy path. | D:R2/R10, T-004/T-010 | staged only. | activation and regeneration prove absence. | COVERED |
| G-019 | U1 | Format, strict Clippy, relevant tests, shell/actionlint/security, deterministic double regeneration, supported consumers. | G, T-010 | mixed/stale/NOT RUN. | exact listed gates at target revision. | COVERED |
| G-020 | U1 | Small protected increments, inspect all PR review feedback, integrate promptly through allowed method. | U2/U3, E.5, T-011 | paused. | live review/head reconciliation before merges. | COVERED |
| G-021 | U1 | Final report architecture before/after, merged evidence/timings and demonstrated violations; do not claim incomplete verification complete. | T-012 | not started. | final combined evidence. | COVERED |
| G-022 | U2/U3 | Autonomous agents; frequent signed scoped commits/pushes; minimal branches. | Source register, T-001 onward | continuing constraint. | apply on resume. | COVERED |
| G-023 | R1 | Generated files only by active pinned renderer; actions/runner protocol source; no legacy shims. | I, T-004/T-010 | constraint. | apply/verify. | COVERED |
| G-024 | U1 | Preserve correct existing local atomic promotion work; do not duplicate mechanism. | F, T-006 | recorded. | extend existing command only. | COVERED |
| G-025 | U1 | Maintain one concise authoritative decision/execution record. | E1, this handoff, T-012 | partial. | consolidate at final. | COVERED |
| G-026 | U1 | Completion requires architectural removal, independent review, protected integration, hosted evidence, honest performance proof. | D:R10/R11, T-010..T-012 | NOT COMPLETE. | all final gates. | COVERED |

## C. Handoff requirements matrix

| Requirement ID | Source | Requirement | Section | State | Coverage |
|---|---|---|---|---|---|
| H-001 | U4 | Keep original goal `PAUSED_BY_USER`; stop implementation/workers. | metadata, C, audit record | goal tool reported paused; watcher stopped. | COVERED |
| H-002 | U4 | Preserve code/artifacts and publish draft PR without merge/cleanup. | E, publication receipt | PR #1064 draft, branch remote. | COVERED |
| H-003 | U4 | Inventory worktrees/clones/branches/PRs/stashes and future cleanup gates. | E, audit expansion | known coverage large but not exhaustive per-resource. | VAGUE / BLOCKED |
| H-004 | U4 | Self-contained original goal, decisions, evidence, resume instructions. | source register, matrices, T, J | repaired; original long prompt is atomized not fully quoted. | PARTIAL |
| H-005 | U5 | Audit source fidelity, state, inventory, fresh-reader; repair canonical doc/PR. | audit record | four auditors requested; results incorporated as available. | PARTIAL |
| H-006 | U5 | Exact executable task plan mapped to requirements. | T-001..T-012 | added below. | COVERED |
| H-007 | U5 | Verify remote publication/head/draft/no-auto-merge. | publication receipt/audit receipt | must refresh after final commit. | IN_PROGRESS |

## D. Executable resumed task plan

| Task | Links | Starting evidence / concrete next action | Dependencies / validation / completion |
|---|---|---|---|
| T-001 | H-003,H-007,G-020 | In exclusive worktree, fetch server heads without force/prune; compare local `d97645d9`, server #1044 `60bb9326`, handoff branch, main, all worktree inventories and PR reviews. | First after explicit resume. Use `git worktree list --porcelain -z`, `git for-each-ref`, `git stash list`, `gh pr view`; update resource ledger. Complete when every goal-related resource has owner/recovery/disposition. |
| T-002 | G-001,G-003,G-009 | Reproduce historical fixtures against active and candidate trees; inspect source and generated outputs after T-001. | Depends T-001. Existing policy test modules/contract tests; complete when original stale-pin and no-producer states fail immediately in real logic. |
| T-003 | G-002,G-010,G-012 | Select/install independently controlled required acceptance authority based on live GitHub capability; do not rely on candidate workflow/job names. | External capability is blocker. Complete only with live enforcement proof on workflow mutation/spoof attempt. |
| T-004 | G-003,G-009,G-018,G-023 | Activate only after T-003/T-006; regenerate static workflows twice using activated pinned renderer; remove remaining active generated poll loops. | Validate `--plain --check`, generated shell checks and policy/Preview. Complete when active consumers contain no guessed polling/candidate exception. |
| T-005 | G-004,G-010,G-015 | Harden candidate lane isolation/authority and qualification output semantics; inspect workflow/action boundaries. | T-003 before trusting result. Complete when candidate cannot alter validator/verdict or publish trusted binary and qualification tests pass. |
| T-006 | G-005,G-006,G-007,G-015,G-024 | Repair existing runtime publisher checksum/tag-contract/source-digest/build-identity/readiness lifecycle. | Requires T-001; retain promote mechanism. Complete with failure/no-mutation, duplicate/interrupted/revoked/expired/rollback tests and authenticated all-platform product. |
| T-007 | G-007,G-008,G-009,G-015 | Extend existing graph, not second scheduler: typed EventContext and ArtifactRequirement/Producer/identity/state/deadline; mirror root/s2 or remove superseded path. | Can parallel design with T-006. Complete with cycle/producerless/event/platform/trust/terminal-state mutations. |
| T-008 | G-011,G-012,G-016,G-017 | Build event/prospective-main/fork/bot/rerun and adversarial artifact/manifest/verdict mutation matrix using controlled state/fake clock. | T-003/T-007 foundations. Complete when all required cases execute, not textual assertions. |
| T-009 | G-013,G-014 | Measure cold/warm complete affected paths including queue; design phase result graph before splitting jobs, then prove required aggregate. | Can investigate parallel; implementation depends T-007. Complete only if every observed >120s breach remedied or retained with measured cause and feasible remedies attempted; never call requirement met otherwise. |
| T-010 | G-018,G-019,G-026 | Run target-revision formatting/Clippy/unit/integration/contract/generated-shell/actionlint/security/double regeneration/supported consumers; hosted main/Preview/activation evidence. | After T-003..T-009 integration. Complete with durable command/run evidence. |
| T-011 | G-020,G-026,H-003 | Read every live review/comment/thread on P1 and related required PRs, resolve source overlap, merge only allowed protected increments. | Depends T-001/T-010 per increment. Complete target combined state verified; no stale check reuse. |
| T-012 | G-021,G-025,G-026,H-003 | Publish final decision/evidence record; then execute individually approved cleanup gates for each retained goal-exclusive resource. | Only after T-011; re-observe every candidate. Complete with final architecture/timing report and cleanup receipt. |


## E. Atomic original-goal fidelity addendum

The following supplements G-001..G-026. `MISSING` means engineering/documentation work remains; it does not waive U1.

| ID | Atomic requirement from U1 | State / linked task | Coverage |
|---|---|---|---|
| G-027 | Before every bug fix, name enabling architecture/failure class; retain regression for a deferred root cause. | T-002/T-006/T-007/T-008 | COVERED |
| G-028 | Execute six independent workstreams: incident/producer forensic; lifecycle/trust alternative; typed graph/artifact; regressions/prospective-main; cold/warm performance; integration/security review. | Resume must assign bounded agents across T-002..T-011. | COVERED |
| G-029 | Refresh main/branches/PRs/reviews/runs/jobs/logs/rulesets; inspect actual protection, not one field. | T-001 | COVERED |
| G-030 | Keep bootstrap authority, active renderer, candidate renderer, audited tree and execution context distinct. | T-003..T-006 | COVERED |
| G-031 | Active output equals deterministic declared authenticated active renderer over audited complete config/scan/static inputs in every consumer context. | T-002/T-004 | COVERED |
| G-032 | Candidate source integration does not activate incompatible output; syntax is staged until its renderer product activates. | T-004/T-005/T-006 | COVERED |
| G-033 | New product readiness covers every supported platform, retrieval, integrity, compatibility, interrupted/concurrent/duplicate publication, manifest visibility, retention/revocation and rollback. | T-006 | COVERED |
| G-034 | Same-run bootstrap is only an alternative after trust/self-reference/availability/cold-path proof; single-PR activation needs independent producer and exact integration proof. | T-003/T-006 | COVERED |
| G-035 | Build identity covers all byte-affecting inputs: transitive source/build scripts/embedded files/lock/toolchain/target/host/features/profile/flags/environment/revision stamps. | T-006/T-007 | COVERED |
| G-036 | Never equate PR head/synthetic merge/squash/merge-group/dispatch/rerun/fork/bot identities without proven input equivalence and origin. | T-008 | COVERED |
| G-037 | Graph rejects cycles, producerless requirements, impossible producer event, incompatible platform/build/trust, and prohibited trust transition before execution. | T-007 | COVERED |
| G-038 | Resolver states ready/pending/producer-failed/unavailable/invalid/expired; no producer fails immediately; bounded retries honor inherited remaining time/cancellation. | T-007/T-008 | COVERED |
| G-039 | Scheduler dependency waits are explicit `needs`/artifact relationships and counted in timing; no runner-resident polling. | T-004/T-007/T-009 | COVERED |
| G-040 | Candidate executes only restricted ephemeral worker; trusted acceptance/publication separately controlled; no privileged candidate execution or self-selected verifier. | T-003/T-005 | COVERED |
| G-041 | Verify builder/workflow/repository/revision/parameters/digest/run-attempt/platform/profile/features provenance; signature proves origin only. | T-006/T-008 | COVERED |
| G-042 | Same acceptance predicates on prospective integration and main/Preview; only legitimate publication authorization differs; no environment spoofed push. | T-008 | COVERED |
| G-043 | Trusted final verdict rejects missing/failed/cancelled/unexpectedly skipped work and spoofed names; explicit no-work proof passes. | T-003/T-008 | COVERED |
| G-044 | Measure event creation, queue/dependency, bootstrap, checkout/tool/cache/build/test/artifact/post/verdict/delivery and generator-change-to-activation. | T-009 | COVERED |
| G-045 | Matched cold/warm records retain first attempts/failures/retries/cancellations; cache misses never alter correctness; external limits reported, not excused. | T-009 | COVERED |
| G-046 | Real emitted acquisition/verdict tests use controlled producer states/fake clocks; cover listed hostile identities/products/manifests/cache/self-report and lifecycle races. | T-008 | COVERED |
| G-047 | Mutation tests fail if event compatibility, provenance, active invariant or required edge is removed. | T-008 | COVERED |
| G-048 | Remove superseded permissive paths/polling/conflicting docs after all active consumers migrate; no recovery shim. | T-004/T-010 | COVERED |
| G-049 | Required target validation: actual-pinned formatting, strict Clippy, relevant unit/integration/contract/generated-shell/actionlint/security, two clean renders, supported consumers. | T-010 | COVERED |
| G-050 | Reread every review/comment/thread at final SHA; integrate through actual protected method, verify post-merge main/Preview and follow failures. | T-011 | COVERED |
| G-051 | Final decision/execution record distinguishes documented/implemented/pushed/CI-verified/merged/post-merge verified and reports before/after/timings/remaining violation. | T-012 | COVERED |

## F. Audit-only requirement status

G remains paused. Handoff repair is `PARTIAL`, not `VERIFIED`: source fidelity and task mapping are now materially expanded, but exact exhaustive resource enumeration and per-resource cleanup mapping remain unavailable in the canonical document. This is an administrative quality limitation, not an engineering-completion claim.

## G. Post-pause audit observations (2026-09-21T22:16:16Z; do not rewrite the pause snapshot)

Current observations supersede only current-state claims, not the historical C snapshot:

* W1 is now `codex/goal-handoff-2cc4de09`; prior source checkpoint B1 remains local `codex/activation-foundation@d97645d9`. The observed handoff head at this audit was `958a3e6b`; later documentation commits are recorded in PR body/final receipt.
* Current audit counts were 105 local heads, 20 origin-tracking refs, 25 server heads and 0 stashes. Earlier 104/19 counts are historical.
* B2/PR #1044 is not an anonymous artifact: `60bb9326e6303c577bd15e952558f0dc02fd78f2` is a post-checkpoint goal-related GitHub commit authored/committed by `donbeave` (metadata email `alexey@zhokhov.com`). It is remotely preserved only, deletes `crates/velnor-workflow/tests/bootstrap_transport.rs`, and modifies `selection_plan_handoff.rs` and `verify_set_build_inputs.rs`. Preserve and review it before integration; it is not in this handoff branch.
* #1044 terminal evidence: candidate `35660441660` passed; Policy `35660437293` failed after 11m39s in old candidate acquisition after generated-state scan drift; CI `35660442225` failed because the declared `eed474c4` pin rendered a different tree and because `velnor-tools` test `cleanup_leaves_replaced_regular_temporary_name_instead_of_unlinking_it` failed at `github_raw_store.rs:1038`. `ci-required` and `Control / Required` consequently failed. These failures do not prove the redesign complete or invalid; they are required resume investigation evidence.
* #1064 at that observation remained draft/no reviews/no auto-merge. Candidate `35661464652` passed (job `106537561857`, 1m45s); Policy `35661462744` and CI `35661464904` were not terminal. The documentation and workflow jobs had failures; the handoff PR is not green evidence.
* #1065 is a separate draft paused-handoff PR/branch (`53bc9f1e`) for another branch-consolidation goal. It is a parallel shared repository resource, explicitly excluded from this goal's integration/cleanup scope.

### Audit repair tasks

| Task | Outcome | Completion evidence |
|---|---|---|
| T-AUDIT-001 | On resume, preserve historical snapshot and append terminal/live run evidence for #1044/#1064. | Run URLs/jobs/conclusions recorded against exact SHA. |
| T-AUDIT-002 | Fetch/review/preserve `60bb` separately from B1; map its 3-file fixture-retirement diff to P1. | Exact diff/review disposition and durable ref. |
| T-AUDIT-003 | Reconcile active-renderer generated-state drift caused by fixture retirement only after explicit resume. | Pinned renderer clean second pass plus policy/contract outcome. |
| T-AUDIT-004 | Diagnose `velnor-tools` temporary-name test failure deterministically; do not assume it is infrastructure. | Reproduction/root cause/fix or classified external proof. |
| T-AUDIT-005 | Re-observe #1064 checks after final documentation publication; record terminal result without repair loop. | Exact head/run status. |
| T-AUDIT-006 | Complete the blocked per-resource worktree/branch/PR/stash cleanup ledger; classify #1065 shared/excluded. | Every in-scope resource has exact mapping/disposition. |
| T-AUDIT-007 | Verify remote handoff file/branch/PR body/head after final update. | `git ls-remote`, `gh pr view`, branch-qualified file retrieval agree. |
