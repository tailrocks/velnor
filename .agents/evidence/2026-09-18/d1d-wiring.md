# D1 part D: daemon wiring — evidence

- Branch: `feat/d1-daemon-wiring` (from `origin/docs/bastion-final-plan`)
- Commit: `9d7b6dfd` (signed off, pushed to origin)
- Scope: wire the scale-set adapter into the `velnor-runner` daemon.
  Native path untouched, no Go, no second scheduler.

## What was wired

- Registration adapters: scope/group/set registration + reconciliation
  (`scaleset/registration.rs`).
- Session creation/renewal management (`scaleset/session.rs` via daemon).
- Demand observation → queue submission; capacity advertisement from
  shared grants (`scaleset/shared_ledger.rs`, `demand.rs`, `capacity.rs`).
- Start/completion event handling through the 9-step processor loop.
- Graceful shutdown + crash recovery: reconcile-before-advertise,
  adopt-or-fail active work, never lost / never false-success
  (`scaleset/daemon.rs`, `lane.rs`).
- Key material loading from 0600 files/env without logging
  (`scaleset/key_material.rs`, redacted `Debug` on secret structs).

## Bugs fixed during wiring

1. **JIT fetch self-deadlock (hang).** The lane drove the async JIT
   client via `std::thread::scope` + `Handle::block_on` on a tokio
   worker. When the remaining workers were condvar-parked (none driving
   the shared I/O/time driver), the `block_on` future's I/O and the test
   thread's sleep timer could never fire: total, stable deadlock.
   Structural fix: `WorkerLane::provision` is now `async` end-to-end
   (`converge::ensure_provision_intent`, `scale::provision_pass`,
   `lane::fetch_jit` awaited inline); the lane's `tokio::Handle` field
   is gone. `fetch_jit` borrows only the `Sync` client so the spawned
   run task stays `Send` despite the lane's `!Sync` SQLite handles.
2. **Permit resurrection after adoption.** Adoption releases a dead
   worker's permit while its demand row is still active, so the next
   startup reconcile re-attested the permit from demand — and nothing
   ever released it again (`occupied` stuck at 2). Fix: the terminal
   path's `PermitReleased` arm now releases a held permit, converging to
   the recorded row truth (mirrors the unknown-worker arm).
3. **Session-close retry expectation.** The test asserted 1 close attempt
   against a persistent 500, but upstream `MessageSessionClient.Close`
   rides the shared retryable client (`retryMax=4`, `DefaultRetryPolicy`
   retries 500 — verified against `actions/scaleset@e6daac70`
   `session_client.go`/`common_client.go`). Implementation matches
   upstream (5 attempts); test now asserts 5 and keeps the best-effort
   assertions (run succeeds, triage reported).

## Tests

- `cargo test -p velnor-runner -p velnor-model`: 2511 passed, 0 failed.
- `cargo test -p velnor-runner --features test-support`: all green
  (lib 2379 + all integration targets, incl. `scaleset_daemon` 11/11:
  e2e one-job serve, restart adoption without reprovision, crash with
  dead workers failing explicitly, cleanup-failure veto, session-close
  best-effort, registration cases).
- `daemon_serves_one_job_end_to_end` run 10× consecutively: 10/10 pass
  (~0.08s each; previously hung indefinitely).
- `cargo clippy -p velnor-runner -p velnor-model --all-targets
  --features velnor-runner/test-support`: zero warnings.
- `cargo fmt -p velnor-runner -p velnor-model --check`: clean.
- All debug instrumentation (`LANE-HB`/`TEST-HB`, `blockon_probe.rs`)
  removed from the tree.

## Note for follow-ups

`WorkerRunner::run` (Docker CLI) stays synchronous on the worker during
provision/cleanup. It cannot self-deadlock the runtime (no runtime
dependency), but each in-flight `docker` call occupies a worker thread;
if provision latency grows, move it behind `spawn_blocking`.
