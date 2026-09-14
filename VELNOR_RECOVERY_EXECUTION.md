# Velnor recovery execution record

Binding spec: `velnor-recovery-goal.md`. Update at meaningful milestones.

## Live snapshot (2026-09-15 UTC+7)

| Item | Value |
|---|---|
| **main** | `ad5fc59d6d47f24177c725d7a5c6286388e5e014` |
| **PR #812** | `991d86bd` — `codex/velnor-macos-action-portability-20260915` — portable setup action + generator scan fix |
| **PR #814** | `ce2e783e` (+ local `a249a9e0` daemon fix) — `codex/velnorctl-macos-support-20260915` — macOS velnorctl diagnostics |
| **PR #815** | `781f0e32` (+ local `215358da` daemon fix) — `recovery/pr812` — integration branch (#812 + #814 + docs) |
| **Shared ancestry** | all three branch from merge-all base `fb9a31a6` |
| **Sentry artifact** | `velnor-runner 0.1.274~preview.145+d3e441f` @ `d3e441fb` |

### PR stack relationship

```
fb9a31a6 (merge-all base)
├── #812: 21822888 (portable setup action) → 991d86bd (generator scan fix)
└── #814: ce2e783e (macOS velnorctl diagnostics) → a249a9e0 (dev-host socket fix, local)
    #815 recovery/pr812 = #812 commits + #814 commits + docs + daemon fix
```

## Failure graph (verified)

### PR #809 run [34862656273](https://github.com/tailrocks/velnor/actions/runs/34862656273) @ `fb9a31a6`

| Lane | Result | Root cause |
|---|---|---|
| GitHub (18 jobs) | **PASS** | — |
| Velnor (17 jobs) | **CANCELLED** after ~2h47m | No runner assigned (`runner_id=0`); labels `[self-hosted, velnor-target-mvp]` |
| Policy, DCO | **PASS** | — |

### PR #812 run [34882110261](https://github.com/tailrocks/velnor/actions/runs/34882110261) @ `21822888`

| Lane | Result | Root cause |
|---|---|---|
| GitHub | 17 PASS, **1 FAIL** | `rust-velnor-workflow`: scan input drift `a2f70c15…` → `861def8a…` |
| Velnor (17 jobs) | **QUEUED** | No runner assigned |
| Policy, DCO | **PASS** | — |

**Fix applied:** regenerate scan hash → `861def8a99c861c2` in commit `991d86bd` on #812/#815.

### Sentry (read-only SSH 2026-09-14)

| Pool | registered_slots | routing_valid | Can serve velnor-target-mvp? |
|---|---|---|---|
| `velnor-daemon@tailrocks` (org) | **0** | **false** | **No** — stale DB phases (teardown/recycling), no broker sessions post-upgrade |
| `velnor-daemon@dogfood` (repo) | 4 | true | **Limited** — ~3 idle; degraded; not org-trusted path |

Package upgraded ~15:08 UTC; tailrocks pool never re-registered after restart.

## Dependency-ordered tasks

| # | Task | Owner | Status | Evidence |
|---|---|---|---|---|
| 1 | Live failure graph + execution record | agent | **DONE** | this file |
| 2 | Fix #812 generator scan drift | agent | **DONE** | `991d86bd` on #812/#815 |
| 3 | macOS dev-host daemon socket startup | agent | **DONE local** | `a249a9e0`/#814, `215358da`/#815 |
| 4 | Push #812/#814/#815 branches | agent | **IN PROGRESS** | this turn |
| 5 | Local macOS bootstrap host (break circular dep) | agent | **NEXT** | daemon + real job |
| 6 | macOS velnorctl on-demand host CLI | agent | pending | extend #814 |
| 7 | Merge #812 → #814 → #815 with green checks | agent | pending | — |
| 8 | Main verify + release + artifact install | agent | pending | — |
| 9 | Sentry deploy + rollback prep | agent | pending | — |
| 10 | Scenarios A–D evidence matrix | agent | pending | — |

## Decisions

- **Integration order:** #812 (setup action) → #814 (macOS velnorctl) → #815 (combined recovery); all share merge-all base `fb9a31a6`.
- **Recovery first:** bootstrap temporary macOS host from source before Sentry repair; do not wait for release.
- **Pre-merge candidate:** build from pinned PR branch SHA; replace with merged release before Sentry deploy.

## Blockers

| Blocker | Mitigation |
|---|---|
| No Velnor runners for PR CI | Local macOS temporary host + Sentry tailrocks pool repair |
| macOS velnorctl blocked (`/run/velnor`, socket groups) | **Fixed locally** in `a249a9e0`/`215358da`; push + verify daemon startup |
| #812 behind main by 2 commits | Rebase after generator fix push |
| Sentry tailrocks 0 registered slots | Separate repair track after recovery path proven |

## Reproduction commands

```bash
# Generator drift (PR #812 head)
cd crates/velnor-workflow && cargo run --locked -- --plain --check ../..

# Fix
cargo run --locked -- --plain --force ../.. && cargo run --locked -- --plain --check ../..

# Sentry tailrocks health (redacted)
ssh sentry 'cat /var/lib/velnor-tailrocks/runner/daemons/velnor-tailrocks/health.json'
```

## Next steps

1. Push #812, #814, #815 with latest commits.
2. Verify `velnorctl daemon` starts on macOS; run repo-scoped temporary host.
3. Rebase all three onto current `main` when CI green on GitHub lane.
4. Execute real Velnor-lane job on temporary host; repair Sentry tailrocks pool.
5. Implement `host start` entry point on #814.
