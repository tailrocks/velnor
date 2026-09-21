# GOAL: Agent-policy + Velnor-workflow migration across 34 repos

> PAUSED / WIP — checkpoint for later resumption; not a completion or merge claim.

## A. Identity and pause status

- Handoff ID: `velnor-agent-policy-workflow-rollout--20260921T220154Z--shore-polaris--e3119d71`
- Created (UTC): 2026-09-21T22:01:54Z (pause order received). Last update: 2026-09-21T22:41:24Z.
- Original goal status: `PAUSED_BY_USER` (requested disposition; not proof a runtime job stopped —
  there is no remote runtime job; all work was local agents + GitHub API effects listed in §C/§G).
- Handoff status: `READY` (review cleared; PR publication checks in §K + PR body).
- Worker stop status (checked via `subagent_status` → `not_found`, i.e. zero running):
  - `redmain-fix/92`: clean stop, full written report delivered before exit. VERIFIED.
  - `w2-singles/75`, `deadlock-probe/79`, `completion-queue/91`: stop orders queued 22:02Z,
    unacknowledged after ~40 min; cancelled via `subagent_cancel` (accepted); terminal
    payloads then delivered: 75 = stale mid-run note (no final; work evidenced via merges),
    79 = full deadlock report (`/tmp/report-deadlock.md`), 91 = full pause report
    (`/tmp/h91-paused.md`, key item: unpushed termcomp `4527fda9`, now preserved remotely).
    VERIFIED STOPPED (cancel accepted + roster empty). Shared-placement worktrees untouched.
  - Handoff auditors 94/95/96/97: finished, delivered, none still running.
- Runtime goal-state control: NONE AVAILABLE. `get_goal` shows the goal `active` (97%);
  `update_goal` supports only `complete`/`blocked` (calling either would misrepresent state);
  `create_goal` fails while a goal is active. The pause is therefore administrative:
  this document + worker stops + no further goal work until explicit resume. Recorded honestly
  as a control limitation, not a verified runtime transition.
- Source: Muse Code CLI; session `01a0c10d-b452-7a12-ae8b-bff74cd6a23d` ("shore-polaris");
  goal `goal-78ac4476-108d-46b5-aea3-3106694ba8f9`.
- Primary repo: `tailrocks/velnor`. Handoff path (repo-relative):
  `docs/goal-handoffs/velnor-agent-policy-workflow-rollout--20260921T220154Z--shore-polaris--e3119d71.md`
- Source branch / checkpoint: `goal-handoff/velnor-rollout-20260921-e3119d71`, base
  `origin/main@45ef1ebe` ("chore(ci): bump D19 pin to eed474c4 (#1062)"). Local worktree:
  `/tmp/velnor-handoff-e3119d71` (linked to `…/all-repo/tailrocks_velnor/.git`).
- PR: https://github.com/tailrocks/velnor/pull/1068 (DRAFT, auto-merge off). Base
  `main@45ef1ebe` (observed at freeze; re-check at resume). Published head SHA: see PR body
  / final receipt (commit after this metadata update).
- Remote-portable? PARTIALLY. All merged code, open PR heads (except one), ruleset edits, and the
  termcomp preservation ref are on GitHub. LOCAL-ONLY dependencies (explicit): `/tmp` scratch
  (ledgers `/tmp/audit-*.md`, `/tmp/report-*.md`, `/tmp/final-table.md` NOT built, `/tmp/cq-*`
  and `/tmp/velnor-*` worktrees) and session logs
  (`…/sessions/2026/09/21/01a0c10d-b452-7a12-ae8b-bff74cd6a23d/`, ~97 MB `session.jsonl` +
  `subagent/*/session.jsonl` full worker evidence). Nothing in §E depends on them for
  correctness of the resume plan, but re-derivation without them costs hours.
- Resume authorization: explicit later user request only
  (`/goal Read and resume <path above>`). No auto-resume.

## B. Original goal and success contract

### B.1 Original objective — VERBATIM (recovered intact from `session.jsonl`, also `get_goal`)

> Implement, verify, and merge a consistent agent-policy and Velnor-workflow migration across
> every repository listed below. This is an execution goal: deliver merged changes and
> verification evidence, not just a plan, suggested files, or open PRs.
> (Scope: the 34 repos in §B.2; body §§1–8 as executed. Full text preserved verbatim in
> parent session log `session.jsonl:13883` and in `/tmp/h94.report.md` §1; reproduced here
> in condensed-but-complete form. The §3 shared-rules markdown and §4 gate are quoted
> exactly as enforced:)

Shared root block (§3, canonical for all non-velnor repos per ruling AG-1 — 15 lines):

```markdown
# Rules

- No legacy code. Finish every migration: remove old paths completely—no compatibility shims, aliases, or deprecation periods. Breaking changes are preferred.
- This is a research project. It is unsafe and expected to contain breaking changes; never treat it as production-ready. Break things when needed and deliver new implementations fast.
- Always apply these principles:
  - Judge work by correctness, consistency, and project fit. Never defer a known-wrong state because of ROI, cost, effort, or claims that it is low-value, marginal, or an edge case.
  - Stop only when the required change is proven impossible with the available tools or model. When uncertain, inspect, test, and measure first.
  - Before fixing a bug, identify why the architecture permitted it and whether the same structure permits related bugs.
  - Prefer fixes that remove the enabling condition. Use a symptom-layer patch only when the root fix is proven infeasible or belongs in a separate change, and name the deferred root cause.
- Delegate first: use subagents for parallel research, implementation, review, and independent verification. Resolve ambiguity autonomously using evidence and project documentation.
- Commit meaningful, verified changes frequently and push regularly. Prefer one working branch; create another only when safe work requires it. Merge small PRs promptly after all gates pass.
- Before every PR merge, read all reviews, comments, replies, and unresolved or outdated threads. Use independent subagents to critically verify findings against code, tests, project documentation, and recorded decisions; research uncertainty.
- For accepted feedback, fix, verify, commit, push, and reply on GitHub with the fixing commit URL before resolving. For rejected feedback, reply with evidence and rationale before resolving. Address general comments in linked PR replies. Never delete feedback or resolve it without a justified disposition.
- Re-fetch feedback at the final head SHA. Merge only with no unaddressed feedback or unresolved review threads and all required checks and approvals satisfied. Only explicit, PR-specific human authorization permits ignoring identified feedback; general merge approval is not a waiver.
- Keep agent instructions lean. Put explanations, plans, and progress in documentation, not here.
```

(Velnor itself keeps one extra LOCAL bullet after `# Rules` — the `actions/runner` protocol
source-of-truth invariant — per §3's local-preservation example and ruling AG-1. It must NOT
propagate to other repos.)

§4 gate (enforced on every goal PR): read ALL reviews/comments/threads (paginate, incl.
outdated/resolved); independent-subagent coverage check; accepted→fix+verify+commit+push+
commit-URL thread reply before resolve; rejected→evidence-backed reply before resolve;
re-fetch at final head; merge SHA-guarded only with zero unresolved threads, no unaddressed
feedback, all required checks+approvals. Sole waiver: explicit PR-specific human authorization.

§5 (velnor owns `.github`): generator emits `.github/AGENTS.md` + `.github/CLAUDE.md→AGENTS.md`
symlink on every path; staged full-tree replacement (no stale preservation); inputs outside
`.github` (`.github-gen/velnor-workflow.toml`); determinism (identical bytes/modes/symlinks);
full-tree drift check; pinned-revision/promotion/trust architecture intact.

§6 runner policy: public = GitHub-hosted only; private = Velnor runners only; visibility from
authenticated metadata; no `both`/fallback lanes; every executable job covered incl. matrices,
reusable chains, dispatch, Renovate, release, scheduled.

§7 rollout: small verified PRs, feedback gate each; final immutable verified velnor revision;
consumers regenerated at it via supported promotion (no temp-branch pins); per-repo
migrate→generate→inspect→verify→merge→verify-main; missing capability ⇒ generic velnor fix.

§8 completion: 8 automated proofs (roots+symlinks; generated nested instructions; stale removal;
determinism+drift check; failure/symlink-escape/recovery tests; provider-policy ±tests;
generic scan; green checks + OBSERVED real runs with runner identity); independent audits;
ledger (not AGENTS.md); final all-repo status table + change/feedback summary with links.
A repo is complete ONLY when merged + reproducible tree + gate satisfied + verification passed.

### B.2 Consolidated scope — 34 repos (goal order; all defaults `main`)

tailrocks/velnor, holla-apt, homebrew-holla, homebrew-tablerock, homebrew-ruxel, tablerock,
parallax, schemalane, ruxel, pg-bigdecimal, velnor-actions-fixture, github-terraform,
tracing-request-level, velnor-apt, cloudflare-tofu, homebrew-velnor, termpane, termrock,
parallax-telemetry-playground, homebrew-parallax, holla; jackin-project/jackin,
jackin-the-architect, jackin-github-terraform, jackin-dev, jackin-sentinel,
jackin-role-action, jackin-agent-smith, homebrew-tap; ChainArgos/java-monorepo,
blockchain-nodes; donbeave/terminal-components-claude, task-format, tui-snap.
Corrections: `donbeave/tui-snap` → transferred to `tailrocks/tui-snap`. Only 3 private
(verified via API): tailrocks/github-terraform, tailrocks/cloudflare-tofu,
ChainArgos/java-monorepo.

### B.3 Amendments (verbatim, from session log)

- Signoff (escalating, 4×): commits MUST carry ONLY
  `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` (`:383`, `:13340`, `:13433`, `:13567`).
- Delegation (5× identical): delegate-first via real subagents; parallelize; verify
  independently; parent orchestrates/integrates (`:38069`…`:51920`).
- Autonomy: never ask; unblock via research/subagents; continue to full completion (`:69907`).
- Commit-often: small incremental commits, push regularly, minimize branches (`:70809`).
- Pause order (`:70899`, 39 KB, §§1–7): freeze, preserve, HANDOFF, draft PR, verify, stop.
  This document is its product.

### B.4 Post-resumption obligation (user-imposed)

On explicit resume: integrate ALL required related goal work, resolve its PRs (merge or
close-with-proof per §4), then clean up verified-obsolete goal-owned local worktrees/branches
per §E.6 gates. Do NOT blindly merge every experiment or delete shared/unrelated resources.
Merge permissions: goal PRs only (never unrelated PRs — explicit non-authorization; velnor
#1054–#1058 `fix/*` are OUT of scope until R-09(c) clears them). No deploys/releases/applies
for migration testing (§7); no tofu apply until R-06 lands.

## C. State at the exact interruption point

Pause order received 2026-09-21T22:01:54Z at 97% (parent progress `:70763`).

- Last completed actions: termpane pin PR #24 merged `3b9cb074` (21:53Z, after poisoned-cache
  deletion + green rerun 35657136448); blockchain #724 merged `a7f3efc1` (21:31Z, pin-3 regen
  `d4de960f`, release.yml 485,051 B — R15 resolved via #1049 aggregator; R15 = the release-workflow
  500KB/file-size incident: `release.yml` rendered 546,071 B > GitHub's 500 KB workflow-file
  limit ⇒ `startup_failure` with zero jobs (evidence run 35636344278); fixed generically by
  velnor#1049's `release-verified` aggregator replacing the 37×36 `needs` fan-out); playground #51/#52
  merged, main green first time (run 35657908148); holla #223/#224/#225 merged; 26 pin-`eed474c4` PRs (15 merged + 10 open + 1
  closed-conflicting #23 — h96 PR-row grep) across taps, holla-apt, schemalane, ruxel,
  pg-bigdecimal, tracing, termrock, holla, task-format, velnor-apt, fixture and others.
- In progress at freeze: (a) 10 green pin PRs awaiting §4+merge (R-01); (b) parallax #122 CI
  re-run after xtask pin-const sync push `5e35123b` (result unchecked); (c) termcomp #8:
  local-only progress (`1078c838` capture-contract fix + merge `4527fda9` — PRESERVED remotely,
  §E.3) but parity-contract still red locally, perf leg unverified; (d) velnor main green
  tail (Preview green @`c674f5bb`, CI green @`45ef1ebe` but Preview never ran there);
  (e) `/tmp/final-table.md` NOT built; (f) §8 re-sweep NOT run.
- Workers at freeze: 92 stopped clean with report; 75/79/91 cancelled after unacked stops
  (terminal payloads captured, §A). 75 never produced a final (its repos evidenced via merges
  + PR states). No worker had unpushed code except CQ termcomp (preserved) and redmain's
  explicitly discardable sed edit (A-18).
- External in-flight at freeze: private-trio main runs queued (gtf 35634398902, cto
  35634424059, jmono 35630181731 — 0 velnor runners, proven capacity block); gtf#31 checks
  queued; blockchain Renovate schedule run in progress (22:15Z); #122 CI pending.
- Partially edited / inconsistent: termcomp #8 head `885d0695` predates main `f3313476`
  (base stale); playground #50 base predates s2; termpane #18 / tablerock #81 / parallax #120 /
  blockchain #722 agent-policy PRs open against moved mains (close-with-proof after verifying
  substance — R-02, except #722 TBD). java #2062 base `05a7320b` EQUALS main HEAD (not moved);
  handled with #2063 (R-04).

## D. Requirement-by-requirement progress ledger

Statuses: `VERIFIED_DONE` | `IMPLEMENTED_UNVERIFIED` | `IN_PROGRESS` | `NOT_STARTED` | `BLOCKED`.

| ID | Requirement | Status | Evidence / files / commits | Remaining | Deps |
|---|---|---|---|---|---|
| REQ-01 | §3 roots: shared block + `CLAUDE.md` 120000 symlink, all repos | IN_PROGRESS | 13 GEN-344 repos already §3-clean (AG-1); 17 need 6-bullet append (folded into pin PRs — merged ones verified: holla-apt 344w, velnor-apt 344w, fixture 344w — h97 §1); termpane fixed; termcomp#7 open; java#2062 open; blockchain#722 open | R-01 merges, R-02 closes, R-03/R-04 | R-01…R-04 |
| REQ-02 | §4 feedback gate on every goal PR | IN_PROGRESS | 0 unresolved threads on all 20 open PRs (h96 GraphQL first:100); merged PRs gated per worker reports | Re-gate each R-01 merge at final head (§4 recheck); retro-check #724 attribution (R-09a) | R-01, R-09 |
| REQ-03 | §5 velnor owns `.github` (inc1–3, G-groups, herd, G12, #1049 aggregator) | VERIFIED_DONE | velnor#990, #992, #1006, G1/G3/G4/G5/G7/G8/G9/G10/G11, #1042 (G12, `bdffa8f4`), #1049 all merged; self-regen proven; R15 release 485 KB < 500 KB live | None (behavioral; re-verify via §8 re-sweep proofs 2–5) | R-10 |
| REQ-04 | §6 visibility runner policy everywhere | IN_PROGRESS | Public violations fixed (termpane/telemetry/velnor-apt regen; blockchain #724 merged hosted-only); private trio fully velnor-only in YAML (audit full-file inventory) | Prove with OBSERVED runs: trio queued = BLOCKED on capacity; record as proven blocker per §6 | Capacity |
| REQ-05 | §7 consumer rollout @ final pin | IN_PROGRESS | Final consumer pin = `eed474c4` (contains G12; SOUND per h97 §3 — runtime products green, redness self-CI-only; "no pin-4" stands). 15 pin PRs merged; 10 open (R-01); 4 repos never got pin PRs (R-12: jackin@pin-2, tui-snap@pin-1, cto@pin-2, gtf@pin-2 — verified live) | R-01 + R-12 merges + post-merge green checks | R-01, R-12 |
| REQ-06 | §8 proofs 1–7 (structural, in velnor tooling + audit) | IMPLEMENTED_UNVERIFIED | Proofs landed with generator PRs (worker-reported); strict repo-level sweep 0/34 PASS pre-fix (`/tmp/audit-sec8.md`) | R-10 re-sweep at post-fix heads | R-01…R-05 |
| REQ-07 | §8 proof 8 (observed CI + runner identity) | IN_PROGRESS | Dozens of observed green runs linked in worker reports + h97 §1 (e.g. velnor CI 35657630434, playground 35657908148, termpane 35659754832) | Observe R-01 post-merge runs; trio blocked (capacity); blockchain red (R-05) | R-01, R-05 |
| REQ-08 | Red mains fix-forward | IN_PROGRESS | tablerock/agent-smith/task-format/tui-snap GREEN (reruns); velnor CI green @`45ef1ebe`, Preview green @`c674f5bb`; termcomp Perf green (#9) | Blockchain NEW red (R-05); termcomp gates red (R-03); confirm velnor Preview-on-HEAD | R-03, R-05 |
| REQ-09 | Ruleset/R8 flips + terraform alignment | IN_PROGRESS | 11 red mains fixed via API Policy-add; blockchain 15177496 + playground 19573032 + task-format 23789846 + tui-snap 23746094 + tablerock 19573034 carry Policy | R-06 verify-then-PR variables.tf (BLOCKS any apply); R-07 gtf#31; R-08 termpane R8 entry | R-06…R-08 |
| REQ-10 | Final table + change/feedback summary (§8 ¶4) | NOT_STARTED | `/tmp/final-table.md` NOT built | R-10 after R-01…R-05 | R-10 |
| REQ-11 | Post-integration cleanup (§E.6) | NOT_STARTED | Ledgers ready (§E.1–E.4) | R-11 LAST | R-10 |

## E. Change and preservation inventory

### E.1 Discovery scope and ownership

- Host: `donbeave-mac` (`Alexeys-MacBook-Pro.local`, user `donbeave`). Single-machine goal.
- Inspected (read-only, 22:02–22:15Z): `/Users/donbeave/Projects/github/all-repo/*` (34 goal
  clones = Farms A+B), `/tmp/velnor-*` + `/tmp/cq-*` + `/tmp/*-checkout` (Farms G–K),
  `…/tailrocks/velnor-project/{velnor,velnor3}` (Farms C–D),
  `…/jackin-project/jackin` (E), `…/github/velnor` (F), `~/Projects/**` L1+L2 (N),
  session dir `…/sessions/2026/09/21/01a0c10d-b452-7a12-ae8b-bff74cd6a23d/` (O + `subagent/*/`),
  `all-repo/ledger/` (O, coordination docs, not git).
- Methods: `git worktree list --porcelain`, `rev-parse HEAD/symbolic-ref/@{u}`,
  `status --porcelain=v1`, `stash list`, `branch -a`, cached-ref ahead/behind (NO fetch by
  auditors; coordinator fetched only `tailrocks_velnor` origin for the handoff base).
  Full command list in `/tmp/h95.report.md` §6. PR discovery: `gh pr list --state all`
  per repo + `head=` probes + GraphQL threads (h96).
- Coverage limits: no-fetch ⇒ behind/ahead vs possibly-stale cached refs; ~300 clean detached
  review clones aggregated (repro commands in h95, not row-listed); 2 corrupt `.git` dirs;
  4 transient sweep candidates; live trees mutated during audit (1 observed transient);
  pre-09-12 goal PRs with deleted branches invisible to the PR method (velnor#988, jackin#1016
  recovered via probes; older merged generator PRs live in main history).
- Ownership classes: `GOAL_EXCLUSIVE` (this rollout only), `GOAL_SHARED` (mixed with other
  agents/goals — RETAIN), `UNRELATED`, `UNKNOWN` (retain until resolved).

### E.2 Local worktree and clone ledger

Farm table (IDs stable; details in `/tmp/h95.report.md`):

| Farm | Common dir (short) | Worktrees | Class | Notes |
|---|---|---|---|---|
| A | `all-repo/tailrocks_velnor/.git` | 23 at audit + A-HO = 24 | GOAL_EXCLUSIVE | Primary velnor clone; handoff worktree ADDED here during pause (row A-HO) |
| B×33 | `all-repo/<repo>/.git` | 1 each | GOAL_EXCLUSIVE | One main worktree per consumer clone |
| C | `velnor-project/velnor/.git` | 85, 11 stashes | GOAL_SHARED | RETAIN — other agents/goals active |
| D | `velnor-project/velnor3/.git` | 369 | GOAL_SHARED | RETAIN — review/fixture farm |
| E | `jackin-project/jackin/.git` | 110 | GOAL_SHARED | RETAIN (2 velnor-adjacent wts noted) |
| F | `github/velnor/.git` | 6 | GOAL_SHARED | RETAIN |
| G | `velnor-ci-quota-repair/.git` | 6 | UNKNOWN (h95: GOAL_EXCLUSIVE as "quota-repair goal") | RECLASSIFIED with rationale: quota-repair sits outside the rollout farm (`~/Projects/work/`, not `all-repo` or `/tmp` wave worktrees) and may belong to a separate goal; owner ruling required before any action. RETAIN either way; `work/*` dirty wts untouched |
| H/I | `dual-lane-apt`, `dual-lane-homebrew` | 7/6 | GOAL_SHARED | RETAIN |
| J | ~120 self-contained `/tmp/velnor-*` | 1 each | mixed | 25 dirty/ahead exceptions row-listed in h95 §4; rest clean detached |
| K | `/tmp/velnor`,`-main`,`-live`,… satellites | 2–8 each | GOAL_SHARED | Small farms; exceptions in h95 §4 |
| CQ | `/tmp/cq-{termpane,velnor,gtf,parallax,termcomp}` | 5 | GOAL_EXCLUSIVE | CQ-v2 worktrees; termcomp had UNPUSHED `4527fda9` — PRESERVED (§E.3) |
| L/M/N | actions-checkout, source audits, `~/Projects` misc | — | UNRELATED/UNKNOWN | Excluded; 3 mass-staged-deletion clones flagged gap-G-09 in Farm M (do not commit blindly) |
| O | `all-repo/ledger/` (not a repo) | n/a | GOAL_SHARED | Coordination docs (`ledger.md`, workorders); read-only reference for resume |

Farm A (complete, all clean except noted; all `rollout/*` UNPUBLISHED in cached refs):

| ID | Path | Branch @ HEAD | Status | Disposition |
|---|---|---|---|---|
| A-00 | `…/all-repo/tailrocks_velnor` | `red-main/velnor-pin-bump@93ad5c4d` | clean, pushed, PR #1060 merged (`c674f5bb`) | keep; delete branch after R-11 gates |
| A-01/02 | `/tmp/g11-gate`, `/tmp/g11-main` | detached `b4fbe636` / `c832191f` | clean | keep (gate evidence) |
| A-03/04/05 | `/tmp/velnor-4fa7a3a8`, `-b9c3156`, `-fresh` | detached pins | clean | keep (fleet refs) |
| A-06…A-14 | `/tmp/velnor-g{1,10,11,12,4,5,7,8,9}` | `rollout/velnor-g*@113a6cda/93675b2a/9c29152d/533aafa6/f6ca3469/3fe0b19f/4b2e2f3d/cb8417bb/78ad2dd2` | clean, +1…+8 vs main | POST-MERGE LEFTOVERS: substance on main (squash); verify tree-coverage at R-11, then remove worktrees + delete branches |
| A-15/16/17 | `/tmp/velnor-gen-{6737,80bc,eed4}` | detached pins | clean | keep until R-11 (pinned binaries), then remove |
| A-18 | `/tmp/velnor-main-80bc` | detached `80bc420d` | U1 sed pin edit | DISCARDABLE (redmain-declared) at R-11 |
| A-19 | `/tmp/velnor-pin-be61acbb` | detached `be61acbb` | T3 cargo output | droppable at R-11 (verify not referenced) |
| A-20/21/22 | `/tmp/velnor-verify{,2}`, `-wavepin` | detached | clean | keep → R-11 |
| A-HO | `/tmp/velnor-handoff-e3119d71` | `goal-handoff/velnor-rollout-20260921-e3119d71@45ef1ebe+` | THIS HANDOFF | RETAIN (primary checkpoint; never a cleanup candidate) |

Worktree-less local-only branches in A: `rollout/velnor-g3@a10da8a1` (+1),
`rollout/velnor-ownership@815f9c40` (+9), `rollout/velnor-pin-inc3@aea5c53f` (+1),
`rollout/agent-policy@3555fd05` (+0). All pre-merge-era; substance on main via squash
merges — VERIFY tree coverage at R-11 before deleting (squash ⇒ no ancestry proof).

Farm B (all clean, stash 0; Publ=NO ⇒ local-only tip, see §E.3):

| Repo checkout | Branch @ HEAD | Publ |
|---|---|---|
| ChainArgos_blockchain-nodes | wave@`b7236da6` | yes |
| ChainArgos_java-monorepo | agent-policy@`bb69f554` | yes |
| donbeave_task-format | wave@`cabd0732` | yes |
| donbeave_terminal-components-claude | red-main/perf-fix-fwd@`640c44d2` | yes |
| donbeave_tui-snap | wave@`99ce8038` | NO (+4) |
| jackin homebrew-tap/agent-smith/-dev/github-terraform/role-action/sentinel/the-architect | wave heads | NO (+1 each) |
| jackin-project_jackin | wave@`986f94bc` | NO (+4) |
| cloudflare-tofu/github-terraform/holla-apt | wave heads | yes |
| tailrocks_holla | wave-pin@`1aed386d` | yes |
| homebrew-holla/-parallax/-ruxel | wave heads | NO (+1 each) |
| homebrew-tablerock | wave@`6db6cfb8` | NO (+4) |
| homebrew-velnor | wave@`4c3cfd26` | yes |
| parallax-telemetry-pg | main@`28bb557a` | yes |
| parallax | wave@`f9009e83` | yes |
| pg-bigdecimal | wave@`3c4035fe` | NO (+2) |
| ruxel/schemalane | wave heads | yes |
| tablerock | wave@`731b4e97` | NO (+3) |
| termpane | wave-s1bump@`4463d6de` | yes |
| termrock/tracing-req-level | wave heads | yes |
| velnor-actions-fixture | wave@`487d7b4c` | NO (+2) |
| velnor-apt | wave@`6b4850c0` | yes |

CQ farm: `/tmp/cq-termpane`, `/tmp/cq-velnor` (on `rollout/unit-cache-keys`, no commits,
clean), `/tmp/cq-gtf` (github-terraform @`ee961e29`), `/tmp/cq-parallax`
(`rollout/pin-eed474c4@5e35123b`, PUSHED), `/tmp/cq-termcomp`
(`rollout/velnor-wave@4527fda9`, was UNPUSHED → preserved §E.3, worktree clean).
CQ extras (verified on disk during handoff): `/tmp/cq-bc` = blockchain-nodes checkout
@`d4de960` (clean; equals merged #724 head → remove at R-11); `/tmp/cq-final` = bare
`velnor-workflow-macOS-ARM64` product binary (reproducible download → droppable at R-11);
`/tmp/xtask.log` (505 KB failure-log extract → keep until R-10, then drop).

### E.3 Local and remote branch ledger (goal-relevant only; full map in h96)

- PRESERVED DURING HANDOFF: `donbeave/terminal-components-claude`
  `goal-handoff/termcomp-wave-4527fda9` = `4527fda964d8e6269df1128acf09ab856f0eb354`
  (contains `1078c838` capture-contract fix + merge of `origin/main@f3313476`). PR #8 head
  deliberately NOT moved during freeze. Resume: review → push to `rollout/velnor-wave`
  (fast-forwardable: `885d0695..4527fda9`? VERIFY first — #8 head is `885d0695`, which is an
  ancestor per W3 lineage; confirm with `merge-base --is-ancestor`) → continue R-03.
- UNPUBLISHED Farm-B tips (16): post-merge PR-branch leftovers (remotes deleted the PR
  branches after squash merges; local checkouts still sit on them). Substance is on main;
  at R-11 verify `diff <tip> <merge-commit>` is empty-ish (squash trees) before deleting.
  Highest-attention: `jackin wave@986f94bc` (+4, #1052 merged `edef2c1e`), `tablerock
  wave@731b4e97` (+3, #82 merged), `tui-snap wave@99ce8038` (+4, #5 merged).
- UNPUBLISHED Farm-A tips (9 + 4 branch-only): same post-merge-leftover class (G-PRs merged).
- `red-main/velnor-pin-bump@93ad5c4d`, `red-main/perf-fix-forward@640c44d2`: published, merged;
  delete at R-11.
- Stashes: Farm C 11 entries (GOAL_SHARED — other-goal decisions, RETAIN); Farm A/B/CQ: none.
- Live remote `rollout/*` branches all map to a PR (h96: zero true orphans).

### E.4 Related PR ledger

114 goal PRs: 77 merged / 20 open / 17 closed-unmerged (h96, IDs PR-001…; all authors and
mergers `donbeave`, all bases `main`). Per-repo tables: `/tmp/h96.report.md` (LOCAL-ONLY;
key rows inlined below so the HANDOFF is self-contained for resume).

OPEN (20) — the resume worklist:

| PR | Head → Base | Checks | Action |
|---|---|---|---|
| tablerock#81 (agent-policy) | `2994844f` → main@`94245a2d` | Policy FAIL | R-02 close-with-proof after #83 |
| tablerock#83 (pin-eed474c4+§8) | `f87c1fbb` → @`886e192e` | 16/16 green CLEAN | R-01 merge |
| parallax#120 (agent-policy) | `443528a2` → @`5af3c016` | Policy FAIL + 4 COMMENTED reviews (2 threads, both resolved) | R-02 close-with-proof after #122 |
| parallax#122 (pin-eed474c4+§8) | `5e35123b` → @`9d71bec8` | was 1 pend → re-verify (xtask fix pushed pre-freeze) | R-01 merge |
| gtf#31 (R8 termpane checks) | `4f63dd63` → @`ee961e29` | DCO ok; Policy+Planning QUEUED | R-07 (capacity) |
| termpane#18 (agent-policy) | `0030b694` → @`7602430f` | Policy+fuzz+ci-required FAIL | R-02 close-with-proof |
| playground#50 (agent-policy) | `f334191c` → @`54d09bf7` | 19 fails, base predates s2 | R-02 close-with-proof (supersede-comment posted) |
| playground#53 (pin-eed474c4+§8) | `18ce4617` → @`28bb557a` | 21/21 green CLEAN | R-01 merge |
| architect#477, jackin-gtf#47, dev#47, sentinel#153, role#188, smith#212, tap#504 (pin) | various → mains | 5–7/7 green CLEAN | R-01 merge (×7) |
| java#2062 (agent-policy) | `bb69f554` → @`05a7320b` | 1 canc | R-04 with #2063 |
| java#2063 (schema-2+provider) | `fbc3a925` → @`05a7320b` | Control/ci-required FAIL, 72 skipped | R-04 (capacity) |
| blockchain#722 (agent-policy) | `9e0e7e0d` → @`835a3e1e` | 1 ok, 1 resolved thread | R-02 disposition TBD (verify substance post-#724) |
| termcomp#7 (agent-policy) | `bc6a04aa` → @`7b27732a` | gates+perf FAIL | R-03 after #8 + R-09(b) ruling |
| termcomp#8 (wave@4dec6b9e) | `885d0695` → @`7b27732a` | 8/14 ok; perf/gates/ci-required FAIL; base stale | R-03 (rebase to preserved `4527fda9` line) |

Merged (notable, post-freeze verification in h97 §1): velnor #1042 (`bdffa8f4`), #1060
(`c674f5bb`), #1062 (`45ef1ebe`); #724 → `a7f3efc1`; termpane #21→`61926c4`, #22→`e77f5dc`,
#24→`3b9cb07`; playground #51→`0dd43c5`, #52→`28bb557`; holla #224→, #223→, #225; jackin
#1052→`edef2c1e` (+unrelated #1053→`df4671e4` — NOT goal work, do not touch); termcomp
#9→`f3313476`; parallax#121 = MERGED_SUBSTANCE `9d71bec8` (tree-identical, 502 on PR-state
update — CONFIRMED via tree hash `12057163638a…`, single parent `5af3c016`).
Closed-unmerged: agent-policy mass-close 21:27–21:35Z (12 repos, superseded by wave content —
SPOT-VERIFY substance at R-02, do not assume); termpane #19/#20/#23; parallax#121 (substance
merged, see above); holla#222.
OUT OF SCOPE: velnor#1054–#1058 (`fix/*`, donbeave, opened 20:10–20:32Z) — R-09(c).

### E.5 Integration map and ordered landing plan — FUTURE EXECUTION ONLY

Nothing below was executed during the pause.

Map (worktree → branch → remote ref → PR → target):

- `/tmp/cq-termcomp` → `rollout/velnor-wave@4527fda9` (local) → `goal-handoff/termcomp-wave-4527fda9`
  (remote, `4527fda9…`) → termcomp#8 → `main`. R-03.
- `/tmp/cq-parallax` → `rollout/pin-eed474c4@5e35123b` (pushed) → parallax#122 → `main`. R-01.
- Other 9 pin branches (pushed; LOCAL worktree locations UNVERIFIED — agent-75 `/tmp` clones
  uninventoried, Farm-B jackin checkouts sit on old wave heads not pin branches) → 9 pin
  PRs → mains. R-01. (Heads are safe on the remote regardless; re-derive locations at resume.)
- Agent-policy branches (pushed) → 7 docs PRs → CLOSE-WITH-PROOF (no merge). R-02.
- java branches (pushed) → #2062/#2063 → `main` when capacity. R-04.
- gtf `rollout/termpane-r8-checks` (pushed) → #31 → `main` when capacity. R-07.

Landing order: R-01 (10 merges, parallelizable across repos; §4 recheck each at head;
post-merge CI green each) → R-12 (open the 4 missing pin PRs; merge jackin + tui-snap;
cto/gtf per zero-capacity doctrine) → R-02 (closes cite main SHAs + file URLs) → R-03
(termcomp: restore preserved line → rebase → parity-replay decision → push → CI → merge) +
R-05 (blockchain red-main fix, independent, parallel OK) → R-06 (verify-only first, then PR;
NEVER apply) → R-08 (termpane R8 entry) → R-04/R-07 (capacity-gated) → R-09 rulings as
needed → R-10 (re-sweep + final table) → R-11 (cleanup). Merge methods: respect repo policy
(squash where ruleset mandates, e.g. tui-snap; else squash-per-precedent with
`--match-head-commit` guard). Re-read ALL PR threads before each landing; heads WILL have
moved (e.g. #122's unchecked CI run).

### E.6 Post-integration local cleanup runbook — FUTURE EXECUTION ONLY

Execute ONLY after R-10, re-observing each candidate live. The 5 gates (quoted so a fresh
agent need not fetch the pause order): (1) every required change from the candidate is
verified in the intended target (or explicitly superseded/rejected with rationale + recovery
retained) — compare actual saved AND current tips; (2) all commits/edits/stashes/artifacts
accounted for — no unpreserved work or unresolved ownership; (3) no live agent/process/goal/
PR/worktree/recovery-path depends on it — shared/unknown stays intact; (4) required
post-integration validation passed and this HANDOFF + recovery refs remain reachable after
removal; (5) exact repo/host/common-dir/path/ref/expected-tip re-verified — individually
named resources only. Squash-merges: verify tree coverage, not ancestry. NEVER
wildcard-delete, force-remove dirty worktrees, prune metadata affecting shared farms, or
delete remote branches/tags (remote cleanup NOT authorized).

| Resource | Owner | Expected tip | Integration proof | Recovery | No-use gate | Proposed action |
|---|---|---|---|---|---|---|
| A-06…A-14 worktrees + branches; 4 branch-only `rollout/*` | wave G-owners (gone; resume owner acts) | tips §E.2 | G-PR squash trees cover tips (verify live) | main history (immutable) | re-verify clean + no live process in path | `git worktree remove` (clean only) + `branch -d` after coverage proof |
| Farm-B 16 unpublished tips | wave owners (gone; resume owner acts) | tips §E.2 | PR squash trees cover tips; merge commits cited §E.3–E.4 | main history + cited merges | re-verify tip == recorded tip + clean | checkout main + `branch -d` |
| `red-main/*` branches (2) | redmain-fix/92 (stopped) | `93ad5c4d`, `640c44d2` | #1060/#9 merged | main history | local-only delete; remote RETAIN (no auth) | local `branch -d` |
| CQ worktrees (`/tmp/cq-*` + extras) | CQ-v2/91 (stopped) | §E.2 | R-03 pushed; parallax merged; cq-bc == merged head | preservation ref `4527fda9…` | re-verify no new unpushed commits + no live process | remove after proof |
| A-15…A-22, J/K clean review clones | various (gone) | pins/detached | n/a (no unique content — VERIFY: `status` clean + tip reachable from a remote ref or recorded pin) | recorded pins | re-verify clean + unreferenced live (grep session/farm refs) | remove |
| h95 §4 J/K dirty+ahead exceptions (migfix2 U17, pr953 U17, g3-integration +15, pin +74, `pin3`, `goal-965`, ~15 more) | UNKNOWN (mixed farms) | h95 §4 rows | NOT established | local only | BLOCKED until owner ruling | RETAIN (re-verify live; owner ruling before any removal) |
| A-18, A-19 | redmain-fix/92 (declared) | dirty/droppable | owner declarations (redmain report) | n/a (declared discardable) | declarations on file | remove |
| Remote stale branches (13× agent-policy LIVE→closed, termpane×4 stale, task-format×3, architect/jackin/homebrew-velnor/tui-snap stales per h96) | — | h96 branch map | closes/merges recorded | main history | REMOTE DELETE NOT AUTHORIZED | explicit RETAIN |
| A-HO handoff worktree + branch | coordinator (this doc) | this doc | NEVER (retained record) | origin branch (pushed) | permanent | KEEP |
| Farms C–I, L/M/N, gap-G-09 clones (Farm M), Farm C stashes | shared/unknown | — | shared/unknown | local only | BLOCKED | KEEP (REVIEW_SHARED/BLOCKED) |
| `goal-handoff/termcomp-wave-4527fda9` (remote) | coordinator | `4527fda9…` | R-03 merged | itself (pushed ref) | permanent | RETAIN (remote delete not authorized) |

Post-cleanup: re-enumerate `git worktree list` + `branch -a` per farm, reconcile with §E.2,
write cleanup receipt. Every inventoried goal resource needs a final disposition before R-11
is complete.

## F. Decisions, findings, assumptions, rejected approaches

Settled (with rationale):
- Advisory merge ruling: merge iff platform allows AND only Policy failures are advisory
  pre-flip required-checks; record repo for W6.2 flip. (Else blocks.)
- Termpane ruling-(a): merge #21 despite trusted-runners FAIL = b9c3156-validator /
  4fa7a3a8-template skew on dual-lane `if:` gate; transient tree; PR2 deletes the jobs.
- AG-1: §3 canonical = velnor main AGENTS.md MINUS line 3 (velnor-local runner-protocol
  bullet). F1 VOID (13 repos already clean); F2/F3/F4 append the 6 trailing bullets only.
- Wave pins: be61acbb → 4dec6b9e → bdffa8f4 (G12); consumer-final pin `eed474c4` SOUND
  (runtime products green; self-CI redness root-caused); "no pin-4" stands — do NOT re-pin
  to `45ef1ebe` (contains #1059/#1061 behavior changes).
- Zero-capacity doctrine: private repos merge platform-allowed; unobservable execution =
  PROVEN BLOCKER with failed-operation evidence (0 runners in `velnor-trusted` group).
- Tablerock plutil-shim symptom-fix accepted with deferred root cause recorded.

Findings (sources cited):
- Cache-key structural flaw (CQ, termpane #24): `id_segment` per-kind + `restore-keys` prefix
  + PATHS not in key ⇒ poisoned exact-hit took CI offline. Playbook: delete poisoned caches.
  Follow-up NOT implemented (`/tmp/cq-velnor` on `rollout/unit-cache-keys`, no commits).
- Terraform drift (redmain, UNVERIFIED live): `variables.tf` lacks Policy additions made via
  API ⇒ next apply re-reddens mains. R-06.
- Merge-API 502-after-commit pattern (parallax#121, holla#223/#224 era): PR shows closed but
  commit exists ⇒ merged-in-substance close with tree-hash proof. Verified for #121.
- Debug-built binaries produce different scan digests than release products — ALWAYS render
  with release product binaries (playground proof: Policy 9/11 → 10/11).
- Mass agent-policy close 21:27–21:35Z needs substance spot-checks at R-02 (do not assume).

Open questions: termcomp parity-replay CI step (product-CI decision, R-03); termcomp#7
gates-red merge policy (R-09b); #724 §4 retro-attribution (R-09a); velnor#1054–58 scope
(R-09c); RM-4 live verification (R-06).

Glossary for fresh readers: D19 = velnor's self-hosting pin lane (the `.github-gen`
revision velnor uses to render its own `.github`; #1062 bumped it to `eed474c4`).
`herd` = a batch of small generator fixes landed together (see REQ-03). W6.2 = the
ruleset-flip workstream (add `Policy` to required checks post-green). W5-R7 = blockchain
advisory ruling R7 (dco-2 GitHub App install impossible via API — needs org-admin web flow;
statics kept as passthrough).

## G. Verification evidence and known failures

Live snapshot 22:22–22:35Z (h97; `gh api` read-only):

- PASS (observed green): velnor CI@`45ef1ebe` 35657630434; velnor Preview@`c674f5bb`
  35657208164; playground dispatch 35657908148 (Policy 11/11); termpane CI 35659754832;
  tablerock/agent-smith/tui-snap reruns; termcomp Perf 35652602140; 10 pin-PR rollups green.
- FAIL: blockchain main CI 35657653497 (`docker-celo-op-node` unit checks) + release dispatch
  35657671669 (Verify release); termcomp main CI 35652601666 (gates stable + 1.88.0);
  red open PRs per §E.4 table.
- QUEUED (capacity): gtf 35634398902, cto 35634424059, jmono 35630181731 (unchanged IDs, 5h+);
  gtf#31 Policy+Planning; blockchain Renovate (in progress 22:15Z).
- Preview never ran on velnor@`45ef1ebe` (latest Preview = `c674f5bb` green) — confirm on resume.
- Parallax F12 persists: no CI on HEAD `9d71bec8` (latest = Maintenance@`5af3c016`); #122 merge
  produces first signal.

Handoff-operation checks (coordinator, bounded/nonmutating unless noted):
- `git fetch origin` (tailrocks_velnor) rc=0; base `45ef1ebe` recorded.
- `git worktree add` handoff worktree: OK, clean.
- `git push origin 4527fda9:goal-handoff/termcomp-wave-4527fda9`: OK, verified via ls-remote.
- `subagent_status(running)` → `not_found`: all workers stopped. PASS.
- No product validation campaign run during handoff (per spec).

## H. Ordered remaining-work plan

| ID | Outcome | Deps / parallel | Next action + validation |
|---|---|---|---|
| R-01 | Merge 10 green pin PRs (§E.4) | FIRST TASK; parallel across repos | §4 recheck at head → SHA-guarded merge → post-merge main CI green each. Order: #83 → #122 → #53 → jackin×7 |
| R-02 | Close-with-proof: #120, #50, #18, #81 (+#722 TBD) | After R-01 for #120/#81 | Verify substance on main (file URLs) → explanatory close comment |
| R-03 | termcomp#8: restore `4527fda9` → rebase → fix parity/capture/perf → merge; then #7 | Needs R-09(b) for #7 | CI green on #8 (or ruled waiver); gates decision for #7 |
| R-04 | java#2063 (+#2062) merge | BLOCKED: velnor capacity | Merge when queue drains; until then proven-blocker record |
| R-05 | Blockchain red main fix | Independent (parallel OK) | Fix celo-op-node unit checks + Verify release; main green |
| R-06 | Terraform alignment PR (no apply) | Verify-first | Read `variables.tf` vs live rulesets → PR Policy additions |
| R-07 | gtf#31 merge | BLOCKED: capacity | Merge when queued checks drain |
| R-08 | Termpane R8 map entry | None | Apply entry now main is green |
| R-09 | Rulings: (a) #724 retro-check (b) #7 gates policy (c) #1054–58 scope | Before affected merges | Parent decides with evidence; record |
| R-10 | §8 re-sweep + final table + feedback summary | After R-01…R-05, R-12 | Strict 7-check sweep; publish table + links |
| R-11 | Cleanup per §E.6 | LAST, after R-10 | Gates §E.6; cleanup receipt |
| R-12 | Open MISSING pin PRs: jackin (main@pin-2 `4dec6b9e`), tui-snap (main@pin-1 `be61acbb`) — public, CI observable; cto + gtf (mains@pin-2, private) — open; merge per zero-capacity doctrine at resume | After R-01 (same recipe); cto/gtf merge gated like R-04/R-07 | 4 new pin PRs §4-gated; blockchain already pin-current via #724 (verified `revision = "eed474c4…"`) |

FIRST ACTIONABLE RESUMPTION TASK: R-01 — re-fetch tablerock#83 head/reviews/checks, §4-gate,
SHA-guarded merge, verify post-merge main CI green; then continue the R-01 order.

## I. Environment and operational recovery

- Host: donbeave-mac, user `donbeave`, TZ UTC+7. Tools: `gh` (authed as donbeave; quota
  5000/5000 at 22:22Z), `git`, `cargo/rust 1.98.x`, `mise`. All 34 repos: GitHub, default
  `main`, squash-or-merge per repo policy (tui-snap mandates squash).
- Workdirs: `/Users/donbeave/Projects/github/all-repo/<repo>` (34 clones); `/tmp/cq-*`,
  `/tmp/velnor-*` (worktrees/scratch); session logs (evidence only).
- External effects performed: ~100 merges via `gh pr merge --squash --match-head-commit`;
  ruleset API edits (Policy adds: 11 remediation repos + tablerock/tui-snap/task-format/
  blockchain/playground); termpane Actions cache deletions (2 entries); termcomp
  preservation branch push (handoff op). Repeating merges is guarded by SHA checks; ruleset
  edits are idempotent; cache deletion is safe to repeat.
- Generated vs irreplaceable: pin-product binaries reproducible from velnor main;
  CI run logs live on GitHub (URLs in §G/worker reports); `/tmp` ledgers + session logs are
  irreplaceable — back up before any `/tmp` purge (or accept re-derivation cost).
- Permissions: org admin (tailrocks) held, yet some routes impossible via API (dco-2 app
  install needs web flow — W5 R7). PR token gets ruleset-API 403 (redmain) — entrypoint
  compares declared-vs-declared pre-merge.

## J. Fresh-agent resume runbook

1. Read this document completely + repo `AGENTS.md`/`.github/AGENTS.md` + linked essentials
   (`/tmp/h95.report.md`, `/tmp/h96.report.md`, `/tmp/h97.report.md` if on this host).
2. Recover the checkpoint: `git -C …/all-repo/tailrocks_velnor fetch origin` then
   `git worktree add /tmp/velnor-handoff-resume goal-handoff/velnor-rollout-20260921-e3119d71`
   (exclusive worktree; do not disturb shared checkouts).
3. Compare LIVE state vs this snapshot: re-fetch all §E.4 PR heads/threads/checks, main SHAs,
   queued runs, velnor main vs `45ef1ebe`. Detect intervening changes FIRST. Stale PIDs/SHAs
   in this doc are starting hypotheses, not facts.
4. Reconcile completed work (R-items already done stay done); do not rewrite shared history
   or restart green tasks.
5. Resume the ORIGINAL goal at R-01 (first task, §H), NOT the handoff task. Delegate with
   clear ownership (one integration owner per repo), keep DCO signoff + §4 gates + AG-1 +
   pin discipline (`eed474c4`, no pin-4).
6. Maintain progress in the goal record + scoped checkpoint commits on resume work.
7. After goal completion + R-10 validation, execute §E.5 integration (already mostly merged;
   verify), then ONLY eligible §E.6 cleanup with live re-verification. Account every
   worktree/branch/PR with a final disposition + cleanup receipt.

Resume command:

```
/goal Read and resume docs/goal-handoffs/velnor-agent-policy-workflow-rollout--20260921T220154Z--shore-polaris--e3119d71.md
```

INTERPRETATION RULE: the pause holds until the user requests resumption. A later `/goal Read
and resume <this-file>` authorizes continuation of the ORIGINAL goal. It does NOT instruct the
future agent to pause again, regenerate this handoff, or recursively create another handoff PR.

## K. Blockers, omissions, independent review

Blockers/omissions:
- B-1 (proven, external): velnor-target-mvp fleet = 0 runners ⇒ trio mains + gtf#31 + java
  PRs unverifiable. Remedy: infra owner provisions runners + applies group membership (#29);
  until then record per §6 (never substitute hosted).
- B-2: 75 never produced a final report (cancelled; stale mid-run note only). Mitigation:
  its repos evidenced via merges + h96/h97 live checks. Resume must still re-verify each.
- B-3: no runtime goal-pause control exists (§A) — pause is administrative + worker stops.
- B-4: `/tmp/final-table.md` never built; §8 re-sweep never run (R-10).
- B-5 (scope): velnor#1054–#1058 ownership unverified (R-09c); Farm G `work/*` dirty
  ownership UNKNOWN (retained); G-09 mass-staged clones intent UNKNOWN (untouched).
- Otherwise NONE unchecked: all goal workers stopped (roster verified empty); all essential
  code remotely preserved (merged or preservation ref).

Independent review: DONE (agent `handoff-review/98`, report `/tmp/h98.review.md`,
LOCAL-ONLY). Initial verdict BLOCKED with 12 must-fix + 7 suggestions; all 9 live
spot-checks CONFIRMED (preservation ref, #83, #477, blockchain red, velnor HEAD, #121 tree
equality, #24, #1052, #8 head untouched). All 12 must-fix applied (M-1 dangling §E.2a;
M-2 worktree count; M-3 Farm G rationale; M-4 J/K dirty RETAIN row; M-5 new R-12 for 4
missing pin PRs + live pin verification; M-6 jackin-gtf#47; M-7 java#2062 base==HEAD;
M-8 pin counts 15/10/1=26; M-9 unverified pin-branch locations; M-10 Owner/Recovery columns
+ strengthened gates; M-11 cq-bc/cq-final/xtask.log rows; M-12 R15 defined) and all 7
suggestions applied (S-1…S-7). Recovery test passed: reviewer derived the exact resume
command and R-01/tabby-#83 first task independently. 20/20 open PRs mapped; every §E.6 row
now carries owner + recovery + no-use gate. Reviewer assessment: honest, survivor-verified,
resumable without the parent conversation.
