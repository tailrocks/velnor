# D1 design — Rust Scale Set adapter (bastion campaign, spec §5)

Status: design doc, no code. Inputs: `plans/bastion-three-provider-ci/spec.md` §4–§6,
`/tmp/d1-gapmap.md`, `/tmp/upstream-scout.md` (actions/scaleset @ `fb563005`, actions/runner v2.337.0).
Upstream pin for D1: `fb563005` (re-pin `SCALESET_UPSTREAM_COMMIT` from `cb0405b2`; gapmap PIN-DRIFT).

Grounding (existing code this design builds on):
- Durable journal: `velnor_control::journal::{Journal, Event, JobRecord}`, SQLite `journal.db`
  (runner side: `node/complete.rs` intent/commit/claim/ack pattern, `node/cleanup.rs` outbox recovery).
- Control store: `velnor-control/src/store/{migrations.rs,records.rs}` SQLite tables (slots, jobs,
  transitions, lifecycle_operations, event_stream_state) — migration-numbered schema.
- Model: `velnor-model/src/{scheduler.rs,lifecycle.rs,node.rs}` (`RunnerScaleSetStatistic`,
  `JobState` transition table, `Generation` fencing).
- Protocol clients: `runner/src/protocol.rs` (`BrokerClient`, `DistributedTaskClient`,
  `RunServiceClient`, `RegistrationClient::generate_jit_config`, `OAuthClient`, rate-limit helpers).
- Trust: `runner/src/trust_class.rs` (`TrustClass::derive`, `AdmittedTrust::admit`),
  `runner/src/admission.rs:497 admit_job`, `node/prove.rs` GitHub probes.
- Native Docker: `docker_lease.rs`, `container/`, `docker/` (mediated host-backed; NOT DinD per §5.4).

Non-goals (spec): no Go controller, no K8s/VM, no per-repo daemon, no second scheduler, no native
rewrite to DinD, no raw host socket into jobs, no CPU/RAM ceilings anywhere (§4.3).

---

## 1. Module layout

New subtree `crates/velnor-runner/src/scaleset/` (adapter lives in runner crate next to `node/`;
wire types that fixtures/tests share go in `velnor-model`). Rationale: the adapter is a protocol
lane of the runner daemon, reusing `protocol.rs` HTTP/auth helpers, journal, trust, and Docker
primitives; model crate holds only serde wire shapes + statics (mirrors existing `scheduler.rs` split).

```text
crates/velnor-model/src/
  scheduler.rs            EXTEND: re-pin commit; add wire types below (serde only, no I/O)
    RunnerScaleSetJobMessage::{Available,Assigned,Started,Completed} (types.go L47ff)
    RunnerScaleSetMessage (dispatched batch: MessageID, Statistics, 4 vecs)
    AcquireJobsResponse{count,value}, RunnerScaleSetJitRunner{Setting,Config}
    ScaleSetSession{MessageQueueURL,MessageQueueAccessToken,Statistics}
    ScaleSetWorkerState (the §5.2 lifecycle enum, §4 below)

crates/velnor-runner/src/scaleset/
  mod.rs                  public surface: Config, ScaleSetAdapter::run(), health hooks
  upstream_pin.rs         pinned commit const + fixture-manifest hash check (fail closed on drift)
  client.rs               admin-plane client: NewClientWithGitHubApp/PAT/JWTProvider equivalents,
                          token chain getRunnerRegistrationToken→getActionsServiceAdminConnection→
                          fetchAccessToken, scale-set CRUD, GenerateJitRunnerConfig
                          (mirrors scout §7 client.go L167-L1111)
  session.rs              MessageSessionClient: create/refresh(PATCH on 401)/close,
                          GET MessageQueueURL?lastMessageId= + X-ScaleSetMaxCapacity,
                          202→None, DELETE ack (mirrors session_client.go L39-L262)
  listener.rs             poll→Scale→ACK-after-success loop, lastMessageID tracking,
                          initial synthetic message (InitialMessageID=-1), never-concurrent Scale
                          (mirrors listener.go @ fb563005 L121-L126; single-method Scale trait)
  scale.rs                Scaler impl: the §5.1 9-step processing function (§3 below)
  demand.rs               global oldest-observed queue: first_seen_at + immutable sequence,
                          trust-before-grant gate (wraps trust_class + admission)
  allocator.rs            host-wide max_jobs=N permit ledger (§6; shared with native lane)
  intents.rs              journal event constructors + idempotency-key builders
                          (acquire-intent, provision-intent, stable op/ownership IDs)
  converge.rs             TotalAssignedJobs convergence: desired vs local population,
                          JIT-provision decisions for acquired-but-unprovisioned
  capacity.rs             per-session capacity advertisement derivation
                          (shared grants + commitments → X-ScaleSetMaxCapacity value)
  backoff.rs              poll backoff/rate-limit (reuse protocol.rs GitHubRateLimitStatus +
                          github_api_retry_delay; 202/401/429/5xx policy)
  worker/
    mod.rs                ScaleSetWorkerState machine + transition table (§5.2 lifecycle)
    dind.rs               private DinD provisioning: daemon container, data dir, socket identity,
                          readiness probe, BuildKit cache export/import
    runner.rs             official runner container: digest-pinned image, JIT config injection
                          (env/file, never App keys), shared-netns pairing with DinD,
                          identical-absolute-path binds, connected/running detection
    supervise.rs          runner+DinD supervision: death detection, restart-vs-fail policy,
                          owned-cleanup ordering, diagnostic export before deletion
    ownership.rs          ownership identity records: container/network/volume/workspace names,
                          labels, inner-name/port collision isolation
  reconcile.rs            startup + idle-poll + unknown-event reconciliation (§8)
  fixtures.rs             recorded/sanitized fixture loader + redaction verifier (test + canary use)
  metrics.rs              adapter telemetry: polls, acks, acquires, provisions, capacity, journal lag

crates/velnor-runner/src/node/
  scheduler.rs            EXTEND: ScaleSetV2 activation gate (replace ensure_current block with
                          estate-proof gate once D1 proves equivalence); keep per-slot default

crates/velnor-control/src/
  journal.rs (or journal/) EXTEND: new Event variants + materialized state (§2)
  store/migrations.rs     EXTEND: new migration adding §2 tables (queue/ledger/sessions)

crates/velnor-runner/tests/scaleset/     fixture layout + conformance tests (§7)
```

Why `scaleset/` under runner and not a new crate: the adapter shares the daemon's journal handle,
Docker client, credential providers, and permit ledger in-process (spec §1: one product, one
capacity authority; "useful guardian/controller/job-process isolation preserved" — process split, if
any, comes later via existing guardian/controller boundaries, not a new crate boundary now).

---

## 2. Durable journal schema

### 2.1 Decision: extend the existing Journal event log, add query tables in control store

Two stores already exist and each keeps its job:
- `velnor_control::journal::Journal` (`journal.db` event log + `materialized_state()`): the
  transactionally-ordered source of truth for intents and lifecycle edges. All §5.1 steps 1–6
  effects land here first (replay-safe, generation-fenced, checksummed payloads via
  `payload_checksum`).
- `velnor-control` store tables (migrations.rs): indexed query projections (queue ordering,
  ledger counting, session resume). Projections are rebuilt from the journal on demand; on
  conflict the journal wins.

No third store. No in-memory-only queue/ledger/session state that restart could erase (§4.1:
"occupied work is never erased by resetting a semaphore").

### 2.2 New Journal events (append to `Event` enum; all carry `generation: Generation`)

```text
ScaleSetSessionOpened   { scale_set_id, session_id, owner, queue_url_hash, generation }
                        // NOTE: queue URL + tokens are secret-adjacent; journal stores hashes/
                        // fingerprint only. Live tokens live in the credential provider (§4).
ScaleSetSessionClosed   { scale_set_id, session_id, generation }
ScaleSetMessageSeen     { scale_set_id, session_id, message_id, statistics }   // poll cursor
ScaleSetOfferObserved   { request_id, scale_set_id, first_seen_at, sequence,
                          repo_owner, repo_name, job_id, labels_hash, queue_time } // step 2
ScaleSetOfferGranted    { request_id, permit_id, generation }                    // step 2/3 edge
ScaleSetOfferDeclined   { request_id, reason }        // ineligible/cancelled/blocked + reason (§4.2)
ScaleSetAcquireIntended { batch_id, request_ids[], permit_ids[], generation }    // step 3
ScaleSetAcquireResolved { batch_id, acquired_ids[], missing_ids[], uncertain: bool } // step 4
ScaleSetProvisionIntended{ request_id, operation_id, ownership_id, runner_name,
                          runner_digest, dind_digest, jit_fingerprint, generation }  // step 5
ScaleSetWorkerEdge      { ownership_id, from, to }    // §5.2 lifecycle edges (§5)
ScaleSetMessageAcked    { scale_set_id, session_id, message_id }                   // step 6
ScaleSetStatsObserved   { scale_set_id, statistics, at }  // step 7 (every poll, incl. 202/empty)
ScaleSetPermitReleased  { permit_id, reason }         // terminal+cleanup confirmed OR visible
                                                  // retained reservation on cleanup failure (§4.1)
```

Materialized state additions: `occupied_permits: Map<PermitId, Occupant>`,
`offers: Map<RequestId, Offer>`, `acquire_batches: Map<BatchId, BatchState>`,
`workers: Map<OwnershipId, WorkerState>`, `sessions: Map<ScaleSetId, SessionCursor{session_id,
last_message_id, generation}>`, `demand_sequence: u64` (monotonic allocator for tie-breakers).

### 2.3 New control-store tables (one migration, e.g. `m027_scaleset`)

```sql
-- Global oldest-observed queue projection (§4.2). One row per offered request.
CREATE TABLE scaleset_demand (
  request_id      INTEGER PRIMARY KEY,      -- GitHub runnerRequestId
  scale_set_id    INTEGER NOT NULL,
  first_seen_at   TEXT NOT NULL,            -- RFC3339, immutable; redelivery keeps original
  sequence        INTEGER NOT NULL UNIQUE,  -- immutable tie-breaker from journal counter
  state           TEXT NOT NULL,            -- observed|eligible|reserved|acquire_intent|
                                            -- acquired|uncertain|provision_intent|declined|terminal
  decline_reason  TEXT,                     -- set iff declined
  repo_owner      TEXT NOT NULL, repo_name TEXT NOT NULL,
  job_id          INTEGER NOT NULL,
  labels_hash     TEXT NOT NULL,
  generation      INTEGER NOT NULL,
  updated_at      TEXT NOT NULL
);
CREATE INDEX idx_scaleset_demand_order ON scaleset_demand(state, first_seen_at, sequence);

-- Host-wide permit ledger (§4.1), shared with native lane (§6).
CREATE TABLE job_permits (
  permit_id    TEXT PRIMARY KEY,            -- ulid/uuid
  lane         TEXT NOT NULL,               -- 'scaleset' | 'native'
  state        TEXT NOT NULL,               -- reserved|acquiring|provisioning|assignable|
                                            -- running|cleaning|uncertain|released|retained_failed
  owner_ref    TEXT NOT NULL,               -- request_id (scaleset) or claim id (native)
  generation   INTEGER NOT NULL,
  created_at   TEXT NOT NULL, updated_at TEXT NOT NULL
);
-- N enforced by COUNT(*) WHERE state NOT IN ('released') < N inside a write txn;
-- 'retained_failed' rows keep occupying until operator resolves (never auto-release).

-- Session resume cursors (tokens NOT stored; fingerprints only).
CREATE TABLE scaleset_sessions (
  scale_set_id   INTEGER PRIMARY KEY,
  session_id     TEXT NOT NULL,
  owner          TEXT NOT NULL,
  last_message_id INTEGER NOT NULL DEFAULT 0,
  stats_json     TEXT,                      -- last authoritative Statistics
  generation     INTEGER NOT NULL,
  updated_at     TEXT NOT NULL
);

-- Worker registry: stable op/ownership IDs → Docker identities (§5.2/§5.3).
CREATE TABLE scaleset_workers (
  ownership_id   TEXT PRIMARY KEY,          -- stable across restarts; idempotency key
  operation_id   TEXT NOT NULL UNIQUE,      -- provision operation idempotency key
  request_id     INTEGER,                   -- NULL until bound to an acquired job
  runner_name    TEXT NOT NULL UNIQUE,
  runner_container_id TEXT, dind_container_id TEXT,
  network_name   TEXT, workspace_path TEXT, dind_data_path TEXT,
  runner_digest  TEXT NOT NULL, dind_digest TEXT NOT NULL,
  worker_state   TEXT NOT NULL,             -- §5.2 lifecycle value
  generation     INTEGER NOT NULL,
  created_at     TEXT NOT NULL, updated_at TEXT NOT NULL
);
```

Keys summary: `request_id` (GitHub `runnerRequestId`) keys demand; `permit_id` keys capacity;
`(batch_id)` keys acquire reconciliation; `(operation_id, ownership_id)` key idempotent
provisioning; `(scale_set_id)` keys session cursor; `sequence` + `first_seen_at` order grants.

Redaction rule: journal and tables store hashes/fingerprints of URLs, tokens, labels, JIT blobs —
never raw secrets. `fixtures.rs` redaction verifier asserts this on every recorded fixture (§7).

---

## 3. §5.1 9-step processing loop → functions

One poll iteration = `listener::run_once()` → `scale::process_message()` (the `Scale` impl).
`Scale` is never concurrent (upstream listener.go L126); each step is a pure function over
(journal txn + inputs) returning durable effects, so crash at any point replays safely.

```text
listener::run_once(sess, last_id, max_cap)
  ├─ session::get_message(last_id, max_cap)          // GET + capacity header; 202→None; 401→refresh+retry once
  ├─ stats_observe(msg.stats)                        // ALWAYS, even on None (step 7 input)
  ├─ if msg.is_none() → reconcile::idle_poll()       // step 9 (also on every Nth empty poll)
  └─ if msg → scale::process_message(journal, alloc, demand, msg)  // steps 1–7
        1.umen observé: ingest::apply_observations()        // §5.1(1)
        2. demand::submit_offers() + demand::grant_oldest() // §5.1(2)
        3. allocator::reserve() + intents::record_acquire() // §5.1(3)
        4. session::acquire_jobs() → intents::resolve_acquire() // §5.1(4)
        5. converge::ensure_provision_intent() → worker::provision() // §5.1(5)
        6. (deferred) session::delete_message() after durability  // §5.1(6)
        7. converge::reconcile_population(stats)         // §5.1(7)
     then listener ACKs via session::delete_message(WithoutCancel) ONLY on Ok  // step 6
capacity advertisement recomputed per poll: capacity::advertise()               // step 8
```

Step → function contract:

1. `ingest::apply_observations(journal, msg) -> Result<Observed>` — fold `JobAssigned/Started/
   Completed` into `ScaleSetWorkerEdge`/`ScaleSetMessageSeen` events idempotently (replay of an
   already-seen `message_id` or terminal edge is a no-op via `JobState::is_terminal` +
   `transition_target` legality table). Reconcile durable occupied state: any `request_id` in
   `JobCompleted` whose worker row is not terminal → drive worker machine to `terminal`
   (diagnostic export + cleanup scheduled, permit NOT yet released). Unknown `messageType` →
   record `ScaleSetMessageSeen`, then `reconcile::unknown_event()` (step 9), never panic.
2. `demand::submit_offers(journal, available[]) -> Result<()>` — for each `JobAvailable`, insert
   `scaleset_demand` row iff `request_id` absent (redelivery retains original `first_seen_at`;
   allocate `sequence` from journal counter once). Then `demand::grant_oldest(journal, alloc)` —
   peek oldest `eligible` row by `(first_seen_at, sequence)`; run trust gate
   `TrustClass::derive` → `AdmittedTrust::admit` (+ `admit_job` action-graph check where the
   offer carries enough metadata; offers lacking trust inputs stay `observed` with reason, never
   granted blind); on pass emit `ScaleSetOfferGranted`, else `ScaleSetOfferDeclined{reason}`.
   Grants are generation-fenced: a grant from a stale generation is rejected on use.
3. `allocator::reserve(journal, owner_ref, lane) -> Result<PermitId>` — single-writer txn:
   `SELECT COUNT(*) FROM job_permits WHERE state NOT IN ('released')`; if `< N`, insert
   `reserved` row + `ScaleSetOfferGranted`; else return `CapacityExhausted` (offer stays queued,
   keeps age). Then `intents::record_acquire(journal, batch)` persists
   `ScaleSetAcquireIntended{batch_id, request_ids, permit_ids}` BEFORE any HTTP call.
4. `session::acquire_jobs(request_ids) -> AcquireJobsResponse` then
   `intents::resolve_acquire(journal, batch_id, resp)` — set-reconcile: `acquired = resp.value ∩
   requested`; `missing = requested − resp.value` (never assume all succeeded); transport
   error/timeout after send → `uncertain=true`, permits stay `acquiring` (still counted, §4.1).
   Acquired IDs → demand state `acquired`; missing → permits released (single release path §6),
   demand back to `eligible` with preserved age (re-offerable); uncertain → `uncertain`, resolved
   by later `JobAssigned` observation or idle reconcile (step 9), never double-spend.
5. `converge::ensure_provision_intent(journal, acquired_id)` — persist
   `ScaleSetProvisionIntended` with stable `operation_id = f(scale_set_id, request_id, attempt)`,
   `ownership_id = f(scale_set_id, runner_name)` BEFORE Docker calls; `worker::provision()` is
   idempotent on `(operation_id, ownership_id)`: if containers exist with matching ownership
   labels → adopt, else create. `runner_name` stable per ownership (`<set>-<seq>` from journal
   counter, never random per retry).
6. ACK rule (enforced in `listener.rs`, not in `scale.rs`): `session::delete_message(message_id)`
   only after `process_message` returns `Ok` AND its journal txn committed (replay-safe effects
   durable). ACK need not wait for job completion. Offer-validity guard: an ACK must never drop
   the only reference to deferred work — `process_message` proves every `JobAvailable` in the
   batch reached a durable terminal-for-this-message state (`declined`, `acquired`, `uncertain`,
   or re-queued `eligible` with preserved age) before returning `Ok`; otherwise return `Err` →
   no ACK → redelivery. Session-boundary guard: on session refresh/close, un-ACKed messages are
   expected-redelivered; `ScaleSetMessageSeen(message_id)` dedupes.
7. `converge::reconcile_population(journal, stats)` — `desired = stats.desired_runners()`
   (`TotalAssignedJobs`, authoritative; never batch counts, never stats+reservations double
   count). Compare against local `workers alive + acquiring + provisioning + uncertain` for this
   set; if local < desired and demand has granted-but-unacquired offers → next poll acquires;
   if local > desired → no new acquires/provisions (drain by completion; never kill running to
   chase the number down — GitHub assignment is truth, local tickets are not).
8. `capacity::advertise(journal) -> u32` — per poll, per session: `free = N − occupied_global`
   (occupied = all non-released permits across BOTH lanes + retained_failed). Advertise
   `X-ScaleSetMaxCapacity` per verified upstream semantics (total-vs-free verified against
   fb563005 poll path + live canary §7; initial hypothesis from scout: max capacity the listener
   can take, i.e. free headroom — D1 must confirm by experiment, not assume). Never `N` per
   listener. Races: value computed inside the same critical section as `reserve()` where
   possible; advertisement is advisory, the ledger is authoritative.
9. `reconcile::idle_poll()` (on 202/None + periodic) and `reconcile::unknown_event()` —
   bounded: resolve `uncertain` batches older than T via `JobAssigned` presence / `GetRunner`
   oracle (mirrors run-service `renewjob` oracle pattern in `node/complete.rs:576`); refresh
   expired queue token (401 path in session.rs); exp
...[truncated 8621 chars]