# GOAL: Consolidate every remote branch of tailrocks/velnor into main (oldest-first)

## A. Identity and pause status

| Field | Value |
|---|---|
| Handoff ID | `velnor-branch-consolidation--20260921T220250Z--muse-code--b04e988e` |
| Created (UTC) | 2026-09-21T22:02:50Z |
| Last update (UTC) | 2026-09-21T22:02:50Z (skeleton) — full doc timestamp set at publish |
| Original goal status | `PAUSED_BY_USER` |
| Handoff status | `PREPARING` → set `READY`/`BLOCKED` at §7 verification |
| Source agent/CLI | Muse Code (agentic CLI). Session `01a0bfbe-9f13-7061-8d37-00494245ec74` ("verdant-polaris"). Goal `goal-defdc6ec-56f0-475d-9f1f-55bc2c08c2b6`, 96%. Session log: `$HOME/.local/share/muse/sessions/2026/09/20/01a0bfbe-9f13-7061-8d37-00494245ec74/session.jsonl` (local-only) |
| Repository | `https://github.com/tailrocks/velnor` (remote `origin`, https). Primary checkout `/Users/donbeave/Projects/github/velnor` |
| Handoff path (repo-relative) | `docs/goal-handoffs/velnor-branch-consolidation--20260921T220250Z--muse-code--b04e988e.md` |
| Source branch / HEAD at pause | Primary checkout on `integrate/p962-port` @ `5349ec3297f5c2fcd13fb303c312abc307a88f97` (clean, = remote) |
| Preservation branch | `goal-handoff/velnor-consolidation-b04e988e` @ `<SHA filled at publish>` |
| PR base / observed SHA | `main` @ `45ef1ebe` (#1062; observed via `ls-remote` during audits; local `main` is stale at `14a9ff84`) |
| Handoff PR | `<URL filled at publish>` (DRAFT, auto-merge off) |
| Recovery portability | **NOT fully remote-portable.** Remote-portable: HANDOFF + supporting files on preservation branch, all pushed branches/PRs. Local-only (same-machine): `/tmp/velnor-recovery-20260921.bundle` (107M, deleted-branch history), `/tmp` reports not committed (b40–b58 reports, recon files — summarized in committed ledger copy), session logs. Details §E.2/§K. |
| Resume authorization | Explicit later user request only, via `/goal Read and resume docs/goal-handoffs/velnor-branch-consolidation--20260921T220250Z--muse-code--b04e988e.md` |

**Worker stop status (verified).** At pause directive, exactly one goal worker was believed running
(`policy-mechanic`, subagent `01a0c5f3-…`); its result had already been delivered terminally by the
runtime, and a stop message was rejected with `terminal` — no cancellation needed, no work lost, its
full verdict preserved at `/tmp/policy-mechanic.txt` and committed as a supporting file. All other
goal workers (`p962-port`, `p1063-review`, `p962-challenge`, earlier investigators) were already
terminal with results recorded. The 4 handoff audit agents were pause-scoped (read-only) and are
finished. No goal-owned watchers, retry loops, or background shell tasks remain: `ps` shows no cargo/
velnor/goal-`gh` processes (only an unrelated `vi COMMIT_EDITMSG` in another clone, and jackin-related
polling — both untouched, see §E.2).

**Runtime goal control — limitation recorded.** The agent-accessible controls are
`get_goal`/`report_progress`/`update_goal`, and `update_goal` accepts only `complete`/`blocked`.
There is **no supported runtime "pause" operation**, and calling `complete`/`blocked` would misstate
the goal. The original goal object therefore remains `active` at 96% with progress text recording
`PAUSED_BY_USER`; the pause is expressed by this HANDOFF + the user's directive, not by a runtime
state change. `PAUSED_BY_USER` is the requested goal disposition; it is not proof a runtime job was
stopped (no runtime continuation mechanism exists beyond user/goal-turn invocation, which the user
controls).

**No-merging-or-cleanup rule.** During this handoff: no merges, no branch deletions, no PR closes,
no worktree removals, no stash operations, no `gc`/`prune`. Integration + eligible cleanup are
documented for post-resumption execution only (§E.5/§E.6/§H).

## B. Original goal and success contract

### B.1 Verbatim core (recovered from session `01a0bfbe…`, seq 19 → seq 511; byte-identical across
all 23 goal turns per goal-audit, sha256 `4cbcef5024cb…`)

> Repository: https://github.com/tailrocks/velnor/branches/all
>
> Target branch: main
>
> Your highest priority is to consolidate every remote branch in this repository into main by
> evaluating branches from oldest to newest, preserving and improving everything that supports the
> project's current direction, rejecting what no longer belongs, and deleting each processed source
> branch. Work on this repository only. Normalize the repository URL if it includes /branches/all or
> another GitHub subpath.
>
> Execute the work through completion. A plan, branch inventory, recommendation, draft PR, queued
> merge, or local-only integration is not the finished result. The intended final state is a verified
> main containing all accepted work, appropriately resolved PRs, and no other remote branches
> remaining in this repository.

Plus 10 numbered procedure sections (§1 direction baseline; §2 oldest-first queue + age-fallback
policy; §3 delegate-first/one-active-branch; §4 per-branch analysis; §5 independent challenge;
§6 PR workflow + frequent commits; §7 review/verify/head-guard; §8 close + bundle + lease-delete;
§9 autonomy + ledger; §10 completion audit). Full 33,396-char text committed as supporting file
`docs/goal-handoffs/supporting/velnor-consolidation-b04e988e/original-objective.txt`
(local source `/tmp/audit-objective.txt`). Pause directive full text likewise at
`.../supporting/.../pause-directive.txt` (local `/tmp/audit-pause-full.txt`).

### B.2 Consolidated statement (reconstruction, not quotation)

Consolidate every remote branch of `tailrocks/velnor` into `main`, oldest-first per frozen age keys
(PR-date → fallback committer-date, byte-order ties), via selective ports through the repo PR
workflow with independent challenge on every disposition, lease-guarded deletions, and a recovery
bundle — subject to the amendments in §B.3 — ending in verified green main with only `main`
remaining. **PAUSED BY USER at 96%.**

### B.3 User amendments / standing preferences (verbatim trims from goal-audit)

1. Commits always with `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` — exclusively; never any
   other identity (never Alessio Romano).
2. Commit often / push regularly / minimize branch proliferation.
3. `Fix commits here: https://github.com/tailrocks/velnor/pull/994` and
   `https://github.com/tailrocks/velnor/pull/985/commits` (historical; #985 merged).
4. `Verify all commits from Alessio Romano <alessio@romano.com> ... using security subagents` (sic).
5. Delegate-first ×6: `delegate first, parallelize aggressively, verify independently, then integrate.`
6. Never-ask-questions autonomy; continue until goal fully complete (superseded for now by pause).
7. This pause: freeze, handoff-only objective, draft-PR checkpoint, post-resumption
   integration+cleanup plan, resume only via explicit `/goal Read and resume`.

### B.4 Scope / non-goals / constraints / acceptance

- **Scope:** `tailrocks/velnor` only; all live `refs/heads/*` except `main`; branches with
  no/draft/closed/merged PRs; during-task branches queued at tail by same key policy;
  concurrent-author merges recorded as externally-resolved, never re-evaluated.
- **Non-goals:** other repos/forks; tags/pull-refs/symbolic HEAD as deletion targets; unrelated
  production ops, state files, credentials, manual publishing; disabling checks/bypass; rewriting
  main history; destroying active publishing channels.
- **Constraints:** oldest-first frozen keys, one active source branch; repo is squash-only, branch
  auto-delete on, ruleset = DCO+Policy+ci-required+thread resolution, 0 approvals; independent
  challenge before every disposition; `Signed-off-by: Alexey Zhokhov` exclusively; recovery bundle
  before any ref removal; guarded deletion only (`--force-with-lease=<ref>:<reviewed-SHA>`, never
  unconditional retry); head-SHA merge guards + `git log base..main` before merging; `ls-remote`
  authoritative (tracking refs go stale).
- **Acceptance (§10, condensed):** every branch disposed oldest-first with evidence; accepted work on
  latest main; rejections reasoned and not reintroduced; PRs merged/closed with explanations; checks
  + post-merge verification pass on the **final** main SHA; all source + temp branches deleted; fresh
  full `refs/heads` inventory contains **only main** (timestamped); nothing local-only/unmerged/dirty/
  unpushed; publishing/ops intact; ledger + recovery artifacts durable; final report with SHAs, outcome
  table, PR/commit links, verification results.
- **Post-resumption duty (from pause directive):** integrate all required related goal work, resolve
  its PRs, clean up verified obsolete goal-owned local worktrees/branches — only after explicit
  resumption with fresh safety checks. Not blind merging of every experiment; shared/unrelated
  resources stay intact.

## C. State at the exact interruption point

**Last completed action.** #961 SUBSUMED disposition: source branch deleted lease-guarded and verified
absent from `ls-remote`; PR closed 2026-09-21T20:46:14Z with subsumption comment naming #963 +
patch-ids + redundant commit. Verified live by verify-auditor this handoff.

**In-progress action (frozen mid-flight).** #962 (oldest active source,
`codex/github-first-hosted-g1-security-3ae` @`43ba3b41`, PR #962 OPEN, untouched since 09-20)
selective-port: port PR #1063 OPEN (`integrate/p962-port` @`5349ec32`, 2 commits, 1 file
`crates/velnor-workflow/src/s2/primitives/release.rs` +234/-5, base `eed474c4`, merge-base
`eed474c4`). Independent review returned CHANGES-REQUESTED with exactly one finding: required
`Policy` red (run 35657382801, rule `generated-tree` FAIL 9m50s); scope exact (sorted-diff empty both
commits), 5 ported tests sensitive (fail-without/pass-with proven), threads clean, all other checks
green. Policy-mechanic then root-caused the failure deterministically (stale declared pin `6737cdb3`
vs post-#1060 validator `eed474c4`; fix = rebase onto current main, inherits pin = validator; #1060
cleared as suspect; reviewer's "any PR fails identically" refuted with 6 green post-#1060 PRs).
Mechanic verdict arrived terminally at pause time — **read and preserved, NOT acted on**: no rebase,
no push, no merge, no close, no deletion performed.

**Exact next intended action (FIRST resumption task).** Reconcile + re-pin (fetch-only, no
mutations): `git fetch origin --prune`; record main tip (was `45ef1ebe` #1062, expect drift); fresh
`ls-remote` inventory vs ledger; append missing ledger entries (#1060/#1061/#1062 merges, #1058 queue
insert, #1063 review+mechanic summary, reincarnation triage); re-grep #962 load-bearing anchors on
current main. Then fix #1063 per mechanic recipe (§G/§H).

**Partial states / open hypotheses.** No partially edited files (all worktrees clean, no stashes, no
in-progress merge/rebase/cherry-pick — except possible stale `MERGE_RR`/`COMMIT_EDITMSG` residue in
WT-3, status clean, see §E.2). Open items: (a) 3 no-PR live refs need reincarnation-vs-leftover triage
(`integrate/apple-ci-s2` +4 ahead of merged #985; `codex/ci-performance-campaign` +1 WIP ahead of
merged #968; `codex/rolling-preview-legacy-migration-20260920` DIVERGED +7/−11 vs merged #966 incl.
G10-adjacent gating); (b) #1058 tail insert never ledger-recorded; (c) tail branches advanced past
ledger SHAs (externally, by author); (d) challenger pins (`155c6088`) are 3 behind main — load-bearing
claims need mechanical re-grep, drift so far proven disjoint.

**Workers / jobs at pause.** All goal workers terminal (see §A). No remote jobs launched by this
task still running: #1063 CI runs completed (Policy FAIL deterministic; rest pass/skip). #1044 has an
active CI run (7 pending) — author-side activity on a queued tail branch, not task-launched; observe,
don't touch.

## D. Requirement-by-requirement progress ledger

`ID | Requirement | Status | Evidence/files/commits | Remaining work | Dependencies`

| ID | Requirement | Status | Evidence | Remaining | Deps |
|---|---|---|---|---|---|
| FB-01–58 | 58 fallback branches disposed (ports via #998/#1000/#1004/#1013/#1021/#1034/#1040/#1047, rest nil/rejected) | VERIFIED_DONE | Ledger ll.1–490; spot-check: fb refs absent from `ls-remote` (19 heads, none fb*) | None | — |
| Q-955/957/960 | PR branches NIL + closed+deleted | VERIFIED_DONE | Ledger; refs absent | None | — |
| Q-961 | SUBSUMED by #963, closed+deleted | VERIFIED_DONE | `ls-remote` empty; PR CLOSED 20:46:14Z + pointer comment | None | — |
| Q-962-inv | Investigate #962 @43ba3b41 | VERIFIED_DONE | `/tmp/p962-report-agent.md` (pins matched) → committed supporting `p962-report.md` | Re-grep load-bearing claims on new main | — |
| Q-962-chal | Independent challenge of #962 verdict | VERIFIED_DONE | CONFIRM-PORT, scope corrected to 2 commits (ea9686f0 + 525fc9e0); session log local-only | None (re-verify mechanically at port-fix time) | Q-962-inv |
| P-1063-port | Port 2 commits → PR #1063 | VERIFIED_DONE (unmerged) | #1063 @`5349ec32` OPEN; scope exact, tests sensitive | Rebase onto current main + force-push | Reconcile (§H-1) |
| P-1063-ci | #1063 full-green CI | BLOCKED (deterministic stale-pin fail) | Policy run 35657382801 FAIL; mechanic recipe simulated clean | Rebase → rerun → confirm oracle-1058 path | P-1063-port |
| P-1063-merge | Merge + verify main | NOT_STARTED | — | Head-guarded squash merge; inspect `base..main` first; anchor+test verify | P-1063-ci |
| Q-962-close | Close #962 + lease-delete source | NOT_STARTED | — | Explanation + port pointer; delete @`43ba3b41`; verify absent | P-1063-merge |
| Q-963/973/978/979/980 | Frozen queue PR branches | NOT_STARTED | All OPEN; #963 green, #973 green, #978–980 conflicting+red (stacked, base `97bac4c4`) | Full investigate→challenge→port/nil→delete each | Q-962-close |
| Q-reinc | Moved-branch triage (#966/#968 branches, `integrate/apple-ci-s2`) | NOT_STARTED | PRs #966/#968/#985 MERGED; branches live at new SHAs | Classify reincarnation vs leftover; queue or drop with evidence | Reconcile (§H-1) |
| Q-tail | Tail #1044,#1050,#1052,#1054–#1058 (+#1053 slot) | NOT_STARTED | 9 OPEN (#1058 unrecorded); most green; author merging fast | Externally-merged check first, else evaluate each | Q-963…980 |
| FIN | Pin-forward + green main + only-main + §10 audit | NOT_STARTED | Deferred per ledger decision | Pin to tip, CI/Main+Preview+Runtime green, heads==main, GAP-2 re-check, final reviewer | Q-tail |

"Implemented" ≠ "verified": P-1063-port is content-verified but unmerged; P-1063-ci BLOCKED (not
failed-content). Position ~96% holds: 58/58 fallbacks + 4 queue PRs done; in-flight #962/#1063 +
5 queue + ≤9 tail + final remain.

## E. Change and preservation inventory

**What changed during the goal (all on remote already, except where noted).** 8 task port PRs merged
to main via squash (#998→`de5a1c46`, #1000→`c72eccb9`, #1004→`70c05dd5`, #1013→`02bb53bf`,
#1021→`cc284950`, #1034→`be781371`, #1040→`65b82bb5`, #1047→`14a9ff84`); ~50 source branches
closed/rejected and lease-deleted (recovery: local bundle §E.2); #1063 port PR open unmerged
(2 commits `228fc58f`←`ea9686f0`, `5349ec32`←`525fc9e0`, release.rs +234/-5). **No uncommitted goal
work exists**: every worktree clean, no stashes, primary checkout clean on `integrate/p962-port`.
Pre-existing/unrelated changes intentionally untouched: sibling repos, grok worktree WT-5, clone C1.

**Checkpoint contents (this handoff PR).** HANDOFF (this file) + supporting directory
`docs/goal-handoffs/supporting/velnor-consolidation-b04e988e/`: `ledger.md` (80K copy of
`/tmp/velnor-ledger.md`), `original-objective.txt`, `pause-directive.txt`, `p962-report.md`,
`p1063-review.md`, `policy-mechanic.md`, `audit-goal.md`, `audit-worktree.md`, `audit-branchpr.md`,
`audit-verify.md`. Essential material NOT in the diff and how to recover it: recovery bundle
(local `/tmp/velnor-recovery-20260921.bundle` — see §K nonportable note); b40–b58 per-branch reports
+ recon files (local `/tmp`, summarized in committed ledger copy); session logs (local
`$HOME/.local/share/muse/...`, volatile across machines).

### E.1 Discovery scope and ownership

Inspected (all read-only, 2026-09-21 ~22:00–22:10Z): `git worktree list --porcelain` + per-worktree
status/HEAD in C0; `git branch -a -v`, `for-each-ref`, `stash list`; `git ls-remote --heads origin`
(19 refs, authoritative); `gh pr list --state all --limit 200` + per-open-PR `view`/`checks`;
`/Users/donbeave/Projects/github` top-level dir listing with per-dir `remote -v` (bounded);
`/tmp` goal-artifact listing + `git bundle verify`; non-intrusive `ps`; session JSONL greps;
`/tmp/velnor-ledger.md` full read. NOT scanned: arbitrary home dirs, whole filesystem, other
machines. Coverage uncertainty: (a) local `main` + tracking refs stale (behind; `ls-remote` used
instead); (b) `/tmp` + session logs are machine-local and reboot-volatile; (c) clone C1 + WT-5
observed, not entered (ownership unknown/unrelated). Ownership classes: `GOAL_EXCLUSIVE` (task
branches/PRs/worktrees), `GOAL_SHARED` (`main`, this handoff branch/PR), `UNRELATED`
(sibling repos, grok WT-5, jackin procs), `UNKNOWN` (C1 clone, `goal/` + `_consolidation/` data dirs).

### E.2 Local worktree and clone ledger

Common repo C0: main worktree `/Users/donbeave/Projects/github/velnor`, gitdir
`/Users/donbeave/Projects/github/velnor/.git`.

| ID | Path | Type / owner | Branch / HEAD | State | Contents / preservation | Disposition (future) |
|---|---|---|---|---|---|---|
| WT-0 | `/Users/donbeave/Projects/github/velnor` | main checkout (C0). GOAL_SHARED | `integrate/p962-port` @`5349ec32` = remote tip | CLEAN, no stash, no untracked; ignored: only `target/` + `crates/velnor-workflow-contract/target` | No unpushed work; branch = P-1063-port vehicle | KEEP (primary; NOT_APPLICABLE for removal) |
| WT-1 | `/private/tmp/pr1040-review` | linked detached. GOAL_EXCLUSIVE (review scratch) | detached @`e191eeaa` | CLEAN, on disk, not locked/prunable | Snapshot of merged #1040 head; needs no preservation (on main as `65b82bb5`) | INTEGRATE_THEN_REMOVE (already integrated; remove worktree post-resume) |
| WT-2 | `/private/tmp/pr1047-review` | linked detached. GOAL_EXCLUSIVE | detached @`b7cf08ae` | CLEAN, on disk | Snapshot of merged #1047 head (on main `14a9ff84`) | INTEGRATE_THEN_REMOVE (same) |
| WT-3 | `/private/tmp/velnor-b07-port` | linked branch. GOAL_EXCLUSIVE | `integrate/b07-buildkit-ceilings` @`36c2d6b2`, **upstream gone (local-only)** | status CLEAN but `MERGE_RR`+`COMMIT_EDITMSG` residue → possible stale interrupted-op metadata | **Highest-risk local-only state**; bundle (03:27) may predate it; needs push-or-bundle decision at resume | BLOCKED (preserve first: push to remote or extend bundle; then decide integrate vs drop per ledger b07 entry) |
| WT-4 | `/private/tmp/velnor-gen-4dec6b9e` | linked detached. GOAL_EXCLUSIVE | detached @`4dec6b9e` | CLEAN, on disk | Generator scratch; no unique commits identified | INTEGRATE_THEN_REMOVE (verify no unique content, then remove) |
| WT-5 | `/private/var/.../T/grok-goal-b2531dc065d4/implementer/velnor-048` | linked detached. UNRELATED (external grok-goal agent) | detached @`048a7bda` | CLEAN, on disk | Not goal work; never entered beyond listing | KEEP (do not touch; NOT_APPLICABLE) |
| WT-6 | `/tmp/velnor-handoff-b04e988e` (created by this handoff) | linked branch. GOAL_SHARED | `goal-handoff/velnor-consolidation-b04e988e` @`<SHA at publish>` | Contains only HANDOFF + supporting files | Pushed + draft PR | REVIEW_SHARED (keep until goal completes; then remove worktree + delete branch with PR) |

Other locations: C1 `/Users/donbeave/Projects/velnor-optimizations/velnor` — independent
tailrocks/velnor clone, branch `fix/s2-nested-bun-watch-scoping` @`f7bebb42`, CLEAN; a `vi
.../COMMIT_EDITMSG` (PID 57502) suggests a mid-edit commit; ownership UNKNOWN (another agent/user
session?) — observed via `ps` only, untouched, KEEP/REVIEW_SHARED. Sibling dirs `velnor-actions-
fixture`, `velnor-apt`, `homebrew-velnor`, `homebrew-tap` are separate repos (UNRELATED). `goal/`
(not a repo; one review md) and `_consolidation/velnor/` (LEDGER.md + bundles/ + reports/, data dir,
not a clone) are UNKNOWN, untouched. No stashes anywhere. No missing/inaccessible worktrees
(`prune --dry-run` empty; all gitdirs resolve).

/tmp goal artifacts (local-only, sizes/mtimes 2026-09-21/22): bundle 107M `git bundle verify` OK,
66 refs; ledger 80K; 19× `b40…b58-report-agent.md`; 5× `p955…p962-report-agent.md`; `pr1/pr2-recon.md`;
`p1063-review.txt`; `policy-mechanic.txt`; `p105{3,8,9},p1061,p1063-policy.log` (~100K ea);
`audit-objective.txt`; `audit-pause-full.txt`. Gaps: all reboot-volatile; only ledger + key reports
are committed as supporting files; bundle has NO remote copy.

### E.3 Local and remote branch ledger

Remote (`ls-remote`, 19 refs; tips observed ~22:05Z; main has been advancing hourly — re-observe):

| Remote ref | Tip | Ownership | Upstream/remote-tracking note | PR | Purpose / queue note |
|---|---|---|---|---|---|
| `main` | `45ef1ebe` (#1062 squash, empty commit) | GOAL_SHARED | local `main` stale @`14a9ff84` (behind 9+); tracking refs stale | — | Target |
| `codex/github-first-hosted-g1-security-3ae` | `43ba3b41` | GOAL_EXCLUSIVE | — | #962 OPEN | ACTIVE source; port vehicle #1063 |
| `integrate/p962-port` | `5349ec32` | GOAL_EXCLUSIVE | = WT-0 HEAD, in sync | #1063 OPEN | Selective port (2 commits) |
| `codex/github-first-g3-integration-signed` | `056362aa` | GOAL_EXCLUSIVE | — | #963 OPEN | Next after #962; subsumed #961 |
| `codex/velnor-legacy-rolling-tag-repair-20260920` | `04da35e4` | GOAL_EXCLUSIVE | — | #973 OPEN | Queue |
| `codex/ci-performance-next` | `970a6dd5` | GOAL_EXCLUSIVE | — | #978 OPEN | Queue (stack base `97bac4c4`) |
| `fix/ci-validation-contract` | `9bbf4a4e` | GOAL_EXCLUSIVE | — | #979 OPEN | Queue (stacked) |
| `refactor/holla-parity` | `ab2f12fa` | GOAL_EXCLUSIVE | — | #980 OPEN | Queue (stacked) |
| `codex/activation-foundation` | `60bb9326` | GOAL_EXCLUSIVE | advanced past ledger `4ad4b28e` | #1044 OPEN | Tail |
| `fix/desktop-candidate-evidence` | `96b835d9` | GOAL_EXCLUSIVE | advanced past `1ff4d4a7` | #1050 OPEN | Tail |
| `fix/rust-cache-hit-bootstrap` | `b084c416` | GOAL_EXCLUSIVE | — | #1052 OPEN | Tail; green but CONFLICTING |
| `fix/apple-mise-tool-closure` | `256c24bb` | GOAL_EXCLUSIVE | moved from `b397c052` | #1054 OPEN | Tail |
| `fix/product-receipts` | `2a270947` | GOAL_EXCLUSIVE | moved from `964ef069` | #1055 OPEN | Tail |
| `fix/native-product-closure` | `9f795b2e` | GOAL_EXCLUSIVE | moved from `47269f1c` | #1056 OPEN | Tail |
| `codex/schedule-actions-read` | `70268cd5` | GOAL_EXCLUSIVE | moved from `59335f2e` | #1057 OPEN | Tail, fully green |
| `fix/composable-regen-phases` | `93cd45e9` | GOAL_EXCLUSIVE | NEW, created 20:32:45Z, never ledgered | #1058 OPEN | Tail insert |
| `integrate/apple-ci-s2` | `326fd414` | GOAL_EXCLUSIVE | unknown locally | none | Reincarnated +4 ahead of merged #985 head `2f684383`; needs queue key |
| `codex/ci-performance-campaign` | `155d6b81` | GOAL_EXCLUSIVE | — | none (PR #968 MERGED @`17318e52`) | Reincarnated +1 WIP; needs queue key |
| `codex/rolling-preview-legacy-migration-20260920` | `f587b89f` | GOAL_EXCLUSIVE | — | none (PR #966 MERGED @`432da515`) | DIVERGED +7/−11 (G10-adjacent); needs queue key |

Local branches in C0 (all clean, none checked out except WT-0/WT-3): stale `main` (`14a9ff84`);
`fix985`/`fix985b`, `pr-977`, `pr-977-review` (local-only leftovers, no upstream — awareness only);
`integrate/b08-hosted-admission` (tracks main, behind 83); ~12 `[gone]` integrate/rollout/fix branches
(merged earlier; local tips only). No detached tips outside worktrees. No stashes. Handoff branch
`goal-handoff/velnor-consolidation-b04e988e` (GOAL_SHARED) created from `origin/main` during this
handoff — see WT-6.

### E.4 Related PR ledger

15 OPEN (observed ~22:05Z):

| # | Head → Base | Mergeable/state | Checks (tested head) |
|---|---|---|---|
| #1063 (task port) | `5349ec32` → `c674f5bb` (1 behind tip) | MERGEABLE / BLOCKED | Policy FAIL only (run 35657382801); DCO/ci-required/workflow/docker/topology pass; rest skip |
| #962 | `43ba3b41` → `10483370` | CONFLICTING / DIRTY | Policy FAIL + DCO pass (stale); moot — closes unmerged |
| #963 | `056362aa` → `325719f1` | UNKNOWN | 22 pass / 48 skip, zero fail |
| #973 | `04da35e4` → `9e5c0eb2` | UNKNOWN | 11 pass / 40 skip, zero fail |
| #978 | `970a6dd5` → `97bac4c4` | CONFLICTING / DIRTY | 6 FAIL |
| #979 | `9bbf4a4e` → `97bac4c4` | CONFLICTING / DIRTY | 6 FAIL |
| #980 | `ab2f12fa` → `97bac4c4` | CONFLICTING / DIRTY | 8 FAIL |
| #1044 | `60bb9326` → `45ef1ebe` | MERGEABLE / BLOCKED | 14+ pass, 7 PENDING (active author-side run) |
| #1050 | `96b835d9` → `45ef1ebe` | MERGEABLE / CLEAN | all pass/skip |
| #1052 | `b084c416` → `14a9ff84` | CONFLICTING / DIRTY | ALL GREEN incl Policy — rebase-only |
| #1054 | `256c24bb` → `45ef1ebe` | MERGEABLE / CLEAN | all pass/skip |
| #1055 | `2a270947` → `45ef1ebe` | MERGEABLE / CLEAN | all pass/skip |
| #1056 | `9f795b2e` → `45ef1ebe` | MERGEABLE / CLEAN | all pass/skip |
| #1057 | `70268cd5` → `45ef1ebe` | MERGEABLE / CLEAN | 22 pass, fully green |
| #1058 | `93cd45e9` → `45ef1ebe` | MERGEABLE / BLOCKED | workflow+Required+ci-required FAIL; Policy PASS (oracle log for #1063 fix) |

Merged task ports: #998, #1000, #1004, #1013, #1021, #1034, #1040, #1047 (all squash, commits §E).
Merged author PRs (externally-resolved, not re-evaluated): #985, #966, #968, #1053, #1059, #1060,
#1061, #1062, + earlier per ledger. Closed-unmerged this session: #955, #957, #960, #961 (with
explanatory comments). Handoff PR: `GOAL: ... — paused handoff [b04e988e]` (URL filled at publish;
DRAFT, base `main`).
Stack: #978/#979/#980 share base `97bac4c4`. No other head→head stacking detected.

### E.5 Integration map and ordered landing plan — FUTURE EXECUTION ONLY

Map (not performed): WT-0 (`integrate/p962-port` @`5349ec32`, remote in sync) → PR #1063 → `main`
(after rebase-fix + green + head-guarded squash); then #962 close + lease-delete source. WT-3 local-only
`integrate/b07` → preserve (push/bundle) → disposition per ledger b07. All other items are remote
branches evaluated in queue order at resume; nothing local-only to land except WT-3.

Ordered plan (resume only): (1) reconcile (§H-1); (2) rebase #1063 onto tip, force-push, green CI;
(3) re-review #1063 on new head; (4) squash-merge head-guarded + verify main; (5) close #962 +
lease-delete @`43ba3b41`; (6) triage 3 reincarnations (fresh queue keys; rolling-preview diverged —
full evaluation likely); (7) queue #963→#973→#978→#979→#980 sequentially (one active branch;
#978–980 stacked — map deps explicitly); (8) tail oldest-first with merged-check first (author
merging concurrently — expect external resolutions); (9) pin-forward to tip + full green-main +
only-main inventory + §10 audit. Parallelizable: read-only recon of later branches, independent
reviews. Sequential: all target-branch mutations (merges, closes, deletions).

### E.6 Post-integration local cleanup runbook — FUTURE EXECUTION ONLY

Execute only after explicit resumption + verified integration + re-observation of each row. No
wildcards, no force, no shared-`.git` manipulation; `git worktree remove` (no `--force` on dirty/
active); guarded branch `-d` (narrow exception for squash-merged only with coverage proof).

| Resource | Host/path/ref | Expected tip | Final target | Integration proof | Recovery ref | No-use gates | Proposed action | Status |
|---|---|---|---|---|---|---|---|---|
| WT-1 | `/private/tmp/pr1040-review` | `e191eeaa`, clean | main `65b82bb5` | merged #1040 squash | main commit | re-observe clean/detached/unlocked | `git worktree remove` | PENDING resume |
| WT-2 | `/private/tmp/pr1047-review` | `b7cf08ae`, clean | main `14a9ff84` | merged #1047 squash | main commit | same | `git worktree remove` | PENDING resume |
| WT-4 | `/private/tmp/velnor-gen-4dec6b9e` | `4dec6b9e`, clean | N/A (scratch) | verify no unique commits (`log --all --contains`) | N/A | same | `git worktree remove` | PENDING resume |
| WT-3 | `/private/tmp/velnor-b07-port` + branch `integrate/b07-buildkit-ceilings` | `36c2d6b2`, clean + residue | TBD at resume | TBD (ledger b07) | push-or-bundle FIRST | BLOCKED until preserved + dispositioned | preserve → decide → remove | BLOCKED |
| Stale local branches | `[gone]` integrate/rollout/fix + `fix985*`/`pr-977*`/`b08-hosted-admission` | tips §E.3 | main (merged) or drop | per-branch coverage check | bundle/local | re-observe tip + no worktree uses it | individual `git branch -d` | PENDING resume |
| WT-6 | `/tmp/velnor-handoff-b04e988e` + handoff branch | publish SHA | handoff PR | PR merged/closed at goal end | PR + main | goal complete; HANDOFF retained on main or PR ref | remove worktree, delete branch | PENDING goal end |
| WT-0/WT-5/C1/siblings | — | — | — | — | — | — | KEEP (excluded; see §E.2) | NOT_APPLICABLE |

After cleanup: re-enumerate worktrees/branches, reconcile with ledger, write durable receipt.

## F. Decisions, findings, assumptions, and rejected approaches

- **Externally-resolved precedent** (settled): author-concurrent merges recorded, never re-evaluated
  (ledger ll.34–505 list incl. #1059/#1060/#1061/#1062).
- **During-task branches queue at tail** by PR-date key (settled; ledger).
- **Defer intermediate D19 pin advances** until queue completion (settled; ledger ll.211–215).
- **#961 SUBSUMED by #963** (settled): 17/18 patch-ids verbatim, gap byte-empty; content deferred to
  #963's turn — #963's turn MUST still evaluate the subsumed content.
- **#962 SELECTIVE-PORT scope** (settled, challenged): exactly `ea9686f0` + challenger-added `525fc9e0`;
  close unmerged. Subset-flag ruled PORT-AS-IS (declared stanza replaces release spec; `[workflow]`
  universe coherent). One reviewed semantic: declared stanza *without* the key falls back to provider
  universe instead of inheriting `config.release.verification_providers` (matches `None` docs; no
  in-repo impact).
- **#1063 Policy-red root cause** (finding, proven): stale pin `6737cdb3` vs validator `eed474c4`
  after #1060; candidate in env slot can satisfy pin leg only when head closure == pin closure.
  All four digests recomputed locally, match CI exactly.
- **#1060 cleared** (finding): actual tree diff is pin `6737→eed474c4` (title's `80bc420d` misleads);
  main self-consistent; validator rev = base's declared pin by design. No harness repair.
- **Reviewer's "any PR fails identically" REFUTED** (finding): 6 post-#1060 PRs Policy-green
  (1058/1056/1055/1054/1050/1044); blast radius = #1063 only.
- **Fix = rebase, never pin-to-head** (settled for #1063): pin-to-head would pass this PR but poison
  post-merge `pull_request_target` (setup `rev:` → unpublished product, breaks all future Policy).
  Simulation: clean apply, 23/23 cmp-clean, closure unchanged, no re-render commit.
- **Process lessons** (settled): `git log base..main` before every merge; exit-check load-bearing
  greps; `ls-remote` authoritative; queue transcription vs frozen keys.
- **Standing rejections** carried (settled): b16 discovery-lane/gpgv/sentinel, b17 records design,
  b28–b30 native stack, b31/b48 skills detector, b35/b36 native-product lane, b44–b47 transport family.
- **Assumptions needing validation at resume**: main tip (drifts hourly); tail SHAs (author-active);
  reincarnation nature of 3 no-PR refs; #1044's pending run outcome; `/tmp` artifact survival across
  reboot (bundle/ledger assumed present — re-verify first).
- **Rejected**: merging #962 whole (contradicted/superseded bulk); pin-to-head on #1063 (poison);
  rerunning Policy pre-fix (pointless); re-evaluating author merges; treating `CLOSED` as `MERGED`.

## G. Verification evidence and known failures

| Check | Result | Command / source | Tested rev | Evidence |
|---|---|---|---|---|
| #1063 scope exactness | PASS | sorted content-line diff per commit (implementer + reviewer) | `5349ec32` vs `eed474c4` | empty both; +234/-5 one file |
| #1063 tests local | PASS | `cargo test -p velnor-workflow` (worktree @head) | `5349ec32` | lib 2486/0, release-filtered 376/376, 27 binaries 0 fail |
| #1063 test sensitivity | PASS | revert-fix-keep-tests in worktree | `5349ec32` | all 5 ported tests fail with expected messages |
| #1063 regen no-op | PASS | `generate . --check` (+`--pin-build` in mechanic sim) | `5349ec32` (+sim rebase) | exit 0; 23/23 cmp-clean |
| #1063 clippy/fmt | PASS | `--all-targets`, `fmt --check` | `5349ec32` | 0 warnings, clean |
| #1063 CI (non-Policy) | PASS | `gh pr checks 1063` run 35657383054 | `5349ec32` | DCO/ci-required/workflow(6m46s)/docker/topology pass; 14 skip |
| #1063 Policy | FAIL (deterministic, structural) | run 35657382801 `generated-tree` | `5349ec32` | `PINNED_BINARY … reports closure 0a64505f…, not pin's closure`; NOT content-caused |
| #962 tip CI | FAIL (procedural, moot) | stale run: Policy fail + DCO pass | `43ba3b41` | bootstrap chicken-and-egg; PR closes unmerged |
| #961 disposition | PASS | `ls-remote` + PR state | N/A | ref absent; CLOSED 20:46:14Z + pointer |
| Bundle integrity | PASS | `git bundle verify` | `/tmp/velnor-recovery-20260921.bundle` | OK, 66 refs |
| Rebase simulation | PASS (sim only) | detached worktree + cherry-picks, removed | onto `45ef1ebe` | clean apply; closure unchanged |
| Post-fix Policy path | NOT RUN (oracle cited) | PR #1058 log `/tmp/p1058-policy.log` | `93cd45e9` | `pin eed474c4 shares base closure … 11 rules, 0 failed` |
| Main-tip claims | STALE on resume | pins `155c6088`/`c674f5bb` vs tip | `45ef1ebe`+ | re-grep required; drift proven disjoint so far |
| Full final green-main | NOT RUN | deferred to FIN | — | — |

No full validation campaign run during handoff; only bounded nonmutating reads (audits) + this
publication's own checks (§7).

## H. Ordered remaining-work plan

1. **FIRST TASK — Reconcile + re-pin (fetch-only).** `git fetch origin --prune`; record main tip;
   fresh `ls-remote` vs ledger; append missing entries (#1060/#1061/#1062, #1058 insert, #1063
   review+mechanic, reincarnation triage); re-grep #962 anchors on current main. Validation: ledger
   current, inventory matches. No deps.
2. **Fix #1063 per recipe.** `git rebase origin/main` (expect clean; verify `generate . --check`
   exit 0; commit re-render only if diff); force-push `integrate/p962-port`. Do NOT pin-to-head.
   Validation: clean apply + `--check` green. Deps: H-1.
3. **Re-review #1063.** Scope exact on new head; CI rerun → expect oracle-1058 path (11 rules,
   0 failed); threads clean. Independent reviewer. Deps: H-2.
4. **Merge #1063.** `git log base..main` first; head-guarded squash merge; verify main (pins,
   anchors, lib suites). Deps: H-3.
5. **Close #962 + delete source.** Explanation + port pointer comment; close unmerged;
   `git push --force-with-lease="refs/heads/codex/github-first-hosted-g1-security-3ae:43ba3b41"`
   deletion (re-verify tip first); confirm absent. Deps: H-4.
6. **Queue #963→#973→#978→#979→#980** sequentially, full cycle each (investigate→challenge→
   port/nil→verify→close→lease-delete); map #978–980 stack deps; #963 must cover #961-subsumed
   content. Deps: H-5.
7. **Triage reincarnations** (`integrate/apple-ci-s2`, `ci-performance-campaign`,
   `rolling-preview-legacy-migration`) — can parallelize recon with H-6, disposition in queue order.
   Deps: H-1 (recon) / H-6 (disposition).
8. **Tail oldest-first** (#1044,#1050,#1052,#1054–#1058 + #1053 slot): merged-check first, else full
   cycle. Deps: H-6/H-7.
9. **Finalize.** Pin-forward PR to tip; full green-main (CI/Main + Preview + Runtime); fresh only-main
   `ls-remote` (timestamped); GAP-2 re-check; §10 audit with independent final reviewer; final report.
   Deps: H-8.
10. **Post-integration cleanup** per §E.6 with per-resource receipt. Deps: H-9.

Carry-overs: #966 turn checks G10 flag; #973 turn diffs bodies vs b52 §5; `.17→.16` follow-up (#1047);
b57 backlog (pagination/masking). Nothing is dropped by being listed here — each rides its queue turn.

## I. Environment and operational recovery

- Host: macOS (darwin; `/private/tmp` paths), primary checkout `/Users/donbeave/Projects/github/velnor`.
- Tools: git, `gh` (auth present), cargo/rust (repo builds; `cargo test -p velnor-workflow`), openssl.
  Commit identity configured: Alexey Zhokhov <alexey@zhokhov.com>; DCO signoff (`-s`) on all commits.
- Repo-generated artifacts reproducible via `cargo run -p velnor-workflow -- generate .` (+`--check`,
  `--pin-build`); irreplaceable: recovery bundle (no remote copy), deleted-branch history.
- External ops performed: pushes of `integrate/*` branches, 8 squash merges, ~50 lease-deletions,
  PR comments/closes, CI runs (all completed/observed). Repeating reads is safe; side-effecting ops
  (merge/close/delete) must be re-observed fresh at resume, never replayed blind.
- No deployments, migrations, releases, or infra changes made. Draft handoff PR may trigger CI —
  recorded, not repaired (§7).

## J. Fresh-agent resume runbook

1. `git clone` (or reuse primary checkout) + `git fetch origin <preservation-branch>`:
   `git fetch origin goal-handoff/velnor-consolidation-b04e988e && git checkout
   goal-handoff/velnor-consolidation-b04e988e` — or open the handoff PR (URL in §A) and read
   `docs/goal-handoffs/velnor-branch-consolidation--20260921T220250Z--muse-code--b04e988e.md` there.
   The unmerged handoff lives on its branch, not on `main`.
2. Read this document completely, then repo instructions (`.github/AGENTS.md`), the committed ledger
   copy, and the supporting reports.
3. Recover checkpoint state into the primary checkout WITHOUT overwriting unrelated work: verify
   `/tmp/velnor-recovery-20260921.bundle` exists + `git bundle verify`; if `/tmp` was wiped, record
   the gap (dispositions stand on remote evidence; deleted-branch recovery is degraded).
4. Compare live state vs recorded: `git ls-remote --heads origin`, open PRs, `origin/main` tip,
   `/tmp` survival, WT-3 survival. Detect intervening changes before acting.
5. Resume the ORIGINAL goal at FIRST TASK §H-1 (reconcile + re-pin), not the handoff task.
   Delegate-first, one active branch, independent challenge, lease-guards, signoffs — all §B
   constraints still apply.
6. Keep `/tmp/velnor-ledger.md` (and commit-back discipline per repo conventions) current with
   scoped checkpoint commits.
7. After goal completion + validation, execute §E.5 integration then §E.6 cleanup with live
   re-checks; final receipt per resource.

Resume command: `/goal Read and resume
docs/goal-handoffs/velnor-branch-consolidation--20260921T220250Z--muse-code--b04e988e.md`

**Interpretation rule.** The pause holds until the user requests resumption. A later `/goal Read and
resume <this-file>` authorizes continuation of the ORIGINAL goal. It does NOT instruct the future
agent to pause again, regenerate this handoff, or recursively create another handoff PR.

## K. Blockers, omissions, and independent review

**Blockers for this handoff:** NONE (if §7 publication checks pass; else listed there).

**Omissions / known limits:**
- Recovery bundle is local-only (`/tmp`, 107M, no remote copy). Deleted-branch history recovery
  requires same-machine access. Mitigation at resume: extend bundle to cover new deletions and
  consider durable storage.
- `/tmp` volatility: all audit/report/log files + ledger live copy + bundle are reboot-volatile;
  only the §E-listed subset is committed. Session logs likewise machine-local.
- Clone C1 (`velnor-optimizations/velnor`) + `goal/` + `_consolidation/` data dirs: ownership
  UNKNOWN, intentionally unentered/untouched. WT-5 (grok) UNRELATED, untouched. Mid-edit `vi` in C1
  observed, untouched.
- Local `main` stale (`14a9ff84`); tracking refs stale — `ls-remote` used throughout; resume must
  fetch before trusting any cached ref.
- WT-3 `MERGE_RR`/`COMMIT_EDITMSG` residue: status clean, but interrupted-op metadata possible —
  inspect (read-only) at resume before any checkout/commit there.
- Full verbatim objective (33K) committed as supporting file rather than inline; §B.1 quotes the core.
- Handoff PR CI (triggered by publication) is recorded, not repaired — see §7.

**Independent review.** `<reviewer verdict + corrections filled after handoff-review agent runs on
the published PR; must assess: fresh-agent recoverability, next-action clarity, ledger coverage vs
discovery, unmapped/unpublished work, cleanup-gate completeness.>`

