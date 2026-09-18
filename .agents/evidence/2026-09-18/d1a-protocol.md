# D1 part A evidence — protocol foundation

Branch: `feat/d1-scaleset-protocol` (from `origin/docs/bastion-final-plan` @ `411b8a99`)
Commit: `966f3623` (signed off, pushed to origin)
Scope: design §1 (scaleset/ layout, part A), §2.3 (one migration), §7 fixtures.

## Upstream pin choice

`e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5` (actions/scaleset #127).
Audit: `git diff fb563005..e6daac70 -- '*.go'` is EMPTY (only CI workflow
bumps). All protocol sources byte-identical to the audited `fb563005`:
`client.go`, `session_client.go`, `types.go`, `errors.go`, `config.go`,
`common_client.go`, `jwt_provider.go`. Re-pinned
`SCALESET_UPSTREAM_COMMIT` (was `cb0405b2…`, gapmap PIN-DRIFT) and recorded
the audit note on the const + `upstream_pin.rs`.

## What landed (26 files, +4127/−7)

- `crates/velnor-runner/src/scaleset/`: `mod` (Config/connect), `upstream_pin`
  (pin + fail-closed `require_pin`), `config` (org/repo/enterprise parse,
  hosted detection + FORCE_GHES, api/v3 vs api. routing, registration-token
  paths), `credentials` (AppAuth validate, JwtProvider/Pem/closure, RS256
  iss/iat-60s/exp+9m; keys in-process, never in jobs), `errors`
  (MessageQueueTokenExpired + runner/job sentinels, 400/401/404/409 wrap,
  ActivityId/X-GitHub-Request-Id capture, Agent*Exception mapping, BOM strip),
  `backoff` (retryMax=4/30s/5min-long-poll, DefaultRetryPolicy statuses +
  admin-handshake 401/403, Retry-After via shared GitHubRateLimitStatus),
  `client` (App/PAT/JWT constructors, registration→admin-connection→JWT-exp
  chain with 60s skew + double-checked refresh, api-version default,
  full CRUD + GetRunner(ByName)/RemoveRunner + GenerateJitRunnerConfig),
  `session` (create/PATCH-refresh/close, GET+lastMessageId+X-ScaleSetMaxCapacity,
  202→None, DELETE ACK, AcquireJobs with queue-token auth; 401→refresh→retry-once
  on all three; batch dispatch incl. unknown-kind ignore), `fixtures`
  (manifest pin+sha256 verify, redaction scanner).
- `velnor-model/src/scheduler.rs`: all `types.go` wire types
  (JobAvailable/Assigned/Started/Completed, dispatched message, AcquireJobs,
  JIT setting/config with exact `encodedJITConfig` casing, Label/RunnerGroup/
  RunnerSetting/RunnerScaleSet with #110 no-omitempty, session, list shapes
  with `value` renames, `ScaleSetWorkerState` §5.2 enum).
- `velnor-control` migration v21 `scaleset-protocol-projections`: the 4 §2.3
  tables + demand index, `v21_schema_complete` chain, transactional
  convergence guard, upgrade + fail-closed tests.
- Fixtures: 11 recorded sanitized JSON + sha256 manifest under
  `tests/fixtures/scaleset/` (REDACTED secrets, `.invalid` hosts).
- `tests/scaleset_protocol.rs` (test-support gated, wiremock, zero live calls):
  8 conformance tests (fixture verify, poll headers/query, 202, 401-refresh,
  acquire auth+subset, JIT+ACK+close, CRUD reads + exception fault, ctors).

## Verification (this session, all observed)

- `cargo test -p velnor-runner` (full): 2212 lib + all integration targets, 0 failed.
- `cargo test -p velnor-model -p velnor-control`: all suites ok (138 model lib,
  269 control lib incl. 2 new migration tests, 0 failed).
- `cargo test -p velnor-runner --features test-support --test scaleset_protocol`:
  8 passed.
- `cargo clippy -p velnor-model -p velnor-runner -p velnor-control --all-targets`
  (+ `--features test-support` for runner): 0 warnings.
- `cargo fmt --check`: clean.

## Notes for part B

- Listener/Scale loop, demand/allocator/intents/converge/capacity/worker lane,
  journal events, and node scheduler gate are NOT in this commit (part B).
- `Config`/`connect` are built but the adapter `run()` comes with the listener.
- Deliberate non-mirrors: retry jitter omitted (cap+growth match; documented),
  timestamps stay `String` on wire types (Go zero-time is live traffic).
