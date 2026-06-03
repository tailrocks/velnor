# Goal: Target Workflow Coverage

> **Mission:** see [`../docs/mission.md`](../docs/mission.md). Coverage is not generic — it is
> **Rust-first**. Adapters should be pre-cached and pre-optimized for the latest
> Rust toolchain/tools, exploit the beefy self-hosted host (parallelism,
> aggressive cargo/sccache/rust-cache tactics), and be cheaper + faster than the
> marketplace JS they replace.

> **Direction (source of truth):** [`../docs/`](../docs/) —
> [vision](../docs/vision.md), [roadmap/plan](../docs/roadmap.md),
> [native action adapter contract](../docs/native-action-adapter-contract.md).
> If this prompt and the docs disagree, the docs win.
>
> **Run order: 1 of 3** (see [`README.md`](README.md)). Run first — this is the
> foundation. Jobs must execute correctly before *GitHub UI parity* (#2) and
> *Fixture proof completion* (#3) can build on them.

## Objective

Ensure Velnor's Rust-native action adapters and runtime features **fully cover
the GitHub Actions surface used by the target repositories** (`jackin-project/jackin`
and `ChainArgos/java-monorepo`), so those workflows run on Velnor with no YAML
changes and behave like they do on GitHub-hosted runners.

## Why

Phase 0 success is not "a container runner starts" — it is that the target Rust
workflows *behave correctly*: toolchains install, caches work, path filters gate
downstream jobs, required aggregate jobs get correct `needs.*.result`, Docker /
Buildx / Bake flows work, artifacts move between jobs, and outputs/conclusions
are correct (see [`docs/roadmap.md`](../docs/roadmap.md) "Rust Runner Replacement
Requirement"). Velnor routes known action families to native Rust adapters
instead of executing marketplace JS — for performance and control.

## Ground truth

- Action contract: [`docs/native-action-adapter-contract.md`](../docs/native-action-adapter-contract.md).
- Behavior truth: <https://github.com/actions/runner> and each action's upstream
  repository.
- The fixture exercises these families; the fixture is the contract.

## In scope

The target action families (per roadmap) and their native adapters / supporting
features:

- `actions/checkout`, `actions/cache`, `actions/upload-artifact`,
  `actions/download-artifact`, `actions/upload-pages-artifact`,
  `actions/deploy-pages`
- `dorny/paths-filter`, `jdx/mise-action`, `extractions/setup-just`,
  `Swatinem/rust-cache`, `mozilla-actions/sccache-action`, `rui314/setup-mold`,
  `crazy-max/ghaction-github-runtime`, `renovatebot/github-action`
- `docker/login-action`, `docker/setup-buildx-action`, `docker/metadata-action`,
  `docker/build-push-action`, `docker/bake-action`
- local composites: `aggregate-needs`, `check-deployed-docs`,
  `check-fixture-output`; GitHub-expanded reusable workflows
- runtime features: command files, job/step outputs, `needs.*.result`,
  expression eval (`lit`/`expr`/`format`), `defaults.run`, working-directory,
  matrix, runtime/cache/OIDC env injection, secret masking.

## Out of scope

- The UI representation of output (that is the *GitHub UI parity* prompt).
- Running the real target repositories (operator-owned). Prove against the
  fixture only.
- Executing marketplace JS/TS bundles as the product path (forbidden — adapters
  are native).

## Definition of done

- Every target action family is `IMPLEMENTED` (no `PARTIAL`/`MISSING` that the
  fixture or target surface depends on); known limitations documented.
- Supporting runtime features behave per `actions/runner`.
- The fixture lanes that exercise each family pass (coordinate with the
  *Fixture proof* prompt).
- `cargo fmt --check`, `cargo test -q` pass; adapters covered by tests.

## Work through

➡ **[target-workflow-coverage.checklist.md](target-workflow-coverage.checklist.md)**
