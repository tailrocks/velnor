# Plan 053: parallax — full lanes, collapse 58 cache steps, drop macOS, mold

> **Executor instructions**: Follow step by step; verify each step; STOP
> conditions binding. Update the status row in the velnor repo's
> `plans/README.md` when done.
>
> **Drift check (run first)**: in
> `/Users/donbeave/Projects/tailrocks/parallax-project/parallax`,
> `git log --oneline -1` — planned against `95e83bb9`. On excerpt mismatch, STOP.

## Status

- **Priority**: P2 (Phase 2 #7 — biggest Class B repo)
- **Effort**: L
- **Risk**: MED (release signing/cosign/syft; six workflows)
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md; plans/051-estate-holla.md (Class B pattern donor)
- **Category**: dx / perf
- **Planned at**: velnor repo commit `48b04ad`, target `95e83bb9`, 2026-07-18

## Why this matters

parallax (OTel fan-out product, Rust + Bun UI) has good scripts and sccache
but: **58** `actions/cache` steps (largest cache sprawl in the estate — save
time and GH quota churn), no lanes, a macOS job (forbidden in the standard),
29× `ubuntu-latest`, no mold, action majors behind latest (checkout v6,
cache v5, mise-action v4.1.0).

## Current state

Repo: `/Users/donbeave/Projects/tailrocks/parallax-project/parallax`, `main`.
Workflows: `ci.yml dependency-discovery.yml preview.yml release.yml
scheduled-measurement.yml storage-integration.yml`. Composites:
`.github/actions/{aggregate-needs,setup-macos-sdk,sign-and-attest-archive}`.
Verified 2026-07-18: 58× `actions/cache@27d5ce7f # v5.0.5`; 31× checkout
`@df4cb1c0 # v6.0.3`; 26× mise-action `# v4.1.0`; 11× sccache-action
v0.0.10; 29× `ubuntu-latest`, 3× `${{ matrix.os }}`, 1× `macos-15`;
concurrency 5/6 files; timeout-minutes in 4 files; 3× `fetch-depth: 0`
uncommented, 1× `fetch-depth: 3`; no mold; `CARGO_INCREMENTAL: "0"` present.

Canonical blocks: identical to plan 052 "Current state" (lanes input, inline
matrix with `ubuntu-26.04` GitHub arm, contract env with
`SCCACHE_GHA_ENABLED: "false"` + `SCCACHE_CACHE_SIZE: 20G`, setup order
checkout → mold → sccache-action v0.16.0 → mise → registry cache). Bun jobs
cache by lockfile: one `actions/cache` on Bun's cache dir keyed
`bun-<lane>-<os>-hashFiles('**/bun.lock*')` — not `node_modules`.

## Scope

**In scope**: `.github/workflows/*.yml`; `.github/actions/aggregate-needs`
(only if matrixing forces its inputs); DELETE `.github/actions/setup-macos-sdk`
with its last consumer; new `.github/AGENTS.md`.
**Out of scope**: `sign-and-attest-archive` internals (attestation logic
untouched — writer-gate its invocation only); Rust/Bun sources; `scripts/`
contents (they stay the thin CI entrypoints).

## Git workflow

Branch `velnor-estate-standard`; `ci:` commits, `git commit -s`; no push
without operator instruction.

## Steps

### Step 1: Bump majors (Law 2)

checkout → latest v7 SHA; cache → latest v6 SHA; mise-action → latest v4.2.x
SHA (resolution recipe: plan 052 step 1). All six workflows.

**Verify**: `grep -rhn "uses:" .github/workflows/ | grep -v "@[0-9a-f]\{40\}"`
→ only local `./.github/actions/*` entries.

### Step 2: Lanes + matrix on ci.yml, preview.yml, storage-integration.yml

Canonical input + inline matrix + lane-suffixed names +
`runs-on: ${{ matrix.config.runner }}`. `scheduled-measurement.yml` and
`dependency-discovery.yml` get the matrix too (schedule arm resolves to
Velnor); their mutating steps (issue/PR creation) writer-gated.

**Verify**: actionlint exit 0; `grep -rn "ubuntu-latest" .github/workflows/` → none.

### Step 3: Kill macOS

Delete the `macos-15` job and `${{ matrix.os }}` matrices that exist only to
include macOS; delete `setup-macos-sdk` composite. If the macOS job produced
a release artifact (check `release.yml` asset list), replace with
`cargo-zigbuild` for the same triple on Ubuntu; if the artifact is
Darwin-only and zigbuild cannot produce it validly, STOP (operator decision
§12.4).

**Verify**: `grep -rn "macos\|setup-macos-sdk" .github/workflows/ .github/actions/` → none (or STOP filed).

### Step 4: Collapse the 58-cache sprawl

Inventory every `actions/cache` step (`grep -B2 -A6 "actions/cache@" .github/workflows/*.yml`).
Keep exactly: one registry cache per compile job (standard paths/key), one Bun
cache per Bun job (lockfile key), scoped tool caches that mise does not
manage (justify each kept step with an inline YAML comment). Delete
target-dir caches, duplicate registry caches, and any cache step whose key
ends in a bare `-` after `hashFiles` (empty-hash class). Add mold +
contract sccache env to every compile job.

**Verify**: `grep -c "actions/cache@" .github/workflows/*.yml` total ≤ 15;
every surviving step has a `# cache:` comment naming its owner; actionlint
exit 0.

### Step 5: Hygiene

timeout-minutes on every job (measure-informed: ci 30, storage-integration
45, release 60, others 20); concurrency on the uncovered 6th file; comment or
drop 3× `fetch-depth: 0` (keep `fetch-depth: 3` only with a comment naming
why 3); `persist-credentials: false` on non-pushing checkouts;
`.github/AGENTS.md`.

**Verify**: python timeout check per file exit 0; remaining fetch-depth
overrides commented.

## Test plan

actionlint + YAML parse. Operator: three-lane dispatch on ci.yml +
storage-integration.yml; `both` release dry-run → single writer on sign/
attest/publish. §2.11 timing table; expect the cache collapse to CUT GitHub
lane save/restore time — record before/after totals.

## Done criteria

- [ ] actionlint exit 0; pins all latest-major SHA
- [ ] Lanes everywhere; zero macOS; zero ubuntu-latest; cache steps ≤ 15, all owned
- [ ] mold + contract env on compile jobs; timeouts everywhere
- [ ] Operator dispatches green or deferred; no out-of-scope changes

## STOP conditions

- macOS artifact with no valid zigbuild equivalent (step 3).
- Velnor lane capability failure (cosign-installer, attest steps are the
  likely candidates — runner plan 042 owns those adapters) → STOP + report.
- A deleted cache step turns out to feed a script that reads the cached path
  explicitly (grep scripts/ for the path first) → restore that one step with
  an owner comment, note it, continue.

## Maintenance notes

- Largest cache-behavior change in the estate: reviewer must compare cold vs
  warm GitHub-lane times before/after; if warm regressed >20%, re-add the
  specific measured cache, not the sprawl.
- attest-build-provenance / cosign on Velnor lane depends on runner plan 042;
  until it lands, those steps pass only on the GitHub writer lane — leave
  writer-gated.
