# G1 PREP — ChainArgos/java-monorepo §9.2 unknowns (read-only)

Method: `gh api` reads only, no writes, no clones. G gate not reached.
Live `main` at read time: `38a8fb5777fd02fec8b3904d86387aba05940e9a`
(audited `235e479b…` → live `38a8fb57…`: drift recorded, consequence: all
paths below re-resolved against live tree; G1 execution must re-pin at gate.)
CI contract source: `.github/ci/project.toml` (1017 lines, 71 units).

## (a) The 16 `flywayMigrate jooqCodegen` preparations

Exact set (live `project.toml`; all four command lanes identical per unit):

| # | unit | root | command (`cd backend && …`) |
|---|------|------|------------------------------|
| 1 | gradle-backend | `backend` | `./gradlew --no-parallel flywayMigrate jooqCodegen check --no-daemon` |
| 2 | gradle-backend-bitcoin-domain | `backend/bitcoin-domain` | `… flywayMigrate jooqCodegen :bitcoin-domain:check …` |
| 3 | gradle-backend-bitcoin-flyway | `backend/bitcoin-flyway` | `… flywayMigrate jooqCodegen :bitcoin-flyway:check …` |
| 4 | gradle-backend-eth-domain | `backend/eth-domain` | `… :eth-domain:check` |
| 5 | gradle-backend-eth-flyway | `backend/eth-flyway` | `… :eth-flyway:check` |
| 6 | gradle-backend-legacy-domain | `backend/legacy-domain` | `… :legacy-domain:check` |
| 7 | gradle-backend-legacy-flyway | `backend/legacy-flyway` | `… :legacy-flyway:check` |
| 8 | gradle-backend-report-flyway | `backend/report-flyway` | `… :report-flyway:check` (no report-domain exists) |
| 9 | gradle-backend-temp-domain | `backend/temp-domain` | `… :temp-domain:check` |
| 10 | gradle-backend-temp-flyway | `backend/temp-flyway` | `… :temp-flyway:check` |
| 11 | gradle-backend-transfer-monitor-domain | `backend/transfer-monitor-domain` | `… :transfer-monitor-domain:check` |
| 12 | gradle-backend-transfer-monitor-flyway | `backend/transfer-monitor-flyway` | `… :transfer-monitor-flyway:check` |
| 13 | gradle-backend-tron-domain | `backend/tron-domain` | `… :tron-domain:check` |
| 14 | gradle-backend-tron-flyway | `backend/tron-flyway` | `… :tron-flyway:check` |
| 15 | gradle-backend-whitelabel-domain | `backend/whitelabel-domain` | `… :whitelabel-domain:check` |
| 16 | gradle-backend-whitelabel-flyway | `backend/whitelabel-flyway` | `… :whitelabel-flyway:check` |

1 root + 7 domain + 8 flyway = 16. The other 21 gradle units run bare
`:X:check` with no migrate/codegen prefix.

Actual Gradle graph (read from every module `build.gradle.kts`):

- Flyway plugin applied in exactly 8 modules: bitcoin/eth/legacy/report/
  temp/transfer-monitor/tron/whitelabel `-flyway`. Each declares its own
  `flyway{}` (own `locations` + `schemas`) and
  `tasks.named("flywayMigrate") { dependsOn("classes") }`.
- jOOQ codegen plugin applied in exactly 7 modules: the `-domain` modules
  (no report-domain). Each has `api(projects.XFlyway)`,
  `jooqCodegen(libs.postgresql)`, and
  `tasks.named("compileJava") { dependsOn("jooqCodegen") }`.
- NO task edge `jooqCodegen → flywayMigrate` anywhere. Ordering today comes
  only from CLI order (`flywayMigrate jooqCodegen`).
- NO other module applies either plugin (verified by grep over all 21
  remaining builds: only `projects.*Domain` project deps, zero plugin uses).

Root-vs-module semantics: all 16 invoke from `backend/` with UNQUALIFIED
`flywayMigrate jooqCodegen`, so each unit executes all 8 `flywayMigrate`
tasks + all 7 `jooqCodegen` tasks, then its one `:X:check`. Units 2–16
therefore repeat the full migration+codegen graph 15× for a single-module
check. (`org.gradle.parallel=true` in `gradle.properties`, but every prep
command passes `--no-parallel`.)

Per-module DB/schema matrix (flyway migrates it, domain codegen reads it):

| flyway module | DB (default) | schema | domain reads |
|---|---|---|---|
| bitcoin-flyway | chainargos | public | bitcoin-domain: public/chainargos |
| eth-flyway | chainargos | eth | eth-domain: eth/chainargos |
| legacy-flyway | legacy | public | legacy-domain: public/legacy |
| report-flyway | chainargos | report | (no domain; nothing reads it in-graph) |
| temp-flyway | chainargos | temp | temp-domain: temp/chainargos |
| transfer-monitor-flyway | transfer_monitor | public | transfer-monitor-domain: public/transfer_monitor |
| tron-flyway | chainargos | tron | tron-domain: tron/chainargos |
| whitelabel-flyway | instix | public | whitelabel-domain: public(instix), excludes flyway_schema_history |

All 15 modules honor `POSTGRESQL_DB_HOST` (default `127.0.0.1`) /
`POSTGRESQL_DB_PORT` (default `40000`); each flyway module additionally
accepts `<NAME>_DATASOURCE_URL/USERNAME/PASSWORD/SCHEMA` overrides.
Seed requirement: `docker/postgresql/init-databases.sql` creates DBs
`legacy, instix, transfer_monitor, ethereum, tron, bitcoin, coingecko`
(+`kestra`) and schemas `eth, tron, temp, report` in `chainargos`.

Transitive codegen consumers (units WITHOUT the prep prefix that still need
migrated DBs, via `→ domain → compileJava → jooqCodegen`):

- bitcoin-processor-app → bitcoinDomain; bitcoin-utils →(api) bitcoinDomain
- eth-processor-app → ethDomain; eth-transfer-validation-job → eth+legacy
- transfer-monitor-app → transferMonitor+eth+temp+legacy (4 domains!)
- tron-processor-app → tronDomain; tron-transfer-validation-job → tron+legacy
- whitelabel-app → whitelabel+legacy
- model/utils/coingecko/redshift/tailrocks/toolbox/crypto modules: no domain
  edge → need no DB for codegen (verify at gate: some may still need PG at
  test runtime via Micronaut test-resources).

Dedup hypothesis (equal coverage): replace the unqualified pair with
module-scoped paths per unit, keeping CLI order (migrate before codegen):

- domain unit `:X-domain`: `:X-flyway:flywayMigrate :X-domain:jooqCodegen :X-domain:check`
- flyway unit `:X-flyway`: `:X-flyway:flywayMigrate :X-flyway:check`
  (report-flyway: this alone; nothing else reads `report`)
- root `gradle-backend`: unchanged full `flywayMigrate jooqCodegen check`
- consumer units (processor-apps, validation jobs, utils): ADD scoped
  `:D-flyway:flywayMigrate` for each transitively-read domain D (they
  currently rely on luck/caches — the gap the dedup must close, not widen).

Equal-coverage proof obligations at G1: (1) per unit, every schema its
transitive `jooqCodegen` reads is migrated in the same invocation;
(2) `transfer-monitor-app` migrates 4 flyway modules; (3) no prod/shared DB:
all paths resolve host/port to the job-local PG via `POSTGRESQL_DB_HOST/PORT`
(or per-module `*_DATASOURCE_URL` pointing at the same job DB).

## (b) Testcontainers suites (PG / RabbitMQ / Redis / RustFS)

Crate matrix (`testcontainers =0.27.3`, `testcontainers-modules =0.15.0`):

- PG fixture (`Postgres::default().with_tag("18-alpine")`, `setup_db()` in
  `tests/common/mod.rs` + embedded `*_migration` run): bitcoin-migration,
  eth-migration, legacy-migration, tron-migration, bitcoin/eth/tron-processor-app,
  blockchain-explorer, coingecko-pricing-app, eth/legacy/tron-grpc-server.
- RabbitMQ (`RabbitMq::default()`, lapin 4; `publisher_confirm_test.rs`):
  bitcoin/eth/tron-processor-app (Cargo `rabbitmq` feature = same 3).
- Redis (`Redis::default()`, `setup_redis()`): eth/tron-processor-app
  (Cargo `redis` feature = same 2). processor-compare/monitor depend on
  `redis` crate but declare NO testcontainers (monitor has bare
  `testcontainers` dep for non-module use; its `tests/state_machine.rs` is
  not a container test — verify at gate).
- RustFS (`GenericImage::new("rustfs/rustfs", "1.0.0-beta.8")`, port 9000,
  `/data`, 21 tests in `tests/rustfs_publication.rs`): ONLY
  `lightdash-csv-delivery-app`. Compose pins the same tag + digest
  `sha256:fa19…e6cc`.

Nextest profile selection — CURRENT GAP: all 17 rust units run
`mbx nextest run --locked --all-features --package X --no-tests pass` with
NO `--profile` flag → effective profile is `default`, not `ci`.
`mise run build/test` use `--profile ci`. G1 must add `--profile ci` to the
unit commands. Effective configs (`.config/nextest.toml`):

| key | default (current CI) | ci (intended) |
|---|---|---|
| fail-fast | true | false |
| test-threads | unset (parallel) | 2 |
| retries | unset | 0 |
| slow-timeout | 60s, term-after 3 | 120s, term-after 3 |
| failure-output / final-status | unset | immediate-final / slow |

RustFS serial group: `[test-groups] lightdash-rustfs = { max-threads = 1 }`
+ `profile.default.overrides` filter `package(lightdash-csv-delivery-app)`.
Nextest profiles inherit `default`, so the group also constrains `ci` —
retain as-is; independently verify at G1 with `cargo nextest show-config
--profile ci` (or mbx equivalent) that the group + override resolve.

Live-RPC separation: `rpc_client_test.rs` is offline (httpmock TCP mocks);
`pipeline/*` tests are fixture/block-replay tests. No live
Ethereum/Base endpoint found in the sampled tests; G1 must still label any
network-dependent failure as external, not as a container failure.

## (c) Rust Docker context + 12-target bake contract

Contract file: `backend-rust/docker-bake.hcl`. Shared
`target "_workspace" { context = "."  dockerfile = "backend-rust/Dockerfile"
platforms = ["linux/amd64"] }`. 12 service targets in `group "default"`:

1. bitcoin-processor-app → chainargos/rust-bitcoin-processor
2. eth-processor-app → chainargos/rust-ethereum-processor
3. tron-processor-app → chainargos/rust-tron-processor
4. coingecko-pricing-app → chainargos/coingecko-pricing
5. processor-monitor-app → chainargos/processor-monitor
6. processor-compare-app → chainargos/processor-compare
7. lightdash-csv-delivery-app → chainargos/lightdash-csv-delivery (+SOURCE_COMMIT)
8. blockchain-explorer → chainargos/blockchain-explorer
9. bitcoin-grpc-server → chainargos/bitcoin-grpc-server
10. eth-grpc-server → chainargos/eth-grpc-server
11. tron-grpc-server → chainargos/tron-grpc-server
12. legacy-grpc-server → chainargos/legacy-grpc-server

Root-context proof (`backend-rust/Dockerfile`): `COPY . .` twice (planner
line 38, builder line 67); builder consumes root `Cargo.toml` workspace +
`backend/eth-flyway/...`, `backend/tron-flyway/...` migration resources +
`scripts/lightdash-migration/...` evidence/verify scripts; per-app stages
copy `backend-rust/<app>/config`. A `backend-rust/`-only context cannot
resolve `backend/` or `scripts/` — root context is mandatory.

Current CI gap: unit `docker-backend-rust` runs
`docker build --file backend-rust/Dockerfile --tag … 'backend-rust'`:
wrong context AND no `--target`, so it builds only the Dockerfile's FINAL
stage (`yolo-observer`, past the 12 service stages plus `lightdash-verifier`
and `lightdash-csv-verification-app`). G1 fix: root context + all 12 bake
targets (`docker buildx bake -f backend-rust/docker-bake.hcl`); the
`mise run build-docker-*` tasks already show the correct per-target shape
(`docker build -f backend-rust/Dockerfile --target <app> -t … .`).

## (d) Root Compose global names / ports / home mounts — risk

`docker-compose.yml` (799 lines): `name: chainargos`, global bridge network
`chainargos`, **36 fixed `container_name: chainargos-*`**, `restart: always`
throughout. Published ports (host side, all fixed): 40000 PG, 40001/40002
RabbitMQ, 40004 Redis, 40010 Lightdash, 40011/40012 RustFS, **80/443 nginx**,
8000 kellnr, 30000/30003/30044/30070/30071/30073/30074 app ports, plus
grpc/explorer ports. Home-dir mounts with `~/chainargos-data` defaults:
`POSTGRES_ROOT_DIR`, `RABBIT_ROOT_DIR`, `RUSTFS_ROOT_DIR`, `KELLNR_ROOT_DIR`
(`.env.sample` sets `POSTGRES_ROOT_DIR=$HOME`, `RABBIT_ROOT_DIR=$HOME`).
Second file `docker-compose.processor-verify.yml` exists (not inspected —
G1 must check it too).

Risk verdict: running root Compose unchanged on the shared bastion
namespace guarantees cross-job collisions (container names, ports —
especially 80/443 — network name) and cross-job mutable state via shared
home mounts. G1 must use job-local PG/Testcontainers (per (a)/(b)), never
`docker compose up` of this file; any compose use needs project isolation
+ ephemeral ports + job-scoped volumes.

## (e) Toolchain / browser / Micronaut declarations

| item | declared | site |
|---|---|---|
| GraalVM Java 25 | `oracle-graalvm-25.0.3` (+mise.lock sha256, linux-x64 + macos-arm64) | mise.toml, mise.lock |
| Gradle wrapper | 9.5.1 (`gradle-9.5.1-bin.zip`) | backend/gradle/wrapper/gradle-wrapper.properties |
| Rust | 1.98.1, clippy+rustfmt (mise.lock agrees) | rust-toolchain.toml, mise.toml idiomatic, mise.lock |
| Rust (Docker) | `rust:1.98.0-trixie@sha256:620d…2959` — DRIFT vs 1.98.1 | backend-rust/Dockerfile:6 |
| Node | 24.20.0 (+mise.lock sha256) | mise.toml, mise.lock |
| Bun | 1.3.14 (all 4 bun units `tool_version`; NOT in mise tools — runner-provided) | .github/ci/project.toml |
| protoc | `latest` → locked 35.1 (+protobuf-java 4.35.0; Dockerfile `protobuf-compiler` + libclang/mold) | mise.toml, mise.lock, libs.versions.toml |
| jOOQ / Flyway / PG JDBC | 3.21.4 / 11.20.3 / 42.7.11 | backend/gradle/libs.versions.toml |
| Micronaut | platform 4.10.14, gradle plugins 4.6.2, Docker API override `api.version=1.44` on StartTestResourcesService | libs.versions.toml, backend/build.gradle.kts |
| nextest | 0.9.140 | mise.toml |
| Browsers | platform: @playwright/test 1.63.0 + @axe-core/playwright 4.11.1, `test:browser` → `scripts/browser-gate.ts` (zero-test refusal wrapper), `browser:install` = chromium+webkit; prototype: playwright-core 1.62.1; compose: browserless/chromium v2.56.3; wallet-screening/docs/eventcatalog: none | frontend/*/package.json, docker-compose.yml |
| Java toolchain | `JavaLanguageVersion 25, GRAAL_VM, nativeImageCapable=true` (allprojects) | backend/build.gradle.kts |

Browser-coverage gap: CI unit `bun-platform` runs only
install→lint→typecheck→build→`vitest run`; `test:browser` (the Playwright
gate) is NOT in any unit command — unit green ≠ browser coverage. G1 must
audit how the Playwright gate runs (chromium/webkit provision, browserless
vs local) and wire it explicitly.

## G1 execution proof obligations (at gate, not now)

1. Scoped flyway/jooq commands per unit with transitive-read-set coverage
   proof (§a), incl. 4-DB `transfer-monitor-app` and DB-less `report` check.
2. `--profile ci` on all rust units + `show-config` evidence for profile +
   RustFS group; digest-lock `18-alpine`, `rustfs:1.0.0-beta.8`, and the
   default RabbitMQ/Redis module images.
3. Root-context 12-target bake green; `docker-backend-rust` rewritten.
4. No unchanged root Compose on bastion; job-local PG for all Gradle DB
   access (host/port via `POSTGRESQL_DB_HOST/PORT`).
5. Toolchain kept or tested-updated; resolve Dockerfile rust 1.98.0 drift;
   Playwright gate actually executed; Micronaut `1.44` vs selected daemon
   API checked.
