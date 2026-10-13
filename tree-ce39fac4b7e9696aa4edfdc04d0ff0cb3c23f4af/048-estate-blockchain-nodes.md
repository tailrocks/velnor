# Plan 048: blockchain-nodes — `lanes` input, inline matrix, pinned Ubuntu, cache-key fix

> **Executor instructions**: Follow this plan step by step. Run every
> verification command before moving on. On any STOP condition, stop and
> report. When done, update this plan's status row in `plans/README.md` in the
> velnor repo.
>
> **Drift check (run first)**: in `/Users/donbeave/Projects/ChainArgos/blockchain-nodes`,
> `git log --oneline -1` — planned against commit `d156fe4`. If HEAD moved,
> re-verify the excerpts below; on mismatch, STOP.

## Status

- **Priority**: P1 (Phase 1 reference repo #2)
- **Effort**: S/M
- **Risk**: MED (publishes Docker images)
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md
- **Category**: dx / perf
- **Planned at**: velnor repo commit `48b04ad`, target repo commit `d156fe4`, 2026-07-18

## Why this matters

blockchain-nodes is the Docker image factory (Class C) and already defaults to
Velnor, but: the dispatch input is `lane` (estate standard is `lanes`); a
`matrix-setup` job always runs on GitHub `ubuntu-latest` even for pure-Velnor
runs; and the rust-script cache key uses `hashFiles('**/*.rs')` — a pattern
class that has already produced empty-hash key collapse in this estate
(first save wins, stale forever). The sweep job is also genuinely useful work
(Docker Hub sweep), so unlike java-monorepo's selector it is NOT deleted —
only its lane-selection duty moves into the inline matrix.

## Current state

Target repo: `/Users/donbeave/Projects/ChainArgos/blockchain-nodes`, `main`.
Workflows: `build-publish.yml`, `renovate.yml`. Pins current (checkout v7,
cache v6, mise-action v4.2.0). No sccache (rust-script primary — acceptable,
document). `timeout-minutes` only in build-publish's matrix-setup (15).

`build-publish.yml` excerpts (verified 2026-07-18):

```yaml
# :22-28 — input named `lane`, default velnor
  workflow_dispatch:
    inputs:
      lane:
        type: choice
        default: velnor
        options: [velnor, github, both]

# :44-66 — matrix-setup computes BOTH lane configs and missing packages
  matrix-setup:
    name: Select runner lane and missing packages
    runs-on: ubuntu-latest
    timeout-minutes: 15
    ...
      - id: set
        env:
          LANE: ${{ github.event_name == 'workflow_dispatch' && inputs.lane || 'velnor' }}
        run: |
          github='{"lane":"GitHub","runner":"\"ubuntu-latest\""}'
          velnor='{"lane":"Velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]"}'
          ...

# :72-78 — the fragile cache key
      - name: Cache rust-script
        uses: actions/cache@55cc8345... # v6
        with:
          path: ~/.cache/rust-script
          key: rust-script-sweep-${{ runner.os }}-${{ hashFiles('**/*.rs') }}
```

Canonical inline matrix to paste (identical text in every consuming job):

```yaml
strategy:
  fail-fast: false
  matrix:
    config: ${{ fromJSON((github.event_name == 'workflow_dispatch' && inputs.lanes == 'both') && '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true},{"lane":"GitHub","runner":"ubuntu-26.04","writer":false}]' || (github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') && '[{"lane":"GitHub","runner":"ubuntu-26.04","writer":true}]' || '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true}]') }}
runs-on: ${{ matrix.config.runner }}
```

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Lint workflows | `actionlint` | exit 0 |
| YAML parse | `python3 -c "import yaml,glob; [yaml.safe_load(open(f)) for f in glob.glob('.github/workflows/*.yml')]"` | exit 0 |

Operator-gated: `gh workflow run build-publish.yml -f lanes=velnor|github|both`.

## Scope

**In scope**: `.github/workflows/build-publish.yml`,
`.github/workflows/renovate.yml`, `.github/AGENTS.md` (create if absent:
three lanes + ubuntu-26.04 + rust-script-primary toolchain note).

**Out of scope**: `docker-*/` Dockerfiles, `scripts/` (incl. docker-build.rs),
`mise.toml`. The registry buildcache mechanism (`BUILDX_CACHE: registry`) —
already correct, leave it.

## Git workflow

Branch `velnor-estate-standard`; conventional `ci:` commits with
`git commit -s`; no push/PR without operator instruction.

## Steps

### Step 1: `lane` → `lanes` in both workflows

Rename input + every `inputs.lane` reference. Default `velnor` unchanged.

**Verify**: `grep -rn "inputs\.lane\b\|^\s*lane:" .github/workflows/` → none.

### Step 2: Inline matrix; demote matrix-setup to sweep-only

- Rename `matrix-setup` → `sweep` (`name: Sweep Docker Hub for missing packages`).
  It keeps checkout, mise, rust-script cache, sweep step, and the `packages`
  output. Delete the `set` lane-computation step and the `configs` output.
- Give `sweep` the same canonical inline matrix (so on a pure-Velnor run it
  runs on Velnor, not on a GitHub coordinator). It is read-only — no writer
  gating needed; on `both` it runs on both lanes, which is acceptable
  duplicate read-only work; if the operator objects, gate the job with
  `if: matrix.config.writer` instead — note the choice in the PR.
- Every downstream job (`base`, `build`, `packages` matrix) replaces
  `needs.matrix-setup.outputs.configs` consumption with the canonical inline
  matrix, keeps `needs: [sweep]` only for the `packages` output, and switches
  to `runs-on: ${{ matrix.config.runner }}`. GitHub runner value changes
  `ubuntu-latest` → `ubuntu-26.04` everywhere (also in renovate.yml).

**Verify**: `grep -n "configs" .github/workflows/build-publish.yml` → none;
`grep -rn "ubuntu-latest" .github/workflows/` → none; actionlint exit 0.

### Step 3: Fix the rust-script cache key class

Replace `hashFiles('**/*.rs')` with an explicit, always-non-empty source set,
e.g. `hashFiles('scripts/**/*.rs', 'mise.toml')` (confirm the actual
rust-script sources live under `scripts/` — `ls scripts/*.rs`; if they live
elsewhere, use that path). Guard the empty-hash class: append a literal
version suffix so the key never ends in a bare dash:
`key: rust-script-sweep-${{ runner.os }}-v1-${{ hashFiles('scripts/**/*.rs') }}`.

**Verify**: `grep -n "hashFiles" .github/workflows/build-publish.yml` shows
the scoped pattern; actionlint exit 0.

### Step 4: Timeouts + writer gating

`timeout-minutes`: sweep 15 (keep), base/build 45, package matrix jobs 60,
renovate 30. Confirm publish/push steps in `base`/`build`/`packages` are
writer-gated on `both` (`if: matrix.config.writer` on login/push steps —
non-writer lane builds but does not push).

**Verify**: python timeout check (as in plan 047 step 5) exit 0 per file;
`grep -n "matrix.config.writer" .github/workflows/build-publish.yml` shows
gating on every `docker/login` / push step.

## Test plan

Static gates above. Operator: dispatch all three lane modes; on `both`,
verify exactly one lane pushed images (check Docker Hub tags timestamps) and
the GitHub lane completed build-only. Record §2.11 timings (velnor repo
`VELNOR_PROJECTS_SETUP.md`) in the PR description; warm Velnor bake budget
≤ 90 s per image job.

## Done criteria

- [ ] actionlint + YAML parse exit 0; greps of steps 1–4 clean
- [ ] `lanes` everywhere; no `ubuntu-latest`; no bare-dash cache keys
- [ ] Writer gating verified on push paths
- [ ] Operator three-lane dispatch green (or deferred by operator)
- [ ] No out-of-scope files modified

## STOP conditions

- Velnor lane fails on capability/adapter error → STOP, report exact error;
  fix belongs in the runner.
- The `packages` matrix combination with the lane matrix produces a matrix
  GitHub rejects (nested fromJSON limits) → STOP; this needs the fixture
  (plan 041) to prove a combined shape first.
- rust-script sources are not where step 3 assumes → adjust path only if
  unambiguous; otherwise STOP.

## Maintenance notes

- Sweep-on-both duplicates Docker Hub API reads on parity runs — harmless but
  worth revisiting if rate limits appear.
- Deferred: sccache for rust-script compilation (rust-script has its own
  cache; measure before adding layers) and `docker/setup-qemu-action`
  adoption — only add if multi-arch builds regress.
