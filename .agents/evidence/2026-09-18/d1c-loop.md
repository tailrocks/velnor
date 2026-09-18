# D1 part C evidence — 9-step processor loop + reconcile + fixtures

Branch: `feat/d1-processor-loop` (from `966f3623`, the `feat/d1-scaleset-protocol` tip)
Commit: `3f935eed` (signed off, pushed to origin)
Diff: 28 files, +6161/−24. No `worker/*` touched (d1b owns it).

## What landed

`crates/velnor-runner/src/scaleset/`, new: `scale` (the `Scale` step
function — the module list omitted it but the 9-step processor has to live
somewhere; named per upstream `Scaler`), `listener` (poll→Scale→ACK loop +
durable cursor store), `demand` (oldest-observed queue + trust gate),
`intents` (idempotency keys, fingerprints, acquire-batch + provision-intent
stores), `converge` (`WorkerLane` seam, provision intents, `TotalAssignedJobs`
convergence), `capacity` (C2 ledger port + reserve/advertise), `metrics`
(atomic counters), extended: `backoff` (idle-poll policy), `fixtures`
(transcript/seed loaders), `session` (unknown-kind capture), `mod`.
Model: `RunnerScaleSetMessage.unknown_message_types` (additive).
Control: migrations v22 (`scaleset_acquire_batches`,
`scaleset_provision_intents`) + v23 (`scaleset_demand.event_name`), replay
guards, fail-closed completeness checks. `rusqlite` moved to runner
`[dependencies]` (stores need it; kept in dev-deps for tests).

9-step order per poll: (1) idempotent observations (terminal replays
no-op; stake-less/untracked IDs counted, never claimed); (2) durable
submit (redelivery keeps age) + trust-gated oldest grant; (3) oldest-first
reserve + intent-before-HTTP; (4) returned-ID set reconcile
(acquired/missing/uncertain); (5) query-based provision intents (fresh +
redelivered retries); (6) ACK-after-success, ACK-then-persist-cursor;
(7) `TotalAssignedJobs` convergence (cached for nils); (8) free-headroom
advertisement (`None`→0, never N); (9) idle reconcile (observation
resolution, one overdue re-acquire per nil poll) + unknown-event counting.
Offer-validity guard vetoes the ACK unless every offer has a durable row.
Startup runs reconcile-before-advertise (stale-grant reset, native
passthrough + scale-set attestation, crash-orphaned `intended`→`uncertain`)
on the first poll and every epoch bump.

## Upstream fidelity (AGENTS.md)

Verified against `/tmp/upstream-scaleset` @ `fb563005` (`listener.go`):
initial synthetic message (ID −1, stats-only, never ACKed), `lastMessageID`
from 0 (query iff > 0), `Scale(nil)` on timeout, cursor+ACK only on success,
no concurrent Scale. Deliberate daemon deviations, all documented in
`listener.rs`: ACK-then-persist (durable cursor), free-headroom ads (spec
§5.1(8)), backoff-through-errors (no stop-on-error), 1s nil floor
(instant-202 hot-spin guard). Capacity semantics (free vs upstream's
maxRunners total) need canary confirmation before the estate proof.

## Ledger port (C2 integration)

This base predates the C2 merge, so `capacity::CapacityLedger` mirrors the
C2 `PermitLedger` API method-for-method (names, outcomes, fencing,
reconcile-never-deletes, `advertised_free` None-until-reconciled), read
from `deb9e204` git objects. Integration = thin `PermitLedger` adapter +
swap `MemLedger`; no call-site changes. `is_stale_generation` is the one
addition (generic fenced retry needs it). v21 `job_permits` is NOT used —
the C2 ledger is the one capacity authority.

## Bugs found by the new tests (fixed in-commit)

- Convergence counted `granted` rows as population → granted backlog at
  `local == desired` held forever (liveness). `granted` excluded from
  `local_population`; startup attestation still includes it (retention).
- v23 `ALTER TABLE` broke replay-idempotence tests → `has_column` skip
  guard following the v20 precedent.
- pid-only temp dirs flaked across runs via pid reuse → all loop test
  helpers wipe their dir first (deterministic empty DB).

## Tests (all observed green)

- `cargo test -p velnor-runner --lib`: 2247 pass, 4 ignored (2 consecutive
  full runs; earlier single flake was the temp-dir issue above, fixed).
- `cargo test -p velnor-runner --features test-support --test
  scaleset_loop`: 9 pass (fixtures+transcripts, deferred N=0, redelivery,
  partial, uncertain→idle-resolve, 401-refresh, stale seed, reorder,
  unknown-kind). `--test scaleset_protocol`: 8 pass (part A intact).
- `cargo test -p velnor-model -p velnor-control`: all suites ok (138 model
  lib, 272 control lib incl. 4 new migration tests).
- `cargo clippy -p velnor-runner -p velnor-model -p velnor-control
  --all-targets` and runner `--features test-support`: 0 warnings.
- `cargo fmt --check`: clean.

## Fixtures (21 files, all verified)

New: `message_deferred_offer.json` (+ byte-identical
`message_redelivered.json`), `message_reordered.json`,
`message_unknown_kind.json`, `message_high_water.json`,
`acquire_partial.json`, `session_refreshed.json`,
`transcript_nil_polls.json`, `transcript_redelivery.json`,
`seed_stale_generation.json`. Manifest regenerated; redaction verifier
passes on load (fingerprint/`REDACTED`/`.invalid` only).

## Open follow-ups (for parent/d1b)

- Trust enrichment: PR/`workflow_run` offers park in `observed` forever
  (fail closed, no head-of-line block). Granting them needs head-repo
  inputs (job fetch at acquire time, or operator policy) — a later slice.
- `Declined` demand state unused in C (no wire cancellation signal
  pre-acquire); reserved for policy blocks.
- Missing-ID churn: re-queued `eligible` rows retry every poll until the
  job completes elsewhere; negative caching is a possible later bound.
- Daemon wiring (`run()` + real d1b lane + C2 adapter + node scheduler
  gate) lands at integration, not in this branch.
