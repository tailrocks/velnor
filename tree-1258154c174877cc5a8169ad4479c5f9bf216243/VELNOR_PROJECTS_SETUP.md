# Velnor Estate — Unified CI/CD Setup Standard

> **Status:** analysis + design (2026-07-18). Single source of truth for
> migrating every repo below to **one** standard GitHub Actions configuration:
> Velnor as the default runner, GitHub-hosted as a selectable lane, both-runs
> for comparison, all on Ubuntu, all via `mise`, with one unified caching model.
>
> Owner of this file: the operator. Changes to direction go in `docs/` first
> per `AGENTS.md`; this file records the concrete repo rollout plan.
>
> Companion docs: [docs/mission.md](docs/mission.md),
> [docs/master-plan.md](docs/master-plan.md) (§3a universal caching,
> §3b stability, lane policy), [docs/runner-usage.md](docs/runner-usage.md),
> [docs/comparison.md](docs/comparison.md),
> [docs/reference/target-action-registry.md](docs/reference/target-action-registry.md)
> (Velnor native-adapter surface).

## 0. TL;DR — the one approach

Every estate repo gets the **same workflow shape**, parameterized so the only
runner difference is `runs-on`. The contract already exists in
`ChainArgos/java-monorepo/.github/AGENTS.md` (the operator's documented runner
rule — §7.5) and is proven at zero-divergence parity; we standardize every repo
onto it.

1. **Velnor is the default lane.** Every automatic event (push, PR, schedule)
   runs on `[self-hosted, velnor-target-mvp]`. (jackin-family keeps GitHub
   default by operator policy but still carries the identical plumbing.)
2. **GitHub-hosted is one input away.** A workflow input
   `lane: velnor | github | both` (some repos call it `lanes`) selects the
   runner. `github` = **`ubuntu-26.04` exactly** (or `ubuntu-26.04-arm` for an
   ARM job) — **`ubuntu-latest` is forbidden**; `both` runs a matrix of both
   lanes in parallel for comparison.
3. **Ubuntu everywhere.** GitHub lane pinned to `ubuntu-26.04`; the Velnor job
   image is `ubuntu:26.04`. No `ubuntu-latest`, no third runner image, no
   macOS/Windows test runners (macOS only for cross-compile release artifacts).
4. **`mise` is the only tool installer.** Rust toolchain from
   `rust-toolchain.toml` (idiomatic_version_file); every other tool pinned in
   `mise.toml`. No `dtolnay/rust-toolchain`, no `actions/setup-*`. Renovate
   bumps all pins; **no `latest` anywhere** (except `protoc` which has no
   mise-pinnable version scheme — acceptable, audited).
5. **One caching model.** sccache (`SCCACHE_GHA_ENABLED: true` — real on the
   GitHub lane, host-local no-op on Velnor) + cargo registry/git + cargo
   target + mise tool cache + docker layer cache. Identical YAML on both lanes;
   on Velnor the `actions/cache` adapter no-ops (mount-based warm stores) and
   docker `type=gha` cache options are dropped for local persistent cache —
   **the same workflow file, no per-lane branching.** Cache keys are
   **lane-scoped** (`${{ matrix.config.lane }}`) and only **main** writes.

The hard rule (inherited from `master-plan.md` §3a and the fixture contract):
**never change the job to make Velnor pass.** Identical steps; only `runs-on`
differs. If the Velnor lane fails a step the GitHub lane passes, the bug is in
Velnor.

---

## 1. The estate (13 repos under analysis)

| Repo | Language(s) | Role | Current runner / toolchain | Velnor? |
|------|-------------|------|----------------------------|---------|
| `tailrocks/velnor` | Rust | The runner itself | `ubuntu-latest` (renovate `ubuntu-24.04`); mise | **No — must self-dogfood** |
| `tailrocks/parallax` | Rust + TS(bun) + Java | Most mature tailrocks CI (6 wfs, 20 jobs, `workflow_call`, sign/attest, homebrew-preview) | `ubuntu-latest` (+ macos-15 matrix); mise | No |
| `tailrocks/parallax-telemetry-playground` | Rust + Java(gradle) + web(bun/playwright) | Telemetry sandbox | `ubuntu-latest`; mise; **no caching at all** | No |
| `tailrocks/tablerock` | Rust (TUI, tokio-postgres/clickhouse/redis/turso) | **Outlier** — no real CI, only a dependency-check wf on **macos-15** | `macos-15`; dtolnay; no mise | No |
| `tailrocks/holla` | Rust | Release model reference (ubuntu-26.04, homebrew, apt) | `ubuntu-26.04` (+ macos-26); mise | No |
| `tailrocks/ruxel` | Rust (agent; musl static; python/uv/ansible oracle) | | `ubuntu-latest`; mise + rust-toolchain.toml (1.97.1) | No |
| `tailrocks/termrock` | Rust (TUI component lib; lookbook docs via bun) | cargo-hack feature-powerset, multi-OS, git-cliff release | `ubuntu-latest` (+ macos-latest crossterm); mise (1.97.0) | No |
| `tailrocks/schemalane` | Rust (pg_query C/bindgen; pg integration tests) | Crates.io publish, dep-order idempotent | `ubuntu-latest`; **dtolnay@stable**, no mise.toml | No |
| `tailrocks/pg-bigdecimal` | Rust **library** (NOT a PG extension — ToSql/FromSql for tokio-postgres) | Minimal donbeave-origin crate | `ubuntu-latest`; dtolnay@stable; **plain `@vN` tags, no pin** | No |
| `tailrocks/tracing-request-level` | Rust **library** (axum/tonic) | Minimal; **byte-identical CI to pg-bigdecimal** | `ubuntu-latest`; dtolnay@stable | No |
| `jackin-project/jackin` | Rust | **Pipeline source of truth** | `ubuntu-latest`; mise | No (GitHub by policy) |
| `ChainArgos/java-monorepo` | Rust monorepo + Java/Ansible/Kestra docker | Production target | **Velnor default** ✅; mise | **Yes** |
| `ChainArgos/blockchain-nodes` | Docker images (geth/cardano/arbitrum) | Production target | Velnor configured; buildx | **Yes (partial)** |

> **Cross-cutting (audited 2026-07-18):**
> - **Zero tailrocks repos use Velnor.** Lane plumbing is greenfield across all 10.
> - **No reusable/shared workflow** in any tailrocks repo (parallax `ci.yml` is
>   `workflow_call`-able; holla/parallax use `workflow_run` for preview). Estate
>   unification should introduce a shared composite or reusable workflow.
> - **Action versions fragmented across 3 vintages** (parallax/telemetry on
>   checkout v6.0.3 / cache v5.0.5 / mise v4.1.0; holla on v7.0.0 / v6.1.0 /
>   v4.2.1; velnor on v7 / v6 / v4.2.0). **holla is newest; standardize on
>   holla's pins.**
> - **Ubuntu pinning inconsistent**: `ubuntu-latest` (most), `ubuntu-26.04`
>   (holla), `ubuntu-24.04` (velnor renovate), `macos-15` (tablerock).
> - **Toolchain source split**: mise (parallax, telemetry, holla, velnor, ruxel,
>   termrock) vs `dtolnay/rust-toolchain@stable` (schemalane, pg-bigdecimal,
>   tracing-request-level, tablerock). **Standardize on mise +
>   `rust-toolchain.toml`** (idiomatic_version_file), per jackin/velnor model.
> - **Cache action split**: explicit `actions/cache` stack — full 4-tier
>   (parallax, holla, velnor) vs 2-tier registry+target (ruxel) vs
>   `Swatinem/rust-cache@v2` (termrock, schemalane, pg-bigdecimal,
>   tracing-request-level) vs none (telemetry). **Standardize on explicit
>   4-tier + sccache.**
> - **sccache present** only where the explicit stack is (parallax, holla,
>   velnor, ruxel). Missing everywhere Swatinem/none is used → add it.

Already-standardized reference implementations to copy from:
- **`jackin-project/jackin`** — the 4-layer cache stack + sccache (pipeline
  source of truth per `master-plan.md` §3a).
- **`tailrocks/holla`** — release.yml/release-deb.yml model (tar.gz ×4 + deb +
  homebrew tap) that `velnor` already mirrors.
- **`tailrocks/velnor`** `ci.yml` — the jackin-style stack applied to the
  runner's own CI.
- **`ChainArgos/java-monorepo`** + **`jackin-agent-brown`** — the **dual-lane
  `lane: github|velnor|both` plumbing**, proven at zero-divergence parity
  (`docs/comparison.md`).

---

## 2. The standard lane-plumbing pattern (verified canonical)

This is **not invented** — it is the exact pattern already running in
`ChainArgos/java-monorepo` (`rust.yml`, `rust-docker.yml`, `ansible.yml`),
`ChainArgos/blockchain-nodes` (`build-publish.yml`), `jackin-project/jackin`
(`ci.yml`, `construct.yml`, `jackin-dev.yml`, `preview.yml`, `release.yml`),
and proven at zero-divergence parity
(`docs/comparison.md`: java-monorepo run `27346553702`, agent-brown
`27350691196`, fixture `27341195843`). We copy it verbatim; only the default
lane flips per repo policy.

A **`matrix-setup` metadata job** emits a JSON array of lane-config objects.
Each object carries `lane`, `runner`, `runner_os`, `runner_arch`, and
`cache_writer`. Workload jobs consume `runs-on` (and optionally `cache_writer`)
from `matrix.config`. The lane input filters which objects are emitted.

### 2.1 Workflow inputs

```yaml
on:
  push:
    branches: [main]
  pull_request:
  schedule:
    - cron: "0 4 * * 0"          # optional weekly both-lane canary
  workflow_dispatch:
    inputs:
      lane:                       # some repos name it `lanes`; pick one estate-wide
        description: "Runner lane"
        type: choice
        options: [velnor, github, both]
        default: velnor           # <<< VELNOR IS DEFAULT (flip to github for jackin family)
```

> **Estate naming decision (§10):** standardize on `lane` (singular). jackin
> currently uses `lanes`; rename during its rollout PR.

### 2.2 The `matrix-setup` metadata job (verbatim shape)

The canonical label is **`velnor-target-mvp`** (what every existing repo
registers; see §10 on whether to shorten to `velnor`). GitHub lane is pinned
**`ubuntu-26.04`** (the java-monorepo contract). `both` mode emits both lanes;
per the java-monorepo AGENTS rule, in `both` the GitHub lane is the writer and
the Velnor lane is the comparison reader (`cache_writer: false`) — this is the
jackin pattern that prevents cache stampedes. (java-monorepo currently has both
write; we adopt the read-only-comparison form as the estate standard.)

```yaml
jobs:
  matrix-setup:
    runs-on: ubuntu-26.04            # metadata/coordinator jobs: pinned github ubuntu
    outputs:
      configs: ${{ steps.emit.outputs.configs }}
    steps:
      - id: emit
        env:
          LANE: ${{ github.event.inputs.lane || 'velnor' }}   # default Velnor
        run: |
          set -euo pipefail
          github='{"lane":"GitHub","runner":"\"ubuntu-26.04\"","runner_os":"Linux","runner_arch":"X64","cache_writer":true}'
          velnor_writer='{"lane":"Velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]","runner_os":"Linux","runner_arch":"X64","cache_writer":true}'
          velnor_reader='{"lane":"Velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]","runner_os":"Linux","runner_arch":"X64","cache_writer":false}'
          case "$LANE" in
            github) configs="[$github]" ;;
            both)   configs="[$github,$velnor_reader]" ;;   # github writes, velnor compares
            *)      configs="[$velnor_writer]" ;;            # default = velnor
          esac
          echo "configs=$configs" >> "$GITHUB_OUTPUT"
```

### 2.3 Workload jobs — only `runs-on` differs

```yaml
  build:
    needs: matrix-setup
    strategy:
      fail-fast: false
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      # ... IDENTICAL steps on both lanes. No `if: matrix.config.lane == ...`
      # branches. The ONLY lane-aware code lives in matrix-setup. (State-
      # mutating workflows are the documented exception — §7.7.)
```

> **Coordinator/control/aggregate jobs** (`matrix-setup`, `changes`,
> `ci-required`) stay on the pinned GitHub `ubuntu-26.04` (java-monorepo
> pattern): they must not make a lane comparison look green while running a
> different environment than the workload.

This is the entire runner-selection mechanism. `lane=velnor` (default) →
Velnor only; `lane=github` → GitHub only; `lane=both` → both in parallel for
comparison. The GitHub lane is **never deleted** (`master-plan.md` §4).

---

## 3. The standard Rust workflow (`ci.yml`)

Model: `velnor/.github/workflows/ci.yml` (jackin playbook). Applied to every
Rust repo. The 5 jobs run on the lane matrix; each carries the full cache
stack. `ci-required` aggregates.

Canonical jobs: **fmt · actionlint · deny · clippy · test** + `ci-required`.

Key per-job mechanics (identical on both lanes — Velnor makes the cache actions
warm no-ops, GitHub does real work). All cache keys are **lane-scoped**
(`${{ matrix.config.lane }}`) and only **main** saves (`cache_save` /
`if: github.ref == 'refs/heads/main'`):

- **Checkout**: `actions/checkout` pinned to commit-SHA, v7.
- **Toolchain via mise**: `jdx/mise-action` with
  `install_args: "rust"` (rust from `rust-toolchain.toml`) and
  `cache_save: ${{ github.ref == 'refs/heads/main' }}`. Components (`rustfmt`,
  `clippy`) added via `rustup component add` — mise installs only base
  components. Use `github_token: ${{ secrets.GITHUB_TOKEN }}` (same-repo) or
  `secrets.GH_READONLY_TOKEN` (cross-repo/private siblings, per jackin policy).
- **mold linker** (Linux): `rui314/setup-mold` + workflow env
  `RUSTFLAGS: "-C link-arg=-fuse-ld=mold"` (java-monorepo pattern — faster
  linking). Add `CARGO_BUILD_JOBS` tuning where the host is beefy.
- **rustup cache**: `actions/cache` on `~/.rustup/toolchains` +
  `~/.rustup/update-hashes`, key on `hashFiles('rust-toolchain.toml')`.
- **cargo registry/git cache**: `actions/cache` on
  `~/.cargo/registry/{cache,index,src}` + `~/.cargo/git/db`, key
  `cargo-registry-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('**/Cargo.lock') }}`,
  `restore-keys` branch fallback. (jackin verifies restored content with
  `cargo fetch --locked --offline`; non-main fetches only on verification
  failure and never saves.)
- **sccache**: `mozilla-actions/sccache-action` (`continue-on-error: true` —
  cache outage = slow build, never red). Set **workflow-wide**
  `RUSTC_WRAPPER: sccache` + `SCCACHE_GHA_ENABLED: "true"` (real GHA backend on
  the GitHub lane; host-local no-op on Velnor). Always print
  `sccache --show-stats`.
- **target-dir cache** (clippy/test only — fmt/deny/actionlint don't compile):
  `actions/cache` on `target`, key includes lane + job name + branch +
  `hashFiles('**/Cargo.lock','rust-toolchain.toml')`, 3-level `restore-keys`
  (this branch → base branch → main).
- **mise tool cache** (e.g. `cargo-nextest`):
  `actions/cache` on `~/.local/share/mise/installs/cargo-cargo-nextest`, key on
  `hashFiles('mise.toml')`.
- **Concurrency**: `group: ci-${{ github.ref }}`,
  `cancel-in-progress: ${{ github.event_name == 'pull_request' }}`.
- **Aggregate**: `ci-required` job `needs: [fmt,actionlint,deny,clippy,test]`,
  `if: always()`, fails if any need is `failure`/`cancelled` (jq over `needs`,
  or `gh api …/jobs` like java-monorepo). Runs on pinned `ubuntu-26.04`.

> **Why not `Swatinem/rust-cache`?** The estate standardizes on explicit
> `actions/cache` blocks (the jackin/velnor model) for transparent keys and
> because Velnor's mount-based stores make them warm no-ops uniformly.
> `Swatinem/rust-cache` is supported natively (registry row) and is fine where
> a repo already uses it, but new/standardized repos use the explicit stack so
> every cache key is auditable.

### 3.1 Required root files

```toml
# rust-toolchain.toml
[toolchain]
channel = "1.96.1"          # Renovate-managed; never "stable"/"latest"
components = ["rustfmt", "clippy"]
```

```toml
# mise.toml
[tools]
rust = "1.96.1"             # mirrors rust-toolchain.toml (Renovate keeps both in sync)
actionlint = "latest"      # see note: pin major via Renovate digest
"cargo:cargo-deny" = "0.19.9"
"cargo:cargo-nextest" = "0.9.137"

[settings]
idiomatic_version_file_enable_tools = ["rust"]   # picks up rust-toolchain.toml

[tasks.fmt]      = "cargo fmt --all --check"
[tasks.lint]     = "cargo clippy --workspace --all-targets --locked -- -D warnings"
[tasks.test]     = "cargo nextest run --workspace --locked --color=always"
[tasks.actionlint] = "actionlint"
[tasks.deny]     = "cargo deny check advisories"
[tasks.check]    = { depends = ["fmt","actionlint","deny","lint","test"] }
```

> Local dev runs `mise run check` and gets the identical gate CI enforces.

---

## 4. The standard Docker / container build pattern

For repos building images (`blockchain-nodes`, java-monorepo `rust-docker`,
agent-role publish, `pg-bigdecimal` server image):

- **Buildx, not plain `docker build`.** `docker/setup-buildx-action` with a
  named reusable builder.
- **Cache via `cache-from`/`cache-to`.** On the GitHub lane: registry
  `buildcache` refs (`<registry>/<image>:buildcache-<arch>`, `mode=max`) for
  published images, `type=gha` for PR-only builds. On the Velnor lane the
  adapter **drops `type=gha`** and uses the host-local persistent BuildKit
  cache (`master-plan.md` P3.7) — the workflow keeps `type=gha` and Velnor
  rewrites it, so **no per-lane YAML branching**.
- **`docker/bake-action`** for multi-image repos (the `blockchain-nodes`
  pattern): one discovery job → base/build chain → one matrixed leaf.
- **Cache mounts inside Dockerfiles**: `--mount=type=cache,target=/var/cache/apt`
  (apt) and `--mount=type=cache,target=/usr/local/cargo/registry` (cargo) +
  per-tool layers so one bump doesn't invalidate everything. See
  `velnor/docker/job-ubuntu.Dockerfile` for the reference.
- **Secrets**: `GITHUB_TOKEN`/registry tokens via
  `docker/login-action` (stdin password), never `--build-arg`.
- **Multi-arch**: native arm64 builders (`ubuntu-24.04-arm` on the GitHub lane,
  Velnor arm64 host later) + manifest merge — **no QEMU** emulation tax.

---

## 5. Non-Rust ecosystems (TypeScript / Node / Java / Python) — same shape

`parallax` and `parallax-telemetry-playground` have TS/Node (bun) + Java
(gradle) web stacks alongside Rust; `java-monorepo` uses mise-managed Java
(GraalVM), Node, and Python (Ansible/Kestra). For these, the lane plumbing and
caching principles are identical to Rust:

- **mise still owns the runtime**: `node = "22"`, `python = "3.13"` in
  `mise.toml`; `jdx/mise-action` installs them. No `actions/setup-node` /
  `actions/setup-python` ad-hoc steps.
- **Package-manager cache**: `actions/cache` on
  `~/.bun/install/cache` (bun, keyed on `bun.lock`),
  `~/.cache/pnpm`/`~/.npm` (pnpm/npm) or
  `~/.cache/pip` (pip), keyed on the committed lockfile. Velnor treats these
  as warm no-ops like cargo.
- **Gradle**: `gradle/actions/setup-gradle@v6` with **`cache-provider: basic`**.
  The v6 default "enhanced" cache is a closed-source proprietary component
  (extracted from the MIT action in March 2026); `basic` is the MIT-licensed
  wrapper over `@actions/cache` with clean key derivation — the only choice
  consistent with the estate's auditability rule. Do NOT stack a manual
  `actions/cache` for `GRADLE_USER_HOME` on top (setup-gradle owns it) and do
  NOT combine with `actions/setup-java` `cache: gradle`. Branch-scoped writes:
  `cache-read-only: ${{ github.ref != 'refs/heads/main' }}`. Java itself comes
  from mise (`oracle-graalvm-25.0.3`), never `actions/setup-java`. Keep
  `GRADLE_USER_HOME` inside the workspace (playground pattern) so cache paths
  are lane-independent.
- **Lockfile discipline**: committed lockfile; `bun install --frozen-lockfile` /
  `bun ci` / `./gradlew --no-daemon` in CI.
- **Same lane plumbing** (§2). `runs-on` is the only lane difference; the
  Velnor job image bakes node/protoc/gh so TS/Node/Java tooling is identical
  across lanes.

---

## 6. Per-repo findings + adaptation plan

Audited live `.github/workflows` for all 13 repos (2026-07-18). Each entry:
**current state → delta to standard**. "Lane?" = already has the
`lane/lanes: velnor|github|both` plumbing.

### 6.1 The production / reference repos (already dual-lane — make them the gold)

| Repo | Lane? | Default | GitHub pin | Toolchain | Cache | Delta |
|------|:----:|---------|-----------|-----------|-------|-------|
| **ChainArgos/java-monorepo** | ✅ | `velnor` | `ubuntu-26.04` ✅ | mise (1.97.1) + mold | sccache(GHA) + cargo cache (lane-scoped) + `warm-sccache` | **GOLD — its `.github/AGENTS.md` IS the contract.** Minor: rename `lanes`→`lane` estate-wide (§10); its `both` has both lanes write — switch Velnor to `cache_writer:false` (read-only compare) to match estate standard. |
| **ChainArgos/blockchain-nodes** | ✅ | `velnor` | `ubuntu-latest` ❌ (violates contract) | mise (`cargo:rust-script`) | registry buildcache (`BUILDX_CACHE: registry`, per-pkg `buildcache-<arch>`); QEMU binfmt multi-arch | **Pin GitHub lane to `ubuntu-26.04`**; add `.github/AGENTS.md` runner-policy doc (none today); pin `RENOVATE_GIT_AUTHOR` for DCO; replace QEMU with native `ubuntu-26.04-arm` lane. |
| **jackin-project/jackin** | ✅ | `github` (policy) | `ubuntu-24.04` (+`-arm`) | mise (1.97.0) + rust-toolchain.toml | sccache(**local**, `SCCACHE_GHA_ENABLED:"off"`) + Swatinem/rust-cache + custom registry-cache composite; `workflow_call` (`rust-nextest.yml`); `cache-cleanup.yml` on PR close | **Source of truth for pipeline design.** Rename `lanes`→`lane`; decide Ubuntu pin estate-wide (`24.04` here vs `26.04` in jm — §10). Reusable `rust-nextest.yml` is the model for sharing. |

### 6.2 The tailrocks repos (Velnor greenfield — apply the standard from scratch)

| Repo | Lane? | Default | Toolchain | Cache | Delta (highlights) |
|------|:----:|---------|-----------|-------|--------------------|
| **tailrocks/velnor** | ❌ | `ubuntu-latest` (renovate `ubuntu-24.04`) | mise (idiomatic) | 4-tier `actions/cache` + sccache(GHA) | **Self-dogfood first.** Add lane plumbing + default `velnor`; the runner must run its own CI on itself. |
| **tailrocks/parallax** | ❌ | `ubuntu-latest` (+macos-15) | mise (rust pinned in mise, 1.97.0) | 4-tier + sccache + `${{ runner.arch }}` keys; `workflow_call`; **only repo with sign/attest** (cosign+syft+SLSA) | Most mature (6 wfs, 21 jobs in `ci.yml`). Add lane plumbing (matrix-setup). **Rank-1 donor for the estate shared workflow** — its `ci.yml` is `workflow_call`-able + has portable composites (`aggregate-needs`, `sign-and-attest-archive`). Caveats: drop vestigial `setup-macos-sdk`; port `scripts/ci/{changed-paths,classify-paths}.sh` (or swap to dorny/paths-filter); intra-repo pin drift (`scheduled-measurement` uses upload-artifact v4.6.2 vs v7.0.1); `storage-integration.yml` lacks `concurrency:`; homebrew tap update is out-of-band (not in-tree). |
| **tailrocks/parallax-telemetry-playground** | ❌ | `ubuntu-latest` | mise (rust pinned 1.97.0; +java/protoc) | **NONE — no caching at all** | Add full cache stack + lane plumbing. Rust+Java+web(bun) multi-stack. |
| **tailrocks/tablerock** | ❌ | **`macos-15`** (no real CI) | `dtolnay/rust-toolchain@stable`; **no mise** | none | **Biggest gap.** Add a real `ci.yml` (fmt/clippy/test/deny), migrate to mise, move to Ubuntu, add lane plumbing. |
| **tailrocks/holla** | ❌ | `ubuntu-26.04` (+macos-26) | mise (idiomatic; `rust-toolchain.toml` targets **Linux-only** — macOS legs pay `rustup target add`) | 2-tier (registry+target) + sccache | Newest action pins (v7/v6.1.0/v4.2.1) — **use as the pin baseline estate-wide.** Add lane plumbing + 4-tier cache (it has 2-tier). **No `AGENTS.md` at all** — add the runner-policy doc. **No signing/attestation** (only parallax signs); decide estate-wide. Replace `shasum -a 256` with `sha256sum` (perl-provided, absent on minimal ubuntu). Add `concurrency:` to `release-deb.yml` and `permissions:` to `renovate.yml` (both missing). |
| **tailrocks/ruxel** | ❌ | `ubuntu-latest` | mise + rust-toolchain.toml (1.97.1) | sccache + `actions/cache@v5` manual; musl static agent | Add lane plumbing; `actions/cache@v5`→`v6` estate pin. |
| **tailrocks/termrock** | ❌ | `ubuntu-latest` (+macos crossterm) | mise (1.97.0); nightly for `cargo-public-api` (in `docs.yml`, not `rust.yml`) | **Swatinem/rust-cache@v2**; cargo-hack feature-powerset (**real features** — load-bearing); git-cliff release | Migrate Swatinem→explicit 4-tier + sccache; add lane plumbing (keep macos crossterm matrix as a release-only leg). **Only repo with unpinned actions** (`checkout@v7`, `mise-action@v4` major tags) — SHA-pin everything. Note: `release.yml` is **`workflow_dispatch` tag-creating** (inverted — workflow makes the tag, not triggered by it); termrock was **carved from jackin via `git-filter-repo`** (jackin = donor + parity target, not a CI-pattern reference). No `cargo mutants`. |
| **tailrocks/schemalane** | ❌ | `ubuntu-latest` | **`dtolnay@stable`**, no mise.toml | Swatinem@v2; dep-order crates.io publish | Migrate to mise + rust-toolchain.toml; add sccache + 4-tier; add lane plumbing. C/libclang toolchain — job image has build-essential + libclang. **Integration test uses testcontainers (Postgres)** — needs a Docker daemon: Velnor trusted scope provides it via host socket; document in workflow comment. Replace per-run `cargo install cargo-audit --locked` (10+ min uncached) with mise `cargo:cargo-audit`. Keep the credentialed release job cache-free (deliberate, per its `.github/AGENTS.md`). |
| **tailrocks/pg-bigdecimal** | ❌ | `ubuntu-latest` | **`dtolnay@stable`**, no pin | Swatinem@v2; **plain `@vN` tags, no SHA pin** | Migrate to mise; SHA-pin actions; add sccache + lane plumbing. (Pure Rust lib — NOT a PG extension.) |
| **tailrocks/tracing-request-level** | ❌ | `ubuntu-latest` | **`dtolnay@stable`** | Swatinem@v2 | Byte-identical CI to pg-bigdecimal — fix both in one go. Add `cargo hack --feature-powerset` (has feature flags, untested). `--all-features` pulls tonic/prost → needs `protoc`: install via mise (aqua) so both lanes behave identically (job image already ships protoc). |

### 6.3 Concrete deltas that apply to almost every repo

1. **Add the lane plumbing** (§2) — none of the 10 tailrocks repos have it.
2. **Standardize the action pins** to the holla/newest vintage (SHA + tag
   comment): `actions/checkout@…#v7`, `actions/cache@…#v6`, `jdx/mise-action@…#v4.2.1`,
   `mozilla-actions/sccache-action@…#v0.0.10`,
   `renovatebot/github-action@…#v46.1.19`, `rui314/setup-mold@…#v1`,
   `actions/upload-artifact@…#v7`, `actions/download-artifact@…#v8`.
3. **Standardize Ubuntu**: GitHub lane `ubuntu-26.04`; renovate jobs `ubuntu-26.04`
   (velnor's is `ubuntu-24.04`, holla's is `ubuntu-26.04`); kill `ubuntu-latest`.
4. **Standardize toolchain source**: `mise` + `rust-toolchain.toml`
   (idiomatic). Remove every `dtolnay/rust-toolchain` (tablerock, schemalane,
   pg-bigdecimal, tracing-request-level, termrock's `latest-toolchain` job).
5. **Standardize cache action**: explicit 4-tier `actions/cache@v6` + sccache.
   Replace `Swatinem/rust-cache@v2` (termrock, schemalane, pg-bigdecimal,
   tracing-request-level, and the jackin/java-monorepo stray uses) with the
   explicit stack. (Velnor supports Swatinem natively, so this is style/audit
   consistency, not a Velnor requirement.)
6. **Add `.github/AGENTS.md` runner-policy doc** (the java-monorepo contract,
   §7.5) to every repo. Today only java-monorepo has the *runner-policy* variant;
   `schemalane` has a `.github/AGENTS.md` but it covers *publish-policy* (pinning,
   dep-order, idempotency, least-privilege) — still missing the lane/Ubuntu-pin
   contract. So: every repo gets the runner-policy doc; schemalane merges it into
   its existing file.
7. **Add `cache-cleanup.yml`** (jackin pattern) — delete lane-scoped PR caches
   on PR close to bound the cache budget.

---

## 7. Best-practice catalog (the one way, by concern)

### 7.1 Rust toolchain
- `rust-toolchain.toml` is the single source; `mise.toml` mirrors the channel;
  `idiomatic_version_file_enable_tools = ["rust"]` so both agree.
- `--locked` on every cargo invocation in CI (Cargo.lock committed).
- `cargo nextest` over `cargo test` (faster, better isolation).
- `cargo deny check advisories` as a gate.
- `CARGO_INCREMENTAL=0` in CI (sccache + target cache replace it).
- MSRV declared in `Cargo.toml` (`rust-version`) and checked if the repo cares.

### 7.2 Caching (universal, both lanes)
| Concern | Action | Key |
|---------|--------|-----|
| rustup toolchain | `actions/cache` | `hashFiles('rust-toolchain.toml')` |
| cargo registry/git | `actions/cache` | `hashFiles('**/Cargo.lock')` + restore-keys |
| cargo target | `actions/cache` | branch + job + `hashFiles('**/Cargo.lock','rust-toolchain.toml')` |
| compilation | sccache (`RUSTC_WRAPPER=sccache`, `SCCACHE_GHA_ENABLED=true`) | GHA backend (github) / host-local (velnor) |
| mise tools | `actions/cache` | `hashFiles('mise.toml')` |
| docker layers | buildx `cache-from`/`cache-to` | registry buildcache (published) / `type=gha` (PR) |
| Dockerfile apt/cargo | `--mount=type=cache` | — |
| pnpm/pip/etc | `actions/cache` | lockfile hash |

**Acceptance test (rerun idempotency, `master-plan.md` §3a):** re-running a
finished pipeline on the same commit re-downloads/recompiles **nothing**.

### 7.3 Docker
- Buildx + named builder; bake for multi-image.
- `cache-from`/`cache-to` always; never cold rebuild of unchanged layers.
- Cache mounts for apt + cargo inside Dockerfiles.
- Multi-arch via native builders, never QEMU.
- Secrets via `login-action`, never `--build-arg`.

### 7.4 Action pinning + supply chain
- Every `uses:` pinned to a **commit SHA** (Renovate manages) — never `@vN`
  tags alone and never `@main`.
- `cargo deny check advisories` (Rust); consider `aquasecurity/trivy-action` or
  `gitleaks` for secret/sca on repos that already use them (jackin family).
- Minimal `permissions:` per job (contents: read default; write only where
  needed — release, pages, deb upload).
- `concurrency` group per ref; cancel in-progress on PRs.

### 7.5 Ubuntu rule — the operator contract (verbatim from java-monorepo)

The user's referenced rule is `ChainArgos/java-monorepo/.github/AGENTS.md`
("GitHub Actions runner policy"). It is the load-bearing runner contract; we
adopt it estate-wide verbatim. Summary of its hard constraints:

- **Three lanes, every workflow:** `velnor` (default for push/PR/schedule/manual,
  label `self-hosted` + `velnor-target-mvp`); `github` (the explicit GitHub
  comparison lane, runner **exactly `ubuntu-26.04`** — `ubuntu-latest` is
  forbidden; the only other permitted GitHub label is `ubuntu-26.04-arm` for an
  ARM-specific job); `both` (same workload on both, for comparison).
- **Automatic events stay Velnor-default.** A manual lane choice is honored by
  every workload job. No silent GitHub fallback, no third runner image, no
  `-latest` alias.
- **Coordinator/control/aggregate jobs** are reviewed with the workload matrix —
  they must not make a lane comparison look green while running a different
  environment.
- **State-mutating workflows (e.g. Renovate)** preserve single-writer safety:
  if `both` is offered, the secondary lane is read-only/dry-run; two writers
  never mutate the same repo concurrently (§7.7).
- **Why:** runner choice changes toolchain, Docker daemon, filesystem, cache,
  network, resource limits, and failure modes. A green GitHub job ≠ healthy
  Velnor path and vice-versa. Pinning the three lanes makes CI reproducible and
  lets operators diagnose parity regressions instead of mistaking runner
  differences for application changes.
- **Any change touching `runs-on`, lane inputs, matrix construction, or runner
  images must verify all three modes and preserve the pinned Ubuntu labels.**

> Estate consequence: **`blockchain-nodes` currently violates this** (uses
> `ubuntu-latest`); **jackin pins `ubuntu-24.04`** — the estate must pick one
> (§10). Recommended: `ubuntu-26.04` (matches java-monorepo contract, holla,
  and the Velnor `ubuntu:26.04` job image).

### 7.6 Release model (holla-shaped, for repos that ship binaries)
- `release.yml` on `v*` tag: matrix `aarch64-apple-darwin`,
  `x86_64-apple-darwin` (macos-latest), `x86_64-unknown-linux-gnu`,
  `aarch64-unknown-linux-gnu` (ubuntu-latest + zigbuild). sccache + mold +
  cargo registry cache. tar.gz + sha256 per target → GitHub Release.
- `release-deb.yml` on `v*`: ubuntu-latest + zigbuild + `cargo-deb` (via mise,
  never from-source). Binary-presence + size guard (the 0.1.2 incident).
  Latest Debian only (no old-glibc `.2.17` shims for debs).
- Homebrew formula push to `tailrocks/homebrew-<repo>` once the tap exists.

### 7.7 State-mutating workflows — single-writer safety (Renovate et al.)

The java-monorepo contract forbids two writers mutating the same repo
concurrently. For any state-mutating workflow that offers `both`, the secondary
lane must be read-only/dry-run. Verified pattern (java-monorepo `renovate.yml`):

```yaml
matrix:
  lane: ${{ fromJSON((inputs.lane == 'both') && '["GitHub","Velnor"]' || ((inputs.lane == 'github') && '["GitHub"]' || '["Velnor"]')) }}
runs-on: ${{ matrix.lane == 'GitHub' && fromJSON('"ubuntu-26.04"') || fromJSON('["self-hosted","velnor-target-mvp"]') }}
env:
  # In `both`, GitHub leg is the comparison reader (dry-run); Velnor is the writer.
  RENOVATE_DRY_RUN: ${{ (inputs.lane == 'both' && matrix.lane == 'GitHub') && 'full' || 'false' }}
  RENOVATE_GIT_AUTHOR: "Renovate Bot <renovate@whitesourcesoftware.com>"  # DCO email pin
```

Also: pin `RENOVATE_GIT_AUTHOR` so the DCO `Signed-off-by` email matches the
commit author (DCO-2 requires it) — java-monorepo + jackin do; blockchain-nodes
does not (fix it). Renovate job pins `ubuntu-26.04` (not matrix) where it is
single-lane.

### 7.8 Cache budget hygiene

- **`cache-cleanup.yml`** (jackin pattern): on `pull_request: [closed]`, delete
  lane-scoped PR caches via `gh cache delete` (`permissions: actions: write`).
  Bounds the 10 GB/actions cache budget as PRs accumulate.
- **main-only writes**: `cache_save: ${{ github.ref == 'refs/heads/main' }}` on
  mise-action; cargo registry saves only on verified miss on main (jackin).
- **lane-scoped keys**: every key includes `${{ matrix.config.lane }}` so
  Velnor and GitHub caches never collide.

### 7.9 Workflow clarity + result-reuse rules (mined from jackin PR #810)

`jackin-project/jackin#810` (`perf(ci): reuse build results across workflows`,
branch `perf/subminute-ci`) is the reference for the *clarity* goal: a reader
must understand what a workflow is for from the file name, job names, and step
names alone. Its `.github/workflows/AGENTS.md` codifies these rules; we adopt
them estate-wide (the runner-default flips per §10.3, the rest stands):

**Semantic boundaries (naming = meaning):**
- Never split one crate's tests, one artifact's bytes, or one conceptual
  command into numbered shards, batches, jobs, steps, or parts. One affected
  crate owns one complete test job; one cache artifact is one archive with one
  publish and one restore operation.
- Split jobs/steps only when they have distinct meaning, ownership, or failure
  diagnosis. Transport mechanics are not semantic boundaries.
- Improve runtime through result reuse, caches, and prebuilt artifacts — never
  by trading readability for parallel fragments of the same work.

**Thin YAML, tested logic:**
- CI decisions, parsing, cache contracts, and artifact transport live in a
  Rust tool (`jackin-xtask`/`jackin-dev` there; per-repo equivalent elsewhere)
  with unit tests. **No CI/CD logic in Bash** — not inline, not in composite
  actions, not under `scripts/`. Workflow `run` blocks invoke one prepared
  binary or one tool directly.
- Consequence for clarity: step names read as intent (`Restore exact-result
  proof`, `Audit executed jobs`) instead of 40-line shell heredocs.

**Exact-result reuse (the sub-minute CI trick):**
- CI/Docs/Construct publish **exact-result proofs** keyed by their inputs.
  Input-identical runs skip producer jobs entirely; the required aggregator
  still validates the preserved result, so branch protection keeps one stable
  check name. Measured: full CI repeat in **19 s** (all jobs structurally
  skipped), Docs 15 s, Construct 7 s.
- One stable cargo target seed per crate/feature-mode (debugless,
  non-incremental profile); a warm dependency seed initializes the first
  complete job, which publishes the canonical result.
- **ci_audit**: after the run, a Rust auditor walks every executed job/step
  through the attempt-scoped GitHub API, downloads logs concurrently, and
  **fails** if it finds dependency downloads, third-party compilation,
  source-tool compilation, or cache misses. This turns the §7.2 acceptance
  test ("rerun downloads/compiles nothing") into an enforced gate.

**Cache governance:**
- Add or widen a cache only after measuring usage and timing; each cache
  declares owner, invalidation inputs, restore source, write policy.
- `main` owns durable cache state; PRs restore but don't write (matches §7.8).
- Cargo registry fallback verified with `cargo fetch --locked --offline`.

**Scope/env/token discipline:**
- Third-party CLI selection vars (`BUILDX_BUILDER`, `GH_TOKEN`,
  `RUSTUP_TOOLCHAIN`) at **job** scope; workflow-level `env` only for in-house
  naming with no tool side effect.
- `${{ github.token }}` for same-repo reads; `GH_READONLY_TOKEN` reserved for
  cross-repo reads and mise downloads.
- Metadata jobs (runner selection, change classification) capped at 5 minutes;
  build/test/publish jobs get measured explicit timeouts.
- BuildKit streams `--progress=plain` so logs show cache resolution live.

**Publish/parity:**
- One derived `is_publish` gates every external write; feature branches never
  publish.
- A green PR must predict a green `main`: read-only PR equivalent for every
  main-only invariant; required checks never depend on transient third-party
  network state.
- Cancel stale runs per publish stream; serialize only the job writing a
  shared external resource.

**Estate adoption decision:** the *rules* are standard from day one (they cost
nothing and prevent the 13-copies drift). The *machinery* (exact-result
proofs, ci_audit, xtask-per-repo) lands per repo by maturity: jackin has it;
velnor/parallax are the next candidates; small libs (pg-bigdecimal,
tracing-request-level) adopt the thin-YAML/no-Bash rule but keep plain
cargo-nextest jobs until rebuild cost justifies reuse proofs.

---

## 8. What to develop in Velnor itself

The standard config assumes Velnor runs it unchanged. The full `uses:` surface
across the 13 repos is now known; below are the concrete items, split into
"gates the rollout" vs "already shipped".

### 8.1 Gaps that gate the estate rollout (no per-lane YAML branching allowed)
- **`docker/bake-action` multi-target + `type=gha` → local cache rewrite.**
  java-monorepo `rust-docker.yml` and `blockchain-nodes` bake/build use
  `type=gha` BuildKit caches; on the Velnor lane the adapter must drop `type=gha`
  and use host-local persistent BuildKit cache (master-plan P3.7). Verify
  `docker/bake-action` (not just `build-push-action`) hits the rewrite path.
- **`crazy-max/ghaction-github-runtime`** — jackin uses it to export
  `ACTIONS_*` runtime env. Confirm the native adapter (registry row present)
  no-ops cleanly on Velnor (the `ACTIONS_RUNTIME_TOKEN`/`ACTIONS_CACHE_URL`
  it exports are GHA-specific; Velnor's own cache backends replace them).
- **`jdx/mise-action` Java/Node/Python/bun install surface** —
  parallax-telemetry-playground (java+protoc), java-monorepo (graalvm java,
  node, python), termrock/parallax (bun). Confirm PATH injection for non-rust
  mise tools; the adapter exists (registry row) but the estate pushes it into
  multi-runtime territory.
- **`actions/cache` warm no-op for non-Rust paths** — pip (`~/.cache/pip`),
  ansible collections (`~/.ansible/collections`), rust-script
  (`~/.cache/rust-script`), pnpm/bun stores. All resolve to None on Velnor
  (no host-FS leak) — verify each for the relevant repo.
- **`actions/cache/restore` + `actions/cache/save` (split actions)** — jackin
  uses these for the rustup toolchain bootstrap with verification between
  restore and save. Confirm both split-action variants are no-op-correct on
  Velnor (not just the monolithic `actions/cache`).
- **`dtolnay/rust-toolchain`** — four repos still use it (being migrated to
  mise), but until migrated Velnor may see it via the node sidecar. Either add
  a native adapter or ensure the migration lands before the Velnor lane is the
  default on those repos.
- **`EmbarkStudios/cargo-deny-action`**, **`fsfe/reuse-action`**,
  **`aquasecurity/trivy`/`gitleaks`** — used by tablerock/schemalane/termrock.
  Add native adapters or accept node/docker sidecar (diagnostic fallback).
- **buildx `docker-container` driver gRPC drops** — java-monorepo
  `rust-docker.yml` carries a reset-and-retry workaround for BuildKit gRPC
  streams dying mid-build on the Velnor lane. Root-fix in Velnor's docker
  plumbing, then delete the workaround from java-monorepo. A workaround in a
  workflow is exactly the per-lane divergence the hard rule forbids.
- **Passwordless sudo + apt in the job image** — Playwright
  `install --with-deps` (parallax `browser-*` jobs, telemetry-playground `web`)
  and holla's renovate `sudo install -d` require it. Bake the Playwright system
  deps into `velnor/job-ubuntu` or guarantee passwordless sudo; otherwise these
  jobs can never leave the GitHub lane.
- **Privileged QEMU/binfmt contract** — blockchain-nodes and java-monorepo
  multi-arch builds run `tonistiigi/binfmt` (privileged). Works today via the
  trusted-scope host socket; needs an explicit documented contract + fixture
  coverage so it cannot silently regress (until the native-ARM decision lands,
  §10.6).
- **GitHub Pages deploy / OIDC from the Velnor lane** — termrock `docs.yml` and
  jackin `docs.yml` use `actions/deploy-pages` with `id-token: write`. Verify
  OIDC token issuance under org policy on self-hosted runners, or adopt a
  standing exception "Pages deploy jobs stay on the GitHub lane" (same class
  as the macOS exception).
- **Long-build slot stability** — blockchain-nodes image builds run up to 180
  min with `cancel-in-progress: false`. The daemon must guarantee a slot is
  never drained/reaped mid-job (watchdog excludes busy slots); document the
  guarantee and add a fixture that runs a multi-hour job.
- **Exact-result reuse support (jackin #810 machinery)** — the reuse pattern
  (§7.9) needs: artifact upload/download at full fidelity on the Velnor lane
  (proof publish + restore), attempt-scoped Actions API access from inside
  jobs (`ci_audit` reads job/step logs via `gh api`), and a **runner-neutral
  prepared-xtask bootstrap** (one bundled action, no host Node/unzip/Python/gh
  dependency) working identically on both lanes. Verify the artifact + API
  paths; the bootstrap is a workflow-side pattern, not runner code.
- **Rust-owned toolchain validation** — #810 replaces reliance on concurrent
  mise installs with a Rust tool that validates/repairs the runner slot from
  the pinned manifest (components + targets). On Velnor the job image pre-seeds
  the mise store; the validator must see a truthful picture (no fake shims) —
  already the design, but add a fixture asserting `ci_toolchain`-style
  validation passes on a fresh slot.

### 8.2 Already shipped — keep green, do not regress
- Dual-lane zero-divergence parity (java-monorepo, agent-brown, fixture) —
  `docs/comparison.md`.
- Truthful HOME=/github/home; mount-based cargo/mise stores; image seeding
  (dangling-shim fix).
- Trust-scope enforcement; daemon resource caps (`VELNOR_JOB_CPUS=4`,
  `VELNOR_JOB_MEMORY=12g`); graceful SIGTERM drain; never-exit resilience;
  doctor fleet watchdog.
- Native adapters for the core surface: checkout, cache, upload/download
  artifact, rust-cache, sccache, mise, paths-filter, docker
  login/setup-buildx/build-push/bake/metadata, setup-mold, setup-qemu,
  cosign-installer, hadolint-action, github-runtime, renovate.

### 8.3 Adapter audit — new families appearing in the 13 repos
Cross-checked against
[`docs/reference/target-action-registry.md`](docs/reference/target-action-registry.md).
New (not yet registered) `uses:` families seen:
- `EmbarkStudios/cargo-deny-action` (tablerock) — though most repos call
  `cargo deny` directly via mise; prefer that (no adapter needed).
- `fsfe/reuse-action` (jackin reuse-compliance, termrock reuse lint).
- `aquasecurity/trivy-action` / `gitleaks` (parallax security-hygiene,
  termrock) — `gitleaks` is a binary (mise-installable); prefer direct invocation.
- `cargo hack`/`cargo semver-checks`/`cargo public-api`/`cargo mutants` — these
  are cargo subcommands via mise, **not** actions; no adapter work.
- Homebrew tap PR/push via `peter-evans/create-pull-request` or direct `git
  push` (holla/parallax preview) — `create-pull-request` is on the
  master-plan P3b remaining list; add it.

> **Principle:** prefer mise-installed CLI invocation over marketplace actions
> (matches jackin/java-monorepo policy: "Do not add language-specific setup
> actions"). This shrinks the adapter surface Velnor must support.

### 8.4 Operator/infra items (not runner code, but required for rollout)
- **Canonical label decision.** Every existing repo registers
  `velnor-target-mvp`. Pick: keep `velnor-target-mvp` estate-wide (zero
  re-registration) **or** introduce a clean `velnor` label + migrate. The
  java-monorepo contract text says `velnor-target-mvp` — leaning keep.
- **Ubuntu pin decision.** java-monorepo `ubuntu-26.04` vs jackin
  `ubuntu-24.04`. Recommend `ubuntu-26.04` (contract + holla + job image).
- **Org-level JIT fleet** (master-plan P3.4) — one fleet serves all repos
  instead of per-repo partitions; needs the GitHub App credential (P1.7). This
  is what makes per-repo `runs-on: [self-hosted, velnor-target-mvp]` resolve
  without registering a runner per repo.
- **ARM lane.** jackin has GitHub-ARM (`ubuntu-24.04-arm`); java-monorepo
  mentions `ubuntu-26.04-arm`; blockchain-nodes uses QEMU. Decide: native ARM
  lane (`ubuntu-26.04-arm` + a Velnor arm64 host) vs QEMU.
- **Scheduled `lane=both` canary** per repo (master-plan P5.2) — catches
  regressions, publishes timing/parity via `velnor-tools lane-compare`.
- **Shared reusable workflow.** Extract jackin's `rust-nextest.yml`
  (`workflow_call` + `runner-configs-json`) / parallax's reusable design into a
  shared composite/reusable workflow so the 13 repos stop duplicating.

---

## 9. Rollout plan (PR per repo)

Standard PR title: `ci: adopt estate standard runner config (Velnor default +
GitHub lane + unified caching)`. Each PR:

1. Add **`.github/AGENTS.md` runner-policy doc** (the java-monorepo contract,
   §7.5) — the rule the user referenced, applied uniformly.
2. Add **`lane` input** (default per §10.3) + **`matrix-setup`** metadata job (§2.2).
3. Repoint every workload job to
   `runs-on: ${{ fromJSON(matrix.config.runner) }}`; coordinator/aggregate jobs
   stay on pinned `ubuntu-26.04`.
4. Apply the **standard cache stack** (§3, §7.2): replace ad-hoc installers with
   mise; replace `dtolnay/rust-toolchain` with mise + `rust-toolchain.toml`;
   replace `Swatinem/rust-cache` with explicit 4-tier + sccache; lane-scope
   keys; main-only writes.
5. **Pin Ubuntu** (`ubuntu-26.04`); kill `ubuntu-latest`. No macOS/Windows test
   runners.
6. Standardize **action SHAs** to the holla/newest vintage (§6.3.2).
7. Ensure `rust-toolchain.toml` + `mise.toml` exist and agree (idiomatic).
8. Add **`cache-cleanup.yml`** (§7.8) for PR-cache hygiene.
9. For state-mutating workflows (Renovate), add the **single-writer dry-run**
   guard (§7.7).
10. Verify on a PR: `lane=github` green (baseline) → `lane=both` green (Velnor
    lane matches GitHub lane — `velnor-tools lane-compare`).
11. Merge; default lane = Velnor (per §10.3).

### Rollout order (correctness-first, per master-plan sequencing)
1. **Extract the shared reusable CI workflow** (§10.7) first — the structural
   fix that makes the rest identical instead of copy-pasted.
2. **`tailrocks/velnor`** (self-dogfood — currently GitHub-only; eat our own
   dog food first; proves the runner on itself).
3. Simple Rust libs: `tracing-request-level`, `pg-bigdecimal` (one PR fixes
   both — byte-identical CI today).
4. Rust apps: `holla`, `ruxel`, `termrock`, `tablerock`, `parallax`,
   `parallax-telemetry-playground`.
5. `schemalane` (Rust + C/libclang + pg integration).
6. `jackin-project/jackin` (source of truth — propagates to agent-role repos;
   stays GitHub-default per policy but gains `lane` naming + the Ubuntu pin).
7. `ChainArgos/blockchain-nodes` (fix the `ubuntu-latest` violation + add the
   AGENTS.md doc).
8. `ChainArgos/java-monorepo` (GOLD — just reconcile `lanes`→`lane` and the
   `both`-mode cache-writer semantics; already Velnor-default).

> **Operator gate for real-target repos:** per `AGENTS.md`, do not run the
> Velnor lane on `ChainArgos/java-monorepo` / `jackin` or set
> `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true` without operator-provided runner
> host access. PR authoring (GitHub-lane hygiene + lane plumbing) is allowed;
> live Velnor-lane validation there is operator-run.

---

## 10. Open decisions for the operator (blocking the PR-per-repo rollout)

These surfaced directly from the audit; resolving them fixes the standard so
every PR is identical. Recommendation in italics.

1. **Canonical Velnor label.** Keep `velnor-target-mvp` (zero re-registration,
   matches java-monorepo contract) or migrate to `velnor`? *Recommend keep
   `velnor-target-mvp` until an org-level fleet rename is bundled.*
2. **Canonical Ubuntu pin.** `ubuntu-26.04` (java-monorepo contract, holla,
   job image) or `ubuntu-24.04` (jackin)? *Recommend `ubuntu-26.04` — bump
   jackin to match.*
3. **Default lane per repo family.** Production repos (java-monorepo,
   blockchain-nodes) = Velnor default. jackin family = GitHub default (policy).
   tailrocks repos = ? *Recommend Velnor default for all tailrocks repos*
   (they're the estate we own and want fast); jackin family stays GitHub-default
   per `master-plan.md` §3a until the operator flips it.
4. **`both`-mode cache semantics.** Both lanes write (java-monorepo today) or
   Velnor reads-only (jackin `cache_writer:false`)? *Recommend
   read-only-Velnor-in-`both` (jackin pattern) — avoids cache stampede and is
   the correct comparison semantics.*
5. **Input name: `lane` vs `lanes`.** *Recommend `lane` (singular), estate-wide;
   rename jackin during its PR.*
6. **ARM lane.** Native `ubuntu-26.04-arm` + Velnor arm64 host, or QEMU
   (blockchain-nodes today)? *Recommend native ARM lane; deprecate QEMU
   (master-plan P2.5).*
7. **Shared reusable workflow.** Extract one estate-shared
   `workflow_call`-able CI so repos don't duplicate? *Recommend yes — this is
   the structural fix for "13 repos, 13 copies" (root-cause per the operating
   principles: the architecture permits drift because there's no shared
   workflow).* **Donor ranking (audited):** **#1 parallax** — only repo with a
   `workflow_call`-able `ci.yml` (already consumed by its own `release.yml`) +
   portable composites (`aggregate-needs`, `sign-and-attest-archive` cosign+syft+SLSA)
   + the only AGENTS.md mandating "Jackin's workflow pattern" estate-wide;
   **#2 holla** for pin vintage + tar.gz/deb/homebrew release matrix;
   **#3 jackin** `rust-nextest.yml` for the per-crate reusable-test shape +
   #810's xtask/audit model (§12). Extraction sources:
   `parallax/.github/workflows/ci.yml`, `parallax/.github/actions/{aggregate-needs,sign-and-attest-archive}/action.yml`,
   `parallax/scripts/ci/{changed-paths,classify-paths}.sh`,
   `holla/.github/workflows/{release,release-deb}.yml`,
   `parallax/AGENTS.md`. Parameterize so each repo opts into the lanes it needs
   (Rust-only / Rust+UI / Rust+agent-musl / library-with-features).
8. **Homebrew/apt release scope.** Which repos ship binaries (holla, velnor do;
   parallax preview does)? `pg-bigdecimal`/`tracing-request-level`/`schemalane`
   ship to crates.io, not brew — their release.yml is crates-publish, not the
   holla tarball model. *Confirm per repo in §6.2.*
9. **Org-level JIT fleet timing.** Roll out per-repo runners now, or gate the
   whole estate on P1.7 (GitHub App) + P3.4 (org JIT) first? *Recommend: roll
   out the workflow YAML (GitHub-lane + lane plumbing) now; gate the
   Velnor-default flip on org JIT so one fleet serves all repos.*
10. **jackin #810 machinery scope.** Exact-result proofs + ci_audit +
    repo-owned Rust CI tooling: adopt estate-wide, or jackin-only with
    per-repo opt-in? *Recommend: clarity rules (no-Bash, semantic boundaries,
    cache governance) estate-wide immediately; reuse machinery opt-in by repo
    maturity — velnor and parallax first after jackin.*

---

<!-- APPENDIX: per-repo raw audit from live .github/workflows lands in §6. -->

---

## 11. Reference templates (copy-paste for the per-repo PRs)
These are the concrete, runnable artifacts each repo's PR lands. They encode
the standard from §2/§3/§7. Action versions pinned to the holla/newest vintage
(§6.3.2); Renovate keeps them current. **Only `runs-on` differs across lanes.**

### 11.1 `.github/workflows/ci.yml` (standard Rust CI, both lanes)

```yaml
name: CI

# Estate standard runner config: Velnor default, GitHub comparison lane,
# both for parity. Only `runs-on` differs across lanes (§2).
# Cache + tool layout follows the jackin/velnor playbook (§3).

on:
  push:
    branches: [main]
  pull_request:
  schedule:
    - cron: "0 4 * * 0"          # weekly both-lane parity canary
  workflow_dispatch:
    inputs:
      lane:
        description: "Runner lane"
        type: choice
        options: [velnor, github, both]
        default: velnor

permissions:
  contents: read

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

env:
  CARGO_INCREMENTAL: "0"
  CARGO_TERM_COLOR: always
  RUSTFLAGS: "-C link-arg=-fuse-ld=mold"   # mold linker (Linux); no-op elsewhere
  RUSTC_WRAPPER: sccache
  SCCACHE_GHA_ENABLED: "true"              # real on GitHub lane; host-local on Velnor

jobs:
  matrix-setup:
    runs-on: ubuntu-26.04                  # coordinator job: pinned GitHub ubuntu
    outputs:
      configs: ${{ steps.emit.outputs.configs }}
    steps:
      - id: emit
        env:
          LANE: ${{ github.event.inputs.lane || 'velnor' }}
        run: |
          set -euo pipefail
          github='{"lane":"GitHub","runner":"\"ubuntu-26.04\"","cache_writer":true}'
          velnor_w='{"lane":"Velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]","cache_writer":true}'
          velnor_r='{"lane":"Velnor","runner":"[\"self-hosted\",\"velnor-target-mvp\"]","cache_writer":false}'
          case "$LANE" in
            github) configs="[$github]" ;;
            both)   configs="[$github,$velnor_r]" ;;   # github writes, velnor compares
            *)      configs="[$velnor_w]" ;;
          esac
          echo "configs=$configs" >> "$GITHUB_OUTPUT"

  fmt:
    needs: matrix-setup
    strategy:
      fail-fast: false
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
      - uses: jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d # v4.2.0
        with:
          install_args: "rust"
          cache_save: ${{ github.ref == 'refs/heads/main' && matrix.config.cache_writer }}
          github_token: ${{ secrets.GITHUB_TOKEN }}
      - run: rustup component add rustfmt
      - run: cargo fmt --all --check

  actionlint:
    needs: matrix-setup
    strategy:
      fail-fast: false
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
      - uses: jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d # v4.2.0
        with:
          install_args: "actionlint"
          github_token: ${{ secrets.GITHUB_TOKEN }}
      - run: actionlint

  deny:
    needs: matrix-setup
    strategy:
      fail-fast: false
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
      - uses: jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d # v4.2.0
        with:
          install_args: "rust cargo:cargo-deny"
          github_token: ${{ secrets.GITHUB_TOKEN }}
      - run: cargo deny check advisories bans licenses sources

  clippy:
    needs: matrix-setup
    strategy:
      fail-fast: false
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
      - name: Cache rustup toolchain
        uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6
        with:
          path: |
            ~/.rustup/toolchains
            ~/.rustup/update-hashes
          key: rustup-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('rust-toolchain.toml') }}
      - name: Cache Cargo registry
        uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6
        with:
          path: |
            ~/.cargo/registry/cache
            ~/.cargo/registry/index
            ~/.cargo/registry/src
            ~/.cargo/git/db
          key: cargo-registry-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: |
            cargo-registry-${{ matrix.config.lane }}-${{ runner.os }}-
      - id: sccache
        uses: mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 # v0.0.10
        continue-on-error: true
      - uses: jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d # v4.2.0
        with:
          install_args: "rust"
          cache_save: ${{ github.ref == 'refs/heads/main' && matrix.config.cache_writer }}
          github_token: ${{ secrets.GITHUB_TOKEN }}
      - name: Cache target dir
        uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6
        with:
          path: target
          key: cargo-target-${{ matrix.config.lane }}-${{ github.head_ref || github.ref_name }}-clippy-${{ hashFiles('**/Cargo.lock', 'rust-toolchain.toml') }}
          restore-keys: |
            cargo-target-${{ matrix.config.lane }}-${{ github.head_ref || github.ref_name }}-clippy-
            cargo-target-${{ matrix.config.lane }}-${{ github.base_ref || 'main' }}-clippy-
            cargo-target-${{ matrix.config.lane }}-main-clippy-
      - run: rustup component add clippy
      - run: cargo clippy --workspace --all-targets --locked -- -D warnings
      - if: always() && steps.sccache.outcome == 'success'
        run: sccache --show-stats

  test:
    needs: matrix-setup
    strategy:
      fail-fast: false
      matrix:
        config: ${{ fromJSON(needs.matrix-setup.outputs.configs) }}
    runs-on: ${{ fromJSON(matrix.config.runner) }}
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
      - name: Cache rustup toolchain
        uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6
        with:
          path: |
            ~/.rustup/toolchains
            ~/.rustup/update-hashes
          key: rustup-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('rust-toolchain.toml') }}
      - name: Cache Cargo registry
        uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6
        with:
          path: |
            ~/.cargo/registry/cache
            ~/.cargo/registry/index
            ~/.cargo/registry/src
            ~/.cargo/git/db
          key: cargo-registry-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('**/Cargo.lock') }}
          restore-keys: |
            cargo-registry-${{ matrix.config.lane }}-${{ runner.os }}-
      - id: sccache
        uses: mozilla-actions/sccache-action@9e7fa8a12102821edf02ca5dbea1acd0f89a2696 # v0.0.10
        continue-on-error: true
      - name: Cache mise cargo tools
        uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6
        with:
          path: ~/.local/share/mise/installs/cargo-cargo-nextest
          key: mise-cargo-nextest-${{ matrix.config.lane }}-${{ runner.os }}-${{ hashFiles('mise.toml') }}
          restore-keys: |
            mise-cargo-nextest-${{ matrix.config.lane }}-${{ runner.os }}-
      - uses: jdx/mise-action@e6a8b3978addb5a52f2b4cd9d91eafa7f0ab959d # v4.2.0
        with:
          install_args: "rust cargo:cargo-nextest"
          cache_save: ${{ github.ref == 'refs/heads/main' && matrix.config.cache_writer }}
          github_token: ${{ secrets.GITHUB_TOKEN }}
      - name: Cache target dir
        uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6
        with:
          path: target
          key: cargo-target-${{ matrix.config.lane }}-${{ github.head_ref || github.ref_name }}-test-${{ hashFiles('**/Cargo.lock', 'rust-toolchain.toml') }}
          restore-keys: |
            cargo-target-${{ matrix.config.lane }}-${{ github.head_ref || github.ref_name }}-test-
            cargo-target-${{ matrix.config.lane }}-${{ github.base_ref || 'main' }}-test-
            cargo-target-${{ matrix.config.lane }}-main-test-
      - run: cargo nextest run --workspace --locked --color=always
      - if: always() && steps.sccache.outcome == 'success'
        run: sccache --show-stats

  ci-required:
    needs: [fmt, actionlint, deny, clippy, test]
    if: always()
    runs-on: ubuntu-26.04
    steps:
      - name: Fail on any failed or cancelled need
        env:
          NEEDS_JSON: ${{ toJSON(needs) }}
        run: |
          echo "$NEEDS_JSON" | jq -e 'to_entries | map(.value.result) | any(. == "failure" or . == "cancelled") | not' >/dev/null \
            || { echo "A required job failed or was cancelled." >&2; exit 1; }
```

> **Repo-specific deltas when applying:** (a) jobs that don't compile
> (fmt/actionlint/deny) can drop the compile-cache blocks; (b) `install_args`
> add the repo's extra mise tools (`bun`, `protoc`, `cargo:cargo-hack`, …);
> (c) repos with feature flags add a `cargo hack --feature-powerset` job
> (termrock/schemalane/tracing-request-level); (d) Docker repos add a
> `docker-bake` job (§4); (e) release is a separate `release.yml` (§7.6).

### 11.2 `.github/AGENTS.md` (runner-policy doc — adopt in every repo)

Adapted from the `ChainArgos/java-monorepo` contract (§7.5), generalized:

```markdown
# GitHub Actions runner policy

Every workflow that executes jobs exposes the same lane selection:

- `velnor` — default for push, pull_request, schedule, and manual runs.
  Runner: `["self-hosted", "velnor-target-mvp"]`.
- `github` — explicit GitHub-hosted comparison lane. Runner must be exactly
  `ubuntu-26.04`; never `ubuntu-latest`. The only other permitted GitHub label
  is `ubuntu-26.04-arm` for an ARM-specific job.
- `both` — same workload on Velnor and the pinned GitHub lane for comparison.
  In `both`, the GitHub lane writes caches; Velnor is the read-only comparison
  lane (`cache_writer: false`).

Automatic events stay Velnor-default. A manual lane choice is honored by every
workload job. No silent GitHub fallback, no third runner image, no `-latest`
alias. Coordinator/aggregate jobs run on pinned `ubuntu-26.04`.

State-mutating workflows (Renovate) preserve single-writer safety: in `both`,
the secondary lane runs `RENOVATE_DRY_RUN: full`.

Install every CI tool with `jdx/mise-action`; `mise.toml` and
`rust-toolchain.toml` are the version sources. Do not add language-specific
setup actions (no `dtolnay/rust-toolchain`, no `actions/setup-*`). Pin every
`uses:` to a commit SHA; Renovate bumps them. Cache keys are lane-scoped and
only `main` writes.
```

### 11.3 `.github/workflows/cache-cleanup.yml` (PR-cache hygiene)

```yaml
name: Cache cleanup
on:
  pull_request:
    types: [closed]
  workflow_dispatch:
permissions:
  actions: write
  contents: read
jobs:
  cleanup:
    runs-on: ubuntu-26.04
    steps:
      - name: Delete PR-scoped caches
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
          gh cache list -R "$GITHUB_REPOSITORY" --json key,ref \
            | jq -r --arg ref "$GITHUB_REF" '.[] | select(.ref == $ref) | .key' \
            | while read -r key; do
                gh cache delete "$key" -R "$GITHUB_REPOSITORY" || true
              done
```

### 11.4 `.github/workflows/renovate.yml` (single-writer, both-lane-safe)

```yaml
name: Renovate
on:
  merge_group:
  schedule:
    - cron: "7 6 * * *"        # off-the-hour (avoid :00 fleet stampede)
  push:
    branches: [main]
  workflow_dispatch:
    inputs:
      lane:
        description: "Runner lane"
        type: choice
        options: [velnor, github, both]
        default: velnor
jobs:
  renovate:
    strategy:
      fail-fast: false
      matrix:
        lane: ${{ fromJSON((github.event_name == 'workflow_dispatch' && inputs.lane == 'both') && '["GitHub","Velnor"]' || ((github.event_name == 'workflow_dispatch' && inputs.lane == 'github') && '["GitHub"]' || '["Velnor"]')) }}
    runs-on: ${{ matrix.lane == 'GitHub' && fromJSON('"ubuntu-26.04"') || fromJSON('["self-hosted","velnor-target-mvp"]') }}
    steps:
      - uses: actions/checkout@9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7
      - uses: renovatebot/github-action@22e0a16091fc706b04affe6ae53d5e3358ac4023 # v46.1.19
        with:
          token: ${{ secrets.GH_RENOVATE_TOKEN }}
        env:
          RENOVATE_REPOSITORIES: ${{ github.repository }}
          RENOVATE_ONBOARDING: "false"
          RENOVATE_GIT_AUTHOR: "Renovate Bot <renovate@whitesourcesoftware.com>"
          # both-mode single-writer safety: GitHub leg dry-runs, Velnor writes
          RENOVATE_DRY_RUN: ${{ (github.event_name == 'workflow_dispatch' && inputs.lane == 'both' && matrix.lane == 'GitHub') && 'full' || 'null' }}
          LOG_LEVEL: debug
```

### 11.5 Standard `mise.toml` + `rust-toolchain.toml`

```toml
# mise.toml — rust comes from rust-toolchain.toml (idiomatic); everything else pinned here.
[tools]
actionlint = "1.7.12"
"cargo:cargo-deny" = "0.20.2"
"cargo:cargo-nextest" = "0.9.140"
# add per-repo: bun, protoc, cargo:cargo-hack, cargo:cargo-zigbuild, zig, just, gh, …

[settings]
idiomatic_version_file_enable_tools = ["rust"]
experimental = true

[tasks.fmt]
run = "cargo fmt --all --check"
[tasks.lint]
run = "cargo clippy --workspace --all-targets --locked -- -D warnings"
[tasks.test]
run = "cargo nextest run --workspace --locked --color=always"
[tasks.deny]
run = "cargo deny check advisories bans licenses sources"
[tasks.check]
depends = ["fmt", "deny", "lint", "test"]
```

```toml
# rust-toolchain.toml — Renovate keeps channel in sync with mise/mirrors.
[toolchain]
channel = "1.97.1"
components = ["rustfmt", "clippy"]
```

> Local devs run `mise run check` for the same gate CI enforces — same tools,
> same flags, no setup beyond `mise install`.

---

## 12. Target architecture — clarity & Rust-owned orchestration

Source of inspiration: **`jackin-project/jackin` PR #810** ("perf(ci): reuse
build results across workflows", OPEN, 2026-07-17, +7481/−2246). The user
flagged it as the clarity reference: *current designs are only a starting
point — the goal is a perfect, readable workflow.* PR #810 is the most
advanced CI design in the estate and sets the north-star for §11's templates.
The baseline lane-plumbing (§2/§11) is the floor; this section is the ceiling.

### 12.1 The clarity principles (apply to every repo)

1. **One crate = one complete, independently-readable job.** A job's name tells
   you what it tests (`test (jackin-term)`), and it runs that crate's *entire*
   applicable suite. **Forbidden: shards, batches, numbered transport parts**
   (`crate-1`, `crate-2`, "part 3 of 7"). Splitting a crate across jobs to
   parallelize hides failures and obscures logs.
2. **No workflow-embedded Bash programs.** YAML `run:` blocks may call a
   prepared tool or a direct command — they must not *implement* CI decisions,
   parsing, cache contracts, or artifact transport. Anything non-trivial moves
   into a **Rust binary** (jackin calls it `jackin-xtask`; velnor calls its
   equivalent `velnor-tools`). Commits in #810 literally delete
   `scripts/ci/*.sh` (select-affected-crates, find-crate-target, audit-workflow-
   performance, docs-link-contract, …) and replace each with an `xtask`
   subcommand. This matches velnor's own **Rust-first scripting** hard rule
   (`AGENTS.md`).
3. **Descriptive, stable job + step names.** A maintainer reads the GitHub UI
   and knows what each job/step does and why — no `__run_N`, no opaque batch
   names. (Velnor already closed this gap: `docs/comparison.md` step-name/
   display-name parity.)
4. **Workflow files are thin orchestration.** `ci.yml` shrinks (−308 lines in
   #810) as logic moves to the binary; the YAML becomes a readable declaration
   of *what runs on which lane*, not *how*.

### 12.2 The reuse + verification model (jackin-scale; aspirational for big repos)

PR #810's headline: an input-identical rerun of the whole CI finished in **19
seconds** with every producer structurally skipped. How:

- **Exact-result proofs.** CI/Docs/Construct publish a reusable proof of their
  result; input-identical follow-up runs skip the producer job entirely while
  required aggregators still validate the preserved result.
- **Per-crate Cargo target seeds**, built under one **debugless, non-incremental
  profile**, versioned (`v7`) so incompatible cache generations can't mix. A
  warm dependency seed initializes the first complete crate job, which then
  publishes the canonical result.
- **Rust-owned transport.** `jackin-xtask` owns target discovery, download,
  validation, timestamp normalization, restoration, packing, and publication —
  one archive operation per crate result, no pointer artifacts or sequential
  shell pipelines.
- **Rust-owned toolchain prep.** A prepared binary validates and *repairs* each
  runner slot from the pinned manifest (rustup-based, serialized, warm-only
  activation) — replacing concurrent `mise install` races and stale-shim bugs.
  (Velnor hit the same dangling-shim class — fixed via image seeding; the
  xtask-repair approach is the structural counterpart on the workflow side.)
- **Performance audit as a gate.** `xtask` audits every executed job and step
  through the attempt-scoped GitHub API: downloads logs concurrently with
  bounded retries, rejects invalid responses, and **fails the build when
  dependency downloads, third-party compilation, source-tool compilation, cache
  misses, or missing executed-job logs appear.** This mechanically enforces the
  rerun-idempotency mandate (`master-plan.md` §3a): "watching dependencies
  compile in CI is a defect" becomes a failing check, not an aspiration.
  Resilient to transient GitHub 503s and treats skipped jobs' absent logs as
  expected (not fatal).

> **Right-size it.** pg-bigdecimal / tracing-request-level (single-crate libs)
> need only §12.1 (clear names, no embedded bash, a tiny `xtask` or none).
> parallax / termrock / schemalane / jackin (multi-crate) are candidates for the
> full §12.2 model. The estate **shared reusable workflow** (§10.7) should
> encode §12.1 by default and make §12.2 opt-in.

### 12.3 What this means for Velnor (dev items, adds to §8)

The §12.2 audit/reuse tools run on **both lanes** and must work unchanged on
Velnor. Concrete gates:

- **Log auditability.** The perf-audit gate greps job logs for download/compile
  markers. Velnor authors its own log stream but **passes user-command output
  through verbatim** (`mission.md`), so `Compiling …`, `Downloading …`,
  `cache miss` markers appear identically. **Verify** the auditor's marker set
  matches what Velnor emits (and what Velnor's adapters log for cache hit/miss),
  so the gate is meaningful on the Velnor lane, not just GitHub. This is the
  #810 model proven on Velnor (run `29557016424`).
- **Result/artifact reuse via API.** The proof + artifact-lookup path uses the
  GitHub Actions API + artifacts; both lanes publish/restore identically.
  Confirm Velnor's artifact upload + the `actions/upload-artifact` /
  `download-artifact` adapters support the input-hash-named result keys the
  reuse model depends on (registry row exists; verify large-archive + merge).
- **Runner-neutral bootstrap.** #810's `download-ci-xtask` is a single bundled
  Node action with no host `unzip`/`python`/`gh` dependency. Velnor's node:20
  sidecar (diagnostic fallback) or a native adapter must run it identically;
  prefer a **native Rust adapter** for the bootstrap step so neither lane needs
  Node.
- **Toolchain repair on warm slots.** The xtask repairs stale mise/rustup
  toolchains per slot. Velnor's warm, mount-based tool stores
  (`_velnor_mise`, image seeding) are the runner-side equivalent — the two must
  agree on the pinned manifest so a repaired slot == a warm Velnor slot.

### 12.4 Net design direction for the estate standard

Layer the standard:

| Layer | What | Applies to |
|-------|------|-----------|
| **L0 — lane plumbing** (§2/§11) | `lane: velnor\|github\|both`, matrix-setup, ubuntu-26.04, mise, unified cache | **every repo** (floor) |
| **L1 — clarity** (§12.1) | one-crate-one-job, no embedded bash, descriptive names, thin YAML | **every repo** |
| **L2 — Rust-owned helpers** (§12.1/§12.2) | shared `xtask`/`velnor-tools` subcommands for routing, cache contracts, status gate | multi-crate repos |
| **L3 — reuse + audit** (§12.2) | exact-result proofs, target seeds, perf-audit gate | jackin, parallax (aspirational) |

The per-repo PRs (§9) land L0+L1 everywhere; L2/L3 land repo-by-repo where the
scale justifies it, starting from jackin #810 as the reference.
