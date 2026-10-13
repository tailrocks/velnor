# Plan 032 (investigate): Detect job cancellation even when the broker is unreachable

> **Executor instructions**: This is an **investigate/spike** plan — the
> deliverable is a findings write-up and, if a viable signal exists, a small
> prototype. Do not add a cancellation source that could false-cancel. STOP ⇒
> report. Update `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/protocol.rs`

## Status

- **Priority**: P3
- **Effort**: L (spike: M)
- **Risk**: MED
- **Depends on**: 018 (the HTTP harness makes any prototype testable)
- **Category**: bug (investigate)
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

Job cancellation is observed **only** through the broker cancellation poller. If
GitHub cancels a job while the broker is unreachable for that slot, the poller
backs off and never sees the `JobCancellation` message, so the container runs to
completion and the job is reported `Succeeded`/`Failed` instead of `Canceled`
(wasted compute + wrong conclusion in the UI). Frequency is low — it needs a
broker outage overlapping a cancellation — and it is partly inherent to a polling
model, so this is an investigation: find whether a **secondary** cancellation
signal exists (e.g. the run-service `renew_job` response carrying a cancel flag,
or a periodic run-service state check) so a broker blackout does not blind the
runner.

## Current state (evidence)

- Cancellation poller backs off on broker errors and only sets `canceled` on a
  received `JobCancellation`: `runner.rs:2604+` (`start_broker_cancellation_poll`),
  backoff up to 60s; `runner.rs:2307-2308` sets `TaskResult::Canceled` solely from
  the `canceled` flag.
- The lock-renewal path talks to run-service every 25s
  (`runner.rs:2568`) — its `renew_job` **response** is a candidate carrier of a
  cancel signal if GitHub includes one (investigate the run-service renew response
  shape in `actions/runner` and in `protocol.rs`'s `renew_job` types).

## Deliverables

1. A findings write-up (PR or `docs/`) answering: does the run-service
   `renew_job` response (or any other call the runner already makes during a job)
   carry a cancellation indicator? Consult `actions/runner` (the protocol source
   of truth per AGENTS.md) and the `protocol.rs` types. Conclude whether a
   secondary signal is available **without** new polling.
2. If a viable signal exists: a small prototype that cross-checks it and sets the
   `canceled` flag when the broker is unreachable — behind a flag, with the
   HTTP-harness test (plan 018) proving it cancels on the secondary signal and
   does **not** false-cancel on a normal renew.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|------------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                        | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0          |
| Tests     | `cargo nextest run -p velnor-runner cancel --locked`           | pass            |

## Scope

**In scope (spike)**:
- Investigation of the run-service renew response (and other in-flight calls) for
  a cancel indicator — consult `actions/runner` and `protocol.rs`.
- If viable: a flagged prototype cross-checking that signal, with a harness test.

**Out of scope**:
- Adding a brand-new polling loop just for cancellation (defeats the point;
  cancellation should ride an existing call) unless the investigation shows no
  alternative and the team accepts it.
- Changing the broker poller's normal path.

## Git workflow

- Branch: `advisor/032-cancellation-during-broker-outage`
- Commit: `spike(cancel): cross-check run-service for cancellation during broker outage`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Investigate a secondary cancel signal

Read `actions/runner` (src/Runner.Worker / run-service messages) and
`protocol.rs`'s `renew_job` request/response types. Determine whether the renew
response (or another call the runner already makes) carries a cancel indicator.
Write the finding + recommendation in the PR. If **no** secondary signal exists,
conclude that and stop here (record it as a known limitation; the polling model's
blind spot is documented, not silently ignored).

**Verify**: the finding is written; a clear yes/no on a secondary signal.

### Step 2 (only if a signal exists): Prototype the cross-check

Behind a flag, have the renewal/run-service path set the shared `canceled` flag
when the secondary signal indicates cancellation. Ensure it can **only** cancel
on a genuine indicator (never on a transient error). Reuse the existing
`canceled: AtomicBool` so the completion path already maps it to
`TaskResult::Canceled` (`runner.rs:2307`).

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 3 (if prototyped): Test with the harness

Using plan 018's fake server, script: broker unreachable + run-service renew
response indicating cancellation ⇒ the job is marked `Canceled`. And: a normal
renew response ⇒ **not** cancelled (no false cancel). If plan 018 has not landed,
gate this test behind it and note the dependency.

**Verify**: `cargo nextest run -p velnor-runner cancel --locked` → pass;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- Harness test: secondary-signal cancel during broker outage → Canceled; normal
  renew → not cancelled.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] A written finding on whether a secondary cancel signal exists (with `actions/runner` evidence)
- [ ] If none: the limitation is documented (not silently ignored) and the plan closes as "no viable signal"
- [ ] If one exists: a flagged prototype cross-checks it and cannot false-cancel; a harness test proves both cancel-on-signal and no-false-cancel
- [ ] `cargo fmt`/`clippy`/`nextest` all green
- [ ] `plans/README.md` row updated (note if closed as "no signal")

## STOP conditions

- The only way to detect cancellation during a broker outage would be a new
  dedicated poll — report; do not add it without the team accepting the cost.
- Any prototype path could set `Canceled` on a transient error (false cancel) —
  STOP; a false cancel is worse than the current blind spot.

## Maintenance notes

- This is inherently a low-frequency edge (broker outage ∩ cancellation). The
  correct outcome may be "documented limitation" — that is a valid close.
- Reviewer: the non-negotiable is no false-cancel; scrutinize the signal's
  certainty before it can set `Canceled`.
