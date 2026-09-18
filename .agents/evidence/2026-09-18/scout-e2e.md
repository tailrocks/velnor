# docker-e2e Gate Runbook Scout (Apple Silicon macOS 26 + OrbStack)

Scouted 2026-09-17. Research only; nothing built, no repo edits.

## 0. Local state (read-only, observed today)

- `docker context ls`: `orbstack *` active (`unix://~/.orbstack/run/docker.sock`). OrbStack processes running.
- `docker info`: `OrbStack | 29.4.0 | linux/aarch64`. Host: macOS 26.6.2, arm64. Lane satisfied.
- 26 local images, but **no** `projectjackin/construct:trixie` (dind default) and **no**
  `python:3.14-alpine` (usage_broker capsule image). First gate run pulls both
  (`python:3.14-alpine` is small; construct is GB-scale — prefer local build, step 2).

## 1. Mandated gate runbook

### Prep (once per checkout)

```sh
cd /Users/donbeave/Projects/tailrocks/jackin-project/jackin
mise install                                  # pinned toolchain + nextest etc; no ad-hoc cargo install
open -a OrbStack                              # or verify already running:
docker context use orbstack && docker info    # must succeed; xtask preflights `docker info`
```

### Construct image (only dind_e2e needs it; usage_broker uses python:3.14-alpine)

```sh
cargo xtask construct build-local             # -> jackin-local/construct:trixie (+ :trixie-dev), host platform
export JACKIN_E2E_CONSTRUCT_IMAGE=jackin-local/construct:trixie   # else default projectjackin/construct:trixie is pulled
```

Bake graph: [docker-bake.hcl](docker-bake.hcl) (`construct-local` single-platform load;
`construct-publish` multi-platform push). Sources: [docker/construct](docker/construct).

### Capsule binary (only dind_e2e/session/load tests need it; usage_broker does not)

`cargo xtask ci --e2e` exports it automatically. For direct nextest runs:

```sh
eval "$(cargo run --bin build-jackin-capsule -- --export)"
# --arch arm64 default on this host; zigbuilds aarch64-unknown-linux-gnu.2.17 ELF via rustup target
# bin: crates/jackin/src/bin/build_jackin_capsule/main.rs ; harness asserts ELF + executable (dind_e2e/common.rs)
```

PR-checkout variant (TESTING.md): `jackin-dev pr sync <PR>` then
`source "$(jackin-dev pr path <PR>)/env.sh"` instead of the export eval.

### The gate (mandated command)

```sh
cargo xtask ci --e2e
```

- Runs **all** partitions (lint/policy/tests/powerset/docs/snapshots) **plus** the e2e step:
  `docker info` preflight → capsule export → `cargo nextest run -p jackin --features e2e
  --profile docker-e2e --locked --offline` with `JACKIN_CAPSULE_BIN` set (crates/jackin-xtask/src/ci.rs:311).
- Fast iteration on the lane only: `cargo xtask ci --only e2e [--e2e-filter '...'] [--e2e-capsule <path>]`.
- Direct: `cargo nextest run -p jackin --features e2e --profile docker-e2e`.
- Profile ([.config/nextest.toml](.config/nextest.toml)): binaries
  `dind_e2e|session_send_e2e|usage_broker_e2e|load_options_e2e`, serialized
  (`test-group docker-e2e, max-threads=1`), 2 retries + flaky reporting, slow-timeout 60s×10.
  Full suite ≈ 4 min. Chaos replay: `JACKIN_CHAOS_SEED=<n>` (default `0xc4a0_55eed`).

### Evidence

- JUnit: `target/nextest/docker-e2e/junit.xml` (profile junit `path = "junit.xml"`).
- **Zero matching `usage_broker_e2e` tests = failure.** Durable record goes in the PR
  check/comment, never committed logs/screenshots. Generic daemon/Linux-tmpdir/unit-only
  runs do not satisfy the lane.

## 2. What usage_broker_e2e covers today (11 tests)

File roots: [crates/jackin/tests/usage_broker_e2e.rs](crates/jackin/tests/usage_broker_e2e.rs),
[usage_broker_e2e/docker.rs](crates/jackin/tests/usage_broker_e2e/docker.rs),
[usage_broker_e2e/recovery.rs](crates/jackin/tests/usage_broker_e2e/recovery.rs).
Docker legs use real `usage_relay` tunnels into `python:3.14-alpine` (`docker run --rm`,
bind-mount relay dir, in-container Python proxy + client scripts); broker is the real
`ensure_usage_broker_with_executor` with fake counting/gated providers.

| TESTING.md requirement | Test(s) | Shape |
|---|---|---|
| 2/20-client single-flight | `two_host_processes…`, `twenty_host_processes…`, `desktop_and_two…`, `desktop_and_twenty_docker_capsules…` | 2 or 20 clients → exactly 1 provider call; docker legs use ready/go barriers so provisioning cost never eats provider timeout |
| Owner-loss recovery | `killed_owner_recovers_once_without_a_herd` | owner killed mid-lease; 8 recovery clients retry like desktop; exactly 2 provider calls total (1 owner + 1 takeover) |
| Timeout ownership | `timeout_holds_ownership_until_provider_returns` | join times out (WaitTimeout) but phase stays Updating; after release → Failed/ProviderTimeout; 1 call |
| Desktop/Capsule generation adoption | `capsule_refresh_is_same_updating_generation_in_desktop` | capsule refresh → desktop `current()` sees same gen 1 Updating; both join to identical terminal state; 1 call |
| Capability isolation | `docker_capsule_cannot_access_another_account_or_global_tree` | account-b/codex denied Unauthorized; asserts no `/jackin/usage-shared`, no `JACKIN_USAGE_*_DIR` env; 0 calls |
| Distinct-account concurrency | `distinct_accounts_run_concurrently_within_bound` | A(claude)+B(codex), max_concurrency=2, both start before release, both Complete; 2 calls |
| Shared rate deadline/failure count | `failure_and_rate_deadline_are_identical_for_all_waiters` | 1 RateLimited probe; 8 waiters get identical terminal incl retry_at; refresh suppressed; consecutive_failures=1 |
| Unavailable-state zero-call | `unavailable_state_makes_zero_provider_calls` | symlinked data dir → Unavailable; 0 calls |

Rest of the `docker-e2e` profile (not usage_broker): dind_e2e 9 tests (5 load/exit + 3 chaos +
1 dind-daemon), session_send_e2e 1 test, load_options_e2e 1 test (`load_options_launch`).

## 3. Gaps vs the multi-account tracer bullet

Spec journey 7 (`jackin-accounts-usage-specification.md` §1.7): **one container runs
Claude-A + Claude-B + Codex-C concurrently; no credentials from unselected account D
enter that container.** Current e2e does not prove this:

1. **Single-capability fixture.** `capability()` hardcodes `shared-account/claude`; every
   relay grants exactly `vec![capability()]` (docker.rs:551). No capsule is ever granted
   ≥2 capabilities.
2. **No 3-account positive case.** Isolation test is negative-only (denied account-b).
   Nothing asserts A+B+C all usable in one container with correct per-account labels.
3. **No canary-D credential test.** Container script asserts absence of the shared mount
   and `JACKIN_USAGE_*_DIR` env, but never stages per-account secrets and asserts D's
   absence while A/B/C work.
4. **Same-agent-two-accounts is the known structural gap** (spec §2: `AgentCredentialEnv`
   keyed by agent) with zero e2e coverage.
5. **Concurrency is host-only.** `distinct_accounts…` has no container leg; no test runs
   two accounts through one relay/tunnel concurrently.
6. **Desktop generation adoption is host-client-only.** The adoption test's "desktop" is a
   second host `UsageBrokerClient`; Swift/desktop adoption lives outside nextest (XCTest).

## 4. Concrete test gaps to close (tracer-bullet e2e)

1. `usage_broker_one_capsule_with_three_capabilities_refreshes_all` — grant [A-claude,
   B-claude, C-codex] to one `python:3.14-alpine` capsule; refresh+join each; assert
   distinct generations/terminal states per account and 3 provider calls (change
   `start_capsule` to take a capability list).
2. `usage_broker_unselected_account_d_stays_out_of_container` — canary: D registered on
   host, excluded from grant; in-container assert D's staged secret/label absent while
   A/B/C succeed (extend CAPSULE_SCRIPT + relay grant).
3. `usage_broker_same_surface_two_accounts_concurrent_in_capsule` — A+B refresh
   concurrently through one tunnel within bound (container leg of gap 5).
4. Per-instance label check — capsule client returns account labels; assert tab/instance
   identity matches A/B/C (covers "see their account labels in tabs").
5. Keep negative companion: D's capability explicitly denied (already covered shape —
   extend unauthorized test to a granted-3/denied-1 matrix).
