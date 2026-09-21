# GOAL: Make Velnor a genuinely change-aware, dependency-aware, minimal-work CI/CD workflow generator

## Handoff metadata

- Handoff ID: `change-aware-minimal-work--20260921T220413Z--pine-io--2100e49a`
- Created (UTC): 2026-09-21T22:04:13Z
- Last update (UTC): 2026-09-21T22:34:14Z (intent/resumability audit repair)
- Original goal status: `PAUSED_BY_USER`
- Handoff status: `READY`
- Source agent/CLI: Muse Code (session `pine-io` / `01a0c14d-5940-76d3-bb71-acbc0b658693`), goal `goal-5b993493-6960-4e99-b548-5d470bd5355d`
- Primary repository: `tailrocks/velnor` (local: `/Users/donbeave/Projects/velnor-optimizations/velnor`)
- Secondary repository: `jackin-project/jackin` (local: `/Users/donbeave/Projects/velnor-optimizations/jackin`)
- Handoff path (repo-relative): `docs/goal-handoffs/change-aware-minimal-work--20260921T220413Z--pine-io--2100e49a.md`
- Preservation branch: `handoff/change-aware-minimal-work-20260921` (from `origin/main` @ `45ef1ebe`; this file lives on that unmerged branch until §T-010)
- PR base: `main`; PR URL: https://github.com/tailrocks/velnor/pull/1067 (draft, do-not-merge)
- Resume authorization: explicit later user request only.

`PAUSED_BY_USER` freezes local work, not GitHub-hosted runs. No merge or
local cleanup was performed during this handoff; both are documented for
post-resume execution (§§T-002/T-004/T-010).

## A. Original goal, amendments, operative contract (audited §M)

### A.1 User's goal-defining prompt (verbatim preamble; full text = source S1)

> Make Velnor a genuinely change-aware, dependency-aware, minimal-work CI/CD
> workflow generator.
>
> Treat this as a core product-correctness and architecture goal—not a
> one-off optimization for one Markdown file, one Rust crate, or one repository.
>
> PRIMARY IMPLEMENTATION REPOSITORY https://github.com/tailrocks/velnor
> REQUIRED GENERATOR RULES https://github.com/tailrocks/velnor/blob/main/crates/velnor-workflow/AGENTS.md
> REQUIRED PRODUCT DOCUMENTATION https://github.com/tailrocks/velnor/tree/main/content/docs
> PRIMARY REAL-WORLD REGRESSION https://github.com/jackin-project/jackin/pull/1016
>
> The user-visible problem is simple: this PR changes only
> agent-instruction Markdown, but substantial unrelated CI work runs. Velnor
> must understand what changed, what consumes those changes, and which
> verification or delivery tasks are actually affected.
>
> Fix the structural causes in Velnor, regenerate the relevant consumer
> workflows, verify real behavior, and make these principles permanent in
> code, tests, lean agent rules, and product documentation.
>
> Do not finish with research, a plan, documentation-only changes, a
> hardcoded AGENTS.md exclusion, or hand-edited generated YAML.

Governing principle (verbatim): "Do all necessary work, and no unrelated
work. Make that decision from complete change evidence and a sound
dependency/input model, explain it, and enforce it permanently in Velnor."

Full S1 = 10 sections (~33KB in session log L20); operative condensation follows.
Section numbers below are the user's (§§1–10).

### A.2 Operative rules per goal section (condensed, binding)

- §1 Execution: delegate-first via subagents, parallelize, independent
  verification, continuous integration; bounded ownership, no conflicting
  writes; never ask user (see A.3); commit often + push regularly + DCO
  identity (see A.3); one integration branch per repo preferred; small PRs,
  merge promptly when authorized; disposition ALL reviews/threads before
  merge; re-fetch feedback+checks at final head; never bypass approvals or
  force green; judge by correctness (never ROI); diagnose architecture
  before fixing; remove enabling condition, else name deferred cause.
- §2 Evidence: reproduce jackin #1016 (head `631f8f76…`, base `d0d4ee09…`,
  synthetic merge `2b930129…`, generator `4fa7a3a8…`, runs 35544119651 /
  35544119469 / 35544119478 + any discovered); inspect ALL workflows/jobs/
  steps/attempts; per-task evidence records; reproduce planner selection on
  historical input; trace selection_for_diff/WatchGraph/select_affected/
  expansion/global-input/unmatched-fallback (reuse.rs = lead, not verdict);
  answer 5 why-questions; failing regression BEFORE structural fix.
- §3 Product contract: A soundness (never omit on incomplete info),
  B precision (never run known-unaffected), C explicit no-work (zero tasks
  = first-class success), D explainability (machine + human), E generic
  discovery (no repo/path baked into selection), F one coherent model,
  G early elimination (before runners/caches/toolchains), H continuous
  enforcement (tests/checks/docs).
- §4 Change detection: cumulative whole-PR effective change at immutable
  base/head/merge-base; synthetic-merge semantics PROVEN; revert-aware;
  correct across force-push/rebase/retarget/merge-group; event semantics
  for PR/merge_group/push/dispatch/schedule/tag/release/reusable;
  renames/symlinks/submodules/binary/NUL-delimited; old+new model
  evaluation; diff/API limits handled; never unknown-changes = no-changes.
- §5 Impact model: split depends_on meanings (impact/compile/test-only/
  artifact/schedule/setup); Rust: transitive dependents under
  feature/target configs, no sibling fan-out, prereq-build ≠ own-job, no
  back-expansion to unrelated dependents, no serialization without artifact
  need; full manifest matrix incl. FFI/build-scripts/features; task-effect
  differentiation; ALL families covered (JS/TS, Swift, docs/MDX, Docker,
  shell/infra, policy/Renovate, packaging/release, generator/policy,
  aux/scheduled) with coverage inventory; typed declarative contracts for
  opaque inputs, no arbitrary code exec; known-irrelevance (AGENTS.md-only
  → zero unrelated tasks, but real consumers override; NO blanket
  `*.md`/ignores) vs unknown-impact (resolve → smallest sound domain →
  exact reason + missing contract; never silent); audit global inputs
  incl. `.github/`.
- §6 Generation+gates: inventory all generated workflows/jobs; shared
  deterministic pre-setup plan; proven no-work starts/installs/restores/
  publishes nothing; zero execution where permitted; honest minimal gate;
  no path-filter pending-check breakage; stable required-check contract
  bound to planner expected work (no-work may pass; selected tasks need
  accepted success or verified reusable evidence; missing/skipped/cancelled/
  failed never green; empty-matrix + merge-group deliberate); no gate
  weakening; least privilege + trust boundaries (incl. pull_request_target
  rules, pins, plan validation); duplicate-run audit.
- §7 Optimization: primary-source research; per-optimization record
  (issue/evidence, alternatives, correctness+security, implementation,
  review, before/after); install-only-needed; planner bootstrap
  independent; prebuilt runtime over source-build; minimal fetch; no
  repeated downloads; compatible-only sharing; parallelism without
  duplicated setup; platform scoping; locked resolution; input-closure
  cache identity; cache/artifact/result separation; freshness/trust;
  cold+warm measurement (transport + time).
- §8 Rules+docs: `crates/velnor-workflow/AGENTS.md` keeps scan-first
  contract + compact enforceable rule ≈ "Generate the smallest sound
  execution plan from the full event change set and discovered/declared
  task dependencies. Proven irrelevant changes must schedule no unrelated
  workloads. Select before setup; explain selection, fallback, cache
  invalidation, and reuse. Never replace unknown impact with silent
  skipping, blanket exclusions, or repository-specific exceptions. Every
  generation change must preserve these invariants through tests and
  updated product documentation." Expand `content/docs` (12 listed topics);
  honest vision-vs-implemented; fix contradictions; validate builds/links.
- §9 Regression: multi-layer tests; 16 numbered cases (1 no-work MD,
  2 consumed docs, 3 isolated crate, 4 library dependents, 5 siblings,
  6 cumulative-PR, 7 multi-change/force-push/rebase/merge-group,
  8 deleted/renamed, 9 dep-type/feature/FFI matrix, 10 lockfile scope,
  11 docker/docs/frontend/native/aux, 12 empty-vs-failed diffs, 13 gate
  fail-closed, 14 cache correctness, 15 new-package discovery, 16
  byte-stable regen); property tests; full-verification oracle comparison
  (not permanent full CI); independent command-level verifier (workspace-
  wide command in crate job = over-selection); security/gate reviewer.
- §10 Rollout: vertical slices; generic generator first; regen+verify
  Velnor's OWN workflows + jackin consumer workflows at identifiable rev;
  #1016 = historical evidence only (never merge/modify unrelated policy
  for demo); rollout ordering (never pin unverified runtime); live-CI
  demos (instruction-only no-work, Rust selection, cross-language,
  required checks both cases, no aux bloat, no mainline skew); old-vs-new
  metric comparison; no retroactive-fix claims; external blockers recorded
  precisely. FINAL ACCEPTANCE (13, verbatim core): #1016 class reproduced
  + regression; irrelevant changes → zero unrelated tasks; whole-PR
  semantics; sound dependency propagation; all families audited; setup/
  caching follow selection; gates/trust/reuse correct; generic +
  reproducible output; AGENTS.md rule present; docs explain contract;
  independent reviews + deterministic tests pass; consumer regen + live CI
  verified; committed/pushed/merged where authorized. FINAL REPORT (7):
  root cause + removed condition; changed behavior + families; commits/
  PRs/revs/rollout; before/after CI; regression + independent results;
  docs/rule locations; demonstrated external blockers.

### A.3 Material amendments (verbatim, session-log S2–S4)

Order observed: S4 → DCO refinements → S2 → S3 → S4-repeat → S5. All remain
binding; none superseded (S4-repeat restates S4).

- S2 (delegate): "Use subagents aggressively for all work. Always delegate
  work to subagents whenever delegation is possible. Treat subagents as the
  default execution mechanism, not an optional optimization. [8 execution
  bullets: decompose, spawn per workstream, parallelize, research/review/
  test/verify via subagents, no serial parent work, keep spawning,
  independent verification, synthesize.] Do not merely recommend
  parallelization — actually execute the goal through subagents. [Parent =
  orchestrate/resolve/integrate/final-checks.] Default rule: **delegate
  first, parallelize aggressively, verify independently, then integrate.**"
- S3 (autonomy): "Never ask the user questions or wait for clarification.
  Work fully autonomously. [On ambiguity: spawn subagents, analyze context/
  repo/docs/code/history, research alternatives, compare tradeoffs, verify
  independently, re-verify critical calls, decide and continue.] Do not stop
  because information is imperfect. [Well-researched reversible decision
  over asking; multi-subagent challenge under significant uncertainty.]
  Your responsibility is to unblock yourself. [...] Continue working until
  the goal is fully completed, verified, and no meaningful actionable work
  remains."
- S4 (commits): "Always commit changes frequently while working. [Small
  incremental scoped commits; commit verified units at once; push regularly
  for recoverability/review/bisect.] At the same time, avoid unnecessary
  branches. [Single working branch preferred; new branches only on clear
  technical need.] In short: **commit often, push regularly, and minimize
  branch proliferation.**" + DCO refinements (3×, final): "Commits must
  ALWAYS with Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>. Never
  anything else than Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>"
  (DCO is also a CI merge gate, §C).
- S5 (pause): goal `PAUSED_BY_USER`; freeze/preserve/document/publish/
  stop; evidence-backed inventory + post-resume integrate/cleanup plan; no
  merge/cleanup now; compact receipt; then STOP until explicit resume.
  S5 suspends engineering, it does not narrow §§1–10.
- S6 (this audit): verify+repair handoff only; no implementation/merge/
  cleanup; outcome VERIFIED/PARTIAL/BLOCKED + receipt; then STOP.

### A.4 Consolidated operative goal (resume executes THIS)

Deliver S1 §§1–10 end state (A.1+A.2) under S2/S3/S4 working rules and the
13 acceptance bullets, starting from the checkpoint in §§B–H and executing
§§T-001–T-010 in order. Handoff contract (preserve/document/publish/leave-
cleanup) is administrative scaffolding, not a substitute for any S1 requirement.

## B. Completed and merged (verified 2026-09-21T22:2xZ via `gh pr view`)

Velnor (`tailrocks/velnor`), each local tip SHA == PR head SHA, all `MERGED`:

| PR | Branch @ tip | Merge | Content |
|----|--------------|-------|---------|
| #996 | integrate/change-aware-minimal-work@0c7fefc4 | 7480b78c | runtime slice integration |
| #1001 | rollout/change-aware-runtime@e0694a84 | 9374a4d3 | required-check aggregate binding |
| #1003 | rollout/d19-pin-9374a4d3@61690487 | cff712d2 | D19 pin advance |
| #1007 | rollout/plan-lane-scope@16e371e1 | 44243ed4 | lane scoping |
| #1008 | rollout/d19-pin-44243ed4@df83ac59 | bb497882 | D19 pin advance |
| #1016 | rollout/unmatched-opaque-scope@0895cc1e | 01bc16b2 | D1: opaque-narrowed unmatched fallback |
| #1033 | rollout/typed-read-contracts@e05f507c | 3ef31d90 | D2: typed read contracts |
| #1039 | rollout/verify-set-build-inputs@1f44e3bb | 7576f40f | D3: verify-set scheduling + reads_closed |
| #1046 | followup/d3-major-1-closed-world-visibility@7de01e1c | 9660c9ff | Major-1: closed-exclusion visibility |
| #1051 | fix/s1-nested-bun-watch-scoping@1acbf8f4 | 02089f19 | s1 Bun watch scoping |
| #1059 | fix/s2-nested-bun-watch-scoping@f7bebb42 | 267649e6 | s2 Bun watch port |

Jackin (`jackin-project/jackin`):

| PR | Branch @ tip | Merge | Content |
|----|--------------|-------|---------|
| #1019 | rollout/velnor-change-aware-7480b78c@76cb7c7c | b569b1b5 | consumer rollout |
| #1042 | chore/velnor-01bc16b2@d321e88e | 87521a95 | repromote to D1 |
| #1054 | chore/velnor-d3@4ba4a4cb | c72e21b5 | D3 + contracts repromote |
| #1052 | rollout/velnor-wave@986f94bc | edef2c1e | schema-2 migration (stale base; regressed pin, superseded by #1065) |
| #1062 | cicd/repromote-major1-9660c9ff@0d369a85 | CLOSED unmerged | stale s1 fast-follow, superseded by #1065 |

Live proofs (branches retained, open do-not-merge): jackin #1058
(D3 re-proof: checks show 6 pass + 40 skipping, 0 fail, run 35641999357 —
count corroborated; `planned_no_work=true` value SECONDARY, re-cite on
resume), #1060 (positive control: 33 pass + 13 skipping, 0 fail —
"27/27" mapping SECONDARY, re-cite run URL + job list on resume).

## C. In flight at pause (the resume frontier)

Pause snapshot (2026-09-21T22:2xZ; STALE — audit observation below supersedes; T-001 re-checks live):

- **jackin #1065** `cicd/s2-repromote-80bc420d` @ `909a9f54`, state `OPEN`
  (repromote to generator `80bc420d` + s2 closed-world contracts).
  CI then: one `fail` = `rust-jackin-usage-ffi / Apple`
  ([job](https://github.com/jackin-project/jackin/actions/runs/35658518788/job/106528082244),
  run 35658518788, 40s); one `pending` Swift/XcodeGen job; Apple-leg
  `skipping` cascade; Policy/DCO/Control-Planning green. Reviews: none;
  0 unresolved threads; mergeable MERGEABLE but checks-BLOCKED.
  Local checkout clean and in sync (ahead=0, behind=0).
- **Not started**: s2 0/41 AGENTS.md-only proof, scripts probe, s2 positive
  control, §8 rules/docs verification, §9 case matrix, Velnor self-regen
  check, final completion audit + report.

Audit observation (2026-09-21T22:34Z, host runs move under pause): #1065
checks now show THREE fails (FFI-Apple + `Control/Required` job
106540752761 + `ci-required` job 106540696268); Swift jobs resolved to pass
(xcodegen 23m32s, swift-package 19m20s), zero pending. T-001 re-checks live
state; T-003 covers all failing checks, not just FFI.

## D. Branch inventory (primary checkouts)

Velnor local (`.../velnor`), handoff branch from `origin/main` @ `45ef1ebe`:

| Branch | Tip | Upstream | Verdict |
|--------|-----|----------|---------|
| handoff/change-aware-minimal-work-20260921 | §K head (moves with handoff commits) | origin/handoff, sync | THIS handoff; §T-010 closes |
| main | 89a963f7 | origin/main behind 72 | stale pointer; T-001 fast-forwards |
| fix/s1-nested-bun-watch-scoping | 1acbf8f4 | remote deleted | merged #1051; T-002 deletes |
| fix/s2-nested-bun-watch-scoping | f7bebb42 | remote deleted | merged #1059; T-002 deletes |
| followup/d3-major-1-closed-world-visibility | 7de01e1c | remote deleted | merged #1046; T-002 deletes |
| integrate/change-aware-minimal-work | 0c7fefc4 | remote deleted | merged #996; T-002 deletes |
| rollout/* (7 branches) | e0694a84 df83ac59 61690487 16e371e1 e05f507c 0895cc1e 1f44e3bb | remote deleted | merged #1001/#1008/#1003/#1007/#1033/#1016/#1039; T-002 deletes |
| preserve/repin-trial-be78a6d0 | 4300cc63 | none, but == origin/preserve/repin-trial-be78a6d0 | preserved evidence; keep through T-009, T-010 deletes branch+worktree after audit passes |

Jackin local (`.../jackin`), HEAD `909a9f54` == `origin/cicd/s2-repromote-80bc420d`:

| Branch | Tip | Upstream | Verdict |
|--------|-----|----------|---------|
| cicd/s2-repromote-80bc420d | 909a9f54 | sync | ACTIVE resume frontier (#1065) |
| main | 799deeef | origin/main behind 33 | stale pointer; T-001 fast-forwards |
| chore/velnor-01bc16b2 | d321e88e | remote deleted | merged #1042; T-002 deletes |
| chore/velnor-d3 | 4ba4a4cb | remote deleted | merged #1054; T-002 deletes |
| rollout/velnor-change-aware-7480b78c | 76cb7c7c | remote deleted | merged #1019; T-002 deletes |
| cicd/repromote-major1-9660c9ff | 0d369a85 | sync, remote ref LIVE | closed #1062, superseded; T-010 deletes local; remote-ref deletion needs explicit resume-time confirmation |
| proof/* (4 branches) | 09e0d3f6 5a53e63c b4262258 0587381f | sync | open proof evidence; KEEP (no deletion in any task) |

Squash-merge note: `git cherry origin/main <branch>` shows `+` lines on
multi-commit branches. Squash artifact, not unmerged work: every tip SHA
byte-matches its MERGED PR head (§B). T-002 re-verifies cumulative diff
before deletion.

Stashes: none in either primary repo. Working trees: clean at audit time.

## E. Worktree / clone inventory

Primary checkouts: `.../velnor` (handoff branch), `.../jackin` (#1065 branch).
Registered worktrees (`git worktree list`):

| Path | Repo | State | Verdict |
|------|------|-------|---------|
| /private/tmp/velnor-pin-1e454958 | velnor | detached 1e454958, clean | base-validator pin reference; keep through T-009, T-010 removes |
| /private/tmp/velnor-repin-trial | velnor | preserve/repin-trial-be78a6d0@4300cc63, clean, pushed | preserved evidence; T-010 removes with branch |

Other `/tmp` git repos (verified; `/tmp`→`/private/tmp` symlink):

Goal-related, keep until T-009 passes, T-010 removes:

| Path | State | Note |
|------|-------|------|
| /tmp/velnor-gen-4dec6b9e | detached 4dec6b9e, clean | pre-D3 regressed pin reference |
| /tmp/velnor-gen-80bc420d | detached 80bc420d, clean | s2 runtime reference (origin = primary velnor) |
| /tmp/velnor-src | detached 4fa7a3a8, clean | incident-revision repro (§A.2 §2 generator rev) |
| /tmp/velnor-gen-history | main@01bc16b2, 1159 deleted files | stale D1-era scratch; no work product; T-010 removes |

Foreign goals (do not touch; other workers own them):

| Path | State | Owner signal |
|------|-------|--------------|
| /tmp/velnor-b07-port | integrate/b07-buildkit-ceilings@36c2d6b2, clean | b07 goal |
| /tmp/velnor-b21-review | detached c2343268, clean | b21 review |
| /tmp/velnor-handoff-b04e988e | goal-handoff/velnor-consolidation-b04e988e@5496db7e, clean | velnor-consolidation (= velnor #1065 head) |
| /tmp/jackin-int1002 | integrate/branch36-verify-preview@85a7bd39, clean | branch36 (= jackin #1066 head) |
| /tmp/jackin-b24review | HEAD@1f302c03, clean, origin=/tmp/jackin-int1002 | branch36 review |
| /tmp/jackin-b26-review | main@b9cdae2c, 2612 deleted files | b26 stale scratch |
| /tmp/jackin-handoff-07927053 | goal-handoff/jackin-consolidation-07927053@15a4fb2f, clean | jackin-consolidation (was = jackin #1070 head; head moved to 97f9d4b3 — foreign drift, re-note don't touch) |

Unmapped, no content value: `/tmp/jackin-b28r` (empty repo, no commits,
not this goal's); 17 non-repo `/tmp` dirs (logs/scratch, all probed).
`/tmp/*.bundle`: 23 jackin-branch-*, 1 consolidation, 2 evidence
(p957/p961), 1 velnor-recovery — all foreign-goal refs; this goal
produced no bundles.

Independent clones: `/Users/donbeave/Projects/github/velnor` (clean,
`5349ec32` = velnor #1063 head, foreign); `/Users/donbeave/Projects/github/jackin`
(clean, main@`fce94cea`, foreign); `/Users/donbeave/Projects/new_work/velnor`
(clean, `refactor/holla-parity`@`57a53bb0`, branches hold zero goal refs,
foreign). Untouched.

Discovery scope (explicit): 2 primaries + `/tmp/{velnor,jackin}-*` glob +
`~/Projects/github/{velnor,jackin}` + `~/Projects/new_work/velnor` +
`~/Projects/tailrocks/*` level. NOT searched: full machine, other
`~/Projects/*` subdirs. Counts: 2 primaries, 2 registered goal worktrees,
4 goal /tmp repos, 7 foreign /tmp repos, 3 foreign clones, 0 stashes,
0 local-only commits.

## F. Open-PR inventory

Goal-owned (land or re-prove after resume):

| PR | Head | Note |
|----|------|------|
| jackin #1065 | 909a9f54 | s2 re-scope; BLOCKED on failing checks (§C) |
| jackin #1030/#1045/#1058/#1060 | 09e0d3f6/5a53e63c/b4262258/0587381f | do-not-merge proof evidence; keep open |
| jackin #1067 | 0515ad33 | post-#1053 live-results record; body skim on resume before citing |

Adjacent (touches goal surface; disposition in T-003/T-009):

| PR | Head | Note |
|----|------|------|
| jackin #1044 | 6ff54ce5 | Apple CI generic-recipe migration; s2/Apple context |
| velnor #1052 | b084c416 | skip redundant Rust bootstrap (§7 territory) |
| velnor #1055 | 2a270947 | fail closed on producer failures (§6 territory) |
| velnor #1056 | 9f795b2e | product inputs in selection ownership (§5 territory) |
| velnor #1057 | 70268cd5 | scheduled-check permission scoping (§6 territory) |
| velnor #1058 | 93cd45e9 | composable regen phases |
| velnor #1054 | 256c24bb | shared derived Mise facts (empty body; possible T-003 relevance) |
| velnor #1050 | 96b835d9 | scheduled-check merge_group/concurrency (likely sibling goal) |

Foreign (do not touch): velnor #962 #963 #973 #978 #979 #980 #1044 #1063
#1064 #1065 #1066; jackin #1004 #1007 #1063 #1064 #1066 #1068 #1069 #1070.
This handoff: velnor #1067 (draft, §K).

## G. Unmapped or local-only work

None with content value: no uncommitted changes, no stashes, no unpushed
commits on any live branch, no local-only branches except the pushed
preserve branch (§D). Mass-deletion /tmp checkouts (§E) are stale scratch.
Only open mapping question: ownership of §F-adjacent PRs (resolved by
T-003/T-009, blocks nothing).

## H. Worker status

No live goal workers. Pause-handoff work ran single-threaded
post-compaction. Audit workers (this message): 3 read-only subagents
(intent-fidelity, state, preservation) — all terminal, zero mutations;
fresh-reader review ran on the repaired draft (§M). Prior-session worker
claims are NOT relied upon; T-001 re-establishes ground truth from remote
state only.

## I. Resume runbook rules (bind T-001–T-010)

S2 delegate-first (subagents default; parent orchestrates/integrates);
S3 never-ask (ambiguity → internal investigation; reversible best call);
S4 commit-often + push-regularly + minimize branches, DCO identity exactly
`Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` on every commit;
§1 merge discipline (disposition all threads, re-fetch at head, no bypass,
correctness over ROI, remove enabling condition or name deferred cause).
This file lives on the unmerged handoff branch: check it out per §J,
reconcile live state (T-001) before acting, never repeat completed side
effects, never start a recursive pause cycle.

## T. Executable remaining-work plan (dependency-ordered; needs explicit resume)

- T-001 Re-sync + reconcile. Start: primaries clean; mains stale (72/33
  behind); #1065 CI drifted (§C). Action: `git fetch --all --prune` both
  primaries; `git switch main && git pull --ff-only` both (fail =
  investigate, never force); re-run `gh pr checks 1065` + `gh pr view
  1065 --json state,headRefOid,mergeable,mergeStateStatus`. Done when:
  live #1065 head/checks recorded and §§C–F deltas noted. No deps.
- T-002 Merged-branch deletion safety. Start: §D "T-002 deletes" set (11
  velnor + 3 jackin). Action per branch: confirm tip == merged PR head
  (`gh pr view --json headRefOid`), confirm remote ref deleted
  (`git ls-remote --heads`), squash-aware spot-check that tip diff vs
  merge-base is subsumed by the merge commit; then `git branch -d` +
  prune. Never touch proof/preserve/handoff branches. Done when: set
  deleted locally or each survivor has a named blocker. Dep: T-001.
- T-003 Diagnose #1065 failing checks. Start: fails = FFI-Apple
  (run 35658518788 job 106528082244) + Control/Required + ci-required
  (T-001 refreshes). Repos: jackin branch `cicd/s2-repromote-80bc420d`.
  Action: pull failed logs (`gh run view <run> --log-failed`);
  determine missing-tool vs installer-ordering vs generated-install-subset
  vs aggregate-cascade; inspect the branch's generated unit install
  content and planner/aggregate wiring (discover exact files from the
  failing job's executed commands — do not guess paths); evaluate velnor
  #1054/#1044 as candidate fixes. Pitfall: §C pause snapshot is stale;
  aggregates may cascade from one root — prove it, don't assume. Done
  when: each failing check has a named root cause + fix location (or a
  proven-cascade statement with evidence). Dep: T-001. (Fix itself =
  T-004 work under §1 diagnose-before-fix.)
- T-004 Land #1065. Start: T-003 root causes + fixes on the branch.
  Action: implement fixes (squash-merge observed in this estate — confirm
  method at resume), re-fetch reviews/threads/checks at final head,
  disposition all threads, merge only when green without bypass. Done
  when: #1065 MERGED, merge SHA recorded. Dep: T-003.
- T-005 s2 live proofs (jackin, new `proof/*` branches off post-T-004
  main). Precedent: s1 #1058/#1060 patterns. (a) AGENTS.md-only change
  (no-consumer instruction file; exact file = the one #1058 touched —
  discover via `gh pr view 1058 --json files`): expect 0 workload jobs,
  `planned_no_work=true`, gate green; denominator ("41") = live unit
  count from Control/Planning output — read it, don't assume 40/41.
  (b) Scripts probe: TBD scope — define from #1058's probe coverage,
  record definition before running. (c) Positive control (#1060
  pattern): covered-paths change selects exactly owners. Read-out per
  proof: run URL + job counts + gate verdict. Done when: all three
  green with cited run URLs, or a named product gap filed as a new
 blocking-defect task with evidence (never a weakened criterion).
  Dep: T-004. Known unknown: exact proof branch names/files/commands —
  discover from #1058/#1060 file lists, never invent.
- T-006 §8 rules+docs verification. Start: auditor spot-reports the
  change-aware rule already in `crates/velnor-workflow/AGENTS.md`
  (UNVERIFIED by coordinator — confirm); `content/docs` expansion status
  unknown. Action: confirm rule text ≈ A.2-§8 quote, lean, enforced by
  tests; inventory docs against the 12 §8 topics; close gaps or record
  precise remaining edits; validate docs build + links. Done when: rule
  + docs + validation evidence recorded. Dep: T-001 (parallel with T-003+).
- T-007 §9 16-case matrix. Start: case 1 + case-11-Bun-leg partially
  traced (§B); rest unknown. Action: for each §9 case 1–16 + oracle +
  independent-verifier + security-reviewer items, record DONE (test
  path + last pass) / PARTIAL / ABSENT by inspecting
  `crates/velnor-workflow/tests/` and CI proof runs; implement or file
  gaps (implementation beyond matrix = new scope only if S1 demands
  it — it does; flag honestly). Done when: matrix complete with
  evidence links; ABSENT items have owner tasks or acceptance-risk notes.
  Dep: T-001 (parallel with T-003+).
- T-008 Velnor self-regen check. Start: unknown whether Velnor's own
  `.github/` was regenerated+verified at a post-D3 rev. Action: inspect
  velnor main self-pin + generator-state sidecar; if stale, regenerate
  via supported renderer at an identifiable rev and verify CI; else
  record rev + run evidence. Done when: self-regen rev + verification
  recorded, or explicitly waived with user-traceable reason (waiver
  needs evidence, not convenience). Dep: T-001.
- T-009 Final completion audit + FINAL REPORT. Start: T-004–T-008
  evidence. Action: requirement-by-requirement audit against the 13
  acceptance bullets (A.2-§10) using §L states; resolve §F-adjacent
  ownership; deliver FINAL REPORT (7 items, A.2-§10) as the resume
  session's final message (S1 names no archive file — do not invent
  one; pointer may be appended to §K-notes). Guard: #1016 stays
  historical evidence; no retroactive-fix claims. Done when: every
  bullet proven or precisely-blocked with recovery. Dep: T-004–T-008.
- T-010 Integration close + cleanup (only after T-009 passes). Action:
  delete T-002 leftovers; remove §E goal /tmp repos + worktrees
  (`velnor-pin-1e454958`, `velnor-repin-trial` + its branch,
  gen-refs, `velnor-gen-history`); delete local
  `cicd/repromote-major1-9660c9ff` (remote ref needs explicit
  resume-time confirmation — local cleanup never auto-deletes remote);
  close #1067 after recording final SHAs. Never touch §E-foreign,
  §F-foreign, proof branches. Done when: chain map (§M) shows every
  goal resource integrated-or-justified + disposed, re-verified live.
  Dep: T-009.

Chain map (resource → remote → PR → target → disposition): handoff branch
→ origin → #1067 → main → T-010 close; 11+3 merged branches → ∅ →
MERGED PRs → T-002 delete; preserve branch+worktree → origin 4300cc63 →
no PR (intentional) → T-010 remove; pin/gen detached refs → none → n/a
→ T-010 remove; `cicd/s2` → origin → #1065 → main → T-004 land;
`repromote-major1` → origin live → #1062 CLOSED → T-010 local delete
(+confirmed remote); proofs → origin → OPEN proofs → KEEP.

## J. First resumption task + resume command

First: T-001 (fetch, fast-forward mains, re-verify #1065 live state),
then T-003 diagnosis. Resume executes the ORIGINAL goal (§A), not another
handoff cycle.

Resume command (explicit user message required):

    /goal Read and resume docs/goal-handoffs/change-aware-minimal-work--20260921T220413Z--pine-io--2100e49a.md

Checkout (file lives on the unmerged handoff branch, velnor primary):

    git fetch origin && git switch handoff/change-aware-minimal-work-20260921

## K. Publication receipt

- Handoff commits: `084b6c34` (handoff) + `1d7c2a74` (receipt) + audit
  repair commit (SHA recorded in PR #1067 body, never inside this file).
- Branch push: `handoff/change-aware-minimal-work-20260921` → `origin`.
- Draft PR: https://github.com/tailrocks/velnor/pull/1067 (base `main`, do-not-merge).
- Jackin side: no commit needed (clean, pushed @ `909a9f54` at pause).
- Merging and local cleanup: documented (§T), NOT performed.

## L. Requirements matrix (audit-built; source S1 unless noted)

`ID | Source | Operative requirement | Handoff § | State/evidence | Remaining | Coverage`

G-001 S1-preamble | smallest-sound-plan product goal | A.1/A.4 | merged slices §B | T-004/T-005/T-009 | COVERED
G-002 S1-preamble | no research/plan/docs-only/hardcode/hand-edit finish | A.1 | code slices §B | T-009 guards | COVERED
G-003 S1-§1+S2 | delegate-first subagent execution | I | audit used 3 subagents | T-tasks runbook | COVERED
G-004 S1-§1+S3 | autonomous, never-ask | I | — | runbook | COVERED
G-005 S1-§1+S4 | commit-often/push/DCO identity/minimize branches | I | DCO commits §K | runbook | COVERED
G-006 S1-§1 | merge discipline (threads, re-fetch, no bypass) | I,T-004 | — | T-004 | COVERED
G-007 S1-§1 | correctness-over-ROI; remove enabling condition | I | — | T-003/T-004 | COVERED
G-008 S1-§2 | incident evidence + historical repro + 5 whys | A.2,§E-src | /tmp/velnor-src@4fa7a3a8 | T-003 uses | COVERED
G-009 S1-§2 | failing regression before structural fix | A.2 | D1–D3 tests merged (SECONDARY) | T-007 confirms | COVERED
G-010 S1-§3A–H | full product contract incl. no-work-first-class | A.2 | D1/D2/D3/Major-1 §B | T-005/T-009 prove | COVERED
G-011 S1-§4 | whole-PR/event change semantics | A.2 | runtime slices §B (SECONDARY detail) | T-007/T-009 | COVERED
G-012 S1-§5-rust | Rust dependent rules, no fan-out/back-expansion | A.2 | slices §B (SECONDARY detail) | T-007 | COVERED
G-013 S1-§5-fam | all-families coverage inventory | A.2 | Bun legs §B | T-007/T-009 | COVERED
G-014 S1-§5 | typed contracts; known-vs-unknown doctrine | A.2 | contracts §B (D2/D3) | T-005 | COVERED
G-015 S1-§6 | pre-setup plan; no-work starts nothing | A.2 | #1058 counts §B | T-005 | COVERED
G-016 S1-§6 | required-check contract + fail-closed + trust bounds | A.2,C | #1065 gate BLOCKED not green §C | T-003/T-004 | COVERED
G-017 S1-§7 | optimization records + cache doctrine | A.2 | — | T-009 assesses | COVERED
G-018 S1-§8 | AGENTS.md lean rule, test-enforced | A.2 | rule spotted (UNVERIFIED) | T-006 | COVERED
G-019 S1-§8 | content/docs 12 topics + build/link validation | A.2 | unknown | T-006 | COVERED
G-020 S1-§9 | 16 cases + oracle + 2 independent reviewers | A.2 | case1/11-leg partial | T-007 | COVERED
G-021 S1-§10 | vertical slices; generic-first | B | 11 slices §B | — | COVERED
G-022 S1-§10 | regen+verify Velnor-own + jackin consumers | B,C | jackin chain §B/C; self unknown | T-008 | COVERED
G-023 S1-§10 | rollout ordering; #1016-historical-only; no retro claims | A.2,T-009 | — | T-009 guard | COVERED
G-024 S1-§10 | live-CI demos + old-vs-new metrics | B,C,T-005 | s1 proofs §B | T-005 | COVERED
G-025 S1-§10 | 13 acceptance bullets + 7-item Final Report | A.2,T-009 | — | T-009 | COVERED
H-001 S5 | freeze/preserve/document/publish/stop, stay paused | K,M | branch+PR §K | — | COVERED
H-002 S5 | exhaustive worktree/clone/branch/PR inventory | D,E,F | §§D–F + scope §E | — | COVERED
H-003 S5 | dependency-ordered integrate+cleanup plan for later | T | §§T-001–T-010 | — | COVERED
H-004 S5 | no merge/cleanup during pause | K | none performed | — | COVERED
H-005 S5 | compact receipt + resume command | J,K | receipt delivered | — | COVERED
H-006 S6 | audit §§1–11 + outcome + receipt, then stop | M | this section | receipt on publish | COVERED

Superseded: none (S4-repeat restates S4; S5 suspends but never narrows S1).

## M. Audit record (2026-09-21T22:34Z repair)

- Sources: S1 FULL (session log L20, 32973 chars; handoff condenses §§1–10
  with verbatim preamble+principle+acceptance core); S2 FULL (L29687);
  S3 FULL (L47089); S4 FULL (L1913 + L8943/9059/9588 signoffs, order
  S4→S2→S3→repeat); S5 FULL (L47837, 39385 chars); S6 FULL (audit msg);
  S7 live reads. Register discrepancy fixed: S1 was "FULL verbatim" but
  transcribed as summaries — now labeled condensed+verbatim-core.
- Reviews performed: 3 parallel read-only subagents (intent IF-*, state
  ST-*, preservation IP-*; findings in coordinator /tmp, terminal, zero
  mutations) + fresh-reader pass on repaired draft (below).
- Material gaps found → repaired: §A 5-line summary omitted §§1–10
  operative content (IF-001–011), amendments (IF-012–014), merge
  discipline (IF-015), verbatim quote + acceptance gate (IF-017/022);
  §C CI drift unrecorded (ST-004: 3 fails now, Swift resolved);
  proof run-numbers half-secondary (ST-007/008); §8/§9-case-matrix/
  self-regen untracked (ST-020–023); I.2/I.4/I.5 vague (ST-030–032);
  fix/s2 upstream + #1070 drift + bundle miscount 21→23 + 2 omitted
  foreign clones (IP-002/010/011/013); ambiguous pin/preserve/remote-ref
  dispositions (IP-006/018/019); missing G/H matrix + T-plan (audit §4/§7).
- Fresh-reader review: verdict RESUMABLE-WITH-GAPS → repaired to
  RESUMABLE. 7 findings (FR-001 publish-before-remote-resume, FR-002
  session-log path, FR-003 this line, FR-004 stale-tag, FR-005 checkout
  lines, FR-006 covered by FR-002, FR-007 repair-window dirt): all fixed
  in this revision; live spot-checks (#1065 3-fail state, #1059 merge
  claim) matched byte-for-byte.
- Source-record location: full session log (S1 L20, S2 L29687, S3 L47089,
  S4 L1913 + signoffs L8943/L9059/L9588, S5 L47837) at
  `$HOME/.local/share/muse/sessions/2026/09/21/01a0c14d-5940-76d3-bb71-acbc0b658693/session.jsonl`
  (local to this machine; §A carries all essential meaning if unreachable).
- Unresolved: S1 full 33KB text not embedded (condensed + verbatim core;
  recoverable from session log — acceptable, future agents use §A);
  T-003/T-005 exact files/commands intentionally discovery-based (inventing
  them would be fabrication); §F-adjacent ownership (T-003/T-009).
- Outcome: PARTIAL (all verifiable gaps repaired; S1-condensation and
  live-CI drift bounds keep this from VERIFIED — see Unresolved).
  Original goal remains `PAUSED_BY_USER`; audit ≠ engineering completion.
- Audit publication: branch `handoff/change-aware-minimal-work-20260921`,
  commit (this repair) + push + PR #1067 body sync; triggered CI recorded
  in PR, no CI-repair loop entered.
