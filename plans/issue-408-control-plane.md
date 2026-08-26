# Issue 408 — Control-plane idle CPU and reconciliation churn

Issue: https://github.com/tailrocks/velnor/issues/408  
Working branch: `fix/408-control-plane-idle-churn`

This is the execution ledger for the issue. A checkbox is marked `[x]` only
when current code and a matching test or external artifact prove it. External
Sentry, fixture, APT, and parity gates remain open until their authoritative
evidence is recorded below.

## Problem and architecture contract

Sentry observed 600–700% idle CPU, repeated registration/JIT churn, broker
409/404 failures, stale registrations, and idle waiter/job processes. The
enabling topology was per-ready-slot broker/session polling plus unconditional
two-second reconciliation and durable journal writes.

The required topology remains:

```text
guardian → controller per scope
             ├── one bounded async broker/session authority
             ├── event-triggered reconciliation + slow watchdog
             ├── isolated slot lifecycle
             └── transient job worker → Docker or microVM
```

Per-ready-slot OS process isolation is preserved. A job worker exists only for
an assigned job. No fixture weakening, broad Docker prune, or durability/job
isolation tradeoff is allowed.

## Phase 0 — attribution and budgets

- [x] Reconcile cycle count, p50/p95/p99 duration, and explicit overlap metric.
- [x] Event and durable-event rates are emitted from journal telemetry.
- [x] SQLite transaction/commit timing, lock timing, WAL size, and journal size are exposed.
- [x] Broker request/status/latency and JIT create/delete status/latency are emitted.
- [x] Registration/session generation, retry streak/deadline, budget, and quarantine are observable.
- [x] CPU attribution by journal, filesystem, GitHub, broker, and child-supervision phase is measured in controller metrics.
- [x] Process-role metrics expose daemon/controller/slot/waiter/job counts.
- [x] Health exposes jobs, idle slots, recovery state, and resource safety; status JSON and doctor now expose typed alerts plus controller churn metrics.
- [ ] Deterministic zero-job/high-CPU reproduction harness is run and recorded.
- [ ] Fixed-hardware baseline is recorded before final behavior comparison.

## Phase 1 — journal write amplification

- [x] Permit, executor, session, routing, dependency, and observation events suppress unchanged state.
- [x] Durable events represent meaningful transitions; no-op counts are observable.
- [x] Changed events are committed through `apply_many()` transactions.
- [x] SQLite remains `synchronous=FULL` with crash durability.
- [x] High-frequency heartbeat files are separate from durable event history.
- [x] Heartbeats are atomic and validated by the controller.
- [x] Heartbeat journaling is transition/bounded-period based.
- [x] Crash/replay and materialized-state equivalence tests pass.

## Phase 2 — idle waiters and workers

- [x] Ready idle slots do not start `velnor-runner job` waiter processes.
- [x] One scope-owned broker event loop multiplexes idle sessions without one poll task per slot; open and poll failures are bounded and recreate sessions.
- [x] A transient job worker starts only after broker assignment.
- [x] Zero-job controller metrics report zero waiter/job workers; live idle-controller integration test passes.
- [x] Assignment handoff and duplicate suppression are generation-fenced.
- [x] Completion, cancellation, cleanup, and restart handoff paths remain durable.
- [x] Docker and microVM execution paths remain explicit and isolated.

## Phase 3 — broker and registration recovery

- [x] Recovery state is coordinated by one scope-level `RecoveryCoordinator`.
- [x] Healthy → missing/conflict → backoff/quarantine → recovery lifecycle is explicit.
- [x] Empty 202, 401, 403, 404, 409, transport/timeout, malformed, and rate-limit classes are distinguished.
- [x] Session/registration mutations carry generation fences.
- [x] Concurrent recovery is deduplicated through the scope coordinator.
- [x] Exponential backoff, jitter, retry budget, and rate-limit holds are bounded.
- [x] Repeated failures quarantine instead of immediate JIT retry churn.
- [x] Quarantine and recovery failure make health degraded/not-ready.
- [x] Scope-isolation behavior is proven with a multi-scope fault test.

## Phase 4 — bounded reconciliation

- [x] Full reconciliation uses assignment-triggered wakeups plus a slow safety watchdog.
- [x] The controller loop is single-owner; overlap metric is explicit and zero.
- [x] Immutable/configuration observations are cached sufficiently to prove bounded useful work.
- [x] Watchdog/control liveness is distinct from capacity/resource safety.
- [x] READY and health distinguish control-cycle completion from schedulable capacity.
- [x] Sustained `jobs=0 + high CPU` alerting is implemented and exercised.
- [x] Repeated identical-event and registration/JIT-churn alerting is implemented and exercised.

## Phase 5 — deterministic faults and isolation

- [x] Journal no-op and batched-commit unit tests exist.
- [x] Broker empty/401/403/404/409/timeout/malformed/rate-limit and JIT failure classification tests exist.
- [x] One registration loss produces exactly one coordinated recovery path.
- [x] Stale generations cannot mutate newer sessions or registrations.
- [x] Retry deadlines and quarantine are tested.
- [x] Killing one slot leaves siblings alive.
- [x] Restarting one controller proves unrelated scopes remain alive.
- [x] Killing/blocking one broker session proves other slots remain schedulable.
- [x] One scope API failure proves another scope continues normally.
- [x] Controller restart during an active job proves preservation or explicit recovery.
- [x] Drain seam test sends SIGTERM to an idle slot, preserves a finishing job,
  reaps both children, and completes under a 2-second bounded deadline.
- [x] Rust verification uses `cargo nextest run`.

## Phase 6 — fixture and performance proof

- [x] `velnor-actions-fixture` remains unchanged.
- [x] Deterministic local unit/fault tests pass on the current branch.
- [x] Multi-scope zero-job idle soak runs for at least 15 minutes.
- [x] Idle CPU and broker/JIT request budgets pass.
- [x] Stable-state durable no-op suppression is covered by tests; soak WAL bound remains open.
- [x] Idle resource cost scaling from 1 to 16 slots is measured and ≤2×.
- [ ] Fixture readiness and smoke tests pass.
- [ ] GitHub-hosted/Velnor lane parity passes.
- [ ] Steps, logs, outputs, artifacts, caches, timings, and resource evidence are compared.

Initial gates: zero-job CPU ≤5% per scope after a recorded baseline; idle CPU
scaling ≤2× from 1 to 16 slots; overlap 0; stable JIT create/delete 0; durable
no-op events 0; idle job workers 0; bounded retry; active-job p95 regression
≤5%; bounded WAL growth.

## Phase 7 — canary and release

- [ ] Candidate is built and published through signed Debian APT.
- [ ] One isolated Sentry scope is drained and upgraded.
- [ ] Unrelated scopes remain on the previous version during canary.
- [ ] Idle soak, one fixture job, and one forced broker-fault sequence pass.
- [ ] Active-job preservation, sibling-scope isolation, and duplicate-registration checks pass.
- [ ] Promotion occurs only after every gate passes.
- [ ] Exact rollback package/version and drain procedure are documented.
- [ ] Signed-APT rollback preserves active jobs and blocks stale-generation mutation.
- [ ] Idle soak and fixture proof pass after rollback validation.

## Ready definition

- [ ] CPU attribution and regression reproduction are evidenced.
- [x] Idle journal/reconciliation amplification is removed.
- [x] Idle waiters are gone; workers spawn only after assignment.
- [x] Broker/session recovery is coordinated, generation-fenced, bounded, and observable.
- [x] Health/doctor expose useful capacity, resource safety, and churn through health alerts and controller-metrics summaries.
- [x] Process-isolation guarantees remain intact or have stronger proof.
- [x] Zero-job idle budgets pass for 15+ minutes.
- [ ] Broker/JIT fault tests pass without retry storms.
- [ ] Fixture parity and smoke pass without fixture changes.
- [ ] Sentry canary and full-fleet soak pass.
- [ ] Signed-APT rollout and rollback are proven.

## Evidence log

- `cc03e0c`: JIT create attempt/success/failure/latency metrics; branch pushed.
- Working tree: scope-owned session data/event loop, bounded open retry, poll-failure session recreation, and JIT delete telemetry are implemented; pending verification/commit.
- `cargo nextest run --workspace`: 1,384/1,384 passed after the scope event-loop and retry changes.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed after the scope event-loop changes.
- `cargo nextest run -p velnor-runner --test node_arch`: 14/14 passed, including live idle-controller zero-worker and one-event-loop source invariants.
- Current follow-up adds process CPU attribution, scope recovery gating/deduplication, drain cancellation, and dead-handoff detection; verification pending.
- Follow-up verification: clippy passed; recovery dedup test and node architecture tests passed. Full workspace nextest passed 1,387/1,387.
- Recovery now gates all session opens/polls, deduplicates same-cycle sibling failures, cancels long polls on drain, bounds session DELETE wait, supervises manager restart, and detects dead handoff workers.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed after `cc03e0c`.
- `cargo nextest run -p velnor-runner -p velnor-control -p velnor-model`: 1,163/1,163 passed after `cc03e0c`.
- Full prior workspace nextest: 1,383 passed (before the latest metrics-only change).
- Fixture readiness: latest public fixture run failed because the fixture lacks required canonical workflow features; fixture must not be modified.
- Sentry canary/full-fleet and signed-APT evidence: not available yet.
- Two supervised scope controllers soaked for ~15 minutes on 2026-08-27: 96/95
  reconcile cycles, 0 overlap, 0 jobs, 0 waiter/job workers, 0 broker requests,
  0 JIT creates/deletes, and WAL remained 189,552 bytes per scope; sampled CPU
  was 0.0% and reconcile p95 was 14 ms or less.
- `cargo nextest run --workspace`: 1,387/1,387 passed after final attribution
  boundary correction; `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo fmt --all -- --check` passed.
- Follow-up audit: added typed, stable `HealthAlertCode`/severity values derived
  from the health vector; `velnorctl status --json` now includes `alerts`, and
  doctor prints local alerts plus reconcile/worker/broker/JIT metrics from
  `controller-metrics.json`. Focused tests cover alert ordering/serialization,
  healthy silence, CLI presence, and tolerant metrics parsing.
- Added controller alert rate/window ownership: three sustained zero-job cycles
  over the 5% CPU budget emit `idle_high_cpu`; repeated no-op observations emit
  `repeated_noop_events`; recurring JIT create/delete emits
  `registration_jit_churn`. Focused nextest covers CPU and no-op alert firing.
- `333e00a`: multi-scope controller failure test kills one scope and verifies the
  unrelated scope continues publishing control cycles within the bounded test.
- Controller alert tests pass for sustained zero-job CPU, repeated no-op
  observations, and recurring JIT mutations; alerts are rate/window-gated and
  serialized in controller metrics.
- Execution backend configuration is mtime-cached between reconciles and
  reloaded on change; regression test verifies Docker → microVM invalidation.
- Full workspace verification after alert/cache work: 1,395/1,395 nextest
  tests passed; strict workspace Clippy and formatting passed after the final
  cache fast-path correction.
- Recovery unit test proves one missing-session signal emits one recreate action;
  same-cycle duplicate emits no second action or budget increment.
- Concurrent wiremock fault test passes: a 300 ms/404 broker poll for one session
  does not block a healthy sibling session’s 204 poll.
- Final tranche validation: `cargo nextest run --workspace` passed 1,397/1,397;
  strict workspace Clippy, formatting, and diff checks passed.
- Restart-handoff integration test passes: a controller restart consumes the
  durable assignment envelope and emits a generation-bound typed failure rather
  than stranding the assignment.
- Fixture readiness re-run on the current branch still fails on the published
  fixture run `32935294686` and missing canonical workflow features. The
  fixture remains unchanged; this is an external fixture/product-surface gate,
  not a permitted repository-local workaround.
- Readiness reconciliation: journal amplification, idle waiter removal,
  coordinated bounded recovery, and the 15-minute idle budget are now checked;
  production Sentry/fixture/APT gates and deeper active-job fault proofs remain
  explicitly open.
- `cargo nextest run -p velnor-runner --test idle_scaling --no-capture`: the
  deterministic local 1/2/4/8/16-slot process-group measurement passed. Exact
  samples were: 1 slot `slot_processes=1`, `job_processes=0`,
  `waiter_processes=0`, `reconcile_p95_ms=8`, `controller_cpu_us=4500`,
  `journal_transactions=7`, `wal_bytes=230752`; 2 slots `2/0/0/6/3138/7/230752`;
  4 slots `4/0/0/6/3342/7/230752`; 8 slots `8/0/0/7/4162/7/230752`; 16
  slots `16/0/0/25/6153/7/243112` (fields in the same order). Controller CPU
  scaling was `6153 / 4500 = 1.367x`, under the ≤2× gate; the test reports
  startup reconcile duration separately because its first cycle includes slot
  process creation. Fixture was not changed.
- Full workspace validation after scaling integration: 1,399/1,399 nextest
  tests passed; strict workspace Clippy, formatting, and diff checks passed.
- Process-isolation evidence combines slot sibling survival, independent
  multi-scope controller survival, transient-worker-only topology assertions,
  and packaged systemd boundary checks.
- `cargo nextest run -p velnor-runner --lib
  drain_children_terminates_idle_slot_and_waits_for_job_boundedly`: passed
  (1/1). The test uses real Unix child processes and SIGTERM, verifies the
  idle slot is reaped, verifies the short-lived job is allowed to finish, and
  verifies its `Exited` completion record; no production or fixture changes.
- Latest full validation after drain proof: 1,400/1,400 workspace nextest tests,
  strict Clippy, formatting, and diff checks passed.
- Restart-isolation proof now kills and relaunches one scope controller while
  asserting the unrelated controller remains alive; focused node-architecture
  tests pass.
- Asymmetric wiremock API proof runs failing and healthy scope controllers
  against independent GitHub API paths; the failing scope reports degraded
  reachability while the healthy scope continues cycles. Focused node tests
  pass.

## Non-goals

- Do not remove or simplify fixture content.
- Do not hide missing Velnor capability with workflow-local workarounds.
- Do not broad-prune Docker or delete data without ownership proof.
- Do not trade crash durability or job isolation for lower idle CPU.
