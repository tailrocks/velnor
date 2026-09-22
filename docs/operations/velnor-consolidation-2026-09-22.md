# Velnor consolidation ledger — 2026-09-22

Status: active; not a completion claim.

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
| Registered worktrees | Initial worktree inventory and Git worktree metadata | 85 records; disposition pending |
| Local refs and reflogs | Initial refs/reflogs plus recovery bundle | Captured; analyze |
| Stashes | 11 stash roots preserved and bundled | Analyze individually |
| Remote branches/tags/PR heads | Explicit discovery fetch; GitHub metadata pass pending | Analyze |
| Independent clones and host roots | Host scan delegated; local scan in progress | Pending |
| Detached/recoverable commits | Worktree heads, handoff refs, reflogs, and bundle preserved | Analyze |
| Active use/locks | Process and workspace checks delegated | Pending |

## Change disposition ledger

Every candidate gets a stable source SHA or snapshot digest, logical-change
scope, evidence, and exactly one disposition: `ACCEPT AS-IS`, `ACCEPT WITH
ADAPTATION`, `ALREADY SATISFIED`, `SUPERSEDED/REJECTED`, or `BLOCKED`.

| Source identity | Logical change | Disposition | Evidence / destination |
| --- | --- | --- | --- |
| Pending discovery | Pending | Pending | Pending |

## Integration queue

Integration is serialized through the clean consolidation worktree. Each unit
must be reviewed, focused-tested, committed, and pushed before the next unit.

| Order | Source/change | Dependencies | Verification | Destination |
| --- | --- | --- | --- | --- |
| 1 | Baseline and governance evidence | None | Pending | Pending |

## Verification log

| Scope | Command/evidence | Result |
| --- | --- | --- |
| Recovery bundle | `git bundle verify` | Passed; complete history reported |
| Recovery archives | SHA-256 checksums | Recorded in private recovery record |
| Main baseline | Repository-derived checks below | Pending |
| Integrated state | Focused and full gates from project configuration | Pending |
| Remote landing | Final remote-main SHA and post-merge CI | Pending |

## Deletion manifest

No destructive cleanup is authorized by this ledger yet. A candidate may be
removed only after its unique work is dispositioned, recovery is verified, the
current state is re-read, active use and shared storage are checked, and an
independent verifier approves the exact path/ref.

| Identity | Current state digest | Unique work disposition | Recovery | Active-use check | Approval | Deleted |
| --- | --- | --- | --- | --- | --- | --- |
| Pending discovery | — | — | — | — | — | No |

## Completion checkpoint

Not complete. Remaining required work: finish host/remote/PR discovery; inspect
all candidate changes and reviews; integrate accepted work; land through the
permitted workflow; verify final remote `main`; perform the deletion gate; run a
fresh discovery pass; and record retained exceptions plus restoration steps.
