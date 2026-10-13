# Plan 055: Library trio (schemalane, pg-bigdecimal, tracing-request-level) — Class D rebuild

> **Executor instructions**: Follow step by step per repo; verify each step;
> STOP conditions binding. Update the status row in the velnor repo's
> `plans/README.md` when done (one row covers all three repos; note partial
> completion per repo in the row).
>
> **Drift check (run first)**: `git log --oneline -1` in each repo —
> planned against schemalane `4cbd180`, pg-bigdecimal `4ec9df0`,
> tracing-request-level `aeaca70`. On excerpt mismatch for a repo, STOP for
> that repo only.

## Status

- **Priority**: P3 (Phase 3 #9–#11)
- **Effort**: M (S per repo; three repos)
- **Risk**: LOW (libraries; crates.io release workflows exist — writer care)
- **Depends on**: plans/041-fixture-inline-matrix-and-snippets.md; schemalane's integration job additionally on plans/040-services-parity.md
- **Category**: dx
- **Planned at**: velnor repo commit `48b04ad`, targets `4cbd180`/`4ec9df0`/`aeaca70`, 2026-07-18

## Why this matters

The three tailrocks libraries share one obsolete shape: `dtolnay/rust-toolchain`
+ `Swatinem/rust-cache`, `ubuntu-latest`, **zero** concurrency, zero
timeouts, no mise/sccache/lanes, and (pg-bigdecimal, tracing-request-level)
**unpinned floating tags** (`@v4`, `@v2`, `@stable`). schemalane even runs
`cargo install cargo-audit --locked` inside CI — compiling a tool on every
run. This plan replaces all three CI setups with the canonical Class D
template — identical files, minimal per-repo deltas.

## Current state

Paths and per-repo facts (verified 2026-07-18):

| Repo | Path | Facts |
|------|------|-------|
| schemalane | `/Users/donbeave/Projects/tailrocks/schemalane` | `ci.yml` (3 jobs: rust, integration (Postgres via `cargo nextest run --test postgres_integration --run-ignored all`), audit with `cargo install cargo-audit`), `release.yml`; SHA-pinned but old majors (checkout **v4**); `runs-on: ubuntu-latest`; scripts/check-agent-instructions.sh step; NO mise/rust-toolchain/renovate files |
| pg-bigdecimal | `/Users/donbeave/Projects/tailrocks/pg-bigdecimal` | `ci.yml`, `release.yml`; floating `actions/checkout@v4`, `Swatinem/rust-cache@v2`, `dtolnay/rust-toolchain@stable`; no root config files |
| tracing-request-level | `/Users/donbeave/Projects/tailrocks/tracing-request-level` | same shape as pg-bigdecimal |

schemalane `ci.yml:17-42` excerpt (the shape being replaced — other two are
smaller versions of the same):

```yaml
jobs:
  rust:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@34e11487... # v4
      - uses: dtolnay/rust-toolchain@4be7066a... # stable
        with: { components: rustfmt, clippy }
      - uses: Swatinem/rust-cache@42dc69e1... # v2
      ...
      - name: Test
        run: cargo nextest run --workspace --locked --all-features
```

## The Class D template (write this; pin SHAs at PR time via plan 052 step-1 recipe)

New root files per repo:

- `rust-toolchain.toml`: current stable channel, `profile = "minimal"`,
  `components = ["rustfmt", "clippy"]`.
- `mise.toml`:
  ```toml
  [tools]
  "cargo:cargo-nextest" = "<latest>"
  # schemalane also: "cargo:cargo-audit" = "<latest>"  (kills the cargo-install step)
  [settings]
  idiomatic_version_file_enable_tools = ["rust"]
  ```
- `renovate.json`: copy holla's (`/Users/donbeave/Projects/tailrocks/holla-project/holla/renovate.json`) verbatim.
- `.github/AGENTS.md`: three lanes + ubuntu-26.04 pin, ≤20 lines.

`ci.yml` (complete replacement; one file, all three repos, deltas noted):

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
  workflow_dispatch:
    inputs:
      lanes:
        description: velnor (default) | github | both
        type: choice
        default: velnor
        options: [velnor, github, both]

permissions:
  contents: read

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: "0"
  CARGO_PROFILE_DEV_DEBUG: line-tables-only
  RUSTFLAGS: "-C link-arg=-fuse-ld=mold"
  RUSTC_WRAPPER: sccache
  SCCACHE_GHA_ENABLED: "false"
  SCCACHE_CACHE_SIZE: 20G

jobs:
  rust:
    name: Format, lint, and test (${{ matrix.config.lane }})
    timeout-minutes: 20
    strategy:
      fail-fast: false
      matrix:
        config: ${{ fromJSON((github.event_name == 'workflow_dispatch' && inputs.lanes == 'both') && '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true},{"lane":"GitHub","runner":"ubuntu-26.04","writer":false}]' || (github.event_name == 'workflow_dispatch' && inputs.lanes == 'github') && '[{"lane":"GitHub","runner":"ubuntu-26.04","writer":true}]' || '[{"lane":"Velnor","runner":["self-hosted","velnor-target-mvp"],"writer":true}]') }}
    runs-on: ${{ matrix.config.runner }}
    steps:
      - uses: actions/checkout@<pin> # v7  (default shallow; keep)
        with:
          persist-credentials: false
      - uses: rui314/setup-mold@<pin> # v1
      - uses: mozilla-actions/sccache-action@<pin> # v0.0.10
        with:
          version: v0.16.0
          disable_annotations: "false"
      - uses: jdx/mise-action@<pin> # v4
        with:
          github_token: ${{ secrets.GITHUB_TOKEN }}
      - name: Cache Cargo registry
        uses: actions/cache@<pin> # v6
        with:
          path: |
            ~/.cargo/registry/cache
            ~/.cargo/registry/index
            ~/.cargo/registry/src
            ~/.cargo/git/db
          key: cargo-registry-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: |
            cargo-registry-${{ matrix.config.lane }}-${{ runner.os }}-
      - name: Format
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --workspace --locked --all-targets --all-features -- -D warnings
      - name: Test
        run: cargo nextest run --workspace --locked --all-features
      - name: Package
        run: cargo package --workspace --locked
      # Cache reporting is owned by the sccache action / Velnor adapter post
      # step — no ad-hoc stats step.
```

Per-repo deltas:

- **schemalane** adds two jobs:
  - `integration` — same matrix/timeout(30)/setup, plus a `services:` Postgres
    (`services: { postgres: { image: postgres:<latest-major>, env: POSTGRES_PASSWORD/DB, ports: [5432:5432], options: health-cmd pg_isready ... } }`),
    then the existing command
    `cargo nextest run -p schemalane-core --locked --test postgres_integration --run-ignored all`
    with `DATABASE_URL` env pointing at the service. Inspect
    `crates/*/tests/postgres_integration*` first to learn the URL env var it
    reads; if it spawns its own container (testcontainers) instead of reading
    a URL, keep it as-is WITHOUT `services:` and note that the Velnor lane
    then requires the trusted pool's Docker socket.
  - `audit` — mise-installed `cargo-audit` (`cargo audit`), schedule + lockfile
    paths filter, timeout 15. Keep the existing
    `scripts/check-agent-instructions.sh` step in the `rust` job.
- **release.yml** (all three): keep the existing publish trigger; add the
  canonical matrix + `if: matrix.config.writer` on the `cargo publish` step;
  `timeout-minutes: 30`; pins bumped to latest majors.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Lint workflows | `actionlint` | exit 0 |
| Local gate | `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo nextest run --workspace --locked` | exit 0 |

## Scope

**In scope** per repo: `.github/workflows/ci.yml`, `.github/workflows/release.yml`,
new `rust-toolchain.toml`, `mise.toml`, `renovate.json`, `.github/AGENTS.md`.
**Out of scope**: library sources, Cargo.toml metadata, test code
(EXCEPT reading it to determine the Postgres URL env var).

## Git workflow

Branch `velnor-estate-standard` in each repo; `ci:` commits with
`git commit -s`; no push without operator instruction.

## Steps

1. **schemalane**: write the four root files + replace `ci.yml`/patch
   `release.yml` per template + deltas. Verify: actionlint exit 0; local gate
   passes with nextest.
2. **pg-bigdecimal**: same, minus integration/audit jobs. Verify: same gates.
3. **tracing-request-level**: same as 2. Verify: same gates.
4. Cross-check: `grep -rn "dtolnay\|Swatinem\|ubuntu-latest" <repo>/.github/` → none, all three.

## Test plan

Per repo: local cargo gate green pre-PR; operator dispatches three lane modes
on ci.yml; schemalane `both` must run integration green on BOTH lanes (this
is the estate's `services:` acceptance test — velnor plan 040). §2.11 timing
tables; Class D budget: no-change rerun ≤ 60 s on Velnor.

## Done criteria

- [ ] Three repos: template landed, root files present, pins latest-major SHA
- [ ] Zero dtolnay/Swatinem/ubuntu-latest/floating tags across all three
- [ ] schemalane audit job uses mise cargo-audit (no `cargo install`)
- [ ] Operator dispatches green or deferred; no out-of-scope changes

## STOP conditions

- schemalane's Postgres test turns out to use testcontainers (spawns its own
  Docker) AND the operator wants `services:` instead → STOP; that's a test
  refactor outside CI scope.
- Velnor lane fails `services:` startup → STOP; runner plan 040 owns it.
- `cargo nextest` reveals examples that existed only as rustdoc tests
  → migrate them into nextest-discoverable integration/unit tests or compile-UI tests; never drop coverage.

## Maintenance notes

- These three repos are the template's reference instantiation — future
  Class D repos copy from schemalane. Reviewer: byte-diff the three `ci.yml`s;
  every difference must map to a listed delta.
