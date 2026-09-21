Audit complete. All checks read-only. Two parent claims corrected (main tip, queue shorthand) plus two unrecorded live refs found.

## Interruption point

- **Last done**: #961 SUBSUMED close+delete. Verified: branch absent from `ls-remote` (empty), PR CLOSED 20:46:14Z with donbeave subsumption comment naming #963 + patch-ids + redundant 857646a0.
- **In-flight**: #962 selective-port. Source branch live @`43ba3b41` ✓, PR #962 OPEN ✓ (updated 09-20, untouched). Port PR #1063 OPEN @`5349ec32`, merge-base `eed474c4` ✓, 2 commits 1 file +234/-5 ✓, NOT merged, NOT deleted ✓. State BLOCKED, Policy FAIL (run 35657382801, `generated-tree`), everything else green (DCO, ci-required, workflow 6m46s).
- **Next**: reconcile ledger + re-pin, then fix #1063 per mechanic recipe.

## Corrections to brief

1. **Main is `45ef1ebe` (#1062), not `c674f5bb`.** #1062 (empty commit, pin already done by #1060) landed after the review. Chain: `155c6088`→`eed474c4`→`c674f5bb`→`45ef1ebe`.
2. **#966 and #968 PRs are MERGED** (Sept 20), not open. But their branches live on at *different* SHAs than PR heads (`155d6b81` vs `17318e52`; `f587b89f` vs `432da515`) → reincarnation/move triage needed on resume, not queue evaluation as written.
3. **`integrate/apple-ci-s2` live @`326fd414`, no open PR, unknown locally.** Ledger says merged #985 + absent → reappeared or leftover. Triage on resume.
4. **`fix/composable-regen-phases` (#1058, OPEN, created 20:32:45Z) never recorded in ledger.** Inserts in tail between #1057 and #1059-slot.

## Progress ledger

| ID | Requirement | Status | Evidence | Remaining | Deps |
|---|---|---|---|---|---|
| FB-01–58 | Fallback branches disposed | VERIFIED_DONE (ledger; spot-check: trio refs absent, #955/#957/#960/#961 absent) | Ledger + `ls-remote` (19 heads, no fb refs) | None | — |
| #955/#957/#960 | PR branches NIL + closed | VERIFIED_DONE | Ledger; refs absent; #961 verified live this session | None | — |
| #961 | SUBSUMED close + delete | VERIFIED_DONE | `ls-remote` empty; PR CLOSED + pointer comment 20:46:14Z | None | — |
| #962-invest | Investigate #962 @43ba3b41 | VERIFIED_DONE | /tmp/p962-report-agent.md (pins matched) | Re-verify load-bearing claims on new main (was pinned 0dbcdb25→challenger 155c6088; main now 45ef1ebe) | — |
| #1063-port | Port ea9686f0+525fc9e0 → PR | IN_PROGRESS | #1063 @5349ec32 OPEN; review CHANGES-REQUESTED (scope/correctness clean, Policy red) | Rebase onto 45ef1ebe per recipe, force-push | Reconcile |
| #1063-ci | #1063 full-green CI | BLOCKED (deterministic stale-pin fail) | Policy run 35657382801 FAIL; mechanic: pin 6737cdb3 vs validator eed474c4; fix simulated clean | Rebase → rerun → confirm oracle-1058 path | #1063-port |
| #1063-merge | Merge + verify main | NOT_STARTED | — | Head-guarded merge (inspect delta per ledger lesson), anchor+test verify | #1063-ci |
| #962-close | Close #962 + lease-delete | NOT_STARTED | — | Explanation comment w/ port pointer; delete @43ba3b41; verify absent | #1063-merge |
| #963,#973,#978,#979,#980 | Queue PR branches | NOT_STARTED | All OPEN, heads match recon SHAs | Full investigate→challenge→port/nil→delete each | #962-close |
| #966/#968-branch | Moved-branch triage | NOT_STARTED | PRs MERGED; branches live at new SHAs | Classify reincarnation vs leftover; queue or drop | Reconcile |
| apple-ci-s2 | Unknown live ref triage | NOT_STARTED | Live @326fd414, no PR | Fetch, classify, queue or drop | Reconcile |
| Tail | #1044,#1050,#1052–#1058 | NOT_STARTED | 9 OPEN (#1058 unrecorded) | Externally-merged check, else evaluate each | Queue PRs |
| Final | Pin-forward + green main + only-main | NOT_STARTED | Deferred per ledger decision | Pin to tip, CI/Main+Preview+Runtime green, heads==main, GAP-2 re-check | Tail |

## Known-failure table

| Check | Result | Cause | Verdict |
|---|---|---|---|
| #1063 Policy run 35657382801 `generated-tree` | FAIL 9m50s | Stale pin 6737cdb3 vs validator eed474c4; PINNED_BINARY hard-error before candidate exception | Deterministic, structural; recipe proven (simulation: clean apply, 23/23 cmp-clean, closure unchanged). NOT content-caused. Re-run pointless pre-fix |
| #1063 all other checks | PASS (DCO, ci-required, workflow, docker, topology; 14 skip) | — | Content green |
| #962 tip CI (Policy FAIL, DCO pass) | FAIL, procedural | Bootstrap chicken-and-egg (old renderer can't parse verification_providers); disclosed in body | Moot: main parses field now; PR closes unmerged |
| Reviewer §4 "any PR fails identically" | REFUTED | Post-#1060 PRs (1058/1056/1055/1054/1050/1044) all Policy-green on rebased pin eed474c4 | Blast radius = #1063 only |
| #1060 suspect ("wrong pin") | CLEARED | Tree diff actually 6737→eed474c4; main self-consistent | No harness repair |

## Stale-vs-valid prior results

- **Challenger pin `155c6088` → main `45ef1ebe` (3 behind).** Load-bearing #962 claims (anchors 0-hits, supersession SHAs) need re-grep on resume. Drift so far disjoint: review verified `c674f5bb` = pin-bump only, zero overlap with release.rs; mechanic verified `45ef1ebe` = empty commit. Re-verify mechanically, don't assume.
- **Valid as-is**: #1063 scope exactness (sorted-diff empty both commits), 5-test sensitivity proof, mechanic digests (all four recomputed, match CI exactly), rebase simulation.
- **Ledger stale**: ends at p962-investigation note; missing #1060/#1061/#1062 merges, #1058 appearance, #1063 review+mechanic, #966/#968/#985-branch states.

## Ordered remaining-work plan

1. **FIRST TASK — Reconcile + re-pin** (no mutations beyond fetch): `git fetch origin --prune`; record main tip; full `ls-remote` inventory vs ledger; append missing ledger entries (#1060/#1061/#1062, #1058 queue insert, #1063 review+mechanic summary, #966/#968/apple-ci-s2 triage results); re-grep #962 load-bearing anchors on current main.
2. Fix #1063 per recipe: rebase onto current main (expect clean apply, no re-render — verify `generate . --check --pin-build` exit 0), force-push. Do NOT pin-to-head on branch.
3. Re-review #1063: scope exact on new head, CI rerun → expect oracle-1058 log path (pin==validator early path, 11 rules 0 failed).
4. Merge #1063 head-guarded: `git log base..main` inspect first (ledger lesson); verify main (pins/anchors, lib suites).
5. Close #962 unmerged with explanation + port pointer; lease-delete source @43ba3b41; verify absent.
6. Queue: #963 → #973 → #978 → #979 → #980 (each full cycle), plus #966/#968/apple-ci-s2 triage dispositions.
7. Tail: #1044, #1050, #1052, #1053, #1054, #1055, #1056, #1057, #1058 (merged-check first; author merging fast).
8. Final: pin-forward PR to tip → full main-green (CI/Main + Preview + Runtime) → only-main `ls-remote` check → GAP-2 (#1045 fix) re-confirm → bundle coverage extend if needed.

Deps chain: 2←1, 3←2, 4←3, 5←4, 6←5, 7←6, 8←7. Estimated position ~96% holds: 58/58 fallbacks + 4 queue PRs done; in-flight #962/#1063 + 5 queue + ≤9 tail + final remain.