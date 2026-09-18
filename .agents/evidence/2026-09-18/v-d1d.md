# d1d-wiring verification — CERTIFIED

Branch `feat/d1-daemon-wiring`, commit `9d7b6dfd` (signed off, on origin).
Verified in scratch worktree `/tmp/v-d1d-wt` (detached HEAD 9d7b6dfd).
Spec: `plans/bastion-three-provider-ci/spec.md` §5; design `/tmp/d1-design.md`;
claim `/tmp/d1d-wiring.md`. No edits, no merges, no pushes.

## Verdict: CERTIFIED

Every claimed wire exists and behaves per spec §5.1–§5.2/§5.4.

## What was checked (commit vs its own parent `9d7b6dfd^`)

Own-parent diff is 28 files, all in scope (scaleset/ + runner/args/service/
permit_guard wiring + velnorctl flag plumbing + tests). The extra files in an
`origin/docs/bastion-final-plan..9d7b6dfd` diff are base drift (security-audit
merge after fork), not d1d.

- Registration scope/group/set + reconcile (`registration.rs`, new, 353 lines):
  group by pinned ID or name (fail-closed), set adopt-by-ID with group-mismatch
  refusal or get-or-create-by-name with 409 create-race adoption, label PATCH
  only on drift, empty plan never strips, never deletes. Called first in
  `ScaleSetDaemon::start`, before adoption/session. 4/4 registration tests.
- Session create/renew (`session.rs` via daemon): `start()` creates
  `MessageSessionClient`, loop polls with 401→refresh-once+retry on
  get/acquire/delete; `run()` closes best-effort on shutdown. Close rides the
  retryable client: persistent-500 test asserts 5 attempts (1+retryMax=4) and
  run still succeeds. Matches upstream-mirror claim.
- Demand→queue: `Processor` wired with `DemandStore`/`AcquireBatchStore`/
  `ProvisionIntentStore` in `start()`; e2e proves
  offer→grant→acquire→provision→assigned→started→terminal with 4 ACKs and
  cursor 23 persisted.
- Capacity from shared grants (`shared_ledger.rs`, new): `CapacityLedger` over
  the ONE C2 `PermitLedger` file, pid `None` for containers, no resize/re-epoch
  API. Listener advertises `advertise_free` (N−occupied both lanes) per poll
  after reconcile. Cross-lane test: native-held N=1 advertises "0", queues
  without acquiring, ACKs durably; after native release advertises "1" and
  acquires/provisions under unified `scaleset/<set>/<request>` namespace.
- Start/completion events: `scale.rs` calls `lane.note_assigned/note_started/
  note_terminal`; lane advances records, ticks supervision, drives
  terminal→diagnostic_export→owned_cleanup→permit release, cleanup failure
  retains `uncertain` and vetoes ACK (redelivery retries).
- Graceful shutdown + crash recovery: SIGTERM drain watcher + shutdown flag,
  session close, `shutdown_pass` (healthy left running, dead failed
  explicitly). Startup order: `init_host_permit_ledger` (epoch bump + ONE
  dual-lane reconcile with demand attestation) → lane start (adopt) → slot
  supervision. Listener re-reconciles before first advertisement on every new
  epoch. Kill-recovery test: dead-but-present worker failed explicitly with
  exported diagnostics + released permit; vanished worker keeps tracked +
  `uncertain` permit; neither demand row completes without a GitHub
  observation (no lost job, no false success); redelivered completions converge
  stably; set never deleted. Restart test: adoption without reprovision
  (creates unchanged, JIT fetched once), cursor resumes at lastMessageId=20,
  demand age preserved.
- 0600 key loading, no logging (`key_material.rs`, new): file must be 0600 or
  stricter (0640/0644 rejected naming path only), env or file by reference,
  config has no inline-secret field by construction, PAT placeholder rejected.
  `Debug` redacted on `GitHubAppAuth`, `PemJwtProvider`,
  `InstallationAccessToken`, `AdminToken`, `RunnerSpec` (JIT), each with a
  leak test. Grep over new code: no secret reaches `tracing!`/`println!`/
  `format!` except header construction (Bearer header assembly, not output)
  and JIT-env argv assembly (passed to Docker, never printed).
- NO Go (zero `.go` files), no second scheduler (`node/scheduler.rs`
  untouched, `ScaleSetV2.ensure_current` still errors, no scheduler file in
  diff), native untouched (lane `None` when unconfigured; native-only
  reconcile byte-equivalent via moved `startup_reconcile`, covered by renamed
  test; no `docker.sock`/TCP in job argv asserted e2e).
- Bug-fix claims hold: `provision` async end-to-end, no `Handle` field, no
  `block_on` in scaleset/; permit-resurrection fix present in terminal path
  (`PermitReleased` arm releases re-attested permits); session-close test
  asserts 5 attempts. No `LANE-HB`/`TEST-HB`/`blockon_probe` remnants.

## Test evidence (observed in `/tmp/v-d1d-wt`)

- `cargo test -p velnor-runner -p velnor-model`: all green, 0 failures
  (runner lib 2310 + model 138 + integration targets).
- `cargo test -p velnor-runner --features test-support`: all green —
  lib 2379 passed / 5 ignored, `scaleset_daemon` 11/11.
- `daemon_serves_one_job_end_to_end` ×10: 10/10 pass (0.07–0.49s each).
- `cargo clippy -p velnor-runner -p velnor-model --all-targets --features
  velnor-runner/test-support`: zero warnings. `cargo fmt --check`: clean.

## Finding (minor, non-blocking)

- `crates/velnor-runner/src/scaleset/lane.rs` module doc lines 26–29 still
  describe the pre-fix design ("lane is synchronous … fetch runs on a scoped
  thread driving the daemon runtime handle"). The code is async end-to-end and
  correct; only the doc paragraph is stale. Suggest a follow-up edit to the
  doc; not a contract mismatch.
