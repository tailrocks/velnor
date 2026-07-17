# Velnor Projects Setup — Estate CI Standardization Plan

Status: research + implementation plan (2026-07-18)  
Audience: operators + agents preparing one PR per repo to make **Velnor the default runner**, with GitHub and dual-lane modes always available.

This document is the single planning artifact for standardizing CI/CD across the listed repositories. It is **not** a mandate that every repo already implements the standard — it defines the target contract and the migration order.

---

## 1. Goals (non-negotiable)

| # | Requirement | Meaning |
|---|-------------|---------|
| 1 | **Velnor default** | `push`, `pull_request`, `schedule`, and default `workflow_dispatch` run on Velnor (`self-hosted` + `velnor-target-mvp`, or the estate label set below) |
| 2 | **GitHub selectable** | Any workload can run on pinned GitHub-hosted Ubuntu without YAML forks beyond the lane matrix |
| 3 | **Both / parity** | One workflow run can execute the same jobs on Velnor **and** GitHub for correctness + performance comparison |
| 4 | **Ubuntu only for Linux** | GitHub lane is **pinned** Ubuntu (see §3.2). Never `ubuntu-latest` for workload jobs. Velnor jobs run inside Velnor's Ubuntu job image |
| 5 | **mise + rust-toolchain** | All tools install via `jdx/mise-action`; Rust channel/components come from `rust-toolchain.toml` (idiomatic version file) — no `dtolnay/rust-toolchain` in the product path |
| 6 | **One caching model** | Same cargo registry/git keys, sccache wiring, Docker layer policy, mise cache policy — YAML identical across lanes; Velnor host-persistent mounts make cache steps no-ops where appropriate |
| 7 | **Clarity** | Job/step names state purpose; semantic boundaries over shards/batches; CI decisions in Rust tooling where complexity warrants it |
| 8 | **One approach** | No per-repo “special” runner stack. Complexity is layered (library → mid → production), not divergent |

**Policy override note.** Older estate docs sometimes said jackin-family defaults to GitHub and ChainArgos to Velnor. **This plan sets Velnor default for every listed repo.** GitHub remains a first-class comparison lane forever.

---

## 2. Velnor capability baseline (what the runner already is)

Velnor is a GitHub Actions–compatible self-hosted runner (Rust/tokio) that:

- Speaks **V2 only**: JIT config → broker → run-service → Results Service (live logs).
- Runs each job in a fresh Docker container (`velnor/job-ubuntu`, mise + rust + sccache + mold + buildx preinstalled).
- Executes known marketplace actions via **native Rust adapters** (not the JS product path).
- Mounts **host-persistent** stores on the Velnor lane: cargo registry/git, mise installs, sccache, optional per-trust-scope cargo target buckets.
- Treats `actions/cache` / rust-cache / sccache GHA backends as **no-ops** when paths are already host-persistent — workflows stay drop-in.
- Enforces trust scopes (`trusted` vs e.g. `public-forks`): non-trusted pools reject secret-bearing jobs and omit host Docker socket.

Relevant law in-repo: `docs/mission.md`, `docs/master-plan.md` §3a (universal caching), `docs/runner-usage.md`, `docs/perf-instant-cache-plan-2026-06-11.md`, `docs/reference/target-action-registry.md`.

**Hard limit:** Velnor does **not** run macOS/Darwin jobs. Any `macos-*` matrix leg stays on GitHub-hosted forever (or is dropped if not required).

---

## 3. The Standard CI Contract (“Velnor Estate Standard”)

Every listed repository must converge on this shape. Repos differ only in **which jobs exist**, not in **how lanes/tools/caches are declared**.

### 3.1 Lane plumbing (copy-paste contract)

**Input name:** always `lanes` (plural). Prefer not `lane` alone — blockchain-nodes currently uses `lane`; migrate to `lanes` for consistency.

```yaml
on:
  push:
    branches: [main]
  pull_request:
  schedule:           # optional
    - cron: "..."
  workflow_dispatch:
    inputs:
      lanes:
        description: |
          Runner lane(s).
          velnor = default self-hosted Velnor
          github = pinned GitHub-hosted Ubuntu comparison
          both   = same jobs on Velnor and GitHub in one run
        type: choice
        default: velnor
        options: [velnor, github, both]
```

**Matrix-setup job** (metadata only; always GitHub-hosted; ≤5 min timeout):

```yaml
jobs:
  matrix-setup:
    name: Select runner lane
    runs-on: ubuntu-26.04          # coordinator may use pinned GitHub
    timeout-minutes: 5
    outputs:
      configs: ${{ steps.set.outputs.configs }}
    steps:
      - id: set
        env:
          LANES: ${{ github.event_name == 'workflow_dispatch' && inputs.lanes || 'velnor' }}
        run: |
          set -euo pipefail
          github='{"lane":"GitHub","runner":"\"ubuntu-26.04\""}'
          velnor='{"lane":"Velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]"}'
          case "$LANES" in
            github) configs="[$github]" ;;
            both)   configs="[$github,$velnor]" ;;
            *)      configs="[$velnor]" ;;
          esac
          echo "configs=$configs" >> "$GITHUB_OUTPUT"
```

**Workload jobs:**

```yaml
  some-job:
    name: <purpose> (${{ matrix.config.lane }})
    needs: [matrix-setup / …]
    strategy:
      fail-fast: false
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
```

**Rules:**

- Automatic events always resolve to `velnor` (never silently GitHub).
- `workflow_dispatch` honors `inputs.lanes` exactly.
- Job display names include `(${{ matrix.config.lane }})` so GitHub UI shows **Format and lint (Velnor)** vs **(GitHub)**.
- Cache keys include `${{ matrix.config.lane }}` (or a stable lane field) so GitHub and Velnor caches never corrupt each other when both write GHA cache.
- **State-mutating jobs** (Renovate writers, Docker Hub publish, crates.io, Pages deploy): if `both` is offered, the secondary lane is **read-only / dry-run**. One writer only.
- Coordinator / aggregate / required-status glue jobs may stay on pinned GitHub-hosted Ubuntu; they must not claim parity for work they did not run.

### 3.2 Ubuntu / OS policy (from ChainArgos `.github/AGENTS.md`)

Canonical rule (java-monorepo production policy — adopt estate-wide):

| Lane | Runner label | Notes |
|------|--------------|--------|
| Velnor | `["self-hosted","velnor-target-mvp"]` | Job OS is Velnor's Ubuntu job image (keep image current; align with 26.04 direction) |
| GitHub | **`ubuntu-26.04` only** | Never `ubuntu-latest`. Never unpinned `ubuntu-24.04` once estate migrates |
| GitHub ARM (rare) | `ubuntu-26.04-arm` only when an ARM-specific job is intentional | Do not invent other labels |
| macOS | `macos-15` / `macos-26` etc. | **Not Velnor.** Cross-build release legs may stay GitHub macOS, or move to `cargo-zigbuild` on Linux Velnor where possible |

**Repo policy file:** every repo gets `.github/AGENTS.md` (or `.github/workflows/AGENTS.md`) stating the three lanes + Ubuntu pin. Do not restate the full policy inside every workflow.

### 3.3 Toolchain standard

| Layer | Standard |
|-------|----------|
| Tool installs | `jdx/mise-action` only (pin commit SHA + major comment) |
| Versions | `mise.toml` + `mise.lock` committed |
| Rust | `rust-toolchain.toml` + `settings.idiomatic_version_file_enable_tools = ["rust"]` in mise; components rustfmt/clippy listed there |
| Forbidden in product path | `dtolnay/rust-toolchain`, ad-hoc `curl \| sh`, language-specific setup actions for tools mise covers |
| Cargo tools | Prefer `cargo:<crate>` in mise with pinned versions; install `cargo-binstall` first when many cargo: tools need prebuilts |
| Order that matters | When workflow sets `RUSTC_WRAPPER=sccache` and mold flags **before** mise installs cargo tools that compile: setup mold + sccache **before** `jdx/mise-action` (java-monorepo lesson) |
| mise cache write | `cache_save: ${{ github.ref == 'refs/heads/main' }}` — main owns durable tool cache; PRs restore only |

### 3.4 Caching standard (universal mandate)

Everything cacheable must be cached. Rerun of the same commit on a warm fleet must not re-download crates, recompile deps, reinstall tools, or rebuild unchanged Docker layers.

#### 3.4.1 Rust — four layers (same on every Rust-compiling job)

| Layer | GitHub lane | Velnor lane |
|-------|-------------|-------------|
| **1. sccache** | `mozilla-actions/sccache-action` + `RUSTC_WRAPPER=sccache` + `SCCACHE_GHA_ENABLED=true` | Host `_velnor_sccache` mount; adapter no-ops GHA backend |
| **2. Cargo registry/git** | `actions/cache` on `~/.cargo/registry/{cache,index,src}` + `~/.cargo/git/db`, key includes `Cargo.lock` + lane + os | Host `_velnor_cargo` mounts; cache action no-ops |
| **3. Target dir** (optional / measured) | `actions/cache` on `target` **or** Swatinem/rust-cache with shared-key; prefer **not** both blindly | Optional `VELNOR_CARGO_TARGET_PERSIST` buckets under trust scope |
| **4. Result reuse** (advanced, jackin PR #810) | Publish exact crate/job proofs as artifacts; skip producer when input contract matches | Same YAML; Velnor benefits from less work |

**Env defaults for compiling jobs:**

```yaml
env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: "0"
  RUSTFLAGS: "-C link-arg=-fuse-ld=mold"   # Linux only; skip on macOS legs
  RUSTC_WRAPPER: sccache
  SCCACHE_GHA_ENABLED: "true"              # Velnor adapter redirects
```

**Linker:** `rui314/setup-mold` on Linux workload jobs (or image-baked mold on Velnor — still call the action for GitHub parity).

**Do not use** `Swatinem/rust-cache` **and** a full target `actions/cache` **and** sccache without measuring — pick the stack:

- **Estate default (recommended):** sccache + cargo-registry cache + mold. Skip Swatinem unless a small library needs simpler YAML.
- **Library minimal:** may use sccache + registry only; still no `dtolnay/rust-toolchain`.

#### 3.4.2 Docker / Buildx

| Concern | Standard |
|---------|----------|
| Login | `docker/login-action` or equivalent stdin login (secrets only on writer jobs) |
| Builder | `docker/setup-buildx-action`; named builders OK on Velnor host |
| Build | `docker/build-push-action` or `docker/bake-action` with **plain progress** |
| Cache-from/to | Registry `buildcache-*` refs for publish streams; GHA type only for PR-only ephemeral builds |
| Velnor | Prefer persistent local buildkit; adapters may drop redundant `type=gha` export (already designed) |
| Dockerfiles | apt/apk behind `--mount=type=cache`; pin base digests; cargo-chef feature parity between cook and final build |

#### 3.4.3 Other caches

| Content | Key inputs | Notes |
|---------|------------|-------|
| Bun / Node | lockfile hash | `~/.bun/install/cache` or npm cache |
| rust-script | script sources hash | blockchain-nodes pattern |
| Renovate | renovate cache dir | single-writer lane |
| uv / Python | lockfile | ruxel oracle |
| Postgres integration | image pull is enough; no custom cache unless measured |

### 3.5 Workflow clarity standard (inspired by jackin PR #810)

PR [jackin-project/jackin#810](https://github.com/jackin-project/jackin/pull/810) is **not** the target shape for every small crate, but its **principles** are estate law:

1. **Semantic jobs, not shards.** One crate (or one clear purpose) = one complete job. No numbered parts, batches, or “part 1 of 3” transport jobs.
2. **Readable names.** Prefer `Test bitcoin-processor-app (Velnor)` over `job-3`. Prefer step names that state the contract (`Cache Cargo registry`, `Ensure Rust components`, `Clippy affected packages`).
3. **CI decisions in Rust when complex.** Path routing, result reuse, performance audit, archive transport → `*-xtask` binaries with unit tests. Workflow `run` blocks call prepared binaries or a single tool command — not multi-screen Bash programs.
4. **Exact-result reuse** (optional tier): when a job’s input contract (lockfile + sources + toolchain) is unchanged vs a recent green run, skip the producer and validate the preserved proof. Measured on jackin: full CI repeat in ~19s with structural skips.
5. **Performance audit as a gate** (optional for production monorepos): fail if logs show dependency downloads / third-party compiles / cache misses on “should be warm” runs.
6. **Runner-neutral bootstrap.** Composite actions that download a prepared CI binary must not assume host Node/`unzip`/Python differently per lane (PR #810’s download-ci-xtask redesign).

**What not to copy wholesale into small libraries:** full xtask result-reuse, multi-workflow proof graphs, Docs semantic contracts. Those are jackin/parallax-class investments.

### 3.6 Job taxonomy (shared vocabulary)

Use the same conceptual stages so any repo’s Actions tab is navigable:

| Stage | Examples | Runner |
|-------|----------|--------|
| **Select** | Select runner lane | GitHub pinned |
| **Classify** | Detect changes / paths-filter | GitHub pinned (cheap) |
| **Policy / hygiene** | actionlint, gitleaks, zizmor, reuse, agent-instruction checks | Prefer Velnor matrix when non-trivial; pure lint can stay GitHub if sub-minute |
| **Format / lint** | fmt, clippy, deny | Lane matrix |
| **Test** | nextest / cargo test / integration | Lane matrix |
| **Build image** | docker bake/build | Lane matrix (writer rules) |
| **Package / release** | zigbuild, deb, crates.io, GH release | Lane matrix; publish single-writer |
| **Docs / pages** | site build + deploy | Build on matrix; deploy once |
| **Required aggregate** | `ci-required` / `rust-required` that merges results | GitHub OK; must reflect real lane results |

### 3.7 Shared composite actions (recommended productization)

To kill copy-paste drift, introduce a small shared action set (either `tailrocks/velnor-ci-actions` or per-repo `./.github/actions/*` vendored from a template):

| Composite | Purpose |
|-----------|---------|
| `setup-rust-ci` | mold → sccache → mise (install_args) → rustup components verify → registry cache |
| `select-runner-lane` | matrix-setup snippet as reusable composite or reusable workflow |
| `cache-cargo-registry` | jackin-style verified offline fetch (optional for large workspaces) |
| `aggregate-needs` | required status aggregation (already in jackin/parallax) |

**Velnor native adapter implication:** any new composite is shell/YAML, not a new marketplace action — no adapter work unless it wraps unsupported JS.

### 3.8 Pinning & security hygiene

- Pin third-party actions by **full commit SHA** + trailing `# vX` comment.
- Prefer Renovate to bump SHAs.
- Minimal permissions per workflow (`contents: read` default; write only where needed).
- actionlint + (where present) zizmor on workflow changes.
- Secrets: `GITHUB_TOKEN` for same-repo; dedicated tokens for cross-repo / Docker Hub / crates.io.

---

## 4. Per-repository analysis

Local checkouts used for this research live under `/Users/donbeave/Projects/…` as listed below.

### 4.1 Summary matrix

| Repository | Org | Local path | Complexity | Lane today | Default today | mise | Rust toolchain source | Docker | Primary stacks | Gap vs standard |
|------------|-----|------------|------------|------------|---------------|------|----------------------|--------|----------------|-----------------|
| **jackin** | jackin-project | `jackin-project/jackin` | Production / reference | Partial (`ci`, `construct`, `preview`, `release`, `jackin-dev`) | **github** | Yes (rich) | rust-toolchain + mise | Yes (construct) | Rust, Bun, Docker, cosign, Pages | Flip default → velnor; pin ubuntu-26.04; expand lanes to remaining workflows; keep PR #810 ideas |
| **java-monorepo** | ChainArgos | `ChainArgos/java-monorepo` | Production / reference | Full on rust/ansible/docker/kestra | **velnor** | Yes | rust-toolchain + mise | Yes (bake) | Rust, Java/GraalVM, Node, Python, Ansible, Docker | Align GitHub pin to **ubuntu-26.04** (still uses ubuntu-latest in matrix); keep as reference |
| **blockchain-nodes** | ChainArgos | `ChainArgos/blockchain-nodes` | Production Docker factory | Yes | **velnor** | Yes (minimal) | none (rust-script only) | Yes (many images) | mise rust-script, buildx, Docker Hub | Rename `lane`→`lanes`; pin ubuntu-26.04; registry cache already |
| **parallax** | tailrocks | `tailrocks/parallax-project/parallax` | Mid–heavy product | **None** | github only | Yes | rust-toolchain + mise | Limited | Rust, Bun UI, zigbuild, cosign, sccache | Full lane plumbing; Velnor default; macOS legs stay GitHub |
| **parallax-telemetry-playground** | tailrocks | `…/parallax-telemetry-playground` | Small playground | None | github | Yes | mise rust | No | Rust, Bun, Java | Lane + sccache + registry cache |
| **tablerock** | tailrocks | `tailrocks/tablerock` | Early | None | **macos-15 only** | **Missing** | dtolnay | No | cargo-outdated, cargo-deny-action | Rebuild CI on Linux Velnor; add mise; drop macOS-as-default |
| **holla** | tailrocks | `tailrocks/holla-project/holla` | Mid CLI + release | None (mentions only) | ubuntu-26.04 fixed | Yes | rust-toolchain | No | Rust, zigbuild, deb, sccache | Lane matrix; default velnor; already good Ubuntu pin |
| **velnor** | tailrocks | `tailrocks/velnor-project/velnor` | Mid (self) | None | ubuntu-latest | Yes | rust-toolchain | Yes (job image) | Rust, deb, zigbuild, sccache | Must dogfood Velnor; pin 26.04; lane plumbing |
| **ruxel** | tailrocks | `tailrocks/ruxel` | Mid | None | ubuntu-latest | Yes | rust-toolchain | Spec fixtures | Rust, uv, Ansible oracle, sccache | Lane + pin; keep uv/ansible on Linux |
| **termrock** | tailrocks | `tailrocks/termrock` | Mid library + docs | None | ubuntu-latest + macOS matrix | Partial | mix mise + **dtolnay** | No | Rust, Bun docs, Pages | Remove dtolnay; lane; macOS optional on GitHub only |
| **schemalane** | tailrocks | `tailrocks/schemalane` | Library + PG integration | None | ubuntu-latest | **Missing** | **dtolnay** | No | Rust, Postgres tests, crates.io | Full modernize: mise, sccache, lanes, nextest |
| **pg-bigdecimal** | tailrocks | `tailrocks/pg-bigdecimal` | Tiny library | None | ubuntu-latest | **Missing** | **dtolnay** | No | Rust, crates.io | Same library template as schemalane (lighter) |
| **tracing-request-level** | tailrocks | `tailrocks/tracing-request-level` | Tiny library | None | ubuntu-latest | **Missing** | **dtolnay** | No | Rust, crates.io | Same library template |

### 4.2 jackin-project/jackin

**Role:** Pipeline source of truth for sophisticated Rust monorepo CI (construct image, capsule, docs site, preview Homebrew chain).

**What is already excellent:**

- Dual-lane matrix on major workflows; lane field in job names.
- mise-first tooling; rich `mise.toml` / lock.
- Composite actions: `cache-cargo-registry`, `download-ci-xtask`, sccache helpers.
- `.github/workflows/AGENTS.md` policy (toolchain, caches, semantic boundaries).
- PR #810 direction: result reuse, one-crate-one-job, Rust-owned CI, sub-minute warm runs.

**Gaps vs this plan:**

| Gap | Action |
|-----|--------|
| Default `lanes` = **github** | Flip to **velnor** in every selectable workflow + AGENTS.md |
| `ubuntu-24.04` / `ubuntu-latest` mixed | Standardize GitHub workload + coordinator to **ubuntu-26.04** |
| Some workflows lack lanes (hygiene, docs parts, renovate) | Add lanes where work is non-trivial; keep Renovate single-writer on one lane |
| macOS in hygiene/preview | Keep GitHub macOS; never put on Velnor matrix |
| PR #810 not necessarily merged at analysis time | When merged, treat as jackin-internal standard; extract only portable composites for other repos |

**Target workflows:** `ci.yml`, `construct.yml`, `docs.yml`, `hygiene.yml`, `jackin-dev.yml`, `preview.yml`, `release.yml`, `rust-nextest.yml` (+ policy files).

**Velnor fleet needs:** org or repo JIT for `jackin-project`; Docker socket (construct/dind e2e); high slot count; trusted scope; large cargo + buildkit stores.

### 4.3 ChainArgos/java-monorepo

**Role:** First production Velnor default; instant-cache proven; dual-lane reference for multi-package Rust + Docker + Ansible + Kestra.

**What is already excellent:**

- `lanes` input default **velnor**; matrix-setup; job names with lane.
- sccache + mold + mise + registry cache pattern.
- paths-filter per package; per-package test jobs (clear names).
- `.github/AGENTS.md` Ubuntu-26.04 / three-lane policy (canonical text to copy estate-wide).
- Docker bake + registry layer cache; host-persistent caches on Velnor.

**Gaps:**

| Gap | Action |
|-----|--------|
| GitHub matrix still uses `ubuntu-latest` string | Change to `ubuntu-26.04` to match AGENTS.md |
| Step boilerplate repeated per test job | Optional composite `setup-rust-ci` |
| Large bash in clippy package selection | Optional xtask later; not blocking |

**Keep as reference implementation** for production multi-job Rust.

### 4.4 ChainArgos/blockchain-nodes

**Role:** Docker image factory (base → build → many leaf packages); exists-sweep to skip published tags.

**What is already excellent:**

- Velnor default; matrix per lane; clear staged job names (`docker-debian-blockchain-base (Velnor)`).
- Registry buildcache; mise rust-script for exists checks.
- Concurrency designed for long publish streams.

**Gaps:**

| Gap | Action |
|-----|--------|
| Input named `lane` not `lanes` | Rename for estate consistency |
| GitHub runner `ubuntu-latest` | → `ubuntu-26.04` |
| No rust-toolchain (only rust-script) | OK; document as Docker-primary repo |
| QEMU via raw docker run | Prefer `docker/setup-qemu-action` for adapter coverage / clarity |
| matrix-setup does heavy sweep on GitHub | Fine; optionally allow sweep on Velnor for speed later |

**Velnor needs:** trusted Docker socket, large disk, multi-arch buildx, long `TimeoutStopSec` drain (already runner-side).

### 4.5 tailrocks/parallax

**Role:** OpenTelemetry fan-out product; Rust core + Bun UI; multi-target release with cosign/syft; storage integration.

**Current CI traits:**

- Sophisticated change classification via scripts (`changed-paths.sh`, `classify-paths.sh`).
- sccache + cargo registry + bun cache.
- No Velnor lane at all.
- macOS-15 appears in matrix (SDK / native).

**Target:**

1. Add full lane plumbing (default velnor).
2. Pin GitHub to ubuntu-26.04.
3. Linux workload jobs on matrix; macOS jobs: `if: matrix.config.lane == 'GitHub'` **or** separate `macos-*` jobs outside Velnor matrix.
4. Prefer zigbuild on Linux for cross targets where already used — reduce macOS CI dependency over time.
5. Apply jackin clarity rules to job names; keep scripts or migrate hot path to Rust later.

### 4.6 tailrocks/parallax-telemetry-playground

**Role:** Polyglot OTel/Sentry sample (Rust + Java) for Parallax demos.

**Target:** Small-template CI: lanes + mise + sccache + registry cache + fmt/clippy/test; optional Java via mise. No Docker publish required unless added later.

### 4.7 tailrocks/tablerock

**Role:** Early terminal DB workbench (Rust).

**Current CI is non-compliant** with estate goals: macOS-only, dtolnay, no mise, dependency-only workflow.

**Target:** Replace with library/product skeleton:

- `ci.yml`: lane matrix, fmt/clippy/nextest/deny.
- `mise.toml` + `rust-toolchain.toml`.
- Drop macOS as CI host unless a future SwiftUI app needs it (then GitHub-only job).

### 4.8 tailrocks/holla

**Role:** Adaptive local dev-environment CLI; deb + multi-target release.

**Already strong:** ubuntu-26.04, mise, sccache, mold, nextest, zigbuild, cargo-deb.

**Gaps:** no lane matrix; CI always GitHub-hosted.

**Target:** Add matrix-setup + lanes default velnor; release publish remains single-writer; macOS release legs stay GitHub.

### 4.9 tailrocks/velnor

**Role:** The runner itself — must dogfood the standard.

**Gaps:** no lanes; `ubuntu-latest`; CI/release not on Velnor by default (bootstrap chicken-egg is real for runner changes).

**Target policy:**

- Default **velnor** for routine CI once a healthy fleet slot exists for `tailrocks/velnor`.
- Always keep GitHub lane for runner-breaks-fleet recovery.
- `release-deb` / image builds: Velnor preferred (Docker socket), GitHub fallback.
- actionlint + sccache stack already partially present — complete lane + Ubuntu pin.

### 4.10 tailrocks/ruxel

**Role:** Rust Ansible executor; closed-world fixtures; Python uv oracle.

**Already strong:** mise, sccache, registry/target caches, nextest, uv.

**Gaps:** no lanes; ubuntu-latest.

**Target:** lanes + pin; ensure Ansible/uv steps work in Velnor job image (may need image packages or mise tools for ansible). **Velnor image gap:** verify ansible-core / python tooling availability or install via mise/uv every job (cached on host mise store).

### 4.11 tailrocks/termrock

**Role:** Ratatui component library + Bun docs + Pages.

**Gaps:** dtolnay on some jobs; unpinned `jdx/mise-action@v4` / `Swatinem/rust-cache@v2`; macOS matrix; no lanes; no sccache.

**Target:** mise-only toolchain; sccache; lanes; docs/pages build on Linux; deploy once; macOS optional GitHub-only if crossterm platform matrix remains required (or run Linux-only if sufficient).

### 4.12 Library trio: schemalane, pg-bigdecimal, tracing-request-level

**Shared current pattern:** minimal `ci.yml` + `release.yml` with `dtolnay/rust-toolchain` + `Swatinem/rust-cache`, no mise, no sccache, no lanes, old checkout pins in places.

**schemalane extras:** Postgres integration test job; cargo-audit via `cargo install` (should be mise `cargo:cargo-audit`).

**Standard library template (all three):**

```text
ci.yml
  matrix-setup (velnor default)
  rust (lane matrix): checkout → mold → sccache → mise → registry cache
    → fmt → clippy → nextest → package --workspace (if publishable)
  integration (schemalane only): same setup + postgres service or testcontainers
  audit (optional weekly or on lockfile): cargo-audit via mise

release.yml
  lanes optional; publish single-writer on main/tags
  mise + sccache + cargo publish
```

Add `mise.toml`, `rust-toolchain.toml`, `mise.lock`, `.github/AGENTS.md`.

### 4.13 Unique third-party action surface (estate today)

Adapters or node fallback must cover these families (local composite `./.github/actions/*` excluded):

| Action | Used by | Velnor adapter status (high level) |
|--------|---------|-------------------------------------|
| `actions/checkout` | all | native |
| `actions/cache` (+ restore/save) | many | native + persistent no-op |
| `actions/upload-artifact` / `download-artifact` | jackin, parallax, holla, velnor, playground | native |
| `actions/upload-pages-artifact` / `deploy-pages` / `configure-pages` | jackin, termrock | partial / synthetic |
| `actions/attest-build-provenance` | jackin/parallax release | **gap — verify** |
| `jdx/mise-action` | most modern repos | native |
| `mozilla-actions/sccache-action` | java-monorepo, holla, velnor, ruxel, parallax | native |
| `rui314/setup-mold` | java-monorepo, holla, velnor | native |
| `Swatinem/rust-cache` | jackin, termrock, libraries | native |
| `dorny/paths-filter` | jackin, java-monorepo | native |
| `docker/*` (login, buildx, bake) | jackin, java-monorepo, blockchain | native family |
| `crazy-max/ghaction-github-runtime` | jackin | native |
| `renovatebot/github-action` | several | native-ish / image |
| `dtolnay/rust-toolchain` | libraries, termrock, tablerock | **eliminate from workflows** (no new adapter investment) |
| `EmbarkStudios/cargo-deny-action` | tablerock | **prefer mise cargo-deny** instead |
| `fsfe/reuse-action` | jackin | node sidecar today → prefer mise `reuse` like termrock |
| `sigstore/cosign-installer` | (registry; jackin uses mise cosign) | native install path |
| `hadolint/hadolint-action` | agent roles (not this list) | image-baked |

---

## 5. Reference patterns (what to copy from where)

### 5.1 Lane + naming + package jobs → **java-monorepo** `rust.yml`

Best production template for:

- `lanes` default velnor  
- matrix-setup  
- sccache + mold + mise order  
- per-package job names  
- required-check patterns  

### 5.2 Policy prose → **java-monorepo** `.github/AGENTS.md` + **jackin** `.github/workflows/AGENTS.md`

Merge into one estate policy:

- From java-monorepo: three lanes, ubuntu-26.04 pin, why lanes matter, single-writer on `both`.
- From jackin: mise-only tools, main owns caches, semantic boundaries, no Bash CI programs when Rust can own decisions, token scope.

### 5.3 Result reuse / sub-minute CI → **jackin PR #810**

Adopt principles estate-wide; adopt full machinery only where monorepo cost justifies it (jackin, maybe parallax later).

### 5.4 Docker image factory → **blockchain-nodes** `build-publish.yml`

Exists-sweep, staged base/build/packages, registry buildcache, lane matrix on long Docker jobs.

### 5.5 Instant-cache semantics → **Velnor** `docs/perf-instant-cache-plan-2026-06-11.md`

Workflow authors must **not** invent Velnor-only YAML. Same cache steps everywhere; runner makes them free on Velnor.

### 5.6 Small CLI with good Ubuntu hygiene → **holla**

ubuntu-26.04 + sccache + mise + zigbuild/deb — add only lanes.

---

## 6. Standard workflow templates by repo class

### Class A — Production monorepo (jackin, java-monorepo)

- Multiple workflows by concern (`ci`, `docker`, `docs`, `release`, …).
- Full lane matrix on all workload workflows.
- paths-filter / affected routing.
- sccache + registry + docker layer caches.
- Optional result-reuse / performance audit (jackin).
- `.github/AGENTS.md` + workflow AGENTS.

### Class B — Product / CLI with releases (parallax, holla, velnor, ruxel, termrock)

- `ci.yml` + `release.yml` (+ preview/docs as needed).
- Lane matrix on CI and non-publish release builds.
- Publish jobs: single-writer; `both` → secondary dry-run.
- Cross-compile via zigbuild on Linux Velnor when possible.

### Class C — Docker factory (blockchain-nodes)

- Lane matrix on image build jobs.
- Registry caches mandatory.
- Exists-sweep coordinator (GitHub or Velnor).

### Class D — Library crates (schemalane, pg-bigdecimal, tracing-request-level, tablerock early)

- Single `ci.yml` with lane matrix.
- fmt / clippy / test / optional audit.
- `release.yml` crates.io single-writer.
- mise + rust-toolchain mandatory.
- No macOS unless product requires it.

### Class E — Playground (parallax-telemetry-playground)

- Class D + any polyglot tools via mise (java/bun).

---

## 7. Fleet & operations plan (required before flipping defaults)

### 7.1 Labels

| Label | Use |
|-------|-----|
| `self-hosted` | Required by GitHub self-hosted selection |
| `velnor-target-mvp` | Estate default label (already in production workflows) |

Long-term: consider org-scoped labels `velnor` + `linux` + `x64` once multi-host scale-out lands; migration must be atomic across all workflows.

### 7.2 Registration model

Today: **per-repo** JIT daemons (java-monorepo, blockchain-nodes, fixture, …).

For **13 repos across 3 orgs** (`jackin-project`, `ChainArgos`, `tailrocks`):

1. **Near term:** one trusted daemon (or few) per org with **org-level JIT** + runner group restricted to private/internal repos — **Velnor feature priority** (master-plan already lists org-level JIT).
2. **Alternative near term:** repo-level JIT configs generated for each repo sharing host stores but separate identities (ops heavy).
3. **Trust:** all production private repos on `VELNOR_TRUST_SCOPE=trusted` with Docker socket; public forks never share that pool.

### 7.3 Capacity sizing (initial guess — measure after)

| Pool | Repos | Suggested slots (start) | Notes |
|------|-------|-------------------------|-------|
| ChainArgos | java-monorepo, blockchain-nodes | 10 + 4 (current) | Keep separate units if isolation desired |
| jackin-project | jackin | 6–10 | construct + many parallel crate jobs |
| tailrocks | holla, velnor, parallax, ruxel, termrock, libraries… | 8–12 shared | Start shared org pool; split if queue latency high |

Resource caps: keep `VELNOR_JOB_CPUS` / `VELNOR_JOB_MEMORY` daemon defaults; tune per pool.

### 7.4 Job image gaps to bake (or mise-cache)

| Capability | Needed by | Action |
|------------|-----------|--------|
| rust + clippy + rustfmt + mold + sccache + mise | all | already in image |
| protoc | java-monorepo | mise or image |
| GraalVM / node / python | java-monorepo kestra | host mise store seeding |
| zig + cargo-zigbuild | holla, velnor, parallax, jackin, ruxel | mise cargo: + zig |
| bun | jackin, parallax, termrock | mise |
| uv / ansible | ruxel | mise/uv; verify |
| docker buildx / qemu | docker factories | socket + binfmt |
| postgres client / docker postgres | schemalane | service container or testcontainers — **verify Velnor `services:` support** |

---

## 8. Velnor runner development backlog (driven by this estate)

Prioritized by **unblocks standardization** and **correctness**.

### P0 — Must work before mass default-flip

| ID | Item | Why |
|----|------|-----|
| V0.1 | **Org-level JIT + multi-repo fleet** | Cannot ops-scale 13 repo-level daemons cleanly |
| V0.2 | **Stable online fleet** (P1.9 doctor heal, no zombies) | Defaults to Velnor are dangerous if queues hang |
| V0.3 | **Host-persistent caches** remain correct (HOME/CARGO_HOME truthful) | Universal caching mandate |
| V0.4 | **Native adapters green** for estate action set (checkout, cache, mise, sccache, mold, rust-cache, paths-filter, docker family, artifacts) | Node sidecar is not the product path |
| V0.5 | **`services:` / job containers** parity if schemalane (or others) use service containers | Library Postgres tests |
| V0.6 | **Secret-bearing Docker login** on trusted pools only | blockchain-nodes / java-monorepo publish |

### P1 — Needed for parity quality

| ID | Item | Why |
|----|------|-----|
| V1.1 | **actions/attest-build-provenance** native or documented support | jackin/parallax releases |
| V1.2 | **Pages deploy** full parity | jackin/termrock docs |
| V1.3 | **QEMU / multi-arch** reliable path | blockchain-nodes multi-arch |
| V1.4 | **Dynamic slot autoscaling** | Queue latency under load from many repos |
| V1.5 | **Composite local actions** resolution always eager when checkout needed | historical eager-checkout bugs |
| V1.6 | **Job image track ubuntu-26.04** align with GitHub pin | reduce lane drift |
| V1.7 | **Performance markers** (optional): expose sccache/stats consistently in logs | warm-run audits |

### P2 — Superiority / scale

| ID | Item | Why |
|----|------|-----|
| V2.1 | Multi-host scale-out / shared store design | tailrocks+ChainArgos+jackin together |
| V2.2 | Remote shared sccache (jackin deferred kache comparison) | multi-host compiler cache |
| V2.3 | Richer native coverage for REUSE / gitleaks if not mise-installed | fewer node actions |
| V2.4 | Fixture expansion for every new estate action | contract tests |

### Explicit non-goals for Velnor

- macOS job execution.
- Classic runner protocol.
- Supporting `dtolnay/rust-toolchain` as a product path (workflows must migrate to mise).
- Per-repo special YAML that only works on Velnor.

---

## 9. Implementation plan (PR sequence)

Do **not** open 13 PRs blindly on day one. Order by risk and reference quality.

### Phase 0 — Contract freeze (this doc + Velnor)

1. Land this document in `velnor` (`VELNOR_PROJECTS_SETUP.md`).
2. Confirm fleet labels, org JIT status, and job image contents against §7–§8.
3. Publish a one-page **workflow snippet pack** (matrix-setup, setup-rust-ci composite) — either in `velnor/docs/` or a small `velnor-ci-actions` repo.

### Phase 1 — Reference alignment

| Order | Repo | PR focus |
|-------|------|----------|
| 1 | **java-monorepo** | `ubuntu-latest` → `ubuntu-26.04`; confirm AGENTS match reality |
| 2 | **blockchain-nodes** | `lane` → `lanes`; ubuntu pin; qemu action |
| 3 | **jackin** | default lanes → **velnor**; ubuntu-26.04; AGENTS policy update; optional PR #810 merge first |

### Phase 2 — Dogfood + strong mid-tier

| Order | Repo | PR focus |
|-------|------|----------|
| 4 | **velnor** | lanes + default velnor + ubuntu-26.04 |
| 5 | **holla** | lanes only (already clean) |
| 6 | **ruxel** | lanes + pin; verify ansible/uv on Velnor |
| 7 | **parallax** | full lanes; macOS isolation; pin |

### Phase 3 — Libraries + cleanup

| Order | Repo | PR focus |
|-------|------|----------|
| 8 | **termrock** | remove dtolnay; sccache; lanes; pin actions |
| 9 | **schemalane** | full Class D template + postgres path |
| 10 | **pg-bigdecimal** | Class D template |
| 11 | **tracing-request-level** | Class D template |
| 12 | **parallax-telemetry-playground** | Class E template |
| 13 | **tablerock** | replace macOS dependency CI with Class D skeleton |

### Phase 4 — Estate enforcement

1. `velnor-tools` audit command: scan workflows for `ubuntu-latest`, missing `lanes`, `dtolnay/rust-toolchain`, missing sccache on compile jobs.
2. Fixture snippets updated for any new patterns.
3. Update `docs/master-plan.md` estate table + default-lane policy to match **Velnor default everywhere**.
4. Optional: required status checks named with lane suffix strategy documented per repo (GitHub required checks vs Velnor non-blocking during rollout — **operator choice**, but final state is Velnor required).

### Per-PR checklist (every repo)

- [ ] `.github/AGENTS.md` three-lane + ubuntu-26.04 policy
- [ ] `workflow_dispatch.inputs.lanes` with default `velnor`
- [ ] `matrix-setup` job; workload `runs-on: ${{ fromJSON(matrix.config.runner) }}`
- [ ] Job names include `(${{ matrix.config.lane }})`
- [ ] `mise.toml` + `mise.lock` + `rust-toolchain.toml` (if Rust)
- [ ] No `dtolnay/rust-toolchain` / no unpinned actions
- [ ] sccache + cargo registry cache on compile jobs
- [ ] Docker builds have cache-from/to (if any)
- [ ] Single-writer safety when `both` can mutate
- [ ] macOS jobs not on Velnor matrix
- [ ] Manual verify: `lanes=velnor`, `lanes=github`, `lanes=both` once each
- [ ] Rerun-idempotency smoke on Velnor (no crate download wall)

---

## 10. Ideal minimal `ci.yml` skeleton (Class D)

Illustrative only — adapt names; pin SHAs at PR time.

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
  RUSTFLAGS: "-C link-arg=-fuse-ld=mold"
  RUSTC_WRAPPER: sccache
  SCCACHE_GHA_ENABLED: "true"

jobs:
  matrix-setup:
    name: Select runner lane
    runs-on: ubuntu-26.04
    timeout-minutes: 5
    outputs:
      configs: ${{ steps.set.outputs.configs }}
    steps:
      - id: set
        env:
          LANES: ${{ github.event_name == 'workflow_dispatch' && inputs.lanes || 'velnor' }}
        run: |
          set -euo pipefail
          github='{"lane":"GitHub","runner":"\"ubuntu-26.04\""}'
          velnor='{"lane":"Velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]"}'
          case "$LANES" in
            github) configs="[$github]" ;;
            both)   configs="[$github,$velnor]" ;;
            *)      configs="[$velnor]" ;;
          esac
          echo "configs=$configs" >> "$GITHUB_OUTPUT"

  rust:
    name: Format, lint, and test (${{ matrix.config.lane }})
    needs: matrix-setup
    strategy:
      fail-fast: false
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: actions/checkout@<pin> # v7
      - uses: rui314/setup-mold@<pin> # v1
      - uses: mozilla-actions/sccache-action@<pin> # v0.0.10
      - uses: jdx/mise-action@<pin> # v4
        with:
          install_args: rust cargo:cargo-nextest
          cache_save: ${{ github.ref == 'refs/heads/main' }}
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
      - name: Ensure Rust components
        run: rustup component add rustfmt clippy
      - name: Format
        run: cargo fmt --all -- --check
      - name: Clippy
        run: cargo clippy --workspace --locked --all-targets --all-features -- -D warnings
      - name: Test
        run: cargo nextest run --workspace --locked --all-features
      - name: sccache stats
        if: always()
        run: sccache --show-stats
```

---

## 11. Brainstorm: one configuration system (options)

| Approach | Pros | Cons | Recommendation |
|----------|------|------|----------------|
| **A. Copy-paste standard YAML** | Simple, no extra infra | Drift | OK for libraries short-term |
| **B. Shared composites in each repo** | Readable jobs | Still vendoring | Good mid-term |
| **C. `tailrocks/velnor-ci-actions` reusable workflows** | One upgrade point | Cross-org trust, versioning | **Best mid-term for tailrocks + optional for others** |
| **D. Generate workflows from Rust xtask** | jackin-level consistency | Heavy | Only monorepos that already invest in xtask |
| **E. Policy-as-code audit in velnor-tools** | Prevents regression | Does not install CI | **Do regardless** |

Recommended combination: **C for tailrocks**, **B/A for ChainArgos/jackin** (they already have mature in-repo actions), **E for enforcement**.

---

## 12. Success metrics

| Metric | Target |
|--------|--------|
| Default lane | 100% of listed repos default `velnor` on automatic events |
| Manual modes | `github` and `both` work on every workload workflow |
| Ubuntu pin | Zero `ubuntu-latest` on workload jobs; GitHub = `ubuntu-26.04` |
| Toolchain | Zero `dtolnay/rust-toolchain` in estate product workflows |
| Warm rerun | No dependency download/compile wall on Velnor for unchanged commit |
| Clarity | Job name alone explains purpose + lane |
| Velnor features | P0 backlog items closed before tailrocks mass flip |

---

## 13. Open decisions for the operator

1. **Required checks:** When Velnor is default, are GitHub-lane checks required, optional, or dispatch-only?
2. **Org JIT timeline:** Block tailrocks mass-onboarding until org-level JIT ships, or accept N repo-level daemons temporarily?
3. **Shared action repo vs vendoring:** Approve `tailrocks/velnor-ci-actions`?
4. **macOS release strategy:** Keep GitHub macOS for notarization-style needs, or Linux zigbuild-only?
5. **jackin PR #810:** Merge before or after default-lane flip?
6. **Public vs private:** Any of these repos public with fork PRs? → need non-trusted pool without secrets/Docker socket.

---

## 14. Appendix — Local clone map

| Repo | Path |
|------|------|
| jackin | `/Users/donbeave/Projects/jackin-project/jackin` |
| java-monorepo | `/Users/donbeave/Projects/ChainArgos/java-monorepo` |
| blockchain-nodes | `/Users/donbeave/Projects/ChainArgos/blockchain-nodes` |
| parallax | `/Users/donbeave/Projects/tailrocks/parallax-project/parallax` |
| parallax-telemetry-playground | `/Users/donbeave/Projects/tailrocks/parallax-project/parallax-telemetry-playground` |
| tablerock | `/Users/donbeave/Projects/tailrocks/tablerock` |
| holla | `/Users/donbeave/Projects/tailrocks/holla-project/holla` |
| velnor | `/Users/donbeave/Projects/tailrocks/velnor-project/velnor` |
| ruxel | `/Users/donbeave/Projects/tailrocks/ruxel` |
| termrock | `/Users/donbeave/Projects/tailrocks/termrock` |
| schemalane | `/Users/donbeave/Projects/tailrocks/schemalane` |
| pg-bigdecimal | `/Users/donbeave/Projects/tailrocks/pg-bigdecimal` |
| tracing-request-level | `/Users/donbeave/Projects/tailrocks/tracing-request-level` |

---

## 15. Appendix — Sources consulted

- Local workflows and `mise.toml` / `rust-toolchain.toml` for all 13 repos.
- Velnor docs: `mission.md`, `master-plan.md`, `runner-usage.md`, `perf-instant-cache-plan-2026-06-11.md`, `target-action-registry.md`.
- ChainArgos java-monorepo `.github/AGENTS.md` (runner lane + ubuntu-26.04 policy).
- jackin `.github/workflows/AGENTS.md` and PR [#810](https://github.com/jackin-project/jackin/pull/810) (result reuse, semantic jobs, Rust-owned CI).

---

*End of plan. Next engineering step: Phase 0 snippet pack + Phase 1 reference PRs after operator answers §13.*
