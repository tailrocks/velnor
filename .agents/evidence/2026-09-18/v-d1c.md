# d1c-loop verification — CERTIFIED

Branch `feat/d1-processor-loop`, commit `3f935eed` over base `966f3623`
(d1a tip, single commit on top). Fetched origin, inspected full diff in
scratch worktree `/tmp/v-d1c-work` (detached at `3f935eed`, left pristine;
probe file removed after run). No merges, no pushes, no repo edits.

## Scope

- Diff: 28 files, +6161/−24 — matches evidence claim exactly.
- `worker/*` untouched (0 paths): d1b seam respected; loop talks to d1b
  only through `converge::WorkerLane` (intent-before-lane in
  `ensure_provision_intent`).
- Commit signed off (`Signed-off-by: Alexey Zhokhov`).
- Pin untouched by d1c (`e6daac70…`, from d1a); v-d1a already proved it
  Go-identical to `fb563005`, so "verified against fb563005" is sound.

## 9-step order (spec §5.1) — all present, in order

`scale.rs` (steps 1–5+7): (1) assigned→started→completed folding,
terminal replays no-op, untracked/stake-less IDs counted never claimed;
(2) durable submit (redelivery keeps `first_seen_at`+`sequence`, terminal
never revived) + trust-gated oldest grant; (3) oldest-first reserve +
`record_intended` BEFORE HTTP; (4) `reconcile_returned_ids` set-reconcile
(acquired/missing/uncertain), transport failure → durable uncertain;
(5) query-based provision intents (fresh + redelivery retries);
offer-validity guard vetoes ACK on any missing row; (7) converge on
`TotalAssignedJobs` (cached for nil polls).
`listener.rs` (steps 6/8/9): ACK-then-persist-cursor only on `Ok`;
free-headroom advertise (`None`→0, never N) computed before each poll;
idle reconcile on nils + unknown-event counting, backoff-through-errors.

## Upstream fidelity (checked against `/tmp/upstream-scaleset`)

- `INITIAL_MESSAGE_ID = -1`, stats-only, never ACKed/cursor — matches
  `listener.go:99,139`.
- Cursor from 0, `lastMessageId` query iff > 0 (`session_client.go:138`
  ↔ d1a `session.rs:209`, intact), `Scale(nil)` on 202, cursor+ACK only
  on success, `Scale` never concurrent (`&mut self`).
- `TotalAssignedJobs` as desired count — matches example
  `dockerscaleset/scaler.go:61`; `desired_runners()` = that field only.
- `granted` excluded from `local_population` (liveness), included in
  startup attestation (retention) — both directions read correct.
- Deviations (ACK-then-persist, free-headroom ads, nil floor,
  backoff-through-errors) all documented in `listener.rs` header.

## Durability / fencing / reconcile

- Intent-before-effect on both crash windows (acquire batch, provision
  intent); stable keys (`permit_holder`, `provision_operation_id`,
  `provision_ownership_id`, deterministic `runner_name`); batch retries
  adopt recorded open batch.
- Generation fencing: `reset_stale_grants` at grant-pass + startup,
  fenced reserve/transition with re-read-and-retry-once, `DoubleStale`
  fail-closed.
- Reconcile-before-advertise on first poll and every epoch bump
  (`reconciled_generation` starts `None`); native holders pass through
  untouched; crash-orphaned `intended`→`uncertain`; reconcile never
  deletes (adopt/mark-uncertain); one overdue re-acquire per nil poll.
- Ledger port mirrors C2 `PermitLedger` (read `deb9e204` objects):
  identical `AcquireOutcome`/`ReconcileReport`/7-state/`LedgerError`
  shapes and method names; `is_stale_generation` the one addition.
- Migrations v22 (intent tables) + v23 (`event_name`) with replay
  `has_column` guard and fail-closed `v22/v23_schema_complete` gates;
  fail-closed verified by migration tests.

## Fixtures (21 manifest entries + manifest)

Deferred offer (push+PR pair, desired=2), byte-identical redelivery
(`cmp` IDENTICAL, same sha), reordered completed/started/assigned,
unknown `JobMigrated` kind + live offer, high-water cursor probe (id 41),
partial acquire, refreshed session, nil-poll + redelivery transcripts,
stale-generation seed. Only `.invalid` hosts, `REDACTED` tokens, no
`ghp_/ghs_/PEM/real-host` strings — redaction verifier passes on load.

## Disprove attempts (all failed to break it)

- Independent probe (direct `Processor` drive, second method vs
  Listener+wiremock, since removed): redelivery → 0 re-submits, still
  exactly 1 acquire call, occupancy 1, age/sequence immutable;
  completed-without-assigned → terminal + cleaning + single lane call;
  late assigned → tracked no-op, terminal never revived. 2/2 passed.
- Convergence: `AcquireMore{headroom}` below desired, `Hold` at/above
  (never kills), granted backlog still converges — matches upstream
  desired-count semantics.
- Out-of-order wire batch folds to the same terminal state regardless
  of completion/started/assigned arrival order (fixed fold order).

## Test evidence (scratch worktree, this session)

- `cargo test -p velnor-runner --lib`: 2247 pass, 4 ignored.
- `--features test-support --test scaleset_loop`: 9 pass.
- `--features test-support --test scaleset_protocol`: 8 pass (part A intact).
- `cargo test -p velnor-model -p velnor-control`: 11 suites ok, 0 failures.
- `cargo clippy -p velnor-runner -p velnor-model -p velnor-control
  --all-targets` + runner `--features test-support`: 0 warnings.
- `cargo fmt --check`: clean.

## Verdict: CERTIFIED

The branch implements the §5.1 9-step loop as specified, with durable
intents, idempotent replay, generation fencing, reconcile-before-
advertise, sanitized fixture coverage of every named case, and no scope
creep into `worker/*`. Follow-ups in `/tmp/d1c-loop.md` (trust
enrichment, unused `Declined`, missing-ID churn, daemon wiring) are
accurately disclosed and out of scope for this slice.
