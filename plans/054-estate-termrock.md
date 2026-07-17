# Plan 054: termrock — modernize to Class B (pins, sccache, lanes, no dtolnay)

> **Executor instructions**: Follow step by step; verify each step; STOP
> conditions binding. Update the status row in the velnor repo's
> `plans/README.md` when done.
>
> **Drift check (run first)**: in `/Users/donbeave/Projects/tailrocks/termrock`,
> `git log --oneline -1` — planned against `b7f34da`. On excerpt mismatch, STOP.

## Status

- **Priority**: P3 (Phase 3 #8)
- **Effort**: M
- **Risk**: LOW-MED (Pages deploy)
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md; plans/042-estate-adapter-completion.md (Pages parity on Velnor lane)
- **Category**: dx
- **Planned at**: velnor repo commit `48b04ad`, target `b7f34da`, 2026-07-18

## Why this matters

termrock (Ratatui library + Bun docs + GitHub Pages) is the least compliant
active tailrocks repo: **unpinned floating tags** (`actions/checkout@v7`,
`Swatinem/rust-cache@v2`, `jdx/mise-action@v4`, `dtolnay/rust-toolchain@stable`
— all mutable refs, a supply-chain and Law-2 violation), no sccache, no
lanes, 3× Swatinem, concurrency on 1 of 4 workflows, zero timeouts.

## Current state

Repo: `/Users/donbeave/Projects/tailrocks/termrock`, `main`. Workflows:
`docs.yml hygiene.yml release.yml rust.yml`. Verified 2026-07-18: uses are
tag-floating (7× `actions/checkout@v7`, 6× `Swatinem/rust-cache@v2`,
6× `jdx/mise-action@v4`, 1× `dtolnay/rust-toolchain@stable`,
1× `actions/upload-pages-artifact@v5`, 1× `actions/deploy-pages@v5`,
1× `actions/configure-pages@v6`); 7× `ubuntu-latest`; 2× `fetch-depth: 0`
uncommented; partial mise (`mise+dtolnay` mix); no `mise.lock` check needed —
verify root: repo has partial mise setup only.

Canonical blocks: identical to plan 052 "Current state" (lanes input, inline
matrix with `ubuntu-26.04` GitHub arm, contract env
`SCCACHE_GHA_ENABLED: "false"` / `SCCACHE_CACHE_SIZE: 20G`, setup order
checkout → mold → sccache-action `version: v0.16.0` → mise → registry cache).

Toolchain standard to install here: `rust-toolchain.toml` as the sole channel
source (current stable, `profile = "minimal"`, components rustfmt + clippy);
`mise.toml` with `idiomatic_version_file_enable_tools = ["rust"]` and
`cargo:cargo-nextest`; commit `mise.lock` if the repo uses mise lockfiles
elsewhere in the org (holla does — match it).

## Scope

**In scope**: `.github/workflows/*.yml`, root `mise.toml`,
`rust-toolchain.toml` (create/normalize), `.github/AGENTS.md` (create).
**Out of scope**: Rust sources, docs content, Bun config beyond cache keys.

## Git workflow

Branch `velnor-estate-standard`; `ci:` commits, `git commit -s`; no push
without operator instruction.

## Steps

### Step 1: Pin everything to latest-major SHAs

Every floating tag → latest release, full 40-char SHA + `# v<tag>` comment
(resolution recipe: plan 052 step 1). Delete `dtolnay/rust-toolchain` steps —
replaced by `rust-toolchain.toml` + mise in step 2.

**Verify**: `grep -rhn "uses:" .github/workflows/ | grep -v "@[0-9a-f]\{40\}"` → none;
`grep -rn "dtolnay" .github/workflows/` → none.

### Step 2: Toolchain + standard stack

Create/normalize `rust-toolchain.toml` and `mise.toml` per the Current-state
spec. In `rust.yml` and any compiling job: canonical env + mold +
sccache-action + registry cache; delete all 6 Swatinem steps. Rust jobs run
fmt → clippy → nextest (`cargo nextest run --workspace --locked`; add
`cargo:cargo-nextest` to mise).

**Verify**: `grep -rn "Swatinem" .github/workflows/` → none; actionlint exit 0.

### Step 3: Lanes + Pages writer

Canonical lanes input + inline matrix on `rust.yml`, `docs.yml`,
`hygiene.yml`; `release.yml` matrix + writer gating. Pages: build job runs on
the matrix; `configure-pages`/`upload-pages-artifact`/`deploy-pages` steps
writer-gated (`if: matrix.config.writer`) so `both` deploys once.

**Verify**: actionlint; `grep -n "matrix.config.writer" .github/workflows/docs.yml`
covers the three Pages steps.

### Step 4: Hygiene

`ubuntu-latest` → gone (matrix); concurrency on the 3 uncovered workflows;
`timeout-minutes` everywhere (rust 20, docs 20, release 30); comment or drop
2× `fetch-depth: 0`; `persist-credentials: false`; `.github/AGENTS.md`.

**Verify**: `grep -rn "ubuntu-latest" .github/workflows/` → none; python
timeout check exit 0 per file.

## Test plan

actionlint + YAML parse. Operator: three-lane dispatch on rust.yml +
docs.yml; `both` on docs deploys Pages exactly once. §2.11 timing table;
Class B budget.

## Done criteria

- [ ] All uses SHA-pinned latest-major; zero dtolnay/Swatinem/ubuntu-latest
- [ ] rust-toolchain.toml + mise.toml landed; nextest in CI
- [ ] Lanes + writer-gated Pages; timeouts + concurrency complete
- [ ] Operator dispatches green or deferred; no out-of-scope changes

## STOP conditions

- Velnor lane fails on `configure-pages`/`deploy-pages` (adapter marked
  partial in the estate action table) → STOP; that is runner plan 042's
  deliverable — leave the workflow correct and report.
- The Ratatui MSRV policy conflicts with current-stable toolchain → add a
  separate explicit MSRV check job (matrix-independent), do not pin the main
  toolchain back; if unclear, STOP.

## Maintenance notes

- First repo where Pages parity on Velnor gets real estate exercise —
  coordinate with runner plan 042 verification.
- Renovate: add `renovate.json` mirroring holla's if absent, else SHA pins go
  stale (Law 2 requires the update loop, not just the pin).
