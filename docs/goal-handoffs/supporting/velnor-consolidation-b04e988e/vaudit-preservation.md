Preservation audit complete. All reads read-only; no mutations. Audit window ~22:25–22:35Z (HANDOFF last update 22:19:14Z).

# 1. Confirmed/corrected inventory deltas

## §E.2 worktrees/clones (C0): all 7 confirmed, zero drift
| ID | HEAD now | Status | Verdict |
|---|---|---|---|
| WT-0 `velnor` | `5349ec3297f5c2fcd13fb303c312abc307a88f97` on `integrate/p962-port` | clean, in sync with remote | CONFIRMED |
| WT-1 `pr1040-review` | `e191eeaa009af2f41a39f281d8d4ebd93f1704a8` detached | clean | CONFIRMED |
| WT-2 `pr1047-review` | `b7cf08ae24efc5b4b84db1bd4951e21f00ec6499` detached | clean | CONFIRMED |
| WT-3 `velnor-b07-port` | `36c2d6b26d452d2e2af61173c928d8862c164c31` on `integrate/b07-buildkit-ceilings` [gone] | clean; MERGE_RR (0-byte) + COMMIT_EDITMSG + ORIG_HEAD residue present | CONFIRMED |
| WT-4 `velnor-gen-4dec6b9e` | `4dec6b9ec28b0d51cb370fd8f5d5401c6186adf0` detached | clean | CONFIRMED |
| WT-5 grok | `048a7bdaed8240cf652127c94434e60528633dec` detached | listing-only, not entered (boundary kept) | CONFIRMED present |
| WT-6 handoff | `5496db7e242c95dfc9f8ace6800af77cbe15f350` == remote == PR #1065 head | clean, tracks origin/main [ahead 4] (cosmetic only) | CONFIRMED |
- Stashes: none in C0 or C1. `prune --dry-run`: empty. No unlisted C0 worktree/branch/detached tip.
- **CORRECTION — C1 MOVED (was recorded dormant):** C1 `/Users/donbeave/Projects/velnor-optimizations/velnor` was `fix/s2-nested-bun-watch-scoping @f7bebb42`; now on `handoff/change-aware-minimal-work-20260921 @1d7c2a741ad23214f1c9d77b2b2a8853df2818e4`, clean, tracking live remote ref. **C1 is actively pushing** (see #1067). `vi` PID 57502 on C1 COMMIT_EDITMSG persists (same PID). Disposition KEEP stands, urgency up (R4).

## §E.3 branches: 19/19 §E.3 tips byte-identical; remote 25 → 27 refs
- All 19 §E.3 tips match full SHAs, incl. `main @45ef1ebef769c78f45315e11a798fdaafaef4c4e` (no advance since 22:05Z). #961 ref still absent.
- **§K-addendum drift, reclassified + 1 newer drift:**
  - `codex/goal-handoff-2cc4de09`: `958a3e6b` → **`b85e4008179c05d2e7b6836755435e897d909ba8`** (advanced by other goal; PR #1064 CI re-running). NEWER DRIFT.
  - `preserve/handoff-1402ca52/velnor-rust-scan @01e3ce81…`, `velnor-pin-80bc @473eb7b6…`: unchanged; publisher id `1402ca52` now **RESOLVED** → other-goal handoff PR #1066 (reclassify UNKNOWN→other-goal; checkout location still unknown, not under `github/` top-level).
  - `preserve/stray-runner-header-0abc3675 @0abc3675…`: unchanged, still UNKNOWN owner.
  - `preserve/repin-trial-be78a6d0 @4300cc63…`: unchanged; publisher now **RESOLVED** → C1 operator (C1 holds objects + worktree on this branch). Reclassify UNKNOWN→C1-operator/other-goal.
- **2 NEW refs (post-HANDOFF, now-exact):** `goal-handoff/generic-macos-swift-ci--1402ca52 @8c904fbf1b82f74715d7236f171c2bcc05b877ea` (PR #1066) and `handoff/change-aware-minimal-work-20260921 @1d7c2a741ad23214f1c9d77b2b2a8853df2818e4` (PR #1067).
- **CORRECTION — `[gone]` count:** HANDOFF "~12" → **exactly 16**: `fix/include-closure-destructure @47285797`, `fix/preview-main-repairs @a5f5fbd7`, `integrate/b07 @36c2d6b2`, `b08-phase2 @3bb644d3`, `b08-phase3 @3b1b1e1d`, `b11 @45eda280`, `b27 @a548392d`, `b37 @72aa997d`, `b39 @7dfdcb70`, `b43 @403714e4`, `promote-d19-after-974 @20adffd3`, `reconcile-selective @c24be56e`, `release-leg-seed-pin-fetch @b26eff15`, `runner-hardening @4064cecb`, `rollout/d19-pin-1e454958 @7bdace2d`, `rollout/d19-pin-70c05dd5 @391cb3c7`. (Pre-existing undercount, not drift — both `fix/*` remotes were auto-deleted before 22:00Z.)
- Local-only tips recorded (HANDOFF gave no SHAs): `fix985 @c54231c9`, `fix985b @d7544a3d`, `pr-977 @7308307b`, `pr-977-review @838fb296`. `b08-hosted-admission @5af89559` [behind 83] confirmed; its tip == merged PR #983 head (preserved).

## §E.4 PRs: 15 → 19 open; §E.4 fifteen all head-identical
- #1063: head/checks/state identical (Policy FAIL run 35657382801 only; BLOCKED/MERGEABLE). #962: same (Policy FAIL stale run 35498407618).
- **#963, #973: mergeable UNKNOWN → CONFLICTING/DIRTY**; checks unchanged (zero fail/pending) — GitHub recomputation, not new signal.
- **#1044: pending run RESOLVED → FAIL** (run 35660442225 + Policy 35660437293: Policy 11m39s, velnor-workflow, velnor-tools, Required, ci-required fail). Author-side; observe-only per HANDOFF.
- #978/#979/#980 fail counts exact (6/6/8). #1050/#1052/#1054/#1055/#1056/#1057 zero-fail; #1052 still green-but-conflicting. #1058 signature unchanged incl. **Policy PASS preserved** (run 35659330075 — oracle for #1063 fix intact).
- **#1064** (other goal): head `958a3e6b→b85e4008`, DRAFT, CI running (docs + rust-velnor-workflow fail; Policy + runner pending).
- **#1065** (this handoff): head `5496db7e` == remote == WT-6, DRAFT, base main. **New CI run 35662167518/Policy 35662164076** supersedes recorded 35661403991 — same red signature (docs, rust-velnor-workflow, Policy, Required, ci-required; bun+planning pass). Recorded-not-repaired rule still applies.
- **#1066** (NEW, other goal `1402ca52`): DRAFT, head `8c904fbf`, red (docs, Policy, Required, ci-required fail).
- **#1067** (NEW, other goal `2100e49a`, C1's): DRAFT, head **`084b6c34 → 1d7c2a74` force-pushed DURING this audit** (~2 min window); docs fail, Policy/ci-required pending. Intermediate `084b6c34` absent from both local stores — unrecoverable locally (their own DRAFT churn; note only).
- #961 still CLOSED 20:46:14Z. No other state changes in `--state all` (limit-200 list reviewed).

# 2. Unmapped-resource list (newly found, adversarial hunt)
- U1: C1 worktree `/private/tmp/velnor-pin-1e454958`, detached `@1e454958` (old-main snapshot), clean. C1-operator scratch. Do not touch.
- U2: C1 worktree `/private/tmp/velnor-repin-trial`, `preserve/repin-trial-be78a6d0 @4300cc63` = remote tip, clean. Remote-preserved; do not touch.
- U3: remote `goal-handoff/generic-macos-swift-ci--1402ca52` + PR #1066 (other goal; resolves U4's publisher).
- U4: remote `handoff/change-aware-minimal-work-20260921` + PR #1067 (C1-operator's; live pushes).
- U5: `1402ca52` goal checkout location — unknown (not under `github/` top-level; out of documented scope to hunt further).
- Negative results: no unlisted C0 worktree/branch/stash/detached tip; no new `velnor*` clone under `github/` top-level; no goal-owned processes (`ps` shows only known `vi` 57502); `/tmp` artifacts 100% survive (bundle 111999064 B `verify OK`, 66 refs; ledger 82292 B; 19 b-reports; 5 p-reports; pr1/pr2-recon; 5 policy logs; objective/pause-full/p1063-review/policy-mechanic).

# 3. Chain breaks (resource chain: worktree→branch→remote ref→PR→target→cleanup)
- B1 — WT-3 `integrate/b07 @36c2d6b2`: local-only, no `refs/heads` ref. HANDOFF §E.2/E.5/E.6 already BLOCKED-correct. **Fix stands; add step:** at resume, `git fetch origin pull/982/head` (PR #982 merged with head == `36c2d6b2`) to confirm GitHub-side exact-SHA survival, then push to a `preserve/` ref before any removal. (Bundle head-list lacks the ref; commit predates bundle by 12 min but objects presumably unreachable from bundle's old main `59396040` — treat as absent.)
- B2 — `fix985 @c54231c9` / `fix985b @d7544a3d`: local-only, committed 09:41 +0700, **post-bundle → definitively unpreserved**, no PR. HANDOFF §E.6 gates `-d` on a coverage check that currently cannot pass from any remote ref. **Fix:** at resume, content-diff on record, then either push to `preserve/` ref (keep) or document drop rationale, before any `-d`.
- B3 — `pr-977 @7308307b` / `pr-977-review @838fb296`: local-only, no remote ref; predate bundle but containment unverified (no extraction performed — would mutate). Same fix as B2; check reachability from `main`/bundle first.
- B4 — WT-4 detached `@4dec6b9e` (= #1041 G10 commit; almost certainly on `main`): E.6 gate (`log --all --contains`) correctly specified, just unexecuted. Not a break — ready to verify at resume.
- B5 — `preserve/handoff-1402ca52/*`, `preserve/stray-runner-header`: remote ref with no PR and no local counterpart (chain ends at preserved ref, owner unknown/uncoordinated). HANDOFF §K-addendum do-NOT-delete rule correct. **Fix:** unchanged — §H-1 absorbs via fresh `ls-remote`; coordinate before any deletion.
- B6 — C1 checkout + U1/U2: live operator-owned chains outside goal scope; U2 remote-complete, U1 snapshot-only. **Fix:** none — re-observe at resume (R4), never touch.
- B7 — #1067's orphaned `084b6c34`: broken link in another goal's chain; no HANDOFF section (post-HANDOFF). Note only.

# 4. Preservation risks
- R1 `/tmp` volatility: unchanged — everything present now, still reboot-volatile, bundle still has no remote copy. Re-verify first at resume (§J-3 stands).
- R2 **Bundle coverage gap (exact):** 19 of 27 live tips postdate the bundle and are reached only via remote refs: `main @45ef1ebe`, `integrate/p962-port @5349ec32`, `codex/activation-foundation @60bb9326`, `codex/schedule-actions-read @70268cd5`, `fix/apple-mise-tool-closure @256c24bb`, `fix/composable-regen-phases @93cd45e9`, `fix/desktop-candidate-evidence @96b835d9`, `fix/native-product-closure @9f795b2e`, `fix/product-receipts @2a270947`, `fix/rust-cache-hit-bootstrap @b084c416`, `integrate/apple-ci-s2 @326fd414`, `goal-handoff/b04e988e @5496db7e`, `codex/goal-handoff-2cc4de09 @b85e4008`, `goal-handoff/1402ca52 @8c904fbf`, `handoff/change-aware @1d7c2a74`, + 4× `preserve/*`. Only 8 live tips are bundle-identical (the untouched `codex/*`, `fix/ci-validation-contract`, `refactor/holla-parity`). Any future deletion needs bundle extension first (HANDOFF §B.4 rule stands).
- R3 **Concurrent pushers active:** C1 operator pushed #1067 mid-audit; #1064 advanced post-HANDOFF. Resume MUST re-observe everything (§H-1); no HANDOFF SHA outside §E.3's 19 may be trusted.
- R4 **C1 is live, not dormant:** head moved since HANDOFF while `vi` persists. Strengthen §E.2/K disposition: re-observe C1 (`HEAD`/status/stash/worktrees) at resume before any shared-remote action.
- R5 **E.6 under-enumeration:** "stale local branches" row must list 16 `[gone]` branches (§1 correction), not "~12", before any individual `-d`.

Net assessment: HANDOFF §E is accurate for all goal-owned state (zero drift in 19 branch tips, 7 C0 worktrees, /tmp artifacts, bundle); all deltas are other-goal/concurrent activity plus two HANDOFF imprecisions (`[gone]` count, C1 dormancy assumption). No lost goal work; no incorrect disposition found.