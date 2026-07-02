# Plan 018: Add an in-repo HTTP harness for the broker / run-service protocol sequence

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done. This is a **test-infrastructure**
> plan — it adds the first dev-dependency and the first protocol integration test.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/protocol.rs crates/velnor-runner/Cargo.toml`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: LOW
- **Depends on**: 012 (assert on typed status in error cases) — soft; can land
  first and add the typed-status assertions after 012
- **Category**: tests
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The "zombie fleet" class of production failures lives in the broker / run-service
**loop**, but the 43 `protocol.rs` tests are all pure functions (URL builders,
serde shapes, status classifiers). Nothing constructs a protocol client or drives
`create_session → get_message → acquire_job → complete_job`, so a refactor that
mis-wires backoff, reintroduces the 401-misclassification, or fails to terminate
a session passes CI green and is caught only in production or the out-of-repo
live fixture. This is the missing verification baseline (playbook "finding #1").
The clients already accept an injectable `base_url`, so they are seam-ready — the
only thing missing is a fake GitHub server to point them at. This harness unblocks
plans 011 and (the loop-level parts of) 019.

## Current state

- `crates/velnor-runner/src/protocol.rs` — the clients carry an injectable
  `base_url` (grep `base_url` in `protocol.rs`; the broker, run-service, and
  distributed-task clients each hold one, roughly lines 907/914/985/1200) and
  make real `reqwest` calls in `create_session`/`get_message`/`acquire_job`/
  `complete_job` with retry+backoff (`tokio::time::sleep` at
  `protocol.rs:535,1184,2860,2937`).
- **No** HTTP mock library is present: `crates/velnor-runner/Cargo.toml` has no
  `[dev-dependencies]` block; grep for `wiremock`/`mockito`/`httpmock`/`axum`/
  `TcpListener` in `src/` and `tests/` returns nothing.
- The one integration test, `crates/velnor-runner/tests/daemon_cli.rs`, only
  exercises `--dry-run-jit-config`, which skips polling.
- The request-shape assertions already in `protocol.rs` tests
  (`acquire_job_request_matches_run_service_shape`, the `broker_urls_match...`
  tests) document the exact payloads/routes the fake server should expect.

## Commands you will need

| Purpose      | Command                                                          | Expected              |
|--------------|------------------------------------------------------------------|-----------------------|
| Format       | `cargo fmt --all --check`                                        | exit 0                |
| Lint         | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0                |
| New harness  | `cargo nextest run -p velnor-runner --test broker_protocol --locked` | new tests pass    |
| All tests    | `cargo nextest run --workspace --locked`                        | all pass              |

## Scope

**In scope**:
- `crates/velnor-runner/Cargo.toml` — add a `[dev-dependencies]` block with one
  HTTP mock/server crate.
- `crates/velnor-runner/tests/broker_protocol.rs` (create) — the harness + tests.
- If a client field needs `pub(crate)` visibility to be constructible from a
  `tests/` integration file, either widen visibility minimally or add a
  `#[cfg(test)]`/`pub(crate)` constructor — prefer the smallest change.

**Out of scope**:
- Changing protocol behavior — this plan only observes it.
- The full slot loop (`run_slot`) — that is plan 019 / a follow-up; this harness
  targets the protocol client sequence.

## Git workflow

- Branch: `advisor/018-broker-runservice-test-harness`
- Commit: `test(protocol): fake GitHub broker/run-service harness + acquire→complete tests`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Choose and add the test server dependency

Prefer `wiremock` (async, ergonomic request matching) as a dev-dependency; if
the team prefers zero new deps, a hand-rolled `tokio::net::TcpListener` + minimal
HTTP responder is acceptable but more work. Add:

```toml
[dev-dependencies]
wiremock = "<latest>"
```

Note: the protocol clients use `curl` for some calls and `reqwest` for others
(broker) — the fake server must be reachable by **both** transports, i.e. a real
local HTTP endpoint (which `wiremock` provides). Confirm the client honors the
injected `base_url` for the calls you test (read the client constructors).

**Verify**: `cargo build --workspace --locked` → exit 0.

### Step 2: Drive the happy-path sequence

In `tests/broker_protocol.rs`, start a `wiremock` server, construct the broker
and run-service clients with `base_url` pointed at it, and script one happy path:
`create_session` → `get_message` (returns Idle) → `get_message` (returns a job)
→ `acquire_job` → `complete_job`. Assert the client issues the expected requests
(method, path, body shape — reuse the expectations from the existing
`*_matches_...` pure tests) and returns success.

**Verify**: `cargo nextest run -p velnor-runner --test broker_protocol --locked`
→ passes.

### Step 3: Drive the failure branches (the zombie-fleet regressions)

Add a table of failure responses and assert the client classifies each correctly:
- `get_message` → 401/403 (auth) ⇒ classified as an error (not "Idle/no
  message") — this is the original zombie-fleet bug; assert it is treated as an
  error/refresh path, not silently swallowed.
- `get_message` → empty body with non-2xx ⇒ error (not Idle).
- `acquire_job` → the non-retriable statuses vs retriable statuses behave per
  `is_retriable_completion_status` / the acquire classifier.
- `complete_job` → 5xx retries; 4xx (non-408/429) does not.

If plan 012 has landed, assert on `GitHubApiError.status`; otherwise assert on
the current error surface and add a TODO to tighten after 012.

**Verify**: `cargo nextest run -p velnor-runner --test broker_protocol --locked`
→ all pass; `cargo nextest run --workspace --locked` → all pass.

## Test plan

- Happy-path `create_session → get_message(Idle) → get_message(job) →
  acquire_job → complete_job`.
- Failure table: auth on poll, empty-body non-2xx on poll, acquire
  retriable/non-retriable, completion 5xx/4xx.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `crates/velnor-runner/Cargo.toml` has a `[dev-dependencies]` block with an HTTP test server
- [ ] `tests/broker_protocol.rs` drives the full acquire→complete sequence against a fake server
- [ ] The auth-on-poll case is asserted to be an error/refresh path, not "no message" (locks the zombie-fleet fix)
- [ ] `cargo fmt --all --check`, `cargo clippy ... -D warnings`, `cargo nextest run --workspace --locked` exit 0
- [ ] Only `Cargo.toml`, `Cargo.lock`, and the new test file (+ minimal visibility changes) modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The clients cannot be constructed from a `tests/` file even with minimal
  `pub(crate)` — report the visibility boundary.
- A client ignores the injected `base_url` for a call you need to test (hardcoded
  host) — report which call.
- `curl`-transport calls cannot be pointed at the local fake server — report;
  you may need to test the `reqwest`-path (broker) end-to-end and the
  curl-path calls at the request-building layer only.

## Maintenance notes

- This harness is the foundation for loop-level regression tests (plans 011, 019,
  and future protocol work). Keep the fake-server setup reusable (a helper
  function other test files can call).
- Reviewer: the highest-value assertion is the auth-on-poll-is-an-error case;
  ensure it genuinely exercises the classifier, not a stubbed path.
