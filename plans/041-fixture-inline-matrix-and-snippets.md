# Plan 041: Fixture — canonical inline matrix, backend-selection jobs, services job, registry sync

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in the velnor repo's
> `plans/README.md` when done. TWO repos are touched: the fixture repo and
> velnor (its enforcement tooling) — keep commits separate per repo.
>
> **Drift check (run first)**: fixture repo
> `/Users/donbeave/Projects/tailrocks/velnor-project/velnor-actions-fixture`
> planned at `c103d01`; velnor at `48b04ad`
> (`git log --oneline -1` in each). On excerpt mismatch, STOP.

## Status

- **Priority**: P0 (V0.10 — HARD GATE for every estate plan 047–057)
- **Effort**: M
- **Risk**: MED (the fixture is the contract; changes must strengthen, never weaken)
- **Depends on**: plans/033 (manifest covers new patterns — soft; can land
  before 033 with a manifest TODO), plans/034 (kache adapter for the backend
  jobs — the `kache` fixture job lands DISABLED/`continue-on-error: false`+
  `if: false` until 034 ships), plans/040 (services parity for the Postgres job)
- **Category**: tests / direction
- **Planned at**: velnor `48b04ad`, fixture `c103d01`, 2026-07-18

## Why this matters

`VELNOR_PROJECTS_SETUP.md` §2.1 canonicalizes an inline lane-matrix
expression and REJECTS the selector-job pattern — but the fixture (the
ground truth every estate migration copies) still runs selector jobs, uses a
different `lanes` input shape (`all/github-only/velnor-only`), sets
`SCCACHE_GHA_ENABLED: "true"`, uses Swatinem, and has explicit
`sccache --show-stats` steps. Until the fixture proves the canonical
patterns on both lanes, no estate PR may merge (gate V-A). This plan also
adds the two new pattern families the estate needs: compiler-cache backend
selection (`off | sccache | kache`) and a `services:` Postgres job — and
fixes known registry drift in the enforcement tooling.

## Current state (verified by the fixture audit, 2026-07-18)

Fixture repo (`velnor-actions-fixture`, HEAD `c103d01`):
- Workflows: `compat, docker, fixture-rust-check (reusable), multi-arch,
  pages, renovate, schedule` + `reuse-caller`. Lane mechanism everywhere:
  `matrix-setup` selector job emitting `configs`;
  `runs-on: ${{ fromJSON(matrix.config.runner) }}`; GitHub runner value
  `"ubuntu-latest"`.
- `lanes` input (compat.yml:11-15 et al.):
  `default: all`, `options: [all, github-only, velnor-only]` — WRONG shape
  vs the standard (`velnor|github|both`, default `velnor`).
- sccache: `SCCACHE_GHA_ENABLED: "true"` (compat.yml:96-98) +
  `sccache --show-stats` into the summary (compat.yml:152-159) — both violate
  the strict contract's local-only + adapter-owned-reporting rules.
- Swatinem/rust-cache with lane-scoped shared-key (compat.yml:116-119 et al.).
- No `services:` anywhere; no kache anywhere.
- Success assertions: `aggregate-needs` composite (scalar + bulk),
  `compare-results.py` cross-lane equivalence, `check-fixture-output`,
  docker-compare — KEEP ALL; they are the fixture's teeth.

Velnor enforcement tooling (`crates/velnor-tools/src/main.rs`):
- `fixture_required_snippets()` — the registry vec at `main.rs:791-1008`;
  entries are `(path, vec![(label, substring)])`. Known DRIFT vs local
  fixture: `main.rs:803` expects `extractions/setup-just@v4` in compat.yml
  (absent — just via mise); `main.rs:813-814` expect `actions/cache@v5` +
  `restore-keys:` in compat.yml (absent — only renovate.yml has them);
  `main.rs:905-906` expect `renovatebot/github-action@v46` (local is
  SHA-pinned so the substring misses).
- `check_fixture_lanes` (`main.rs:4135-4195`) + `check_matrix_parity_job`
  (`main.rs:4198-4282`): accepts either an inline both-lane matrix sequence
  (`:4219-4232`) or the selector-expression form (`:4233-4259`);
  `fixture_parity_workflows()` list at `main.rs:4124-4133`;
  hardcoded-lane-string scan `:4287-4338`.
- Audited remote defaults to `donbeave/velnor-actions-fixture@main`
  (`main.rs:17-18`).

Canonical blocks to install (from `VELNOR_PROJECTS_SETUP.md` §2.1/§9 — the
same text estate plans use): `lanes` input `default: velnor`,
`options: [velnor, github, both]`; the inline `fromJSON(...)` ternary with
`{"lane","runner","writer"}` records, GitHub arm `ubuntu-26.04`; contract
sccache env (`SCCACHE_GHA_ENABLED: "false"`, `SCCACHE_CACHE_SIZE: 20G`).

## Commands you will need

| Purpose | Command (velnor repo root) | Expected |
|---------|---------------------------|----------|
| Snippet audit | `cargo run -p velnor-tools -- fixture-audit --path <local-fixture-checkout>` (check the subcommand's path flag via `--help`; default audits the remote) | exit 0 after sync |
| Lane parity | `cargo run -p velnor-tools -- check-fixture-lanes` (same path note) | exit 0 |
| Workflow lint | `actionlint` (fixture repo root) | exit 0 |
| Gates (velnor) | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |

## Scope

**In scope**: fixture repo `.github/workflows/*` + `README.md`; velnor
`crates/velnor-tools/src/main.rs` (`fixture_required_snippets`,
`fixture_parity_workflows`, `check_matrix_parity_job` inline-form updates).
**Out of scope**: fixture crates (app-a/app-b/shared source); weakening ANY
existing assertion (compare-results, aggregate-needs, check-fixture-output
stay bit-for-bit); velnor adapters (033/034/040 own those).

## Git workflow

Fixture: branch `velnor-estate-standard`; velnor: branch
`velnor-estate-standard`. `ci:`/`feat(tools):` commits, `git commit -s`;
no push without operator instruction (fixture verification REQUIRES pushing
to the fixture repo + dispatching — operator does that step).

## Steps

### Step 1: Migrate lane mechanism in every fixture workflow

For `compat, docker, multi-arch, pages, schedule, renovate, reuse-caller`:
replace `matrix-setup` selector jobs with the canonical inline matrix
(schedule/renovate hardcode both lanes today — their matrix becomes the
two-record `both` form inline, no input); `lanes` input → canonical shape
(default `velnor`); `runs-on: ${{ matrix.config.runner }}` (drop the extra
`fromJSON` — the config record already carries the array); GitHub runner
value → `ubuntu-26.04`; job names keep `(${{ matrix.config.lane }})`.
`fixture-rust-check.yml` (reusable) keeps its `runner` input mechanism —
callers pass records from their inline matrix.
KEEP `packages` input and all assertions.

**Verify**: `actionlint` exit 0 in the fixture repo;
`grep -rn "matrix-setup\|github-only\|ubuntu-latest" .github/workflows/` → none.

### Step 2: Contract-conformant cache surface

`SCCACHE_GHA_ENABLED` → `"false"` everywhere + `SCCACHE_CACHE_SIZE: 20G`;
sccache-action steps pinned `version: v0.16.0`, `disable_annotations:
"false"`; DELETE the `sccache --show-stats` summary steps; DELETE Swatinem
steps, replace with the standard registry `actions/cache` step (latest v6
SHA-pin, keys include `${{ matrix.config.lane }}`) so the cache-adapter
no-op path stays covered.

**Verify**: `grep -rn "SCCACHE_GHA_ENABLED\|Swatinem\|show-stats" .github/workflows/` → only `"false"` hits.

### Step 3: Backend-selection jobs (compat.yml)

Three literal jobs — `cache-off`, `cache-sccache`, `cache-kache` — each with
the inline matrix, each compiling the same small crate (`app-a`), asserting
via a step that the expected wrapper is/はn't active
(`test "${RUSTC_WRAPPER:-}" = sccache` / `= kache` / `-z`). Exactly one
setup action per job (contract); the kache job carries the contract's five
pinned inputs and `if: ${{ false }}  # enable when velnor plan 034 lands`
plus a tracking comment. NO job contains both actions.

**Verify**: actionlint; `python3` YAML walk asserts no job has both
sccache-action and kache-action steps.

### Step 4: `services:` Postgres job (compat.yml)

Job `services-postgres` (inline matrix): Postgres service (pinned major
image, health-cmd `pg_isready`, env, `5432:5432`), step connects via `psql`
(or a tiny cargo test in `shared`) and asserts a round-trip query. This is
plan 040's acceptance artifact.

**Verify**: actionlint; job present with health options.

### Step 5: Sync the velnor registry (velnor repo)

`fixture_required_snippets()` (`main.rs:791-1008`): fix the three known
drifts (setup-just, cache@v5-in-compat, renovate @v46 substring); ADD
required snippets for: the inline matrix ternary (a distinctive substring,
e.g. `inputs.lanes == 'both'`), `"lane":"Velnor"` records with
`ubuntu-26.04`, `SCCACHE_GHA_ENABLED: "false"`, the three backend job names,
`services:`+`pg_isready`. `check_matrix_parity_job`: the inline-sequence
branch (`main.rs:4219-4232`) must accept the ternary-expression form —
extend it to treat a `config:` whose string contains both
`"lane":"Velnor"` and `"lane":"GitHub"` as parity-satisfying; add the new
jobs to `fixture_parity_workflows()` if matrixed. Update tests near
`main.rs:4340+`.

**Verify**: `cargo nextest run -p velnor-tools` green;
`fixture-audit`/`check-fixture-lanes` against the LOCAL migrated fixture
exit 0 (use the path/repo flag; if the subcommands can only audit the
remote, run them after the operator pushes — note it in the status row).

### Step 6: Operator verification (both lanes)

Operator pushes fixture branch, dispatches `compat.yml` with
`lanes=velnor`, `github`, `both` (cancel stale runs first per AGENTS rule).
All green except `cache-kache` (disabled). `compare-results` parity holds.

**Verify**: three run URLs recorded in the PR/status row.

## Test plan

velnor-tools tests updated (step 5); fixture proven live (step 6). The
inline ternary either parses or the whole estate plan set STOPs here — that
is the point of proving it first.

## Done criteria

- [ ] Fixture: canonical input+matrix everywhere; zero selector jobs; zero
      GHA-sccache/Swatinem/stats; backend trio + services job present
- [ ] velnor-tools registry synced; its tests green; audits exit 0
- [ ] Operator three-lane runs green (kache job pending 034)
- [ ] No assertion weakened (compare-results/aggregate-needs untouched —
      `git diff --stat` on `.github/scripts/` + `aggregate-needs` empty)

## STOP conditions

- GitHub's expression parser rejects the inline ternary (multiline or
  one-line) → STOP IMMEDIATELY; this invalidates §2.1 of the standard and
  every estate plan — the operator must re-decide the canonical mechanism.
- The Velnor lane fails any migrated pattern → STOP; runner gap (033/034/040
  territory); never re-weaken the fixture (AGENTS hard rule).
- `fixture-audit` has no local-path mode and the remote default
  (`donbeave/...`) mismatches where the branch can be pushed → note and STOP
  for operator routing.

## Maintenance notes

- The fixture is now the canonical-snippet source; estate plans 047–057 copy
  from it VERBATIM after this lands. Any future standard change edits
  fixture-first (gate V-A).
- Enable `cache-kache` in the same PR that lands plan 034 (single-line `if:`
  flip + registry snippet).
