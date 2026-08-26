# Production-readiness P0 PR/branch inventory

Captured 2026-08-27 from GitHub API, repository `tailrocks/velnor`.

## Baseline

- Working tree: clean.
- Campaign branch: `fix/watchdog-registration-deadline`.
- Local HEAD: `c9794f394d13bb85b4320ce5ad6c2df0bf4a3f80`.
- GitHub `main`: `814b41d70b2ccd7c0e66c2236bfeabccd76d1255`.

## Open PRs and commits relative to `main`

| PR | head | status | ahead/behind | unique commits in comparison order |
|---:|---|---|---:|---|
| 409 | `fix/package-velnor-tools-v2` | diverged | 4/1 | `e199a78`, `db38560`, `13c2df2`, `4b2ab39` |
| 406 | `fix/microvm-review-findings` | diverged | 7/1 | `2c5ea66`, `650f815`, `e1c30c7`, `c0fe28e`, `cd998cd`, `d6de052`, `bea0b5b` |
| 405 | `docs/agent-instructions` | diverged | 10/1 | `afd34fa`, `5b8c563`, `4ea9fe9`, `28fbc75`, `4abdb86`, `5427f97`, `d913679`, `83854d9`, `a1c7d7a`, `777b2a2` |
| 404 | `fix/watchdog-registration-deadline` | ahead | 16/0 | `bc2be99`, `f11fc2a`, `494ce08`, `a55998e`, `6fc7699`, `e1c6d6a`, `2dbbbde`, `5f37c9b`, `fc23536`, `fdcb1ec`, `197ace7`, `ae3a1f5`, `ba966cb`, `f186e0e`, `75c55ad`, `c9794f3` |
| 403 | `perf/release-registry-cache` | diverged | 1/1 | `1387637` |

## All remote branches

| branch | compare status | ahead/behind |
|---|---|---:|
| `ci/revalidate-d913` | diverged | 7/1 |
| `ci/revalidate-d913-2` | diverged | 7/1 |
| `ci/revalidate-gh-838` | diverged | 8/1 |
| `ci/revalidate-gh-a1c` | diverged | 9/1 |
| `ci/revalidate-gh-d913` | diverged | 7/1 |
| `docs/agent-instructions` | diverged | 10/1 |
| `feat/microvm-guest-repo-actions` | diverged | 12/3 |
| `fix/408-control-plane-idle-churn` | diverged | 50/1 |
| `fix/microvm-review-findings` | diverged | 7/1 |
| `fix/package-velnor-tools-v2` | diverged | 4/1 |
| `fix/watchdog-registration-deadline` | ahead | 16/0 |
| `main` | identical | 0/0 |
| `perf/release-registry-cache` | diverged | 1/1 |

Source commands: `gh pr list`, `gh pr view`, `gh api repos/tailrocks/velnor/branches --paginate`, and `gh api repos/tailrocks/velnor/compare/main...<branch>`.
