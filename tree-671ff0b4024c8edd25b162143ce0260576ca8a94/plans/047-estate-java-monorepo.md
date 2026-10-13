# Plan 047: java-monorepo — inline lane matrix, contract-conformant sccache, timeouts

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` in the velnor repo — unless a reviewer dispatched you
> and told you they maintain the index.
>
> **Drift check (run first)**: in `/Users/donbeave/Projects/ChainArgos/java-monorepo`,
> `git log --oneline -1` — this plan was written against commit `7b3dfdb3`.
> If HEAD moved, diff `.github/workflows/` against the "Current state" excerpts
> below before proceeding; on a mismatch, STOP.

## Status

- **Priority**: P1 (Phase 1 reference repo #1)
- **Effort**: M
- **Risk**: MED (production CI of a production monorepo)
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md (matrix expression must be fixture-proven first)
- **Category**: dx / perf
- **Planned at**: velnor repo commit `48b04ad`, target repo commit `7b3dfdb3`, 2026-07-18

## Why this matters

java-monorepo is the production Velnor-default reference (Class A). Its
remaining defects: (1) a `matrix-setup` coordinator job that always burns a
GitHub-hosted `ubuntu-latest` runner even on pure-Velnor runs — a "lying
fourth state"; (2) `SCCACHE_GHA_ENABLED: "true"`, which violates the accepted
strict capability contract (local-only compiler cache; the GHA backend needs a
separate operator approval); (3) zero `timeout-minutes` anywhere — one hung job
parks a shared Velnor slot for GitHub's 6-hour default; (4) two workflows still
name the dispatch input `lane` (singular). Fixing the reference repo first
gives every later migration a correct pattern to copy.

## Current state

Target repo: `/Users/donbeave/Projects/ChainArgos/java-monorepo`, branch `main`.
Workflows: `ansible.yml`, `kestra-build-publish.yml`, `renovate.yml`,
`rust-docker.yml`, `rust.yml`. All actions already SHA-pinned at current majors
(checkout v7, cache v6, mise-action v4.2.0, sccache-action v0.0.10,
setup-mold v1, buildx v4, bake v7.3.0).

`rust.yml` (excerpts, verified 2026-07-18):

```yaml
# rust.yml:28-37 — input is already `lanes`, default velnor (correct)
  workflow_dispatch:
    inputs:
      lanes:
        ...
        type: choice
        default: velnor
        options: [velnor, github, both]

# rust.yml:69-70 — VIOLATES strict contract
  RUSTC_WRAPPER: sccache
  SCCACHE_GHA_ENABLED: "true"

# rust.yml:78-80 — the coordinator job to eliminate
  matrix-setup:
    name: Select runner lane
    runs-on: ubuntu-latest
```

- `renovate.yml` and `kestra-build-publish.yml` use `lane:` (singular) as the
  dispatch input name.
- `grep -rn "timeout-minutes" .github/workflows/` → no matches.
- 3× `Swatinem/rust-cache@...` remain (double-cache with sccache +
  `actions/cache`) — check each usage; remove where sccache + registry cache
  already cover the job.
- 8× `runs-on: ubuntu-latest` (coordinators/single-lane jobs).

Governing law (inline so you need no other file): the **canonical inline
matrix** replaces any coordinator job —

```yaml
strategy:
  fail-fast: false
  matrix:
    config: ${{ fromJSON((github.event_name == 'workflow_dispatch' && inputs.lanes == 'both') && '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true},{"lane":"GitHub","runner":"ubuntu-26.04","writer":false}]' || (github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') && '[{"lane":"GitHub","runner":"ubuntu-26.04","writer":true}]' || '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true}]') }}
runs-on: ${{ matrix.config.runner }}
```

and the **contract-conformant compiler-cache env** is:

```yaml
  SCCACHE_GHA_ENABLED: "false"  # strict contract: local-only; GHA backend needs separate approval
  SCCACHE_CACHE_SIZE: 20G
```

Consequence to preserve in comments: the GitHub lane becomes a cold compiler
baseline; the cargo-registry `actions/cache` step stays (it is approved and
keeps downloads warm). Do not "fix" GitHub-lane slowness by re-enabling GHA.

## Commands you will need

| Purpose | Command (from repo root) | Expected |
|---------|--------------------------|----------|
| Lint workflows | `mise x actionlint@latest -- actionlint` (or `actionlint` if installed) | exit 0 |
| YAML sanity | `python3 -c "import yaml,glob; [yaml.safe_load(open(f)) for f in glob.glob('.github/workflows/*.yml')]"` | exit 0 |
| Grep gates | see Done criteria | |

Dispatch verification (operator-gated — requires `gh` auth + healthy Velnor
fleet): `gh workflow run rust.yml -f lanes=velnor`, then `github`, then `both`.

## Scope

**In scope** (only these):
- `.github/workflows/*.yml` (all five)
- `.github/AGENTS.md` (update lane/ubuntu policy prose if it contradicts the changes)

**Out of scope**:
- Any Rust/Java source, Dockerfiles, `mise.toml`, `rust-toolchain.toml`.
- Any weakening of job coverage to make a lane pass (fixture-is-contract).
- Adding kache (separate runner-side plan 034; sccache stays default).

## Git workflow

- Branch: `velnor-estate-standard`
- Commits: conventional (`ci: ...`), signed off (`git commit -s`)
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Replace `matrix-setup` coordinator with the inline matrix

In `rust.yml`, `rust-docker.yml`, `ansible.yml`, `kestra-build-publish.yml`
(wherever a coordinator job computes lane configs): delete the coordinator job
and paste the canonical inline matrix into every job that consumed its output.
Keep each job's existing `needs:` minus the coordinator. Preserve
`fromJSON(matrix.config.runner)`-style consumption by switching to
`runs-on: ${{ matrix.config.runner }}` (the config already carries a JSON
array for Velnor). Jobs that must run exactly once (aggregate/required-status,
single-writer publishes) keep a single-lane form: gate on
`matrix.config.writer == true` or give them the same expression with only the
writer record.

**Verify**: `grep -rn "matrix-setup" .github/workflows/` → no matches;
actionlint → exit 0.

### Step 2: `lane` → `lanes` in renovate.yml and kestra-build-publish.yml

Rename the dispatch input, update every `inputs.lane` reference, default
`velnor`, options `[velnor, github, both]`.

**Verify**: `grep -rn "inputs\.lane\b\|^\s*lane:" .github/workflows/` → no matches.

### Step 3: Contract-conformant sccache env

In `rust.yml` (and any other workflow exporting `SCCACHE_GHA_ENABLED`):
set `"false"`, add `SCCACHE_CACHE_SIZE: 20G`, update the explanatory comment
(rust.yml:60-70) to describe the local-only contract and the cold GitHub-lane
baseline. Pin `version: v0.16.0` + `disable_annotations: "false"` as the only
`with:` inputs on every `mozilla-actions/sccache-action` step. Remove any
`sccache --show-stats` run steps (reporting belongs to the action post step).

**Verify**: `grep -rn "SCCACHE_GHA_ENABLED" .github/workflows/` → only
`"false"`; `grep -rn "show-stats" .github/workflows/` → no matches.

### Step 4: Remove double-cache

For each of the 3 `Swatinem/rust-cache` steps: if the job also runs sccache +
a cargo-registry `actions/cache` step, delete the Swatinem step. If a job has
ONLY Swatinem (no sccache), replace it with the standard pair (sccache-action
step + registry cache step copied from `rust.yml`'s existing pattern).

**Verify**: `grep -rn "Swatinem" .github/workflows/` → no matches.

### Step 5: `ubuntu-latest` → `ubuntu-26.04`; timeouts

Every remaining `runs-on: ubuntu-latest` becomes `ubuntu-26.04`. Add
`timeout-minutes` to every job: 30 for build/test jobs, 15 for
lint/policy/coordinator-class jobs, 60 for docker publish jobs (adjust upward
only if a comment cites a measured need).

**Verify**: `grep -rn "ubuntu-latest" .github/workflows/` → no matches;
`for f in .github/workflows/*.yml; do python3 - "$f" <<'EOF'
import sys,yaml
d=yaml.safe_load(open(sys.argv[1]))
missing=[n for n,j in (d.get("jobs") or {}).items() if "timeout-minutes" not in j]
sys.exit(1 if missing else 0)
EOF
done` → exit 0 for every file.

### Step 6: Reconcile `.github/AGENTS.md`

Update its lane/OS prose to: inline matrix (no coordinator), `lanes` input
everywhere, `ubuntu-26.04` pin, local-only sccache. Keep it short.

**Verify**: read it; no statement contradicts steps 1–5.

## Test plan

Static: actionlint + YAML parse + the greps above. Dynamic (operator-gated):
dispatch `lanes=velnor`, `lanes=github`, `lanes=both` on `rust.yml` and
`rust-docker.yml`; all green; job sets identical across lanes; on `both`, the
writer-gated publish steps run exactly once. Record cold/warm/no-change
timings per the velnor repo's `VELNOR_PROJECTS_SETUP.md` §2.11 in the PR
description.

## Done criteria

- [ ] actionlint exit 0; YAML parse exit 0
- [ ] Greps of steps 1–5 all clean
- [ ] Every job has `timeout-minutes`
- [ ] Operator dispatch of three lane modes green (or explicitly deferred by operator)
- [ ] No files outside scope modified (`git status`)

## STOP conditions

- The Velnor lane fails with an unsupported-capability or adapter error →
  STOP; report the exact step + error. The fix belongs in the Velnor runner,
  never in this workflow.
- The inline matrix expression is rejected by GitHub's parser → STOP; the
  fixture proof (plan 041) must be revisited; do not invent an alternative.
- A `Swatinem` removal would leave a compiling job with no sccache AND no
  registry cache and you cannot apply the standard pair cleanly → STOP.
- Any job's semantics (what it builds/publishes) is unclear enough that
  renaming/matrixing might change behavior → STOP and describe it.

## Maintenance notes

- This repo is the Class A reference: changes here propagate as the pattern
  for jackin (plan 049) and everything after. Reviewer should scrutinize the
  writer-gating on `both` runs for the docker/kestra publish paths.
- Deferred: `setup-rust-ci` composite extraction (velnor repo §2.8) — do not
  build it here.
