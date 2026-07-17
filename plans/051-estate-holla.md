# Plan 051: holla — add lanes (cleanest repo; smallest diff)

> **Executor instructions**: Follow step by step; verify each step; STOP
> conditions binding. Update the status row in the velnor repo's
> `plans/README.md` when done.
>
> **Drift check (run first)**: in `/Users/donbeave/Projects/tailrocks/holla-project/holla`,
> `git log --oneline -1` — planned against `376419a`. On mismatch with the
> excerpts, STOP.

## Status

- **Priority**: P2 (Phase 2 #5)
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md
- **Category**: dx
- **Planned at**: velnor repo commit `48b04ad`, target `376419a`, 2026-07-18

## Why this matters

holla is the cleanest estate repo — already `ubuntu-26.04`, mise, sccache,
mold, `CARGO_INCREMENTAL=0`, SHA-pinned current majors. It lacks only the
lane machinery, timeouts, and the contract sccache flip. Smallest possible
migration diff; good early Phase-2 confidence builder.

## Current state

Repo: `/Users/donbeave/Projects/tailrocks/holla-project/holla`, `main`.
Workflows: `ci.yml preview.yml release-deb.yml release.yml renovate.yml`.
Verified: 12× `runs-on: ubuntu-26.04` already; 3/5 workflows have
concurrency; zero `timeout-minutes`; 2× `fetch-depth: 0` (uncommented);
`ci.yml:26-36` runs sccache-action with `continue-on-error: true` and a
follow-up step writing `SCCACHE_GHA_ENABLED=true` + `RUSTC_WRAPPER=sccache`
into `$GITHUB_ENV`; `ci.yml:55-63` adds a **target-dir actions/cache**
(`path: target`) on top of sccache — double-cache; `ci.yml:69-70` has an
explicit `sccache stats` step.

Canonical blocks (inline; these exact texts):

```yaml
# dispatch input
      lanes:
        description: velnor (default) | github | both
        type: choice
        default: velnor
        options: [velnor, github, both]

# inline matrix (single line in real YAML) + runs-on
strategy:
  fail-fast: false
  matrix:
    config: ${{ fromJSON((github.event_name == 'workflow_dispatch' && inputs.lanes == 'both') && '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true},{"lane":"GitHub","runner":"ubuntu-26.04","writer":false}]' || (github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') && '[{"lane":"GitHub","runner":"ubuntu-26.04","writer":true}]' || '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true}]') }}
runs-on: ${{ matrix.config.runner }}

# contract compiler-cache env
  RUSTC_WRAPPER: sccache
  SCCACHE_GHA_ENABLED: "false"
  SCCACHE_CACHE_SIZE: 20G
```

## Scope

**In scope**: `.github/workflows/*.yml`, new `.github/AGENTS.md`.
**Out of scope**: sources, `mise.toml`, packaging configs; release artifact
set (zigbuild targets stay exactly as-is).

## Git workflow

Branch `velnor-estate-standard`; `ci:` commits, `git commit -s`; no push
without operator instruction.

## Steps

### Step 1: Lanes on ci.yml + preview.yml

Add `lanes` dispatch input (default velnor); canonical inline matrix on every
job; `runs-on: ${{ matrix.config.runner }}`; lane suffix in job names.

**Verify**: `actionlint` exit 0; `grep -n "workflow_dispatch" -A8 .github/workflows/ci.yml` shows the input.

### Step 2: Contract sccache + de-double-cache

In `ci.yml`: drop the `continue-on-error` + `$GITHUB_ENV` enable dance;
plain sccache-action step (`version: v0.16.0`, `disable_annotations:
"false"`), workflow env `RUSTC_WRAPPER: sccache`, `SCCACHE_GHA_ENABLED:
"false"`, `SCCACHE_CACHE_SIZE: 20G`. Delete the `Cache Cargo target` step
(ci.yml:55-63) and the `sccache stats` step (ci.yml:69+). Keep the registry
cache step. Apply the same env to other compiling workflows.

**Verify**: `grep -rn "SCCACHE_GHA_ENABLED\|show-stats\|path: target" .github/workflows/` → only `"false"` hits, no stats, no target cache.

### Step 3: Writer gating on releases

`release.yml`/`release-deb.yml`: inline matrix + `if: matrix.config.writer`
on publish/upload steps (deb, Homebrew, GitHub release assets). Builds run on
both lanes under `both`.

**Verify**: `grep -n "matrix.config.writer" .github/workflows/release*.yml` covers every publish step.

### Step 4: Hygiene

`timeout-minutes` everywhere (ci 20, preview 20, releases 45, renovate 30);
concurrency on the 2 uncovered workflows; comment or drop the 2×
`fetch-depth: 0`; `persist-credentials: false` on non-pushing checkouts.
Write `.github/AGENTS.md` (three lanes + 26.04).

**Verify**: python timeout check per file exit 0; every remaining
`fetch-depth: 0` commented.

## Test plan

actionlint + YAML parse. Operator: three-lane dispatch on ci.yml; `both` on
release-deb with a dry tag → exactly one writer. §2.11 timing table in PR;
no-change rerun ≤ 90 s (Class B) on Velnor.

## Done criteria

- [ ] actionlint exit 0; step greps clean
- [ ] Every job timeout; 5/5 concurrency; lanes on all workload workflows
- [ ] Operator dispatches green or deferred
- [ ] No out-of-scope changes

## STOP conditions

- Velnor lane capability failure → STOP, report; runner-side fix.
- Homebrew/deb publish steps not cleanly writer-gatable → STOP, describe.

## Maintenance notes

- holla is the Class B template donor: after this lands, plans 052/053/054
  copy its shape. Reviewer: check the sccache flip didn't lose the
  `continue-on-error` resilience purpose (the standard replaces it with a
  hard dependency — if sccache-action download flakes on GitHub lane, that
  is an upstream availability incident, not a reason to soften the standard).
