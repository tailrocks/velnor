# Velnor recovery execution record

Binding spec: `velnor-recovery-goal.md`. Update at meaningful milestones.

## Live snapshot (2026-09-15 UTC+7)

| Item | Value |
|---|---|
| **main** | `ad5fc59d` (2 commits ahead of PR bases) |
| **PR #809** head | `fb9a31a6` branch `codex/merge-all-velnor-20260914` — OPEN, BLOCKED |
| **PR #812** head | `21822888` branch `codex/velnor-macos-action-portability-20260915` — OPEN, BLOCKED |
| **Ancestry** | #812 = #809 + 1 commit (`fix(action): make setup runtime portable`) |
| **Local recovery branch** | `recovery/pr812` @ `21822888` + generator scan fix (unpushed) |
| **Sentry artifact** | `velnor-runner 0.1.274~preview.145+d3e441f` @ `d3e441fb` |

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

**Fix applied locally:** regenerate `.github/ci/.github-actions-generator-state` scan hash → `861def8a99c861c2` (commit pending push).

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
| 2 | Fix #812 generator scan drift | agent | **DONE local** | commit on `recovery/pr812` |
| 3 | Push #812 fix + rebase onto main | agent | **NEXT** | PR head SHA change |
| 4 | Local macOS bootstrap host (break circular dep) | agent | **IN PROGRESS** | build + daemon |
| 5 | macOS velnorctl + on-demand host CLI | agent | pending | branch `codex/velnorctl-macos-support-20260915` exists |
| 6 | PR-targeted recovery semantics | agent | pending | — |
| 7 | Merge #809 → #812 stack with green checks | agent | pending | — |
| 8 | Main verify + release + artifact install | agent | pending | — |
| 9 | Sentry deploy + rollback prep | agent | pending | — |
| 10 | Scenarios A–D evidence matrix | agent | pending | — |

## Decisions

- **Integration order:** #809 then #812 (stacked); prefer landing #812 with #809 content intact.
- **Recovery first:** bootstrap temporary macOS host from source before Sentry repair; do not wait for release.
- **Pre-merge candidate:** build from pinned PR branch SHA; replace with merged release before Sentry deploy.

## Blockers

| Blocker | Mitigation |
|---|---|
| No Velnor runners for PR CI | Local macOS temporary host + Sentry tailrocks pool repair |
| macOS velnorctl blocked (`/run/velnor`, socket groups) | Implement dev-host adapters (see `codex/velnorctl-macos-support-20260915`) |
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

1. Push generator fix to `codex/velnor-macos-action-portability-20260915`.
2. Rebase #812 onto `main`; re-run GitHub lane to confirm generator check passes.
3. Build `velnor-runner` + `velnorctl` from recovery branch on macOS; start repo-scoped daemon.
4. Cherry-pick/integrate macOS velnorctl branch; implement `host start` entry point.
5. Execute real Velnor-lane job on temporary host; then repair Sentry tailrocks pool.
