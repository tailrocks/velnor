# GOAL: Make Velnor a genuinely change-aware, dependency-aware, minimal-work CI/CD workflow generator

## Handoff metadata

- Handoff ID: `change-aware-minimal-work--20260921T220413Z--pine-io--2100e49a`
- Created (UTC): 2026-09-21T22:04:13Z
- Last update (UTC): 2026-09-21T22:23:23Z
- Original goal status: `PAUSED_BY_USER`
- Handoff status: `READY`
- Source agent/CLI: Muse Code (session `pine-io` / `01a0c14d-5940-76d3-bb71-acbc0b658693`), goal `goal-5b993493-6960-4e99-b548-5d470bd5355d`
- Primary repository: `tailrocks/velnor` (local: `/Users/donbeave/Projects/velnor-optimizations/velnor`)
- Secondary repository: `jackin-project/jackin` (local: `/Users/donbeave/Projects/velnor-optimizations/jackin`)
- Handoff path (repo-relative): `docs/goal-handoffs/change-aware-minimal-work--20260921T220413Z--pine-io--2100e49a.md`
- Preservation branch: `handoff/change-aware-minimal-work-20260921` (from `origin/main` @ `45ef1ebe`)
- PR base: `main`; PR URL: (filled at publication; draft, do-not-merge)
- Resume authorization: explicit later user request only.

`PAUSED_BY_USER` freezes local work, not GitHub-hosted runs: jackin #1065 still
shows a `pending` Swift job at audit time. No merge or local cleanup was
performed during this handoff; both are documented below for post-resume execution.

## A. What this goal is

Make Velnor select the smallest sound execution plan from the full event
change set and discovered/declared task dependencies. Proven-irrelevant
changes (e.g. AGENTS.md-only, jackin #1016) must schedule no unrelated
workloads. Enforced permanently in generator code, tests, `crates/velnor-workflow/AGENTS.md`,
and `content/docs`. No hardcoded AGENTS.md exclusion, no hand-edited generated YAML.

## B. Completed and merged (verified 2026-09-21T22:2xZ via `gh pr view`)

Velnor (`tailrocks/velnor`), each local tip SHA == PR head SHA, all `MERGED`:

| PR | Branch @ tip | Merge | Content |
|----|--------------|-------|---------|
| #996 | integrate/change-aware-minimal-work@0c7fefc4 | 7480b78c | runtime slice integration |
| #1001 | rollout/change-aware-runtime@e0694a84 | (merged) | required-check aggregate binding |
| #1003 | rollout/d19-pin-9374a4d3@61690487 | (merged) | D19 pin advance |
| #1007 | rollout/plan-lane-scope@16e371e1 | (merged) | lane scoping |
| #1008 | rollout/d19-pin-44243ed4@df83ac59 | (merged) | D19 pin advance |
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
| #1052 | rollout/velnor-wave@986f94bc | (merged) | schema-2 migration (stale base; regressed pin, superseded by #1065) |
| #1062 | cicd/repromote-major1-9660c9ff@0d369a85 | CLOSED unmerged | stale s1 fast-follow, superseded by #1065 |

Live proofs (prior session evidence, branches retained): jackin #1058
(D3 re-proof 0/40, `planned_no_work=true`), #1060 (positive control 27/27).
Proof branches stay open as do-not-merge evidence.

## C. In flight at pause (the resume frontier)

- **jackin #1065** `cicd/s2-repromote-80bc420d` @ `909a9f54`, state `OPEN`.
  Repromote to generator `80bc420d` with s2 closed-world contracts.
  CI: exactly one `fail` = `rust-jackin-usage-ffi / Apple`
  ([job](https://github.com/jackin-project/jackin/actions/runs/35658518788/job/106528082244),
  run 35658518788, 40s); one `pending` Swift/XcodeGen job; Apple-leg
  `skipping` cascade; everything else `pass` (Policy, DCO, Control/Planning green).
  Local checkout (`.../jackin` on this branch) is clean and in sync (ahead=0, behind=0).
- **Not yet started**: s2 0/41 AGENTS.md-only proof, scripts probe, positive
  control on s2, final completion audit + report. These require #1065 landed first.

## D. Branch inventory (primary checkouts)

Velnor local (`.../velnor`), HEAD `45ef1ebe` == `origin/main`:

| Branch | Tip | Upstream | Verdict |
|--------|-----|----------|---------|
| handoff/change-aware-minimal-work-20260921 | 45ef1ebe | origin/main, sync | THIS handoff; push + draft PR |
| main | 89a963f7 | origin/main behind 72 | stale pointer; update after resume |
| fix/s1-nested-bun-watch-scoping | 1acbf8f4 | remote deleted | merged #1051; safe to delete after resume |
| fix/s2-nested-bun-watch-scoping | f7bebb42 | sync | merged #1059; safe to delete after resume |
| followup/d3-major-1-closed-world-visibility | 7de01e1c | remote deleted | merged #1046; safe to delete after resume |
| integrate/change-aware-minimal-work | 0c7fefc4 | remote deleted | merged #996; safe to delete after resume |
| rollout/* (7 branches) | e0694a84 df83ac59 61690487 16e371e1 e05f507c 0895cc1e 1f44e3bb | remote deleted | merged #1001/#1008/#1003/#1007/#1033/#1016/#1039; safe to delete after resume |
| preserve/repin-trial-be78a6d0 | 4300cc63 | none, but == origin/preserve/repin-trial-be78a6d0 | preserved evidence; keep until final audit |

Jackin local (`.../jackin`), HEAD `909a9f54` == `origin/cicd/s2-repromote-80bc420d`:

| Branch | Tip | Upstream | Verdict |
|--------|-----|----------|---------|
| cicd/s2-repromote-80bc420d | 909a9f54 | sync | ACTIVE resume frontier (#1065) |
| main | 799deeef | origin/main behind 33 | stale pointer |
| chore/velnor-01bc16b2 | d321e88e | remote deleted | merged #1042; safe to delete after resume |
| chore/velnor-d3 | 4ba4a4cb | remote deleted | merged #1054; safe to delete after resume |
| rollout/velnor-change-aware-7480b78c | 76cb7c7c | remote deleted | merged #1019; safe to delete after resume |
| cicd/repromote-major1-9660c9ff | 0d369a85 | sync | closed #1062, superseded; delete after resume |
| proof/* (4 branches) | 09e0d3f6 5a53e63c b4262258 0587381f | sync | open proof evidence; KEEP |

Squash-merge note: `git cherry origin/main <branch>` shows `+` lines on
multi-commit branches (integrate/*, rollout/*, chore/*). This is a squash
artifact, not unmerged work: every tip SHA byte-matches its MERGED PR head
(§B). Resume step I.1 re-verifies with cumulative diff before any deletion.

Stashes: none in either primary repo. Working trees: both clean except this
untracked handoff file (committed by this handoff).

## E. Worktree / clone inventory

Primary checkouts: `.../velnor` (handoff branch), `.../jackin` (#1065 branch).
Registered worktrees (`git worktree list`):

| Path | Repo | State | Verdict |
|------|------|-------|---------|
| /private/tmp/velnor-pin-1e454958 | velnor | detached 1e454958, clean | base-validator pin reference; keep |
| /private/tmp/velnor-repin-trial | velnor | preserve/repin-trial-be78a6d0@4300cc63, clean, pushed | preserved evidence; keep |

Other `/tmp` git repos (all verified this session; `dirty=N` = `status --porcelain` lines):

Goal-related, keep until final audit:

| Path | State | Note |
|------|-------|------|
| /tmp/velnor-gen-4dec6b9e | detached 4dec6b9e, clean | pre-D3 regressed pin reference |
| /tmp/velnor-gen-80bc420d | detached 80bc420d, clean | s2 runtime reference (origin = primary velnor) |
| /tmp/velnor-src | detached 4fa7a3a8, clean | incident-revision repro (goal §2 generator rev `4fa7a3a8...`) |
| /tmp/velnor-gen-history | main@01bc16b2, 1159 deleted files | stale D1-era scratch; files deleted, no work product; leave as-is, remove after resume |

Foreign goals (do not touch; owned by other workers):

| Path | State | Owner signal |
|------|-------|--------------|
| /tmp/velnor-b07-port | integrate/b07-buildkit-ceilings@36c2d6b2, clean | b07 goal |
| /tmp/velnor-b21-review | detached c2343268, clean | b21 review |
| /tmp/velnor-handoff-b04e988e | goal-handoff/velnor-consolidation-b04e988e@5496db7e, clean | velnor-consolidation handoff (= velnor #1065 head) |
| /tmp/jackin-int1002 | integrate/branch36-verify-preview@85a7bd39, clean | branch36 (= jackin #1066 head) |
| /tmp/jackin-b24review | HEAD@1f302c03, clean, origin=/tmp/jackin-int1002 | branch36 review |
| /tmp/jackin-b26-review | main@b9cdae2c, 2612 deleted files | b26 stale scratch |
| /tmp/jackin-handoff-07927053 | goal-handoff/jackin-consolidation-07927053@15a4fb2f, clean | jackin-consolidation handoff (= jackin #1070 head) |

Unmapped, no content value: `/tmp/jackin-b28r` (empty repo, no commits);
`/tmp/{jackin-1019,jackin-re3,...}` and `/tmp/velnor-{18-*,d1-verify,...}`
are NOT repos (logs/scratch). `/tmp/*.bundle` (21 jackin-branch-*, 2
consolidation, 2 evidence, 1 velnor-recovery) all belong to foreign goals;
this goal produced no bundles.

Independent clone: `/Users/donbeave/Projects/github/velnor`, clean,
HEAD `5349ec32` (= velnor #1063 p962-port head, foreign goal). Untouched.

Counts: 2 primary checkouts, 2 registered goal worktrees, 4 goal-related
/tmp repos, 7 foreign /tmp repos, 1 foreign independent clone, 0 stashes,
0 local-only commits (preserve branch matches its remote SHA).

## F. Open-PR inventory (verified this session)

Goal-owned, must land or re-prove after resume:

| PR | Head | Note |
|----|------|------|
| jackin #1065 | 909a9f54 | s2 re-scope; BLOCKED on FFI-Apple fail (§C) |
| jackin #1030/#1045/#1058/#1060 | proof branches | do-not-merge proof evidence; keep open |
| jackin #1067 | 0515ad33 | post-#1053 live-results record (no-work claims FAIL/unproven); evidence doc |

Adjacent (touches goal surface; disposition on resume):

| PR | Head | Note |
|----|------|------|
| jackin #1044 | 6ff54ce5 | Apple CI generic-recipe migration; s2/Apple context for #1065 |
| velnor #1052 | b084c416 | skip redundant Rust bootstrap (§7 territory) |
| velnor #1055 | 2a270947 | fail closed on producer failures (§6 territory) |
| velnor #1056 | 9f795b2e | product inputs in selection ownership (§5 territory) |
| velnor #1057 | 70268cd5 | scheduled-check permission scoping (§6 territory) |
| velnor #1058 | 93cd45e9 | composable regen phases |
| velnor #1054 | 256c24bb | shared derived Mise facts (empty body; possible #1065 relevance) |
| velnor #1050 | 96b835d9 | scheduled-check merge_group/concurrency (likely sibling goal) |

Foreign (other goals; do not touch): velnor #962 #963 #973 #978 #979 #980
#1044 #1063 #1064 #1065; jackin #1004 #1007 #1063 #1064 #1066 #1068 #1069 #1070.

## G. Unmapped or local-only work

None with content value. Specifically: no uncommitted changes (both trees
clean modulo this file), no stashes, no unpushed commits on any live branch,
no local-only branches except `preserve/repin-trial-be78a6d0` whose SHA
`4300cc63` already matches its remote. The two mass-deletion /tmp checkouts
(§E) are stale scratch, not work product. Ownership of §F-adjacent PRs is
the only open mapping question; it does not block preservation.

## H. Worker status

No live workers in this session: pause-handoff work ran single-threaded
after context compaction; no subagents spawned, none running. Prior-session
worker claims (re-scope stopped, s2 merger terminal) could not be
re-verified post-compaction and are NOT relied upon; resume step I.0
re-establishes ground truth from remote state only.

## I. Dependency-ordered resume plan (AFTER explicit user resume; NOT done now)

0. Re-sync: `git fetch --all --prune` in both primaries; re-check #1065
   head/checks (main may have moved; #1065 may have been retried).
1. Verify merged-branch deletion safety: for each §D "safe to delete"
   branch, confirm `git diff <merge-base>..<tip>` content is subsumed by
   its merged PR (squash-aware spot check), then delete local branches and
   prune stale tracking refs. Keep proof branches and preserve branch.
2. Diagnose #1065 FFI-Apple failure (run 35658518788): missing tool vs
   installer ordering vs generated install subset. Check whether velnor
   #1054 (Mise facts) is the fix; if so, land it first, re-pin, re-render.
3. Land #1065 (no bypass; re-fetch reviews/checks at final head).
4. Run s2 proofs: AGENTS.md-only 0/41 (`planned_no_work=true`), scripts
   probe, positive control; compare against s1 results (#1058/#1060).
5. Final completion audit against goal §§1–10 acceptance criteria + final report.
6. Cleanup (only after audit passes): remove §E goal /tmp scratch
   (`velnor-gen-history` et al.; keep pin/reference checkouts until then),
   delete superseded local branches, close this handoff PR.
   Never touch §E-foreign or §F-foreign resources.

## J. First resumption task + resume command

First task: I.0 re-sync + re-verify #1065 state, then I.2 FFI-Apple diagnosis.

Resume command (explicit user message required):

    /goal Read and resume docs/goal-handoffs/change-aware-minimal-work--20260921T220413Z--pine-io--2100e49a.md

## K. Publication receipt (filled by publisher)

- Handoff commit: (SHA after commit)
- Handoff branch push: `handoff/change-aware-minimal-work-20260921` → `origin`
- Draft PR: (URL after creation; base `main`, do-not-merge)
- Jackin side: no commit needed (clean, pushed @ `909a9f54`)
- Merging and local cleanup: documented in §I, NOT performed during handoff.
