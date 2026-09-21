Audit complete. All commands read-only (`ls-remote`, `gh pr view/checks`, `gh api compare`, `git branch/worktree`; no fetch, no mutations).

## Branch ledger (remote, `ls-remote` authoritative — 19 refs)

`main` @ `45ef1ebe` (#1062 squash). Local checkout is STALE (main @ `14a9ff84`, behind 9; tracking refs stale, e.g. `origin/codex/activation-foundation` shows `bb968cc6` vs real `60bb9326`).

| Ref | Tip SHA | Ownership | PR | Purpose / notes |
|---|---|---|---|---|
| `codex/github-first-hosted-g1-security-3ae` | `43ba3b41` | GOAL_EXCLUSIVE | #962 OPEN | ACTIVE source; #1063 is its selective port |
| `integrate/p962-port` | `5349ec32` | GOAL_EXCLUSIVE | #1063 OPEN | Port vehicle for #962 (= local checkout HEAD, in sync) |
| `codex/github-first-g3-integration-signed` | `056362aa` | GOAL_EXCLUSIVE | #963 OPEN | Queued; subsumed #961 |
| `codex/velnor-legacy-rolling-tag-repair-20260920` | `04da35e4` | GOAL_EXCLUSIVE | #973 OPEN | Queued |
| `codex/ci-performance-next` | `970a6dd5` | GOAL_EXCLUSIVE | #978 OPEN | Queued (stacked w/ #979/#980, base `97bac4c4`) |
| `fix/ci-validation-contract` | `9bbf4a4e` | GOAL_EXCLUSIVE | #979 OPEN | Queued (stacked) |
| `refactor/holla-parity` | `ab2f12fa` | GOAL_EXCLUSIVE | #980 OPEN | Queued (stacked) |
| `codex/activation-foundation` | `60bb9326` | GOAL_EXCLUSIVE | #1044 OPEN | Tail (advanced past ledger's `4ad4b28e`) |
| `fix/desktop-candidate-evidence` | `96b835d9` | GOAL_EXCLUSIVE | #1050 OPEN | Tail (advanced past `1ff4d4a7`) |
| `fix/rust-cache-hit-bootstrap` | `b084c416` | GOAL_EXCLUSIVE | #1052 OPEN | Tail; CI fully green but CONFLICTING |
| `fix/apple-mise-tool-closure` | `256c24bb` | GOAL_EXCLUSIVE | #1054 OPEN | Tail (moved from `b397c052`) |
| `fix/product-receipts` | `2a270947` | GOAL_EXCLUSIVE | #1055 OPEN | Tail (moved from `964ef069`) |
| `fix/native-product-closure` | `9f795b2e` | GOAL_EXCLUSIVE | #1056 OPEN | Tail (moved from `47269f1c`) |
| `codex/schedule-actions-read` | `70268cd5` | GOAL_EXCLUSIVE | #1057 OPEN | Tail (moved from `59335f2e`); fully green |
| `fix/composable-regen-phases` | `93cd45e9` | GOAL_EXCLUSIVE | #1058 OPEN | Tail; NEW — not in ledger (created 09-21T20:32:45Z) |
| `integrate/apple-ci-s2` | `326fd414` | GOAL_EXCLUSIVE | none | Reincarnated: +4 scaleset commits AHEAD of merged #985 head `2f684383`; needs queue key |
| `codex/ci-performance-campaign` | `155d6b81` | GOAL_EXCLUSIVE | none | Reincarnated: +1 WIP commit ahead of merged #968 head `17318e52`; needs queue key |
| `codex/rolling-preview-legacy-migration-20260920` | `f587b89f` | GOAL_EXCLUSIVE | none | DIVERGED +7/−11 vs merged #966 head `432da515` (incl. trusted-push gatings flagged as G10-adjacent); needs queue key |
| `main` | `45ef1ebe` | SHARED | — | Target |

UNRELATED on remote: none. Local-only leftovers (not on remote, ignore at resume except awareness): `fix985`, `fix985b`, `pr-977`, `pr-977-review`, `integrate/b08-hosted-admission` (tracks main, behind 83), plus `[gone]` integrate/rollout/fix branches.

## PR ledger (15 OPEN)

| # | Head → Base | Mergeable / state | Checks |
|---|---|---|---|
| #1063 (port) | `5349ec32` → `c674f5bb` (#1060, 1 behind tip) | MERGEABLE / BLOCKED | Policy FAIL only; workflow/topology/docker/ci-required/DCO pass, rest skip. 2 commits: `228fc58f` declared providers + `5349ec32` tarball bind |
| #962 | `43ba3b41` → `10483370` | CONFLICTING / DIRTY | Policy FAIL, DCO pass (stale run) |
| #963 | `056362aa` → `325719f1` | UNKNOWN / UNKNOWN | 22 pass / 48 skip, zero fail |
| #973 | `04da35e4` → `9e5c0eb2` | UNKNOWN / UNKNOWN | 11 pass / 40 skip, zero fail |
| #978 | `970a6dd5` → `97bac4c4` | CONFLICTING / DIRTY | 6 FAIL (Required, docs, Policy, runner, workflow, ci-required) |
| #979 | `9bbf4a4e` → `97bac4c4` | CONFLICTING / DIRTY | 6 FAIL (Required, docs, Policy, topology, workflow, ci-required; runner passes) |
| #980 | `ab2f12fa` → `97bac4c4` | CONFLICTING / DIRTY | 8 FAIL (adds workflow-contract) |
| #1044 | `60bb9326` → `45ef1ebe` | MERGEABLE / BLOCKED | 14+ pass, 7 PENDING (Policy + runner/workflow/tools/topology/bench/velnorctl) — active run |
| #1050 | `96b835d9` → `45ef1ebe` | MERGEABLE / CLEAN | All pass/skip, zero fail |
| #1052 | `b084c416` → `14a9ff84` | CONFLICTING / DIRTY | ALL GREEN incl. Policy — needs rebase only |
| #1054 | `256c24bb` → `45ef1ebe` | MERGEABLE / CLEAN | All pass/skip |
| #1055 | `2a270947` → `45ef1ebe` | MERGEABLE / CLEAN | All pass/skip |
| #1056 | `9f795b2e` → `45ef1ebe` | MERGEABLE / CLEAN | All pass/skip |
| #1057 | `70268cd5` → `45ef1ebe` | MERGEABLE / CLEAN | 22 pass, zero skip/fail — fully green |
| #1058 | `93cd45e9` → `45ef1ebe` | MERGEABLE / BLOCKED | workflow + Required + ci-required FAIL; Policy pass |

Stack deps: #978/#979/#980 share base `97bac4c4` (stacked per ledger). No other head→head stacking detected (all other bases are main commits).

## Merged task PRs — verified

Task ports (all `integrate/*` → squash on main): #998→`de5a1c46`, #1000→`c72eccb9`, #1004→`70c05dd5`, #1013→`02bb53bf`, #1021→`cc284950`, #1034→`be781371`, #1040→`65b82bb5`, #1047→`14a9ff84`. ✅ All MERGED with merge commits on main.

#1059→`267649e6`, #1060→`c674f5bb`, #1061→`155c6088`, #1062→`45ef1ebe`: ✅ all MERGED, but these are the author's own branches (fix/s2-nested-bun-watch, red-main/velnor-pin-bump, migfix3, m4-pin-bump) — externally resolved, NOT task ports.

## Worktree → branch → PR map

- `/Users/donbeave/Projects/github/velnor` → `integrate/p962-port` @`5349ec32` → PR #1063 (in sync with remote tip)
- `/tmp/pr1040-review`, `/tmp/pr1047-review` → detached review snapshots (stale, harmless)
- `/tmp/velnor-b07-port` → `integrate/b07-buildkit-ceilings` (remote ref gone; stale)
- `/tmp/velnor-gen-4dec6b9e` → detached; plus one detached grok implementer worktree `048a7bda`

## Integration order (ledger-aligned + deltas)

Ledger tail ended at #1061; remaining-queue order now: **#962 (active; #1063 unblocks it: merge #1063 → close #962 + delete source) → #963 → #973 → #978 → #979 → #980 (oldest-first frozen group) → #1044 → #1050 → #1052 → #1054 → #1055 → #1056 → #1057 → #1058 (tail by createdAt)**. Deltas vs ledger: #1053/#1059/#1060/#1061 merged since; #1058 new at tail; 3 no-PR branches (`integrate/apple-ci-s2`, `codex/ci-performance-campaign`, `codex/rolling-preview-legacy-migration-20260920`) need tip-date queue keys at resume — the rolling-preview one is diverged, not a simple reincarnation.