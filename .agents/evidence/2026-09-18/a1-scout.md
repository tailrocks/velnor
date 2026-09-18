# A1-prep scout (read-only, no edits) — 2026-09-17

Workspace: /Users/donbeave/Projects/tailrocks/velnor-project/velnor3

## 1. .github/workflows/*.yml + line counts (13,191 total)

| file | lines | jobs (real jobs only) |
|---|---|---|
| ci-main.yml | 2124 | plan, velnor-lane-admission, policy, prepare-cargo, github-*/velnor-* unit pairs (bun, docker, docs, opentofu, rust-policy, rust-unit-collector, rust per-crate ×10, production-topology), ci-required |
| ci-pr.yml | 1956 | same shape as ci-main minus policy job (plan, lane-admission, prepare-cargo, unit pairs, ci-required) |
| ci-policy.yml | 178 | policy (pull_request_target, pinned-base validator) |
| ci-release-package-signer.yml | 61 | attest (workflow_call) |
| ci-runtime-products.yml | 286 | closure, build, publish (PRODUCER: owner-only mainline publisher) |
| ci-unit-bun.yml | 483 | verify-github, verify-velnor (workflow_call) |
| ci-unit-docker.yml | 523 | verify-github, verify-velnor-trusted (workflow_call) |
| ci-unit-docs.yml | 481 | verify-github, verify-velnor (workflow_call) |
| ci-unit-opentofu.yml | 486 | verify-github, verify-velnor (workflow_call) |
| ci-unit-rust.yml | 860 | velnor-prepare-cargo-sources, verify-github, verify-velnor (workflow_call) |
| maintenance.yml | 279 | prune-pr-cache, cache-budget |
| nightly.yml | 114 | dispatch-ci-main, nightly-red-to-signal, nightly-alert |
| preview.yml | 1060 | identity, guest-payload, metadata, debian, sign-deb, publish |
| release.yml | 4300 | admit-runner, verify, release-github-* ×17, release-velnor-* ×17, build, image-admission, image-platform, image, metadata, guest-payload, debian, sign-deb, publish |
| AGENTS.md | — | not a workflow (notes file) |

Planning jobs: `plan` in ci-main.yml:43 + ci-pr.yml (affected-selection; primitive `affected-plan` in primitives/plan.rs).
Policy jobs: `policy` in ci-main.yml + ci-policy.yml; per-lane `github-rust-policy`/`velnor-rust-policy`.
unit-bootstrap jobs: NONE. No job or symbol named unit-bootstrap/unit_bootstrap exists (only "Mark tool bootstrap end" timing markers per unit file).

cargo/source-build fallback in consumer paths:
- ci-unit-rust.yml:565 — candidate-generator packaging: reuses unit's own `target/debug/velnor-workflow` if its `--closure` matches head, else `cargo build --locked -p velnor-workflow` in a detached worktree. Fail-closed (errors if neither). This is PRODUCER-side candidate packaging, not a consumer fallback.
- ci-runtime-products.yml:151 — producer `cargo build --locked --no-default-features --release` (the one sanctioned compile).
- release.yml:3727 + preview.yml:715 — `cargo install cargo-deb` (packaging tool only).
- Consumer (setup action) has NO cargo fallback: fails closed naming the producer.

## 2. crates/velnor-workflow module structure (src .rs only)

- lib.rs (17,701) — generator core: scan→config→render pipeline, CLI surface, product/provision logic.
- main.rs (13) — CLI entry, delegates to lib.
- build.rs (138) — build-time stamping (closure/features metadata).
- closure.rs (509) — source-closure identity: digest of closure inputs names the product.
- config/mod.rs (2773) — repo-owned generation config (.github-gen/velnor-workflow.toml) parsing/resolution.
- config/canonical.rs (77) — canonical serialization for stable config digests.
- estate.rs (447) — config-driven package-update (Renovate-style) rendering for adopted templates.
- policy.rs (3088) + policy/tests.rs (1457) — `velnor-workflow policy` trust validator + tests.
- runners.rs (99) — trust-gated Velnor job runner-label availability (declared, never probed).
- runtime.rs (4237) — Rust replacement for CI/policy/release shell helpers (parse, order, select, validate, package).
- template_memory.rs (373) — fail-closed GitHub TemplateMemory (10 MiB) startup estimate.
- primitives/mod.rs (1416) — declared-CI-surface registry: dispatch of `[[declare]]` rows to primitives.
- primitives/ir.rs (5953) — WorkflowIr: resolved lane/toolchain render environment.
- primitives/release.rs (4834) — release-side families (publish, preview, maintenance, signer).
- primitives/runtime_products.rs (1351) — stage-0 runtime-product PRODUCER primitive.
- primitives/snapshot.rs (1983) — snapshot identity + cache retention contract.
- primitives/cache.rs (287) — `cache-contract` transport a unit job restores/saves.
- primitives/lanes.rs (99) — `lane-matrix` hosted/self-hosted fan-out + trust gate.
- primitives/pipeline.rs (216) — per-unit verification pipelines (one primitive per unit kind).
- primitives/plan.rs (32) — `affected-plan` plan job every aggregate starts from.
- primitives/regen.rs (61) — `regen-gate` generated-surface honesty check.
- primitives/renovate.rs (469) — self-hosted Renovate workflows.
- primitives/watch.rs (341) — `watch-graph` derived affected-selection watch paths.
- primitives/aggregate.rs (68) — `unit-aggregation` aggregate composer (PR/main/nightly).
- scan/mod.rs (246) — scan pass pipeline turning a repo dir into typed RepositoryShape.
- scan/file_walk.rs (445) — file walk + repo-path helpers shared by detectors.
- scan/rust.rs (1359) — Rust detector (Cargo manifests, workspace graph).
- scan/node.rs (422) — Node/Bun detector (manifests, workspace graph).
- scan/gradle.rs (990) — Gradle detector (per-module units + root).
- scan/docker.rs (117) — Docker detector (per-Dockerfile-dir units).
- scan/docs.rs (50) — docs detector (Markdown unit).
- scan/opentofu.rs (36) — OpenTofu detector.
- scan/swift.rs (203) — Swift detector (packages, Xcode schemes).
- scan/homebrew.rs (22) — Homebrew detector (Formula/Casks/Brewfile).
- scan/signals.rs (26) — capability signals from policy/tooling files.
- tui/mod.rs (1377) — interactive scan/configure/generate terminal lifecycle + state.
- tui/view.rs (1076) — pure terminal projection for the TUI.

## 3. Producer/consumer + runtime-product code, setup-action sources

- PRODUCER primitive: crates/velnor-workflow/src/primitives/runtime_products.rs (stage-0, owner-only publisher of immutable binaries).
- PRODUCER workflow: .github/workflows/ci-runtime-products.yml (closure→build→publish; tag `velnor-workflow-runtime-v1-<digest16>` in tailrocks/velnor).
- Closure identity: crates/velnor-workflow/src/closure.rs.
- Consumer refs (policy provisioner, runtime acquisition): policy.rs, runtime.rs, lib.rs, primitives/{mod,ir,release,snapshot,regen}.rs, config/mod.rs, estate.rs.
- Setup action SOURCE: .github-gen/sources/actions/setup-velnor-workflow/action.yml (product-only consumer: resolve closure → gh release download → attestation verify → sha256 + --closure check → PATH; never compiles).
- Setup action RENDERED/checked-in copy: .github/actions/setup-velnor-workflow/action.yml.
- Product repo pinned in action: tailrocks/velnor, signer workflow tailrocks/velnor/.github/workflows/ci-runtime-products.yml.

## 4. Tool availability

- actionlint: PRESENT — /Users/donbeave/.local/share/mise/installs/actionlint/1.7.12/actionlint.
- cargo: PRESENT — 1.98.1 (via mise command-wrapper).
- target/debug/velnor-workflow: PRESENT — Mach-O arm64 executable, 30 MB (built Sep 17 06:40; an earlier probe during the session raced the build and missed it).

## 5. scripts/verify-release.sh — DOES NOT EXIST

find for *verify-release* returns nothing; scripts/ holds 18 files, none matching. Closest existing release-verification scripts:
- scripts/target_verify.sh, scripts/target_smoke_common.sh, scripts/chainargos_target_smoke.sh, scripts/jackin_target_smoke.sh, scripts/check-release-feature-boundary.sh (+ test).
No 10-line summary possible for a missing file; A1 must either create it or retarget to scripts/target_verify.sh (uninspected beyond listing, per read-only scope).
