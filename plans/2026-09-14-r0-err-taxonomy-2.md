# Plan 2026-09-14: typed-error slice 2 at the broker/completion boundary (GOAL 31)

Base: origin/main tip 5b4dfa54. Branch: r0-err-taxonomy-2. Worktree: /tmp/velnor-err2.
Slice 1 (PR #733, merged) typed the Docker boundary; this slice covers the
broker/completion boundary: acquire / complete / ack paths in `protocol.rs`,
`runner.rs`, `node/complete.rs`.

## Problem

Retry/abandon decisions on the acquire/complete/ack paths re-derived
their verdict at the decision site instead of reading a category produced at
the boundary where the status and body are observed (timeouts were never in
scope — every timeout on these paths is a fixed constant; see the
correction note at the end of this file):

1. `runner.rs` `handle_v2_message` re-parsed the raw skipped-acquire body via
   `acquire_reply_is_definitely_gone(&body)` to decide abandon-vs-leave.
2. `node/complete.rs` `record_attempt_failure` re-derived permanence from the
   error chain via `completion_failure_is_permanent`, while the completion
   boundary already knew the class (`classify_completion_response`) and then
   dropped it on the terminal-error path.
3. `acquire_job` retried terminal failures (deterministic 4xx) through the
   whole 5-attempt budget and only classified at exhaustion — terminal did
   not fail fast.
4. `acknowledge_runner_request` failures were unclassified; the single
   best-effort caller logged them with no category.

## Fix (this branch, strict scope)

`protocol.rs` (new taxonomy owner, same shape as slice 1's
`DockerErrorCategory`):

- `BrokerErrorCategory::{Transient, Terminal, Conflict}` (pub: it rides on
  the pub `AcquireJobOutcome::Skipped`).
  - Transient: transport/5xx/408/429 — backoff retry inside the bounded
    budget (acquire <= 5 attempts, complete <= 6 attempts, both pre-existing
    bounds, unchanged).
  - Terminal: deterministic refusal — fail fast, no retry; on the
    completion path it spends the durable budget at once so the slot is
    released now.
  - Conflict: another delivery or holder may own the job — never abandon,
    never double-send; the row stays for the `renewjob` oracle.
- `GitHubApiError` gains `pub(crate) category: Option<BrokerErrorCategory>`.
  The category rides on the typed error rather than in a wrapper, so
  `Display` (`{action} failed: status={status}, body={body}`, both `{}` and
  `{:#}`) and the error chain are byte-identical: downstream `GitHubApiError`
  downcasts (credential refresh on 401/403, quota/rate-limit hints) work
  untouched. `None` everywhere except the classifying production sites.
- `broker_error_category(&anyhow::Error) -> Option<BrokerErrorCategory>`:
  chain accessor. Callers define the unclassified default explicitly: the
  completion journal fails OPEN (retry) — a finished job's outcome must
  never be lost to one unrecognized error — the reverse of the Docker
  boundary's fail-closed default, where the cost asymmetry runs the other
  way. Transport/status-less failures stay unclassified (no status to
  classify from) and therefore retry, as today.
- `classify_acquire_skipped(body)`: typed gone (run-service 404/422) ->
  Terminal; anything else skipped (409, untyped or foreign-sourced body) ->
  Conflict. Carried on `AcquireJobOutcome::Skipped::category`, set at both
  production sites (`acquire_job`, `parse_acquire_job_response`).
- `acquire_job` loop is now category-driven per attempt: a terminal
  failure returns `AcquireJobError::Permanent` immediately (fail fast —
  request credentials are fixed for the call, so the same request fails
  identically on every attempt); only transient failures sleep and retry
  to the unchanged 5-attempt bound.
- `complete_job_payload_with_acknowledgement` attaches `Terminal` to a
  deterministic refusal via `github_api_error_categorized` (same message).
  Exhausted-transient and transport failures stay unclassified -> retry.
- `acknowledge_runner_request` attaches `classify_broker_ack_error(status)`
  (409 -> Conflict, retriable shape -> Transient, else Terminal).

`runner.rs`:

- The `Skipped` arm matches the boundary `category` via the new
  `acquire_skip_abandons_intent` policy (Terminal -> abandon now; anything
  else -> leave the row for the oracle). `acquire_reply_is_definitely_gone`
  keeps its signature/tests and is now used only by the boundary
  classifier (import moved to the test module).
- The best-effort ack log line reports the boundary category
  (`category=transient|terminal|conflict|unclassified`); still never
  retried by design — the ack is a Busy marker, the broker redelivers
  regardless, a duplicate ack is safe.

`node/complete.rs`: `record_attempt_failure` keeps calling
`completion_failure_is_permanent`, now category-driven (boundary verdict
wins; unclassified falls back to the historical status derivation);
policy comment records the budget-spend reasoning.

`node/controller.rs`, test literals: mechanical `category: None`.

Idempotence reasoning (duplicate delivery must be safe):

- Acquire Conflict: re-recording the same provisional intent is
  idempotent (`acquire-intent-idempotent`); the `renewjob` oracle, not the
  409, decides ownership, so a duplicate delivery can neither strand nor
  double-claim the job.
- Complete: the journal's single-send claim plus `RemoteAcked` mean an
  already-terminal remote observation is acked, never re-sent; a
  duplicate broker message cannot manufacture a second terminal send.
- Ack: a Busy-marker POST; redelivery-safe by construction.

## Behavior deltas (intended)

- Terminal acquire failures (deterministic 4xx, e.g. 401) now return
  `Permanent` after 1 attempt instead of after 5. Same terminal outcome
  (session teardown at the existing `is_transient_acquire_error` gate),
  faster, less load on a struggling control plane. A 401-that-would-succeed
  is impossible inside one call (fixed credentials); worst case is one
  extra supervisor session cycle, self-healing via broker redelivery.
- The best-effort ack failure log line gains `(category=...)` forensics.
  No error `Display` changed anywhere.

## Tests

New (focused):

- `protocol.rs`: `acquire_skipped_category_is_terminal_only_for_typed_gone`
  (typed 404/422 -> Terminal; 409/untyped/foreign/empty -> Conflict),
  `acquire_job_terminal_failure_fails_fast_without_retrying` (wiremock 401,
  `expect(1)`, permanent), `completion_permanence_prefers_the_boundary_category`
  (boundary wins both directions; unclassified keeps legacy derivation),
  `complete_job_terminal_refusal_carries_boundary_category` (wiremock 400,
  `expect(1)`, category + permanence + exact historical message),
  `broker_ack_errors_classify_for_forensics` (status matrix).
- `runner.rs`: `acquire_skip_abandons_intent_only_for_terminal`.
- `tests/broker_protocol.rs`: skipped-statuses test now asserts the
  boundary `Conflict` on untyped bodies.

Gates: `cargo fmt` clean, `cargo clippy --locked -p velnor-runner
--all-targets --all-features -- -D warnings` clean, `cargo nextest run
--locked --all-features -p velnor-runner` 2112 passed / 2 skipped.

## Follow-ups (remaining untyped/decision-site boundaries, found here)

1. `renew_failure_is_job_gone` — typed body check but derived at the
   decision site (`probe_provisional_acquisition`); the renew boundary
   could attach Terminal/Conflict so the probe reads a category.
2. `is_credential_poll_error` (401/403 chain derivation) — broker poll,
   lock-renewal, and completion-refresh paths; a `CredentialExpired`
   category at the boundary would unify the three refresh decisions.
3. `lock_renewal_refresh_is_terminal` — registration-deleted OR 404 chain
   derivation inside the renew loop; join with (1).
4. `cleanup_failed_jit_registration` — inline 4xx-minus-transient status
   derivation (JIT path; adjacent, out of broker scope).
5. Broker poll error policy (`poll_broker_message`,
   `BrokerPollState::received_error`) — typed via downcast, no boundary
   category; session-teardown vs retain could join the taxonomy.
6. Transport errors stay unclassified by design (fail-open on completion);
   if a future policy must distinguish "no answer" from "classified
   transient", add a typed marker at the transport boundary.
7. `parse_acquire_job_response` is dead (no callers; the file carries
   `#![allow(dead_code)]`); updated for the new field here, but remove or
   wire up separately.
8. `acquire_reply_is_definitely_gone` is still `pub` though only the
   boundary classifier and tests use it; consider `pub(crate)` (API
   change, separate cleanup).
9. Slice-1 docker follow-ups still open: `cleanup_stale` "not found",
   `buildkit.rs` "no builder", `daemon_reports_missing` re-matching.

## Load flakes (not caused by, not fixed by this branch)

Box load ~29 (15 users, parallel agents). Two full-suite runs each failed
one unrelated timing-sensitive test (`idle_scaling` CPU-ratio gate x2,
`artifact_upload ... finalize` x1); each passes alone on this branch, the
unmodified base passes the full suite, and the final branch run is fully
green (2112 passed). Same class as the slice-1 `gc_leader_lock` note:
timing gates under contention, needs hermetic-budget work of its own.

## Correction (r0-798-corr, PR follow-up to #798)

Review found this slice overpromised: the retry loops did not all read the
taxonomy, and the Problem statement above claimed timeout decisions the
taxonomy never covered (fixed by editing "Retry/timeout/abandon" to
"Retry/abandon" in place). The follow-up branch `r0-798-corr`
(`plans/2026-09-14-r0-798-corr.md`) fixes the five in-scope omissions —
the untyped `acquire run-service job` producer, the acquire loop's legacy
derivation, the complete loop's local `retriable` bool, the untyped
exhausted-transient completion producer, and the untyped
`create broker session` producer plus its retry-everything loop — and
records the complete remaining-untyped-sites list there. All timeouts stay
fixed constants (30 s broker/run-service calls, 70 s poll, fixed backoff
schedules); no category-derived timeouts exist or were introduced.
