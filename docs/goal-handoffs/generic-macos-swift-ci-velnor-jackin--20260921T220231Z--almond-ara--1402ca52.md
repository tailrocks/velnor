# GOAL: Generic, correct, fast macOS and Swift CI in Velnor; migrate Jackin

> Resume command: `/goal Read and resume docs/goal-handoffs/generic-macos-swift-ci-velnor-jackin--20260921T220231Z--almond-ara--1402ca52.md`
> Pause applies until the user requests resumption. That resume command authorizes
> continuing the ORIGINAL goal; it does NOT authorize pausing again or regenerating this handoff.

## A. Identity and pause status

- Handoff ID: `generic-macos-swift-ci-velnor-jackin--20260921T220231Z--almond-ara--1402ca52`
- Created (UTC): 2026-09-21T22:02:31Z. Last update (UTC): 2026-09-21T23:05:00Z (review corrections + publication).
- Original goal status: `PAUSED_BY_USER` (requested disposition; not proof of runtime stop).
- Handoff status: `READY` (preservation + publication + review verified §7).
- Worker stop status (verified via terminal subagent results, not mere Markdown):
  - `main/repin3-merge-1044/59` — TERMINAL before pause with STOP report (re-pin-3 done,
    #1044 merge blocked by mold defect). No further action from it.
  - `main/m4-pin-bump/60` — TERMINAL after pause: acknowledged stop message, returned stop
    report (M4 PR #1062 already merged; nothing in flight). Verified stopped.
  - Auditors 61–64 — all TERMINAL, read-only, no mutations.
  - No other goal-owned workers, watchers, retry loops, or background tasks exist in this
    session (roster checked; prior workers all terminal).
- Runtime goal-pause control: NOT AVAILABLE. Observed goal controls are only
  complete/blocked/progress-report; no pause API exists. The goal record was deliberately left
  active (marking complete/blocked would be false). Freeze is enforced by coordinator stopping
  all goal work. Recorded as limitation.
- Source agent: Muse Code CLI, session `almond-ara` (`01a0c0c6-f3c6-7640-a1c1-11f14fd24d07`),
  goal `goal-56104e90-5936-4fdf-a6c0-d390649489ab`.
- Repositories: primary `tailrocks/velnor` (this HANDOFF); subordinate `jackin-project/jackin`
  (preservation branches only, no file changes, no Jackin PR).
- Handoff path (velnor-relative): `docs/goal-handoffs/generic-macos-swift-ci-velnor-jackin--20260921T220231Z--almond-ara--1402ca52.md`
- Source branch/HEAD at pause: velnor checkout on `m4-pin-bump` @
  `72d0d92bcceb93460cb932c5a194e03ab3ca76af` (clean); jackin checkout on
  `migrate/apple-ci-generic` @ `6ff54ce541b2a5bf0a2a128b0fb814503a7d54c9` (clean).
- Observed remote heads at audit: velnor `origin/main` = `45ef1ebe` (M4 #1062);
  jackin `origin/main` = `df4671e4`.
- Preservation branch: `goal-handoff/generic-macos-swift-ci--1402ca52` (velnor, from
  `origin/main` @ `45ef1ebe`). Handoff PR: https://github.com/tailrocks/velnor/pull/1066
  (DRAFT, auto-merge off; final head SHA in PR body).
- Recovery: fully remote-portable for all goal-owned Git work (preservation branches pushed,
  §E). Session logs are local-only conveniences, not required for resume (§K).
- Resume authorization: explicit later user request only.

## B. Original goal and success contract

Full objective is ~101KB (audit §§1–12 + copy-ready `/goal`). It cannot be embedded verbatim
in full; key passages below are verbatim QUOTEs, remainder are labeled SUMMARY. Full text
recoverable from session log line 19 (local-only;
`/Users/donbeave/.local/share/muse/sessions/2026/09/21/01a0c0c6-f3c6-7640-a1c1-11f14fd24d07/session.jsonl`).

Verbatim (user-provided):

- QUOTE (title): `# Generic macOS and Swift CI for Velnor: Jackin audit, implementation report, and /goal`
- QUOTE: `Research snapshot: **20 September 2026 UTC**.`
- QUOTE (recommendation): `extend Velnor's existing typed scanner, product graph, planner, and runtime so it can discover SwiftPM packages, Xcode/XcodeGen applications, and their native-library producers. Give each product explicit inputs, toolchain requirements, outputs, and validation rules. Generate GitHub Actions and supported Velnor execution from that model. Remove Jackin's opaque CI task workaround after this model preserves its complete application contract.`
- QUOTE (`/goal` opener): `/goal Generic, correct, fast macOS and Swift CI in Velnor; migrate Jackin` …
  `Implement a reusable macOS/Swift build model in tailrocks/velnor's velnor-workflow, and use it to remove Jackin's custom native CI orchestration while preserving and completing the required verification of Jackin's real macOS application.`
- QUOTE: `Make GitHub-hosted runners the default.` …
  `A standard Debian host can perform scanning, orchestration and supported Linux work; it cannot validate a native Xcode/SwiftUI/AppKit application.`

Execution rules (verbatim excerpts): `Delegate first.` … `A separate verifier must review
material conclusions and completed implementations without relying on an implementer's claim
that they work.` / `Do not ask clarification questions to resolve task ambiguity.` /
`Diagnose the structural cause before fixing a symptom.` / `Commit each coherent, verified
increment promptly and push regularly. Prefer one active integration branch per repository.` …
`Fetch and merge current main into working branches; do not rebase published history.` …
`Open small, coherent PRs, address all review comments, merge once each exact candidate
satisfies its required gates` … `Keep a concise committed progress and handoff document with
decisions, outstanding work, tested SHAs and concrete continuation steps.`

Session amendments (verbatim user steers):

- DCO: `Commits must ALWAYS with Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>. Never anything else than Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>`
- Delegation: `Default rule: **delegate first, parallelize aggressively, verify independently, then integrate.**`
- Autonomy: `Never ask the user questions or wait for clarification. Work fully autonomously.` (plus subagent-investigation protocol)
- Commit cadence: `Always commit changes frequently while working.` … `commit often, push regularly, and minimize branch proliferation.`
- Merge policy: merge main, never rebase (from goal text + session practice); squash-merge PRs (session practice).

SUMMARY of scope/non-goals/acceptance (reconstructed from full text; not verbatim):

- Scope: typed schema-2 scanner discovery (SwiftPM/Xcode/XcodeGen/BoltFFI), product graph +
  identity, planner/placement, runtime execution/liveness, caching/artifact policy,
  GitHub-hosted macOS integration, Jackin migration off `swift-package-native-ci`,
  executor-portability proof, benchmarks, cleanup.
- Non-goals: unrelated subsystem rewrites; third Apple planner; unimplemented adapter claims
  (UniFFI/cbindgen/Tuist); Intel-slice removal (job already arm64); numeric green-rate promises.
- Acceptance contract (summary): no-execution static scan; honest classification; app/producer
  discovery on clean checkout; invalidation correctness; cold/hit/corrupt cache behavior;
  bounded liveness regression tests; full app/harness/drift/bundle/dSYM/UI assertions retained;
  hosted-macOS green; Debian cannot be picked as Apple executor; deterministic regeneration;
  drift gate; aggregate gate rejects missing/cancelled/skips; PR #1013 review disposition incl.
  Landlock P1; controlled benchmark matrix (cold/warm/Swift-only/Rust-change/lock/profile/
  toolchain/corrupt cases, ≥3 reps, median/range, no invented p95); deliverable links/SHAs.
- Post-resumption obligation (from pause instruction): integrate all required related goal
  work, resolve its PRs, and clean up verified-obsolete goal-owned local worktrees/branches —
  no blind merging of every experiment, no deletion of shared/unrelated resources.
- Merge permissions: squash-merge small green PRs; address all review comments; DCO signoff
  exactly `Alexey Zhokhov <alexey@zhokhov.com>`; protected distribution stays in protected jobs.

## C. State at the exact interruption point

Last completed actions (all 2026-09-21 UTC):

1. 21:03Z — Velnor PR #1061 (migfix3, defect-C sccache/mise edge) squash-merged as `155c6088`;
   runtime products published (run 35654843370).
2. ~21:15Z — Jackin #1044 re-pin-3 pushed: merge `8c46b0c7` (pin → velnor `eed474c4a1d9`)
   + `c02a37d9` (1-line generator-state fix) + forward merge `6ff54ce5` (= PR head, = local
   checkout). Policy failure at `8c46b0c7` fixed by `c02a37d9`.
3. 21:31Z — Velnor PR #1062 (M4 self-pin-bump to `eed474c4`) squash-merged as `45ef1ebe`
   after one `rust-velnor-workflow` flake retry; main CI green on merge.
4. Repin3 worker verified defect C fixed (mise installed 5/5 tools incl. boltffi 0.30.1) and
   returned STOP: new Velnor generator defect blocks #1044 merge (mold on macOS, §G).
5. Pause received; M4 worker acknowledged stop and returned its report; 4 read-only audits ran.

In progress at pause: NOTHING — both implementation workers were already terminal; no partial
edits, conflicts, or detached work in the two primary checkouts (both clean). The only
untracked item is this handoff skeleton (now this document).

Interruption point in one line: goal work is frozen between "M4 merged, main green" and
"fix mold defect → re-pin-4 → merge #1044".

FIRST resumption task (explicit): reproduce the mold failure from
https://github.com/jackin-project/jackin/actions/runs/35657319778/job/106524303464
(`Set up mold 2.42.0` → `unsupported mold architecture: arm64`), then open a Velnor generator
fix PR that skips mold cache/setup on macOS runners (both s1 and s2 emitters + both unit-test
modules), merge it, confirm the runtime-products release for the new SHA, and only then do
Jackin re-pin-4 on #1044. Do NOT hand-edit Jackin's generated `ci-unit-rust.yml`.

Partially edited / inconsistent state: none in primary checkouts. Dirty volatile worktrees
(B2/B3/A4/80bc) were committed verbatim to `preserve/handoff-1402ca52/*` remote branches
(§E.2) — their local worktrees now sit on those branches, clean.

Open hypotheses: (a) whether the mold fix needs `ToolRequirement::Mold` insertion changes,
  `linker:` neutralization, or both (§G.3); (b) whether velnor #1054 (Mise-closure, green) is
  required by the Apple leg or lands independently; (c) #1044 vs jackin #1065 merge order.

## D. Requirement-by-requirement progress ledger

`ID | Requirement | Status | Evidence/files/commits | Remaining work | Dependencies`

| ID | Requirement | Status | Evidence | Remaining | Deps |
|----|-------------|--------|----------|-----------|------|
| R-EV | Evidence/liveness: bounded execution, no false silence-timeout | VERIFIED_DONE | velnor #985 `c832191f` (wall deadline), checks 23 pass | none | — |
| R-SCAN | Generic platform split + typed discovery groundwork | VERIFIED_DONE | #1025 `d771961b` (23 pass), #1030 `b68a6ea5`, #1031 `f3b94af3`, green mains | none | R-EV |
| R-PROD | Product contract: prereq/artifact/closure semantics | VERIFIED_DONE | #1036 `b634efd2`, #1043 `690b3935` (16/16), #1061 `155c6088` | mold fix is separate defect, see R-MOLD | R-SCAN |
| R-PACK | Preview packaging + per-arch artifacts | VERIFIED_DONE | #1035 `6737cdb3`, #1045 `a850b255` (22–23 pass) | observe main Preview @ `45ef1ebe` (peer tree green) | R-PROD |
| R-PIN | Self-pin rollouts (M2/M3/M4) | VERIFIED_DONE | #1026 `28f16b7a`, #1038 `e430c6a8`, #1062 `45ef1ebe` (23 pass, main green) | none | R-PACK |
| R-MOLD | Velnor generator: no mold on macOS | BLOCKED (fix NOT_STARTED) | failing job 106524303464; generated `ci-unit-rust.yml:367-370,796`; sources `s2/mod.rs:6183`, `src/lib.rs:5931` | fix PR both emitters + tests, merge, products release | R-PROD |
| R-MIG | Jackin migration #1044 merged | IN_PROGRESS (head green except mold leg) | branch `migrate/apple-ci-generic` @ `6ff54ce5`; 42 pass / 3 fail (2 consequential) | re-pin-4 after R-MOLD → green → merge | R-MOLD |
| R-5C | 5c hosted evidence (main Preview + hosted runs) | IMPLEMENTED_UNVERIFIED | main CI green @ `45ef1ebe`; Preview @ merge SHA unobserved | collect Preview run evidence | R-PIN |
| R-BENCH | Controlled benchmark matrix (§12.2) | NOT_STARTED | — | ≥3-rep cold/warm/Swift-only/Rust-change/… runs after R-MIG | R-MIG |
| R-PROV | Native macOS provider proof or demonstrated blocker | NOT_STARTED | — | implement + real-Mac e2e, or record external blocker | R-MIG |
| R-1013 | PR #1013 review disposition (incl. Landlock P1) | VERIFIED_DONE (out of scope) | jackin #1013 MERGED 07:58Z as `9ee50f6a`; off-goal product PR | drop from goal tracking | — |
| R-DOC | Committed progress/handoff doc | VERIFIED_DONE | `plans/apple-ci-progress.md` (D19) + this HANDOFF | keep current on resume | — |
| R-CLEAN | Remove superseded paths, final cleanup | NOT_STARTED | inventory §E (no cleanup performed during pause) | execute §E.6 after integration | R-MIG |

Stale-claim notes: (i) earlier "22/22 PR CI" counts refer to older PRs, not current heads;
(ii) #1044 title still says "pin b634efd2" — actual pin is `eed474c4`; (iii) velnor main had
carried failures at `eed474c4`/`c674f5bb` — historical, HEAD `45ef1ebe` is green.

## E. Change and preservation inventory

Primary-checkout changes: none pending — both primary checkouts are clean; all merged work is
on `origin/main` of each repo; #1044's branch is pushed (= remote head). The checkpoint code
is therefore the set of merged commits + open PR heads + preservation branches, all remotely
durable. No pre-existing/unrelated changes were touched. Recovery of scratch/rehearsal lines
is via `preserve/handoff-1402ca52/*` branches (§E.2), not via /tmp paths.

### E.1. Discovery scope and ownership

Inspected (read-only `git worktree list --porcelain`, `status`, `stash list`, `log`,
`for-each-ref`, `branch -vv`, `ls-remote --heads`, `gh pr view/checks`, `gh api` runs;
timestamps ~22:05–22:20Z; no pagination truncation encountered except noted `gh search`
repo-query rejection worked around by full list+title scan):

- R1 `/Users/donbeave/Projects/github/velnor` (origin `tailrocks/velnor`) — primary.
- R2 `/Users/donbeave/Projects/github/jackin` (origin `jackin-project/jackin`) — primary.
- R3 `/Users/donbeave/Projects/github/all-repo/tailrocks_velnor` — second velnor clone.
- R4 `/Users/donbeave/Projects/github/all-repo/jackin-project_jackin` — second jackin clone.
- R5 `/Users/donbeave/Projects/github/velnor-bastion` — checked, no goal work (UNRELATED).
- R6/R7 `agy-repos/*`, other `all-repo/*` — spot-checked, no goal work (UNRELATED).
- Linked worktrees: R1 5, R2 2, R3 22 (incl. W6) — 29 total, all enumerated (§E.2);
  R4 none. Initial audit undercounted R3; corrected per independent review (§K).
- Session logs under `/Users/donbeave/.local/share/muse/sessions/2026/09/21/01a0c0c6-.../`
  (local-only; retrieval instructions §K, not required for resume).

Coverage uncertainty: LOW. All registered worktrees enumerated; all local branches
enumerated; remote heads observed fresh via `ls-remote` (not just tracking refs);
PR discovery covered open+merged+closed via full-list scan (velnor #964–1063, jackin goal
range). Remaining risk: PRs outside scanned ranges with non-obvious titles (none indicated).

Ownership rule used: branch/worktree/PR is GOAL_EXCLUSIVE if created for this goal's
increments; GOAL_SHARED if it carries goal work plus other work or is a shared gate line;
UNRELATED if evidence shows other purpose; UNKNOWN if ambiguous (retained + preserved).

### E.2. Local worktree and clone ledger

Stable IDs W1–W12. Host: `mac-studio` (local operator Mac) for all.

| ID | Repo | Path | Type | Branch / HEAD | Upstream | State @observe | Ownership | Disposition |
|----|------|------|------|---------------|----------|----------------|-----------|-------------|
| W1 | R1 | `/Users/donbeave/Projects/github/velnor` | main checkout | `m4-pin-bump` / `72d0d92b` (clean) + untracked `docs/goal-handoffs/` (this doc) | `origin/m4-pin-bump` (STALE — remote deleted) | clean, no stash/lock | GOAL_EXCLUSIVE | KEEP (primary checkout; re-branch on resume) |
| W2 | R2 | `/Users/donbeave/Projects/github/jackin` | main checkout | `migrate/apple-ci-generic` / `6ff54ce5` (clean, = origin) | `origin/migrate/apple-ci-generic` | clean | GOAL_EXCLUSIVE | KEEP (primary checkout; resume re-pin-4 here) |
| W3 | R2 | `/private/tmp/jackin-m1rehearsal` | linked | was detached `8fb49688` +13 dirty → NOW `preserve/handoff-1402ca52/jackin-m1rehearsal` @ `2c57f74b`, clean, pushed | origin preserve branch | preserved remotely | GOAL_EXCLUSIVE | INTEGRATE_THEN_REMOVE (adjudicate vs #1044, then remove) |
| W4 | R2 | `/private/tmp/jackin-scratch` | linked | was detached `c2306de9` +dirty+untracked → NOW `preserve/handoff-1402ca52/jackin-scratch` @ `0b02a313`, clean, pushed | origin preserve branch | preserved remotely | GOAL_EXCLUSIVE | INTEGRATE_THEN_REMOVE |
| W5 | R1 | `/private/tmp/velnor-gen-c832191f` | linked | was detached `c832191f` +rust.rs edit → NOW `preserve/handoff-1402ca52/velnor-rust-scan` @ `01e3ce81`, clean, pushed | origin preserve branch | preserved remotely | GOAL_EXCLUSIVE | INTEGRATE_THEN_REMOVE (triage 4-line edit) |
| W6 | R3* | `/private/tmp/velnor-main-80bc` | linked (of R3) | was detached `80bc420d` +1-line pin → NOW `preserve/handoff-1402ca52/velnor-pin-80bc` @ `473eb7b6`, clean, pushed | origin preserve branch | preserved remotely | GOAL_SHARED (gate line) | INTEGRATE_THEN_REMOVE |
| W7 | R1 | `/private/tmp/velnor-1057-generated-repair` | linked | `codex/1057-generated-repair` @ `73c0071a`, clean | none observed | clean | UNRELATED (#1057 off-goal) | NOT_APPLICABLE (leave for owner) |
| W8 | R1 | `/private/tmp/velnor-1057-rebase-current` | linked | `codex/1057-rebase-current` @ `70268cd5` = #1057 head, clean | none observed | clean | UNRELATED | NOT_APPLICABLE |
| W9 | R1 | `/private/tmp/velnor-review-1050-current` | linked | detached `96b835d9` = velnor #1050 head, clean | n/a | clean | GOAL_SHARED (review aid) | REVIEW_SHARED |
| W10 | R1 | `/private/tmp/velnor-wavepin2` | linked | detached `4dec6b9e` = #1041 merge, clean | n/a | clean | GOAL_SHARED (pin archaeology) | REVIEW_SHARED |
| W11 | R3 | `/Users/donbeave/Projects/github/all-repo/tailrocks_velnor` | independent clone main checkout | `red-main/velnor-pin-bump` @ `93ad5c4d`, clean; local `main` = `4dec6b9e` | n/a | clean (corrected per review; earlier `a77a2c10` claim was wrong — that SHA is only an old controller commit in R3) | GOAL_SHARED | REVIEW_SHARED |
| W12 | R4 | `/Users/donbeave/Projects/github/all-repo/jackin-project_jackin` | independent clone main checkout | `rollout/velnor-wave` @ `986f94bc`; local `main` = `d0d4ee09` | n/a | clean (corrected per review; `0ec60b9d` does not exist in R4) | GOAL_SHARED (mirror) | REVIEW_SHARED |
| W13 | R3 | 21 linked `/private/tmp/*` worktrees (group; W6 listed separately) | linked ×21 | see enumeration below | n/a | all clean except `velnor-pin-be61acbb` (3 dirt lines — content re-verify on resume; earlier "disposable" note unconfirmed) | GOAL_SHARED | REVIEW_SHARED (retain; re-observe before any action) |

W13 enumeration (all `/private/tmp/…`, `git worktree list` + `status --porcelain` observed
2026-09-21T22:55Z; all clean unless noted): `g11-gate` b4fbe636 detached;
`g11-main` c832191f detached (= goal PR #985 merge — goal-adjacent);
`velnor-4fa7a3a8` 4fa7a3a8 detached; `velnor-b9c3156` b9c3156c detached;
`velnor-fresh` f3b94af3 detached (= goal PR #1031 merge — goal-adjacent);
`velnor-g1` 113a6cda [rollout/velnor-g1]; `velnor-g10` 93675b2a [rollout/velnor-g10];
`velnor-g11` 9c29152d [rollout/velnor-g11]; `velnor-g12` 533aafa6 [rollout/velnor-g12];
`velnor-g4` f6ca3469 [rollout/velnor-g4]; `velnor-g5` 3fe0b19f [rollout/velnor-g5];
`velnor-g7` 4b2e2f3d [rollout/velnor-g7]; `velnor-g8` cb8417bb [rollout/velnor-g8];
`velnor-g9` 78ad2dd2 [rollout/velnor-g9]; `velnor-gen-6737` 6737cdb3 detached
(= goal PR #1035 merge — goal-adjacent); `velnor-gen-80bc` 80bc420d detached;
`velnor-gen-eed4` eed474c4 detached (= #1044's pin — goal-adjacent);
`velnor-pin-be61acbb` be61acbb detached +3 dirt lines;
`velnor-verify` f3b94af3 detached; `velnor-verify2` be61acbb detached;
`velnor-wavepin` be61acbb detached.
Note: host is shared — `cq-*`, `chainargos-*` /tmp names and other goals' worktrees exist;
only the above were inspected. R4 has no linked worktrees (single checkout).

\* W6's common dir is R3's `.git`. No locks, no in-progress merge/rebase/cherry-pick, no
stashes, no submodules, no detached tips remaining anywhere after preservation commits.
No goal-owned running processes (all subagents terminal; no watchers/daemons started).

Inaccessible worktrees: NONE — every registered worktree path existed and was readable.

### E.3. Local and remote branch ledger

Velnor R1 branches (observed post-preservation; remote state via `ls-remote --heads`):

| ID | Ref | Tip | Upstream / remote head | PR | Ownership | Disposition |
|----|-----|-----|------------------------|----|-----------|-------------|
| VB1 | `m4-pin-bump` | `72d0d92b` (W1) | STALE tracking; remote DELETED | #1062 MERGED | GOAL_EXCLUSIVE | delete after resume re-branch (fully merged) |
| VB2 | `main` (local) | `59396040` (98 behind `origin/main`) | `origin/main` = `45ef1ebe` | n/a | GOAL_SHARED | fast-forward on resume; never delete |
| VB3 | `fix/e0277-include-str` | `90e71de5` = #1017 head | remote deleted | #1017 MERGED | GOAL_EXCLUSIVE | delete (fully merged) |
| VB4 | `fix/unit-platform-split` | `d4b0570f` (+1 scheduler commit past #1025 head `6603e98c`) | remote deleted | #1025 MERGED | GOAL_SHARED | salvage +1 commit first, then delete |
| VB5 | `integrate/apple-ci-s2` (+ stale `origin/` tracking) | `326fd414` (diverged past #985 with scaleset commits) | remote STILL ON ORIGIN (stale) | #985 MERGED | GOAL_SHARED | salvage/confirm scaleset commits, then delete local + remote |
| VB6 | `preserve/stray-runner-header-0abc3675` | `0abc3675` | PUSHED this handoff (was local-only) | none | UNKNOWN | REVIEW_SHARED (now remotely durable) |
| VB7 | `goal-handoff/generic-macos-swift-ci--1402ca52` | branch head (= PR head; see §A) | pushed during §6 publication | handoff PR (draft) | GOAL_EXCLUSIVE | KEEP until goal completes |

Velnor remote-only relevant: `fix/apple-mise-tool-closure` (#1054 OPEN, §E.4);
`preserve/handoff-1402ca52/{velnor-rust-scan@01e3ce81,velnor-pin-80bc@473eb7b6}` (this
handoff). Deleted-from-origin (verified): all merged goal branches except VB5's remote.

Jackin R2 branches:

| ID | Ref | Tip | Upstream / remote head | PR | Ownership | Disposition |
|----|-----|-----|------------------------|----|-----------|-------------|
| JB1 | `migrate/apple-ci-generic` | `6ff54ce5` (W2, = origin) | in sync | #1044 OPEN BLOCKED | GOAL_EXCLUSIVE | KEEP — resume re-pin-4 here |
| JB2 | `main` (local) | `fce94cea` (pre-#1013 era) | `origin/main` = `df4671e4` | n/a | GOAL_SHARED | fast-forward on resume |
| JB3 | `integrate/velnor-apple-ci` | `8fb49688` (M1 pre-commit; 693-line project.toml diff vs #1044) | PUSHED as `preserve/handoff-1402ca52/integrate-velnor-apple-ci` | none (competing formulation) | GOAL_EXCLUSIVE | adjudicate vs #1044 (keep one), then delete |
| JB4 | `pr-1013` | `997c18fe` | local-only | none | UNRELATED (leftover) | retain (not goal-owned) |

Jackin remote-only relevant: `preserve/handoff-1402ca52/{jackin-m1rehearsal@2c57f74b,
jackin-scratch@0b02a313,integrate-velnor-apple-ci@8fb49688}` (this handoff);
`cicd/repromote-major1-9660c9ff` (closed PR #1062's branch, still on origin — remote cleanup
candidate after resume); #1065's branch (OPEN, head `909a9f54`, not fetched locally).

R3/R4 long tail (all-repo clones): ~25 `rollout/*` + `codex/*` branches, mostly `[gone]`
upstreams mirroring merged gate work (e.g. `rollout/wave-b-*`, `p962-*`), plus a few live
ones. Each recorded with tip SHA in the worktree auditor's report (`/tmp/audit-wt.txt`,
local-only convenience copy; canonical record: the branches themselves in the persistent
R3/R4 clones). Ownership GOAL_SHARED; disposition REVIEW_SHARED; remote preservation NOT
performed (mirrors of merged work; local clones retained; re-verify before any deletion).

Stashes: none in any repo. Reflog-only tips: none goal-relevant identified.

### E.4. Related PR ledger

Velnor `tailrocks/velnor` (all states scanned #964–1063):

| ID | PR | Title | State | Head | Merge/base | Checks @head | Reviews | Future action |
|----|----|-------|-------|------|------------|--------------|---------|-------------|
| VP985 | #985 | bound CI cmd exec by wall deadline | MERGED 11:42Z | `2f684383` | `c832191f` | 23 pass | COMMENTED, non-blocking | done |
| VP1017 | #1017 | destructure include_str_paths tuple (e0277) | MERGED 12:10Z | `90e71de5` | `ef42b0f9` | pass+12 skip | none | done |
| VP1025 | #1025 | split collapsed hosted jobs by platform | MERGED 12:52Z | `6603e98c` | `d771961b` | 23 pass | none | done |
| VP1026 | #1026 | bump D19 pin to 9b0d2a8f | MERGED 13:34Z | `9b20ab46` | `28f16b7a` | 23 pass | none | done |
| VP1030 | #1030 | BoltFFI Cargo profile via [native.apple] | MERGED 13:15Z | `76f90a08` | `b68a6ea5` | pass+skips | none | done |
| VP1031 | #1031 | Apple deploy floor as MACOSX_DEPLOYMENT_TARGET | MERGED 14:05Z | `dd398209` | `f3b94af3` | pass+skips | none | done |
| VP1036 | #1036 | Apple CI generator fixes (migfix1) | MERGED 15:30Z | `9c2ddd84` | `b634efd2` | pass+skips | none | done |
| VP1035 | #1035 | Preview packaging: aarch64 linker + deb twin | MERGED 16:03Z | `351b9aad` | `6737cdb3` | 23 pass | none | done |
| VP1038 | #1038 | bump D19 pin to 6737cdb3 (M3) | MERGED 16:29Z | `5b3d0e51` | `e430c6a8` | 23 pass | none | done |
| VP1043 | #1043 | install-subset closure + XcodeGen join (migfix2) | MERGED 18:35Z | `48d186c1` | `690b3935` | pass+skips | none | done |
| VP1045 | #1045 | twin debian legs via per-arch artifacts | MERGED 20:12Z | `5080aa52` | `a850b255` | 23 pass 1 skip | none | done |
| VP1061 | #1061 | install subsets over mise backends (migfix3) | MERGED 21:03Z | `84877bf5` | `155c6088` | 12 pass 12 skip | none | done |
| VP1062 | #1062 | bump D19 pin to eed474c4 (M4) | MERGED 21:31Z | `72d0d92b` | `45ef1ebe` = origin/main | 23 pass 1 skip | none | done |
| VP1054 | velnor #1054 | share derived Mise provider facts | OPEN, MERGEABLE/CLEAN | `256c24bb`, base `45ef1ebe` | — | 8 pass 14 skip, run 35658288961 | none | land on resume if Apple leg needs it |
| VP1050 | #1050 | evidence-lossless | OPEN green CLEAN | `96b835d9` | — | green | none | triage on resume (adjacent) |
| VP1052 | #1052 | rust-bootstrap-skip | OPEN | — | — | 22 pass | none | triage (adjacent) |
| VP1055 | #1055 | product-receipts | OPEN green | — | — | green | none | triage (adjacent) |
| VP1056 | #1056 | native-product-closure | OPEN green | — | — | green | none | triage (adjacent) |
| VP1058 | #1058 | composable-regen | OPEN FAILING | `93cd45e9` | — | ❌ rust-velnor-workflow, run 35659333172 | none | BLOCKED triage (likely out of scope) |
| VP1063 | #1063 | p962-port | OPEN FAILING | — | — | ❌ Policy | none | BLOCKED triage (likely out of scope) |
| VP-HO | handoff PR | `GOAL: Generic macOS and Swift CI in Velnor; migrate Jackin — paused handoff [1402ca52]` | DRAFT (created §6) | `goal-handoff/...` | `45ef1ebe` | auto CI (observe only) | none | KEEP until resume; close after goal done |

Adjacent merged (context, not goal-owned): #1039 D3 (7576f40f), #1041 G10 wavepin (4dec6b9e),
#1046 Major-1 (9660c9ff), #1048 cache guards (80bc420d), #1053 cargo-bin flag (eed474c4 —
#1044's pin), #1059 s2 Bun watch.

Possibly goal-adjacent OPEN (reviewer-flagged; triage on resume, not goal-owned until
confirmed): velnor #962 `fix: route arm64 release producers to native hosted runners`;
velnor #978 `fix(workflow): resolve tool prerequisites and complete package inputs`;
jackin #1007 `ci: pin the attested workflow runtime and regenerate`. Newer velnor
#1064–1065 / jackin #1066–1070 are other goals'/features' PRs — out of scope.

Jackin `jackin-project/jackin`:

| ID | PR | Title | State | Head | Checks @head | Reviews | Future action |
|----|----|-------|-------|------|--------------|---------|---------------|
| JP1013 | #1013 | multi-account feature | MERGED 07:58Z as `9ee50f6a` | `5fb4cb5b` | all pass | n/a | off-goal; drop from tracking |
| JP1044 | #1044 | migrate Apple CI to generic recipe (title stale: "pin b634efd2"; actual `eed474c4`) | OPEN, MERGEABLE/BLOCKED | `6ff54ce5`, base `df4671e4` (0 behind) | ❌ 3 fail / 42 pass / 38 skip, run 35657319778 | none blocking | re-pin-4 after mold fix → merge |
| JP1065 | #1065 | s2-repromote to 80bc420d | OPEN BLOCKED | `909a9f54` (not fetched locally) | ❌ same Apple FFI fail (40s), run in_progress | none | resolve order vs #1044 (fold or sequence) |
| JP1062 | #1062 | Major-1 repromote | CLOSED unmerged | branch `cicd/repromote-major1-9660c9ff` still on origin | n/a | n/a | superseded by #1065; delete remote branch after resume |
| JP-P | #1030/#1045/#1058/#1060 | proof PRs (do-not-merge) | various | — | — | — | retire per policy after #1044 |

Adjacent merged jackin: #1017 s1-mise-desktop, #1018 phases pin, #1019 change-aware pin,
#1024 swift-packing-liveness, #1041 pin f406baff, #1042 D1 repromote 01bc16b2, #1052 schema-2
wave @4dec6b9e (moved pin BACKWARD off D3), #1054 D3 repromote 7576f40f.

Refresh note: this ledger was refreshed after pushing the `preserve/handoff-1402ca52/*`
branches; no new PRs were created by preservation (branches only, no PRs).

### E.5. Integration map and ordered landing plan — FUTURE EXECUTION ONLY

Map (`worktree → local branch → remote ref → PR → target`):

```
W2 → JB1 migrate/apple-ci-generic → origin (same) → JP1044 → jackin main (after R-MOLD + re-pin-4)
W3 → preserve/handoff-1402ca52/jackin-m1rehearsal@2c57f74b → origin (same) → none → ADJUDICATE vs JP1044
W4 → preserve/handoff-1402ca52/jackin-scratch@0b02a313 → origin (same) → none → ADJUDICATE vs JP1044
W5 → preserve/handoff-1402ca52/velnor-rust-scan@01e3ce81 → origin (same) → none → triage 4-line rust.rs edit
W6 → preserve/handoff-1402ca52/velnor-pin-80bc@473eb7b6 → origin (same) → none → record only (re-derivable)
VB6 preserve/stray-runner-header-0abc3675 → origin (same) → none → REVIEW_SHARED
JB3 integrate/velnor-apple-ci@8fb49688 → preserve/…/integrate-velnor-apple-ci → none → ADJUDICATE vs JP1044
(no wt) fix/apple-mise-tool-closure@256c24bb → origin → VP1054 → velnor main (if needed)
(no wt) #1065 branch@909a9f54 → origin → JP1065 → jackin main (order vs JP1044 TBD)
W1 → VB7 goal-handoff/… → origin → VP-HO (draft) → CLOSE after goal done (never merge as feature)
```

Dependency-ordered landing plan (execute only after explicit resumption):

1. Velnor mold-fix PR (new): source = fresh branch from velnor main; target = velnor main;
   prereq = repro evidence; validation = `hosted_mold_setup_tests` (both emitters) + full
   PR matrix + products release for merge SHA; merge method = squash-merge.
2. Optionally VP1054 if the Apple leg needs Mise-closure (green, CLEAN, base `45ef1ebe`);
   re-validate base hasn't moved; squash-merge. Parallelizable with step 1 (different files
   expected — verify no overlap before concurrent landing).
3. Adjudicate JP1044 vs JP1065 vs JB3/W3/W4 (three formulations + main-line repromote):
   keep ONE migration line; document supersession rationale for the others. Expected overlap:
   same generated tree (`.github/`, `.github-gen/`) — conflicts likely; resolve by regen, not
   hand-merge.
4. Re-pin-4 on JB1 → JP1044 green → merge per Jackin policy. Validation: full 83-check
   matrix incl. `rust-jackin-usage-ffi / Apple`, `swift-package-native`, xcodegen legs,
   `ci-required`, `Control / Required`.
5. Triage W5 rust.rs edit (01e3ce81) and VB6 (0abc3675): integrate, supersede with rationale,
   or reject — record decision.
6. Retire proof PRs JP-P per repo policy; close VP-HO handoff PR after goal completion.

The resuming agent MUST reread all PR review comments, compare live heads with recorded SHAs,
and reconcile intervening changes before landing anything. This checkpoint is NOT landed work.

### E.6. Post-integration local cleanup runbook — FUTURE EXECUTION ONLY

Documented now; execute only after explicit resumption + verified integration. Re-observe each
candidate immediately before any destructive action; if head/contents/owner/processes changed,
stop that deletion and reconcile.

| Resource ID | Exact host/path/ref | Expected HEAD/tip | Final target | Integration proof | Recovery reference | Gates | Proposed action | Status |
|-------------|---------------------|-------------------|--------------|-------------------|--------------------|-------|-----------------|--------|
| W3 | mac-studio `/private/tmp/jackin-m1rehearsal` | `2c57f74b` | JP1044 or recorded supersession | adjudication record | origin `preserve/handoff-1402ca52/jackin-m1rehearsal` | §E.6 gates 1–5 | `git worktree remove` (R2) | PENDING |
| W4 | mac-studio `/private/tmp/jackin-scratch` | `0b02a313` | JP1044 or recorded supersession | adjudication record | origin preserve branch | gates 1–5 | `git worktree remove` (R2) | PENDING |
| W5 | mac-studio `/private/tmp/velnor-gen-c832191f` | `01e3ce81` | velnor main or rejection record | triage record | origin preserve branch | gates 1–5 | `git worktree remove` (R1) | PENDING |
| W6 | mac-studio `/private/tmp/velnor-main-80bc` | `473eb7b6` | record only | n/a (re-derivable) | origin preserve branch | gates 1–5 | `git worktree remove` (R3) | PENDING |
| VB1 | R1 `m4-pin-bump` | `72d0d92b` | velnor main `45ef1ebe` | PR #1062 MERGED | merge commit | gates | `git branch -d` after re-branch | PENDING |
| VB3 | R1 `fix/e0277-include-str` | `90e71de5` | main `ef42b0f9` | PR #1017 MERGED | merge commit | gates | `git branch -d` | PENDING |
| VB4 | R1 `fix/unit-platform-split` | `d4b0570f` | main + salvaged commit | PR #1025 + salvage record | merge commit + salvage ref | salvage first | `git branch -d` | PENDING |
| VB5 | R1 `integrate/apple-ci-s2` + remote | `326fd414` | main + scaleset salvage | PR #985 + salvage record | merge commit + salvage ref | salvage first | delete local + remote | PENDING |
| JB3 | R2 `integrate/velnor-apple-ci` | `8fb49688` | JP1044 or supersession | adjudication record | origin preserve branch | gates | `git branch -D` only w/ coverage proof | PENDING |
| W9/W10 | review/archaeology wts | `96b835d9`/`4dec6b9e` | n/a | n/a | PR #1050 / merge #1041 | no-use check | `git worktree remove` | PENDING |
| R3/R4 tail | rollout/* [gone] etc. | various (recorded) | merged mains | per-branch verification | merge commits | per-branch | guarded `-d` only | PENDING |
| REM-VB5 | remote `tailrocks/velnor:integrate/apple-ci-s2` | expected `326fd414` (re-observe) | velnor main | PR #985 MERGED + scaleset salvage record | merge commit + salvage ref | gates 1–5 + no other-goal use | `git push origin --delete integrate/apple-ci-s2` | PENDING |
| REM-JC | remote `jackin-project/jackin:cicd/repromote-major1-9660c9ff` | expected `0d369a85` (re-observe) | superseded by JP1065 | JP1062 CLOSED-unmerged + JP1065 disposition | JP1065 ref | gates 1–5 + no other-goal use | `git push origin --delete cicd/repromote-major1-9660c9ff` | PENDING |

Required gates (ALL must hold): (1) every required change verified in target or explicitly
superseded/rejected with rationale + recovery retained; (2) all commits/edits/stashes/nested
repos accounted for, no unpreserved work; (3) no active agent/session/process/other-goal/
stacked-PR dependency; (4) post-integration validation passed at relevant revision; handoff +
recovery refs durable outside candidate; (5) exact repo/host/common-dir/path/ref/expected-tip
verified. Never bulk/force/wildcard delete. Never `git worktree remove --force` on dirty
work. Prefer guarded `git branch -d`; narrowly-scoped exception after squash merge only with
independent coverage proof. Local cleanup never deletes remote branches except the two
explicitly authorized stale ones (VB5's remote, `cicd/repromote-major1-9660c9ff`) after
dependency + recovery checks. After cleanup: re-enumerate worktrees/branches, reconcile with
ledger, write durable receipt.

KEEP exclusions (never cleanup candidates): W1, W2 (primary checkouts), VB2/JB2 (local
mains), VB7 (until goal done), JB1 (until #1044 merged), JB4 (unrelated), W7/W8 (unrelated),
VB6 (until REVIEW_SHARED resolved), W11/W12 clones (shared mirrors).

## F. Decisions, findings, assumptions, and rejected approaches

Decisions (with rationale):

- D1 Fix generators upstream, never Jackin workarounds (goal-amended constraint). Applied to
  defect C (migfix3) and now mold (R-MOLD). Rationale: generated files carry "do not
  hand-edit"; Jackin has zero `mold|linker` config surface.
- D2 Merge main, never rebase; squash-merge small DCO PRs. Applied to all 13 merged Velnor
  PRs + M4.
- D3 Re-pin-3 target = velnor main HEAD `eed474c4` (had published products) rather than
  migfix3 `155c6088` directly. Rationale: pin to newest shippable main.
- D4 Correct mold fix = skip mold on macOS, NOT mapping `arm64` into the case arms.
  Rationale: mold is Linux/ELF-only (auditor finding; re-verify from mold docs on resume).
- D5 Preserve-first pause: all dirty/local-only goal work pushed to `preserve/*` remote refs
  before writing this doc. Rationale: /tmp volatility + local-only risk.

Findings (with sources):

- F1 Defect C fixed: `install_args: … cargo-binstall cargo:sccache`, mise 5/5 incl. boltffi
  0.30.1 in 227s (job 106524303464 log).
- F2 Mold block pre-existing + newly unmasked: byte-identical at old pin `6f025e5a`; was
  masked by defect C (job died at mise install in ~46s).
- F3 #1065 reproduces the same Apple FFI failure at a different pin (80bc420d) → generator-side
  defect, not branch-specific.
- F4 Velnor main green at `45ef1ebe` (products + CI/Main success); carried failures at
  `eed474c4`/`c674f5bb` historical.
- F5 No approving reviews on any goal PR (`reviewDecision` empty everywhere) — process gap.
- F6 Local velnor `main` (59396040, 98 behind) and jackin `main` (fce94cea, pre-#1013 era)
  are severely stale; always use `origin/main`.

Rejected approaches:

- R1 Mapping `arm64` in the mold case statement — rejected (mold can't link Mach-O).
- R2 Hand-editing Jackin's generated `ci-unit-rust.yml` — rejected (violates D1 + file header).
- R3 Merging #1044 with red Apple leg — rejected (BLOCKED state enforced).
- R4 Re-pin-4 before mold fix — rejected (would reproduce the same failure).

Assumptions needing validation: A1 mold Linux-only (recheck docs); A2 VP1054 independence
from Apple leg; A3 JP1044-vs-JP1065 fold direction; A4 Preview @ `45ef1ebe` green (peer tree
evidence only).

## G. Verification evidence and known failures

Conventions: PASS / FAIL / INTERRUPTED / NOT RUN / STALE. Commands run by auditors/workers
(read-only unless noted); working dirs R1/R2 unless noted.

| Check | Command / source | Rev tested | Result | Evidence |
|-------|------------------|------------|--------|----------|
| #1061 merge + checks | `gh pr view 1061`, `gh pr checks 1061` | `84877bf5` | PASS | MERGED 21:03Z as `155c6088`; 12 pass 12 skip 0 fail |
| migfix3 products | velnor run 35654843370 | `155c6088` | PASS | all jobs success incl. Publish; 3 runtime artifacts |
| #1062 merge + checks | `gh pr view/checks 1062` | `72d0d92b` | PASS | MERGED 21:31Z as `45ef1ebe`; 23 pass 1 skip |
| main CI @ HEAD | velnor CI/Main + products | `45ef1ebe` | PASS | products + CI/Main success (one flake retry) |
| Preview @ `45ef1ebe` | — | `45ef1ebe` | NOT RUN | peer Preview on identical tree green (unverified claim) |
| re-pin-3 push state | `git log`, `gh pr view 1044` | `6ff54ce5` | PASS | head == local == remote; tree clean |
| pin products @ `eed474c4` | velnor run 35654862115 + release `velnor-workflow-runtime-v1-ec8123d55e2f34e2` | `eed474c4` | PASS | success; manifest + 3 binaries live; macOS digest matches |
| defect C fix | job 106524303464 log | `6ff54ce5` | PASS | mise 5/5 tools, boltffi 0.30.1, 227s |
| #1044 full matrix | jackin run 35657319778 (83 checks) | `6ff54ce5` | FAIL (3) | `rust-jackin-usage-ffi / Apple` 4m40s (root cause) + `ci-required` + `Control / Required` (consequential); 42 pass, 38 skipping |
| Policy @ #1044 | run 35657315637 | `6ff54ce5` | PASS | success |
| Prior Policy fail | run 35656343929 | `8c46b0c7` | STALE (fixed) | fixed by `c02a37d9`, superseded by 35656783056 success |
| #1065 Apple leg | in-progress run @ `909a9f54` | `909a9f54` | FAIL | same FFI Apple fail at 40s (independent repro of generator defect) |
| Local gates (mold) | NOT RUN | n/a | NOT RUN | no local fix attempted (pause before fix) |
| Benchmarks | NOT RUN | n/a | NOT RUN | blocked on R-MIG |

Known failure detail (mold): `Set up mold 2.42.0` prints `unsupported mold architecture:
arm64`, exit 1 (~21:42:16Z, job 106524303464). Generated `ci-unit-rust.yml` emits mold setup
unconditionally for github-hosted Rust jobs; `case "$(uname -m)"` matches only
`x86_64|aarch64`; Darwin reports `arm64`. Block appears twice in the file (lines ~367 and
~796 — both jobs affected). Generator sources: `hosted_mold_setup()` in
`crates/velnor-workflow/src/s2/mod.rs:6183` and `src/lib.rs:5931`; emitted at
`s2/primitives/ir.rs:9758` / `primitives/ir.rs:7239`, gated only on hosted-lane +
`ToolRequirement::Mold` (inserted s2:6983,9341 / s1:4165,6957; `linker:` uses s2:3987,4203,
4256 / s1:1991,2191,2244). Tests: `hosted_mold_setup_tests` in both files. Line numbers are
auditor-observed at `eed474c4`-era tree — re-verify on resume (nit: worker cited `:355`,
actual block at `:367-370`).

No full validation campaign was run during handoff (per instruction); only bounded read-only
checks to verify preservation/document accuracy.

## H. Ordered remaining-work plan

DO NOT execute during pause. Each item: outcome, files, deps/parallelism, next action,
validation, blockers.

1. **H1 — Velnor mold-fix PR (FIRST).** Outcome: mold setup/cache skipped on macOS; merged to
   velnor main with products release. Files: `crates/velnor-workflow/src/{s2/mod.rs,
   s2/primitives/ir.rs,lib.rs,primitives/ir.rs}` + both `hosted_mold_setup_tests`. Deps:
   none (starts immediately). Validation: new unit tests green, full PR matrix green,
   products run success + release live. Blocker: none known.
2. **H2 — VP1054 disposition.** Outcome: landed or explicitly deferred with rationale. Deps:
   none; parallel with H1 (verify file overlap first). Validation: PR checks green on current
   base.
3. **H3 — #1044/#1065/JB3 adjudication.** Outcome: ONE migration line chosen; others recorded
   superseded. Deps: after H1 (need green-capable pin). Validation: written rationale +
   retirement of losing lines.
4. **H4 — Re-pin-4 + merge #1044.** Outcome: JP1044 green and merged. Files: JB1
   (`migrate/apple-ci-generic`). Deps: H1 (+H3). Validation: 83-check matrix green incl.
   Apple FFI, swift-native, xcodegen, `ci-required`, `Control / Required`; byte-stable regen
   `--check`; actionlint clean.
5. **H5 — 5c hosted evidence.** Outcome: main Preview @ new HEAD observed green + hosted-run
   evidence recorded. Deps: H1/H4. Validation: run URLs + artifact listing.
6. **H6 — Benchmark matrix (§12.2).** Outcome: ≥3-rep controlled cold/warm/Swift-only/
   Rust-change/lock/profile/toolchain/corrupt runs with median/range + coverage report. Deps:
   H4. Validation: evidence table with run links; no invented speedups.
7. **H7 — Native macOS provider.** Outcome: real-Mac e2e proof or demonstrated external
   blocker with capability recorded unsupported. Deps: H4 (parallel with H6). Validation:
   executed runs or blocker evidence.
8. **H8 — W5/VB6 triage + proof-PR retirement.** Outcome: every preserved line has a final
   disposition. Deps: H4. Validation: ledger updated.
9. **H9 — §E.5 integration + §E.6 cleanup.** Outcome: all required work in targets; obsolete
   local resources removed with receipt. Deps: H1–H8. Validation: re-enumeration receipt.
10. **H10 — Final docs + close VP-HO.** Outcome: progress doc current, handoff PR closed,
    goal completion verified requirement-by-requirement. Deps: H9.

FIRST actionable resumption task: H1 (mold repro + fix PR). No other task may precede it
except `git fetch` + head re-verification (§J step 3).

## I. Environment and operational recovery

- Platforms: Apple Silicon Mac (operator host `mac-studio`); GitHub-hosted runners
  (macos-26-arm64 etc.); standard Debian Velnor lanes. Tool versions: repo-pinned
  (`mise.toml`, `rust-toolchain.toml`, `Cargo.lock`); Velnor runtime via release
  `velnor-workflow-runtime-v1-*` per pin.
- Working dirs: R1 `/Users/donbeave/Projects/github/velnor`, R2 `.../jackin`.
  Authenticated `gh` CLI required (API + `gh run view --log`).
- Setup: `git fetch --prune` in R1/R2 (+R3/R4 before touching tail branches);
  fast-forward local mains; `gh pr checks` to re-verify. Do NOT trust recorded SHAs without
  re-observation — mains may have moved during pause.
- External operations performed: 13 velnor merges + 1 jackin merge (all durable remote
  state); preservation pushes (§E.2/§E.3); CI runs triggered by those pushes (observed, not
  repaired during pause). Repeating pushes is safe (new branches); re-running CI is safe.
  No deployments, migrations, releases (other than automatic products), or infra changes.
- Generated artifacts (reproducible): `.github/`, `.github-gen/` outputs, Preview artifacts,
  runtime releases. Irreplaceable: none — everything goal-owned is in git remotes after §E
  preservation. Session logs are convenience-only.
- Rollback: merged PRs revert via normal revert PRs if needed; open PRs simply unmerged.
  No rollback executed or needed at pause.

## J. Fresh-agent resume runbook

Retrieval: this file lives on velnor branch `goal-handoff/generic-macos-swift-ci--1402ca52`
(DRAFT PR VP-HO — URL in §A after publication), NOT necessarily on velnor `main`. Fetch it:

~~~sh
cd /Users/donbeave/Projects/github/velnor
git fetch origin goal-handoff/generic-macos-swift-ci--1402ca52
git show origin/goal-handoff/generic-macos-swift-ci--1402ca52:docs/goal-handoffs/generic-macos-swift-ci-velnor-jackin--20260921T220231Z--almond-ara--1402ca52.md | head -5
~~~

Exact resume command:

```text
/goal Read and resume docs/goal-handoffs/generic-macos-swift-ci-velnor-jackin--20260921T220231Z--almond-ara--1402ca52.md
```

The future agent MUST:

1. Read this document completely + repo `AGENTS.md` files + `plans/apple-ci-progress.md`.
2. Recover checkpoints into exclusive branches/worktrees without overwriting unrelated work
   (primary checkouts W1/W2 are the resume homes; re-branch W1 off fresh `origin/main`).
3. Compare live repo/PR/CI state with recorded SHAs (`git ls-remote`, `gh pr view/checks`);
   detect intervening changes; never trust stale IDs or blindly rerun side-effecting cmds.
4. Reconcile completed work without discarding progress, rewriting shared history, or
   restarting finished tasks.
5. Resume the ORIGINAL goal at H1 (mold fix), not the handoff task. Use subagents with clear
   ownership; preserve constraints (DCO signoff, merge-not-rebase, small PRs, delegation +
   independent verification); verify real acceptance criteria.
6. Maintain the goal progress record; scoped checkpoint commits during resumed work.
7. After goal validation: execute §E.5 integration, verify combined target state, then ONLY
   eligible §E.6 cleanup with live re-observation before each action; final disposition +
   receipt for every inventoried resource.

Interpretation rule: the pause lasts until the user requests resumption. A later `/goal Read
and resume <this-file>` authorizes ORIGINAL-goal continuation — NOT another pause, handoff
regeneration, or recursive handoff PR.

## K. Blockers, omissions, and independent review

Blockers/omissions (honest inventory):

- K1 No runtime goal-pause control exists (tooling limitation) — goal record left active;
  freeze enforced by coordinator. Remedy: none available; resuming user steer overrides.
- K2 Full 101KB objective not embedded verbatim (size) — key passages quoted verbatim in §B;
  full text in session log line 19 (local-only path recorded). Remedy: none needed for
  resume (§B + §13 summary sufficient).
- K3 R3/R4 `rollout/*` long tail recorded per-branch in `/tmp/audit-wt.txt` (local-only
  convenience) + canonical branches retained in persistent clones — NOT individually
  enumerated in this doc (volume). Remedy: resuming agent re-enumerates via
  `git for-each-ref` in R3/R4 before touching them.
- K4 Jackin #1065 head `909a9f54` never fetched locally — needs `git fetch` on resume.
- K5 Mold-fix line numbers are auditor-observed, must be re-verified (tree moved since).
- K6 Preview @ `45ef1ebe` unobserved; `rust-velnor-workflow` flake (1 retry on #1062)
  unattributed — could recur.
- K7 Zero approving reviews across all goal PRs — process debt for resumed work.
- Otherwise NONE: no inaccessible worktrees, no unpreserved goal-owned Git work, no
  unstopped workers, no failed preservation/publication ops (all pushes succeeded).

Independent review: performed by `main/review-handoff/65` (read-only; report
`/tmp/review.txt`, local-only convenience). Initial verdict NOT-READY; all findings resolved:

- E1 (branch/PR unpublished at review time) — resolved by §6 publication below.
- E2/E3 (W11/W12 rows wrong) — ACCEPTED; rows rewritten from direct `git worktree list`
  evidence. Note: reviewer also claimed `velnor-pin-be61acbb` unregistered — refuted; it IS
  registered in R3 (detached `be61acbb` + 3 dirt lines).
- E4 (W6 "unregistered") — REFUTED with direct evidence: W6 IS in R3's `worktree list`
  (`473eb7b6 [preserve/handoff-1402ca52/velnor-pin-80bc]`); reviewer evidently checked R1's
  registry. `git worktree remove` (R3) stands.
- E5 (behind count) — ACCEPTED; corrected to 98 (verified `rev-list --count`).
- §2 missing R3 tail — ACCEPTED; added W13 group enumeration (21 worktrees, all observed).
- §2 adjacent PRs — ACCEPTED; added #962/#978/#1007 triage lines (all verified OPEN).
- Cleanup-gate structure — reviewer confirmed complete; added REM-VB5/REM-JC rows with
  expected tips for the two authorized remote deletions.
- Secrets — reviewer scan clean; coordinator grep clean.
- Fresh-agent recoverability — reviewer: main line yes (goal, H1 first action, resume homes,
  remote checkpoints all accurate); friction items (E1, R3/R4) fixed above.

Corrections made: W11, W12, W13 (new), VB2 count, F6, E.1 counts, E.4 triage lines, E.6
remote rows, VB7/§A/§J publication states. Unresolved: none — READY after §6 verification.



