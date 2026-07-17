# Plan 045: Timing observability — step/cache report in job summary, pickup SLOs in doctor

> **Executor instructions**: Follow step by step; run every verification; STOP
> conditions binding. Update the status row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 48b04ad..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/executor.rs crates/velnor-runner/src/telemetry.rs crates/velnor-runner/src/slot_log.rs`
> Re-locate by symbol; mismatch → STOP.

## Status

- **Priority**: P1 (V1.14 + V1.15 + V1.7)
- **Effort**: M
- **Risk**: LOW-MED (log-format contract is law — additive only)
- **Depends on**: plans/043 step 4 (lifecycle timestamps) soft; plans/034
  (adapter-owned stats) soft
- **Category**: dx / perf
- **Planned at**: commit `48b04ad`, 2026-07-18

## Why this matters

The mission demands pipelines "very detailed and very clear about what's
going on": every compile job should end with an adapter-owned cache report
(hit rate, bytes, store identity), every job should expose queue-wait +
per-step wall + teardown timings, and doctor should enforce the §2.11 SLOs
(pickup ≤ 3 s free-slot, pickup→first-step ≤ 5 s warm). Today: one tracing
span exists (`handle_v2_message`), lifecycle events are plain forensic
lines, no metrics/SLO code exists at all (grep-confirmed), and cache stats
were ad-hoc workflow steps (being deleted estate-wide by the strict
contract).

## Current state (from the audits; re-locate by symbol)

- Spans: only `tracing::info_span!("handle_v2_message", ...)`
  (`runner.rs:1690`, instrument `:1709`). `telemetry.rs:106` writes spans to
  `logs/trace.jsonl` with busy/idle on close (`FmtSpan::CLOSE`,
  `telemetry.rs:120`); rotation `:49`; OTLP behind `otel` feature +
  `VELNOR_OTLP_ENDPOINT` (`:142`).
- Forensics: `SlotForensics` (`slot_log.rs:41`,
  lifecycle/broker/registry `:56-64`); pickup event `runner.rs:1685`.
- Job summary: GitHub's `$GITHUB_STEP_SUMMARY` mechanism works through the
  command files (the fixture writes summaries — `compat.yml` did stats
  there); adapters' post steps run at `executor.rs:1836-1874`.
- Doctor: `runner.rs:5762`; no SLO logic.
- Log-format contract (`docs/log-format-contract.md`): live lines RAW,
  uploaded lines timestamp-prefixed — nothing here may alter step log line
  SHAPES; summaries and forensic files are the additive channels.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Gates | fmt / clippy / `cargo nextest run --workspace --locked` | exit 0 |
| Guard tests | `cargo nextest run -p velnor-runner log` | log-contract tests green |

## Scope

**In scope**: spans around the lifecycle hot path, a per-job timing record,
adapter post-step summary writing (cache report), doctor SLO section,
`docs/log-format-contract.md` note (summaries are out-of-band — confirm, do
not change the contract), tests.
**Out of scope**: `velnor-tools compare` (plan 046), estate YAML, OTLP
backends, log pipeline internals.

## Git workflow

Branch `velnor-estate-standard`; `feat(runner):` commits,
`git commit -s`; no push without operator instruction.

## Steps

### Step 1: Lifecycle spans + timing record

Add spans (names stable, lowercase-dash): `job-pickup`
(broker message → acquire done), `job-checkout`, `job-container-boot`,
`job-steps`, `job-finalize` (last step → completion posted),
`job-teardown`. Emit one summarizing forensic JSON line per job
(`slot_log` lifecycle channel): all six durations + queue wait (message
receipt minus GitHub's job-queued timestamp if the message carries it —
check the job message struct; else omit).

**Verify**: harness run shows the spans in trace.jsonl; unit test on the
record builder.

### Step 2: Adapter cache report in `$GITHUB_STEP_SUMMARY`

In the sccache post step (034's; if 034 unlanded, today's post dispatch at
`executor.rs:1836-1874` for Sccache) and the actions/cache + rust-cache
post/no-op paths: write a compact summary block — backend, store path class,
hit/miss counts (sccache: parse `--show-stats` machine output
`--stats-format=json` if v0.16 supports it, else text), bytes, and for
no-op'd cache steps the line "host-persistent store — restore/save skipped"
(that last line ALREADY has precedent in the adapter — verify with
`grep -rn "host-persistent" crates/` and keep wording consistent).

**Verify**: RecordingRunner tests assert the summary file receives the
block for each adapter class; log-contract guard tests untouched and green.

### Step 3: Doctor SLOs

Doctor reads the last N per-job timing records from the forensic logs of
each slot (they are on disk per the stability doctrine): report p50/p95
pickup, pickup→first-step, finalize, teardown vs the budgets (constants
with env overrides `VELNOR_SLO_*`); breach → doctor WARN (not fail) with
the offending metric. Wire the §2.11 numbers as defaults.

**Verify**: doctor unit test with synthetic records (breach + pass cases).

## Test plan

≥ 4 new tests; log-format guard tests green (the non-negotiable); full
gates. Operator: one warm fixture run — job summary shows the cache report,
trace.jsonl shows the six spans, doctor prints the SLO table.

## Done criteria

- [ ] Gates + log-contract guards exit 0
- [ ] Six spans + per-job record emitted; summary blocks written by adapters
- [ ] Doctor SLO table with §2.11 defaults
- [ ] No step-log line-shape changes (guard tests prove)
- [ ] No out-of-scope changes

## STOP conditions

- sccache v0.16 stats lack machine-readable output and text parsing is
  brittle → emit raw stats text in the summary instead, note it, continue
  (do not invent numbers).
- The job message carries no queued-at timestamp → omit queue wait from the
  runner record (doctor uses broker-poll gap instead); note it.
- Any log guard test fails → STOP immediately (contract is law).

## Maintenance notes

- Span names + record fields become the interface for plan 046's compare and
  the perf campaigns — treat as semi-stable API; version the record with a
  `v` field.
- New hot paths must add spans (stability doctrine) — reviewers check.
