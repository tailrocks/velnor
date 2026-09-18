# Bastion retained evidence: revalidation inputs and known issues

Status: **audit facts only** — branch `docs/bastion-final-plan` (PR `tailrocks/velnor#912`).
Authority: `plans/bastion-three-provider-ci/spec.md` governs implementation; this file governs nothing.
Steps: `plans/bastion-three-provider-ci/work-plan.md` (re-resolved at STEP A0). Checklist: `plans/bastion-three-provider-ci/checklist.md`.

Every fact below is a **revalidation input or known issue**, not a deployment pin and not proof that a failure still exists. At execution, re-resolve each item against live state and record `old observation → new evidence → consequence` without changing the target architecture.

## 1. Audited baseline identities (re-resolve at A0)

| Repository | Audited source SHA | Audited generator pin | Preserved inventory |
| --- | --- | --- | --- |
| `tailrocks/velnor` | `3353310c7648fca22698b6c0f4a69ab245127786` | `b9c3156cdb88e63c11b9e595a3e694b02238c09a` | 17 verification units; GitHub-hosted + Velnor defaults |
| `jackin-project/jackin` | `92f347ac39fbf0d6f9853168e2896a6c60522924` | `b9c3156cdb88e63c11b9e595a3e694b02238c09a` | 9 workflows; 40 units: 36 Rust, 1 Bun, 1 Docker, 2 Swift; hosted-only defaults |
| `ChainArgos/java-monorepo` | `235e479b150aeb949bc8a5190fba5b84f6303c80` | `1279c4f92c97b75dc4cc627f122e119f8a5eae16` | 11 workflows; 71 units: 37 Gradle, 17 Rust, 11 Docker, 4 Bun, 1 Node, 1 Docs; Velnor-only defaults |

## 2. Velnor baseline unit IDs (17)

```text
bun-velnor
docker
docs
opentofu
rust-policy
rust-unit-collector
rust-velnor-bench
rust-velnor-client
rust-velnor-control
rust-velnor-model
rust-velnor-render
rust-velnor-runner
rust-velnor-tools
rust-velnor-workflow
rust-velnor-workflow-contract
rust-velnorctl
rust-production-topology
```

## 3. ChainArgos baseline unit IDs (71)

| Unit | Kind | Root |
| --- | --- | --- |
| bun-chainargos-docs | bun | `frontend/docs` |
| bun-chainargos-eventcatalog | bun | `frontend/eventcatalog` |
| bun-platform | bun | `frontend/platform` |
| bun-platform-prototype | bun | `frontend/platform-prototype` |
| docker-ansible-configs-config-selene-observability-compose-maple | docker | `ansible-configs/config/selene-observability/compose/maple` |
| docker-ansible-configs-config-selene-observability-compose-parallax | docker | `ansible-configs/config/selene-observability/compose/parallax` |
| docker-ansible-configs-config-selene-observability-compose-sentry-proxy | docker | `ansible-configs/config/selene-observability/compose/sentry-proxy` |
| docker-backend-rust | docker | `backend-rust` |
| docker-docker-containers-docker-dbt-fusion | docker | `docker-containers/docker-dbt-fusion` |
| docker-docker-containers-docker-jvm-base | docker | `docker-containers/docker-jvm-base` |
| docker-docker-containers-docker-kestra-backup | docker | `docker-containers/docker-kestra-backup` |
| docker-docker-containers-docker-kestra-playwright | docker | `docker-containers/docker-kestra-playwright` |
| docker-frontend-platform | docker | `frontend/platform` |
| docker-frontend-platform-prototype | docker | `frontend/platform-prototype` |
| docker-frontend-wallet-screening | docker | `frontend/wallet-screening` |
| docs | docs | `.` |
| gradle-backend | gradle | `backend` |
| gradle-backend-bitcoin-domain | gradle | `backend/bitcoin-domain` |
| gradle-backend-bitcoin-flyway | gradle | `backend/bitcoin-flyway` |
| gradle-backend-bitcoin-model | gradle | `backend/bitcoin-model` |
| gradle-backend-bitcoin-processor-app | gradle | `backend/bitcoin-processor-app` |
| gradle-backend-bitcoin-utils | gradle | `backend/bitcoin-utils` |
| gradle-backend-coingecko-common | gradle | `backend/coingecko-common` |
| gradle-backend-coingecko-price-scraper-job | gradle | `backend/coingecko-price-scraper-job` |
| gradle-backend-coingecko-scraped-pricing-import-job | gradle | `backend/coingecko-scraped-pricing-import-job` |
| gradle-backend-crypto-utils | gradle | `backend/crypto-utils` |
| gradle-backend-eth-domain | gradle | `backend/eth-domain` |
| gradle-backend-eth-flyway | gradle | `backend/eth-flyway` |
| gradle-backend-eth-model | gradle | `backend/eth-model` |
| gradle-backend-eth-processor-app | gradle | `backend/eth-processor-app` |
| gradle-backend-eth-transfer-validation-job | gradle | `backend/eth-transfer-validation-job` |
| gradle-backend-legacy-domain | gradle | `backend/legacy-domain` |
| gradle-backend-legacy-flyway | gradle | `backend/legacy-flyway` |
| gradle-backend-redshift-dump-job | gradle | `backend/redshift-dump-job` |
| gradle-backend-report-flyway | gradle | `backend/report-flyway` |
| gradle-backend-tailrocks-jooq-utils | gradle | `backend/tailrocks-jooq-utils` |
| gradle-backend-tailrocks-type | gradle | `backend/tailrocks-type` |
| gradle-backend-tailrocks-type-converters | gradle | `backend/tailrocks-type-converters` |
| gradle-backend-temp-domain | gradle | `backend/temp-domain` |
| gradle-backend-temp-flyway | gradle | `backend/temp-flyway` |
| gradle-backend-toolbox | gradle | `backend/toolbox` |
| gradle-backend-transfer-monitor-app | gradle | `backend/transfer-monitor-app` |
| gradle-backend-transfer-monitor-domain | gradle | `backend/transfer-monitor-domain` |
| gradle-backend-transfer-monitor-flyway | gradle | `backend/transfer-monitor-flyway` |
| gradle-backend-tron-domain | gradle | `backend/tron-domain` |
| gradle-backend-tron-flyway | gradle | `backend/tron-flyway` |
| gradle-backend-tron-model | gradle | `backend/tron-model` |
| gradle-backend-tron-processor-app | gradle | `backend/tron-processor-app` |
| gradle-backend-tron-transfer-validation-job | gradle | `backend/tron-transfer-validation-job` |
| gradle-backend-whitelabel-app | gradle | `backend/whitelabel-app` |
| gradle-backend-whitelabel-domain | gradle | `backend/whitelabel-domain` |
| gradle-backend-whitelabel-flyway | gradle | `backend/whitelabel-flyway` |
| gradle-backend-whitelabel-model | gradle | `backend/whitelabel-model` |
| node-wallet-screening | node | `frontend/wallet-screening` |
| rust-bitcoin-grpc-server | rust | `backend-rust/bitcoin-grpc-server` |
| rust-bitcoin-migration | rust | `backend-rust/bitcoin-migration` |
| rust-bitcoin-processor-app | rust | `backend-rust/bitcoin-processor-app` |
| rust-blockchain-explorer | rust | `backend-rust/blockchain-explorer` |
| rust-chainargos-scripts | rust | `scripts` |
| rust-coingecko-pricing-app | rust | `backend-rust/coingecko-pricing-app` |
| rust-eth-grpc-server | rust | `backend-rust/eth-grpc-server` |
| rust-eth-migration | rust | `backend-rust/eth-migration` |
| rust-eth-processor-app | rust | `backend-rust/eth-processor-app` |
| rust-legacy-grpc-server | rust | `backend-rust/legacy-grpc-server` |
| rust-legacy-migration | rust | `backend-rust/legacy-migration` |
| rust-lightdash-csv-delivery-app | rust | `backend-rust/lightdash-csv-delivery-app` |
| rust-processor-compare-app | rust | `backend-rust/processor-compare-app` |
| rust-processor-monitor-app | rust | `backend-rust/processor-monitor-app` |
| rust-tron-grpc-server | rust | `backend-rust/tron-grpc-server` |
| rust-tron-migration | rust | `backend-rust/tron-migration` |
| rust-tron-processor-app | rust | `backend-rust/tron-processor-app` |

## 4. Jackin inventory boundary

The audit reports 40 units with kind counts (36 Rust, 1 Bun, 1 Docker, 2 Swift) but no full per-unit table. Do not invent those IDs. Re-read the generated unit manifest at the audited SHA and the execution SHA, keep the counts and native/E2E obligations, and record every coverage change.

## 5. Known failure signatures to revalidate

- **Velnor run `35129353335`:** failed Policy on `generated-tree` against `b9c3156c…` while Planning succeeded. Source `velnor-runner` was `0.1.275` with no matching `v0.1.275` release found at inspection; ~2 minutes spent building the policy runtime from source. Never confuse crate version, runtime-product release, official runner release, and daemon package.
- **Runtime race (PR #904 at `62a74bf58993c8c073d47274897f0db88cbd6159`, run `35136207272`):** failed Planning because product closure `f1f88c200e5b3b82` for revision `48a66ad7d56636f8bfa6069fbaf810d0089fef39` was unavailable until later. Reuse verified useful work; do not copy the race. PR #901's ruleset-403 fallback is a separate historical issue.
- **Jackin run `35114867283`:** Swift-on-Ubuntu failures (`desktop xcframework requires macOS (Apple Silicon)`, `swift: command not found`); desktop test read a deleted `release.yml`. Default nextest excluded `dind_e2e`, `session_send_e2e`, `usage_broker_e2e`, `load_options_e2e`.
- **ChainArgos run `35094895601` (Policy job `104789709640`):** two failed rules (`generated-tree`, `trusted-runners`); all 71 units skipped; trust findings on maintenance/nightly jobs. Inspected tree lacked `.github/actions/` despite references to `report-velnor-ci-outcomes`. Rust Docker builds had the wrong context and omitted bake targets.
- **APT gap:** `velnor-apt/.github-gen/NO_WORKFLOWS_REQUIRED.md` declared APT publication primitives absent (blob `3172bb883ec343a676d82c2594cb1a399191bf07` at last read). The generator must implement the capability; do not remove the notice until generated coverage genuinely replaces it.

## 6. Upstream reference identities (re-resolve + conformance-test, never assume)

- `actions/scaleset` main `fb56300503fd21caa788feeb85c63071d15155c6` (2026-09-15): acquisition responsibility in the scaler. Older `v0.4.0` = `6ce025902cd964747a078c2aabe7340ebc667eca` with a different listener API — never mix interfaces.
- Official runner baseline `v2.337.0`; container behavior inspected at source `80bb1fb827fa44d489263061e71ef4adba7ad8cd`. Always resolve digests and conformance-test the pinned revision.

## 7. Last live-refresh snapshot (proves priority, not deployment)

Velnor main returned the audited SHA; PR #904 open at the recorded head; APT omission notice still present; Scale Set main still `fb563005…`. Refresh all main refs, PR status, run attempts/logs, releases, runner permissions, image digests, and host state at execution.

Live checks: [Velnor main ref](https://api.github.com/repos/tailrocks/velnor/git/ref/heads/main), [PR #904](https://github.com/tailrocks/velnor/pull/904) (its description's green claims are not rerun evidence), [APT omission declaration](https://github.com/tailrocks/velnor-apt/blob/main/.github-gen/NO_WORKFLOWS_REQUIRED.md), [Scale Set main ref](https://api.github.com/repos/actions/scaleset/git/ref/heads/main).

## 8. Input provenance (exact supplied bytes, re-verified at commit)

| Source | File | SHA-256 |
| --- | --- | --- |
| S1 | `bastion-three-runner-spec.md` | `6280fc883c3d297a0f49366f9342acf1b395107c800357dcc6b8efa67eddb3a7` |
| S2 | `repository-and-upstream-evidence.md` | `d83a1494cfe3e20e11086e9de79a42501923d20b18124f22a18a704374b3e2fc` |
| S3 | `setup-three-runners-goal.md` | `015f89f26ddf9b5cd75981e282fd7bd4783bcd47652ee90ff434f98adc0ee109` |
| S4 | `bastion-three-runner-spec-v2.md` | `c538f2dbb17ad159310d8f6ab824355251968e2413b922212d31c204c2a17d2d` |
| S5 | `repository-and-upstream-evidence-v2.md` | `92ad2eb9bfb5349b360bd1e024a7b7aa35a66ebec19291967f5c4d7e860ffae5` |
| S6 | `setup-three-runners-goal-v2.md` | `b4882398c64fec8bf5ec8835c11afd75572db892a3f19d4141787804e4437b38` |
| S7 | `velnor-ci-cd-audit-2026-09-17.md` | `a6e99e93011ebecca1ea6618e55fe939d4cee4bcd688474ec2442367fcbd756e` |
| S8 | `velnor-first-bootstrap-spec.md` | `e9b50856179b04e7527f354e83c7aef12d4ea20d37219546d98c1bb13953c4b6` |
| S9 | `velnor-first-bootstrap-goal.md` | `a09988e7c30f7e70be3d0bb115556ae12ca2e0277b6cd3225119800fb95aca45` |

Evidence locator: baseline SHAs/counts S1§2, S2 repo sections, S5 baselines; Jackin failures/E2E/DinD/relay/20-capsule in S2 Jackin "Existing failures" + "Real Docker acceptance requirements"; ChainArgos policy/PostgreSQL/nextest-RustFS/Docker-context/bake-targets/local-action in S2 ChainArgos "Observed current failure" + "Docker and database correctness requirements" + "Cache, reports, and side effects"; native mediated-lease vs DinD, cgroup prerequisites, credential refresh gap, package lock/activation in S2 Velnor "Existing Docker behavior" + "Topology feasibility and credential gap" + "Packaging and operator procedure"; Scale Set acquisition/statistics/replay/assignment/namespace/path/externals in S2 "Verified scale-set design research"; APT gap, Policy drift, PR #904 race, version-vs-release in S7, expanded S8§3–6.

Technical cross-checks: [Docker bind mounts](https://docs.docker.com/engine/storage/bind-mounts/) (paths evaluated on the daemon host), [Docker resource constraints](https://docs.docker.com/engine/containers/resource_constraints/) (unconstrained defaults + OOM implications), [Debian trixie apt-secure](https://manpages.debian.org/trixie/apt/apt-secure.8.en.html), [Debian trixie sources.list](https://manpages.debian.org/trixie/sources.list%285%29) (scoped Signed-By), [actions/scaleset](https://github.com/actions/scaleset) (statistics-based demand, homogeneous ephemeral runners — pin + conformance-test the exact API; do not propagate the older blanket "one label only" rule).
