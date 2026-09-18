# G1 Testcontainers PREP — spec §9.2 Testcontainers clause (read-only)

Method: `gh api` reads only against ChainArgos/java-monorepo live `main`,
plus public upstream module source. No writes, no clones. G gate not reached.
Live `main` at read time: `38a8fb5777fd02fec8b3904d86387aba05940e9a`
(same SHA as /tmp/g1-prep.md; no drift since that read).

Spec clause (paraphrased): Rust Testcontainers PG/RabbitMQ/Redis/RustFS suites
run; intended nextest CI profile explicitly selected; one-at-a-time RustFS
group retained; effective config independently verified; digests resolved and
locked (tags `rustfs/rustfs:1.0.0-beta.8`, `postgres:18-alpine` not assumed
immutable); fixture acceptance separated from live RPC suites; external
failures labeled.

## 1. Live usage map (verified, not carried over)

Crates: `testcontainers ==0.27.3`, `testcontainers-modules ==0.15.0`
(pinned with `=` everywhere; lightdash comment: 0.28 breaks bollard-stubs).

| suite | fixture | crates (count) |
|---|---|---|
| PG | `Postgres::default().with_tag("18-alpine")` + embedded migration run in `setup_db()` | 12: 4 *-migration, 3 *-processor-app, blockchain-explorer, coingecko-pricing-app, 3 *-grpc-server (eth/legacy/tron). bitcoin-grpc-server has NO testcontainers |
| RabbitMQ | `RabbitMq::default()`, NO tag override, `publisher_confirm_test.rs`, lapin 4 | 3: bitcoin/eth/tron-processor-app |
| Redis | `Redis::default()`, NO tag override, `setup_redis()` | 2: eth/tron-processor-app |
| RustFS | `GenericImage::new("rustfs/rustfs","1.0.0-beta.8")`, port 9000, cmd `/data`, env RUSTFS_ACCESS/SECRET_KEY, per-test `create_bucket("lightdash-test")` readiness loop (30x250ms) | 1: lightdash-csv-delivery-app, `tests/rustfs_publication.rs`, 20 `#[tokio::test]` (recount: /tmp/g1-prep.md said 21) |

Non-suites: processor-monitor-app declares bare `testcontainers` but
`tests/state_machine.rs` contains zero container refs → not a container test.
processor-compare-app declares no testcontainers at all. Both still need a
Docker-capable job (harmless) unless G1 splits lanes.

Effective image tags today (module defaults resolved from
testcontainers-rs-modules-community `v0.15.0` source):

| image | effective tag | source |
|---|---|---|
| postgres | `18-alpine` | explicit `.with_tag` in all 12 fixtures (each migration_test.rs + each common/mod.rs + blockchain-explorer rest_test.rs x2) |
| rabbitmq | `3.8.22-management` | module `const TAG` (v0.15.0 `src/rabbitmq/mod.rs:4`); EOL upstream, no fixture override |
| redis | `5.0` | module `const TAG` (v0.15.0 `src/redis/standalone.rs:4`); no fixture override |
| rustfs/rustfs | `1.0.0-beta.8` | explicit `GenericImage::new`; root compose pins same tag + `sha256:fa19210a…e6cc` (candidate digest, re-verify at gate) |

## 2. Nextest config (live `.config/nextest.toml`, full file read)

- Intended CI profile name: `ci` (proof: `mise.toml` `[tasks.build]` and
  `[tasks.test]` both pass `--profile ci`; unit commands do not).
- Current gap (confirmed): all 17 rust units run
  `mbx nextest run --locked --all-features --package X --no-tests pass`
  with NO `--profile` → effective profile `default` (fail-fast=true,
  parallel threads, 60s slow-timeout), not `ci` (fail-fast=false,
  test-threads=2, retries=0, immediate-final, 120s slow-timeout).
- RustFS serial group: `[test-groups] lightdash-rustfs = { max-threads = 1 }`
  + `profile.default.overrides` filter `package(lightdash-csv-delivery-app)`.
  Nextest profiles inherit `default`, so `ci` inherits the group; G1 must
  still prove it with `show-config --profile ci`.

## 3. How CI provides/disables these today

- Provides: nothing explicit. `ci-unit-rust.yml` (343 lines, read whole)
  runs on `[self-hosted, velnor-target-mvp]`, no `services:`, no DinD, no
  socket mount, no `TESTCONTAINERS_*`/`DOCKER_HOST` env. Testcontainers talk
  to the runner-host daemon implicitly. No `TESTCONTAINERS_*` or `DOCKER_HOST`
  references exist in any `.rs` file (code search); only docs mention the word.
- Disables: nothing. No `#[ignore]`, no env-gated skips found in the sampled
  fixtures; `--no-tests pass` only tolerates crates with zero tests.
- Policy doc (`docs/contributing/testing.md:37`): integration tests use
  testcontainers and do NOT depend on root `docker-compose.yml`; no dev
  containers need to be running. Root compose must stay out of the picture
  (its fixed names/ports are the §(d) collision risk).
- `CARGO_NET_OFFLINE=true` during unit checks affects cargo only, not daemon
  image pulls; first-run pulls still hit the registry through the daemon.

## 4. Live-RPC separation (sampled)

`rpc_client_test.rs` and `network_verification_test.rs` (eth-processor-app)
are fully httpmock-based (`MockServer::start_async`, `/` URL). No live
Ethereum/Base endpoint found in sampled tests. G1 rule: any failure naming a
real external host/endpoint is labeled external, never a container failure;
a failure in an httpmock test is a code/fixture failure, never external.

## 5. G1 TARGET DESIGN

T1. Explicit profile: add `--profile ci` to the nextest invocation in all 17
rust units x all four lanes (pr/full x github/velnor) in
`.github/ci/project.toml`, regenerating workflows. No other flag changes;
`--locked --all-features --package X --no-tests pass` retained.

T2. RustFS serial group: keep `[test-groups] lightdash-rustfs` +
`profile.default.overrides` exactly as-is. Gate evidence:
`cargo nextest show-config --profile ci` (or mbx equivalent) showing the
`lightdash-csv-delivery-app` package resolving to `lightdash-rustfs` with
`max-threads = 1`, plus a passing lightdash run under `--profile ci`.

T3. Digest locks: resolve and lock at gate (never assume tag immutability):
`postgres:18-alpine`, `rabbitmq:3.8.22-management`, `redis:5.0`,
`rustfs/rustfs:1.0.0-beta.8` (compose digest `sha256:fa19…e6cc` is the
starting candidate for the last). Mechanism to spike at gate, in order of
preference: (a) fixture-level digest pin if the testcontainers 0.27 API
accepts a digest-suffixed reference; (b) job-local pre-pull by digest with
the fixture tag re-pointed at the pulled digest; (c) registry mirror with
immutable tags. Record chosen mechanism + resolved digests in G1 evidence.
Out of scope (name it): upgrading the EOL `3.8.22-management` / `5.0`
defaults is a SEPARATE source change with its own test run, not part of the
lock (spec: lock tested digests).

T4. Job-local services on bastion: (a) every rust-unit job gets Docker access
equivalent to today's velnor-target-mvp implicit daemon (socket or job-scoped
daemon — bastion design decision, not a fixture change); fixtures already
isolate correctly (ephemeral mapped ports via `get_host_port_ipv4`, no
`with_container_name`, no fixed host ports — spot-verify no fixed names at
gate); (b) Gradle PG stays on job-local PG via `POSTGRESQL_DB_HOST/PORT`
(per /tmp/g1-prep.md §a — interface note, owned by the flyway work); (c) root
`docker-compose.yml` is never started on the bastion (see §(d) risk).

T5. Failure labeling: classify per run — container-start/pull failures (daemon,
registry, digest mismatch) vs test assertion failures vs external-network
failures (only if a real remote host appears in the log; httpmock tests can
never produce this class). External class fails the run as external, with the
host/endpoint quoted — never counted as a container-suite regression.

## 6. Gate proof obligations (Testcontainers slice)

1. `--profile ci` present in all 17 rust unit commands, all lanes.
2. `show-config --profile ci` evidence: ci profile active + RustFS group resolves.
3. Four digests resolved, locked, recorded; mechanism stated.
4. Full 17-crate rust run green on bastion with job-local Docker; no root-compose use.
5. Failure-classification sample: at least one induced pull/start failure labeled
   correctly (or documented taxonomy if induction is infeasible at gate).
