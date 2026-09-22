# Velnor consolidation ledger — 2026-09-22

Status: active; integration PR #1069 is green; post-merge verification and
audited cleanup remain.

This ledger records the evidence and dispositions for consolidating discoverable
Velnor work into authoritative `main`, followed by audited cleanup. Machine-
specific recovery details are kept in the private recovery record identified as
`velnor-consolidation-recovery-20260922`; this document intentionally contains
no credentials or host-private paths.

## Authority and initial snapshot

| Item | Evidence |
| --- | --- |
| Repository | `tailrocks/velnor` via the configured `origin` identity |
| Initial authoritative remote `main` | `45ef1ebef769c78f45315e11a798fdaafaef4c4e` |
| Initial primary checkout HEAD | `ea2e764113cfa23cb17263eabcd2d0d246e81bbd` |
| Initial primary checkout branch | `fix/renovate-chown-unconditional` (upstream gone) |
| Initial primary checkout state | Untracked `.firecrawl/`, `goal-finish-and-merge-ci-runtime-products.md`, and `velnor-bastion-final-plan.md`; no tracked staged/unstaged diff |
| Git state | Shallow repository; existing refs preserved before discovery fetch |
| Discovery fetch | Explicit non-colliding `refs/discovery/origin/...` namespaces; no prune |
| Recovery | Complete bundle, dirty patches, untracked/ignored archives, index/config snapshots, and checksums verified before integration |

## Coverage record

| Surface | Evidence | State |
| --- | --- | --- |
| Primary checkout and nested instructions | `AGENTS.md`, nested workflow `AGENTS.md`, repository manifests/docs | Captured; continue review |
| Registered worktrees | Initial worktree inventory and Git worktree metadata | 86 records; 79 missing/stale records; disposition pending |
| Local refs and reflogs | Initial refs/reflogs plus recovery bundle | Captured; analyze |
| Stashes | 11 stash roots preserved and bundled | Analyze individually |
| Remote branches/tags/PR heads | Explicit non-colliding discovery fetch; 29 non-main branches, main, 20 pre-existing open PRs, no forks | Captured; PR #1069 is the consolidation path |
| Independent clones and host roots | Host scan covered common Velnor roots and clone/worktree stores | 61 live checkouts and 451 stale records observed; no deletion authorized |
| Detached/recoverable commits | Worktree heads, handoff refs, reflogs, and bundle preserved | Captured; 1040 unreachable commits retained |
| Active use/locks | Process and workspace checks delegated | Operation markers and dirty/user material retained; no broad cleanup |

## Change disposition ledger

Every candidate gets a stable source SHA or snapshot digest, logical-change
scope, evidence, and exactly one disposition: `ACCEPT AS-IS`, `ACCEPT WITH
ADAPTATION`, `ALREADY SATISFIED`, `SUPERSEDED/REJECTED`, or `BLOCKED`.

| Source identity | Logical change | Disposition | Evidence / destination |
| --- | --- | --- | --- |
| #1054 `256c24bb` | Apple mise tool closure | ACCEPT AS-IS | Integrated as `e6f55e30`; focused CI passed |
| #1055 `3b0b8f37`, `2a270947` | Product receipts | ACCEPT AS-IS | Integrated as `1f908e1c`, `46019b3f`; focused CI passed |
| #1056 `fce9b563`, `32bb1dee`, `9f795b2e` | Native product closure | ACCEPT WITH ADAPTATION | Rebased onto current main as `0f5e71a0`, `1390a904`, `fd32fff8`; product-input watch closure retained |
| #1057 `67594a08`, `0ecec338`, `d6535337`, `70268cd5` | Schedule/actions read path | ACCEPT WITH ADAPTATION | Integrated as `cddb8a9a`, `ab7f9d9d`, `1a1a1cbf`, `f9f1864f`; conflict resolved to retain both product-input and check-profile tests; marker removed in `84b8caf` |
| #1050 `e43951b4`, `1bed04e3`, `b7a8fb77`, `9d519b4c`, `96b835d9` | Desktop candidate evidence | ACCEPT AS-IS | Integrated as `94e06a11`, `74fad4f9`, `094f851b`, `5b07dfa8`, `9a60c371` |
| #1063 `228fc58f`, `5349ec32` | P962 source integration | ACCEPT AS-IS | Integrated as `7634421a`, `094805b4` |
| `b1a69b9` | Align mise fleet pin and include runner fallback in integrity authority | ACCEPT WITH ADAPTATION | Root Dockerfile and runner pin aligned to `2026.9.12`; `pin-integrity` passed |
| #1052 exact head `1119f83f` | Stale generated/refactored cache bootstrap | SUPERSEDED/REJECTED | Exact cherry-pick aborted; branch was 2 ahead/9 behind and failed current strict gates |
| #1052 valid subset | Verify restored Rust toolchain cache before reuse | ACCEPT WITH ADAPTATION | Current-tree implementation in `7c37f3ed`; generated via Velnor renderer; 6502-test and remote CI pass |
| #1058 exact head | Rust cache bootstrap candidate | BLOCKED | Strict clippy failed with 18 errors; scanner activation ordering was not proven against the pinned runtime; no direct merge |
| #962 exact head | ARM selector/parser work | SUPERSEDED/REJECTED | Whole branch rejected; no current-tree proof justified direct merge |
| #963 exact head | Scanner merge unit | SUPERSEDED/REJECTED | Merge unit rejected; only current-tree invariants would be eligible for separate adaptation |
| #973 | Workflow follow-up | ALREADY SATISFIED | Current `main` already contains the claimed result |
| #978 | Earlier workflow family | SUPERSEDED/REJECTED | Superseded by #980/current expected-work architecture |
| #979 | Earlier expected-work design | SUPERSEDED/REJECTED | Superseded by newer expected-work implementation |
| #980 | Mixed archival/checkpoint payload | SUPERSEDED/REJECTED | Not a safe merge unit; evidence retained only |
| #1044 exact head `60bb9326` | Large activation/policy migration | BLOCKED | Policy gate failed; generated outputs were stale/pin-mismatched; useful concepts not direct merge material |
| #1064–#1068 | Handoff, ledger, macOS, validation, rollout drafts | SUPERSEDED/REJECTED | Evidence extracted; drafts are not merge payloads |

## Integration queue

Integration is serialized through the clean consolidation worktree. Each unit
must be reviewed, focused-tested, committed, and pushed before the next unit.

| Order | Source/change | Dependencies | Verification | Destination |
| --- | --- | --- | --- | --- |
| 1 | Baseline and governance evidence | None | Recovery bundle, rulesets, DCO/squash policy, remote census | Ledger commit `0bdde554`; evidence retained |
| 2 | Fleet pin authority | Baseline | `mise run pin-integrity`; runner mise tests | `b1a69b9` |
| 3 | Accepted product/closure/schedule/desktop/P962 families | Current main | Focused workflow tests; fmt/clippy | Commits listed above; pushed to consolidation branch |
| 4 | Rust cache-hit verifier subset | Current generator source | 2697 workflow tests; `--pin-build --check`; generated output | `7c37f3ed` |
| 5 | D19 promotion experiment | Runtime product must exist first | Dry-run succeeded; real pin caused missing-release bootstrap failure; reverted | Revert `bb102439`; keep published pin until post-merge product exists |
| 6 | Consolidation PR | All integrated units | PR #1069; DCO, Policy, ci-required, Control, all unit jobs pass | Awaiting merge and post-merge main verification |

## Verification log

| Scope | Command/evidence | Result |
| --- | --- | --- |
| Recovery bundle | `git bundle verify` | Passed; complete history reported |
| Recovery archives | SHA-256 checksums | Recorded in private recovery record |
| Main baseline | `mise run fmt`, `actionlint`, workspace check, Bun typecheck/build, and `pin-integrity` | Passed; `audit-ci` failed with 130 errors on clean `origin/main` |
| Integrated state | `mise run fmt`, `actionlint`, `pin-integrity`, `deny`, `lint`, topology, release-boundary scripts | Passed |
| Integrated tests | `mise run test` | 6502 passed, 5 skipped |
| Generator contract | `velnor-workflow --plain --check --pin-build .`; after promotion revert, remote candidate path | Passed locally with candidate pin; final PR CI passed |
| Failed promotion attempt | PR run `35715057697`, Policy `35715057695` | Diagnosed: non-main D19 pin had no immutable runtime release; corrected by `bb102439` |
| Remote PR #1069 | CI / PR run `35715417781`; Policy run `35715415160` | All checks passed at head `bb102439` |
| Remote landing | Final remote-main SHA and post-merge CI | Pending |

## Deletion manifest

No destructive cleanup is authorized by this ledger yet. A candidate may be
removed only after its unique work is dispositioned, recovery is verified, the
current state is re-read, active use and shared storage are checked, and an
independent verifier approves the exact path/ref.

| Identity | Current state digest | Unique work disposition | Recovery | Active-use check | Approval | Deleted |
| --- | --- | --- | --- | --- | --- | --- |
| `/private/tmp/velnor-baseline-audit-20260922` | Temporary read-only baseline worktree | Audit comparison complete; exact removal allowed after process recheck | No user changes | Process recheck pending | Pending | No |
| Missing/stale registered worktrees | Git metadata records without live paths | No unique state disposition yet | Bundle and refs retained | Not assessed per exact path | No | No |
| Dirty primary checkout and independent clone | User/unresolved-operation material | Preserve until separately dispositioned | Private archives and bundle verified | Active-use/markers retained | No | No |

## Completion checkpoint

Not complete. PR #1069 is green but not merged. Remaining required work:
merge only after the final head review/thread audit; verify authoritative
remote `main`; verify the mainline runtime product and perform D19 promotion
through a follow-up PR; close/supersede reviewed stale PRs with evidence; remove
only exact safe temporary/stale artifacts after independent rechecks; rerun
discovery/fsck/manifests; and leave a clean canonical main checkout while
retaining recovery exceptions and restoration steps.
