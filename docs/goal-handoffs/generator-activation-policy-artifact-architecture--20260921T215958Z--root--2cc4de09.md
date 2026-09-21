# GOAL: Redesign Velnor generator activation, policy validation, and CI artifact dependency architecture

**Handoff ID:** `generator-activation-policy-artifact-architecture--20260921T215958Z--root--2cc4de09`  
**Created / last updated UTC:** 2026-09-21T21:59:58Z / 2026-09-21T22:05:00Z  
**Original goal status:** `PAUSED_BY_USER`  
**Handoff status:** `READY`  
**Runtime stop:** verified: `update_goal(status=paused)` returned the original objective and `paused` at 2026-09-21T21:59:40Z. This expresses user pause; it does not cancel already-created GitHub runs.  
**Source agent:** `/root`, Codex session `01a0c510-7a18-7680-8954-8a6121240bb9`; `rtk 0.49.0`.  
**Repository:** `tailrocks/velnor`, `git@github.com:tailrocks/velnor.git` (sanitized).  
**Source checkpoint:** local `codex/activation-foundation` `d97645d917c1fb7680af8678c9243286ec30963f`; observed `origin/main` `45ef1ebef769c78f45315e11a798fdaafaef4c4e`.  
**Preservation branch / PR:** filled after publication in this document's final commit and PR body.  
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
git switch <published-preservation-branch>
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
