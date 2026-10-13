# Plan 005: Close two secret-masking gaps — multi-line secrets and `::add-mask::` in the live feed

> **Executor instructions**: Follow step by step; run each verification and
> confirm before continuing. STOP-condition ⇒ stop and report. Update
> `plans/README.md` when done. This is a **security** plan — do not weaken any
> assertion to make a test pass, and never place a real secret value in a test;
> use obvious placeholders.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/executor.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none (coordinate with plan 020 — see Maintenance notes)
- **Category**: security
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

Velnor masks secrets in job logs, but two gaps let secrets reach the GitHub UI
in the clear:

1. **Multi-line secrets never match.** The mask set is built from raw secret
   values with no newline splitting, and masking is applied **per line**. A
   secret containing a newline (a PEM private key, a service-account JSON, a
   multi-line token) is a single mask string that cannot match any single output
   line, so it is never redacted — in the live feed, the uploaded log blob, or
   the on-disk console mirror. GitHub's runner registers each line of a
   multi-line secret as its own mask specifically to avoid this.

2. **`::add-mask::` values are unmasked in the live feed.** Steps register
   dynamically-computed secrets with `::add-mask::`. Those masks travel on the
   `StepLog.masks` field, but the **live** WebSocket feed applies only the
   static job secrets, not `log.masks`, and the streamed lines are never re-sent
   masked. So an `add-mask`'d value (the exact case add-mask exists for) appears
   in the clear in the live GitHub UI console. The downloadable log is masked;
   only the live stream leaks.

Both are secret-exposure bugs on the runner's core log path.

## Current state

- `crates/velnor-runner/src/runner.rs` — mask set builder, excerpt at
  `runner.rs:3551-3562`:
  ```rust
  fn job_secret_mask_values(job: &AgentJobRequestMessage) -> Vec<String> {
      job.mask.iter().filter_map(|mask| mask.value.clone())
          .chain(job.variables.values()
              .filter(|variable| variable.is_secret)
              .filter_map(|variable| variable.value.clone()))
          .collect()                           // <-- no newline splitting
  }
  ```
- Per-line masking helpers, excerpts at `runner.rs:3036-3044` and
  `runner.rs:5079-5085`:
  ```rust
  fn mask_single_value(line: &str, masks: &[String]) -> String {
      let mut result = line.to_string();
      for mask in masks { if !mask.is_empty() { result = result.replace(mask.as_str(), "***"); } }
      result
  }
  fn mask_value(value: &str, masks: &[String]) -> String {
      masks.iter().filter(|m| !m.is_empty())
          .fold(value.to_string(), |value, mask| value.replace(mask, "***"))
  }
  ```
- Live feed consumer, excerpt at `runner.rs:2860-2865`:
  ```rust
  let masks = job_secret_mask_values(&job);        // static job secrets ONLY
  let lines: Vec<String> = log.lines.iter()
      .map(|l| mask_single_value(l, &masks))       // <-- does NOT apply log.masks (add-mask)
      .collect();
  ```
- Live line emitter, excerpt at `executor.rs:2545-2562`:
  ```rust
  let _ = sender.send(StepLog {
      ...
      lines: vec![line.to_string()],
      masks: Vec::new(),                            // <-- live lines carry no masks
      ...
  });
  ```
- `::add-mask::` is parsed from workflow commands; grep `add-mask` /
  `add_mask` in `crates/velnor-runner/src/workflow_command.rs` and
  `executor.rs` to find where the per-step mask set is accumulated (it ends up
  on `StepLog.masks`, applied to the downloadable log at `runner.rs:4685`).

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| One test  | `cargo nextest run -p velnor-runner mask --locked`              | new tests pass  |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/runner.rs` — `job_secret_mask_values` (newline
  expansion), and the live-feed masking to also apply `log.masks`.
- `crates/velnor-runner/src/executor.rs` — only if the live emitter must carry
  an accumulating mask set (Step 3); prefer applying masks at the consumer.

**Out of scope**:
- Replacing `str::replace` with aho-corasick (performance) — that is plan 020.
  Keep the current `replace` approach here; correctness first.
- The forensic job-message dump sanitizer (`sanitize_job_message_value`) —
  already correct.

## Git workflow

- Branch: `advisor/005-secret-masking-parity`
- Commit(s): `fix(security): split multi-line secrets into per-line masks`,
  `fix(security): mask add-mask secrets in the live log feed`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Expand multi-line secret values into per-line masks

In `job_secret_mask_values`, after collecting each raw secret value, also push
each non-empty line of any value that contains `\n` or `\r`. Keep the whole
value too (single-line consumers still match it). Guard against over-masking:
skip lines shorter than a small threshold (e.g. < 3 chars) and trim surrounding
whitespace, so a secret whose first line is empty or a single space does not
turn every space into `***`. Deduplicate the resulting set.

Target shape:

```rust
fn job_secret_mask_values(job: &AgentJobRequestMessage) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let raw = job.mask.iter().filter_map(|m| m.value.clone())
        .chain(job.variables.values().filter(|v| v.is_secret).filter_map(|v| v.value.clone()));
    for value in raw {
        if value.contains('\n') || value.contains('\r') {
            for line in value.split(['\n', '\r']) {
                let line = line.trim();
                if line.len() >= 3 { out.push(line.to_string()); }
            }
        }
        if !value.is_empty() { out.push(value); }
    }
    out.sort();
    out.dedup();
    out
}
```

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Apply `log.masks` (add-mask) in the live feed consumer

In the live-feed consumer (`runner.rs:2860-2865`), build the mask set as the
union of the static job secrets **and** `log.masks` for the current step, then
mask each live line against that union:

```rust
let mut masks = job_secret_mask_values(&job);
masks.extend(log.masks.iter().cloned());
let lines: Vec<String> = log.lines.iter().map(|l| mask_single_value(l, &masks)).collect();
```

This covers the case where the completed `StepLog` carries the step's masks.

**Verify**: `cargo clippy ...` → exit 0.

### Step 3: Suppress add-mask'd values in already-streamed live lines

The live lines are emitted **one at a time as they are produced**
(`executor.rs:2545`, `masks: Vec::new()`), and an `::add-mask::` may be
registered mid-step *after* earlier lines already streamed. Full retroactive
masking of already-sent lines is not possible over a streaming feed, so the
achievable guarantee is: **once a mask is registered, every subsequent live line
is masked against it.** Implement this by threading the accumulating step mask
set into the live emitter path so that `emit_live_step_log` (or its caller)
masks each line against the masks discovered so far in this step before sending.
Find where `::add-mask::` updates the step's mask set during streaming (grep
`add_mask` in `executor.rs`) and ensure that set is passed to the live emitter.

If wiring the accumulating set into `emit_live_step_log` requires changing more
than the emitter and its immediate caller, STOP and report the call graph —
do not refactor broadly.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 4: Tests

Add tests in `runner.rs` `#[cfg(test)]` (model after the existing masking test —
grep `mask` in the test module, e.g. `run_service_telemetry_masks_step_and_job_secrets`):

- `multiline_secret_is_masked_per_line`: build a job whose secret value is
  `"line-one-secret\nline-two-secret"`; assert `job_secret_mask_values` contains
  both `"line-one-secret"` and `"line-two-secret"`, and that masking a log line
  equal to `"line-two-secret"` yields `"***"`.
- `short_secret_lines_do_not_overmask`: a secret value `"ab\nverylongsecretvalue"`
  does not add the 2-char `"ab"` as a mask (guard against over-masking).
- `live_feed_masks_add_mask_values`: a `StepLog` with `masks = vec!["dynsecret".into()]`
  has its live line `"echo dynsecret"` masked to `"echo ***"` by the consumer
  logic from Step 2 (extract the consumer's line-masking into a small testable
  helper if needed).

**Verify**: `cargo nextest run -p velnor-runner mask --locked` → new tests pass;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- Three tests above, covering: multi-line split, over-mask guard, live-feed
  add-mask application.
- Model after the existing job/step masking test in `runner.rs`.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` exits 0
- [ ] `job_secret_mask_values` splits multi-line values into per-line masks with an over-mask length guard
- [ ] The live-feed consumer masks each line against `job secrets ∪ log.masks`
- [ ] Subsequent live lines in a step are masked against add-mask values registered earlier in the same step
- [ ] `cargo nextest run --workspace --locked` exits 0; the three new tests pass
- [ ] Only `runner.rs` (and `executor.rs` if Step 3 required it) modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- Any excerpt in "Current state" doesn't match (drift).
- Step 3's accumulating-mask wiring would require touching more than the live
  emitter + its immediate caller — report the call graph instead.
- You cannot find where `::add-mask::` accumulates the per-step mask set —
  report; do not guess.

## Maintenance notes

- **Coordinate with plan 020** (log-pipeline perf): plan 020 replaces the
  per-line `str::replace` masking with a single-pass aho-corasick matcher and
  hoists the mask set out of the loop. That plan must preserve the semantics
  established here (multi-line masks, live-feed add-mask). If plan 020 lands
  first, apply Steps 1–3 semantics on top of its matcher; if this lands first,
  plan 020 inherits the expanded mask set.
- Retroactive masking of lines already streamed before an `add-mask` is
  inherently impossible over a live feed — the guarantee is "mask everything
  after the mask is registered." Document this in the PR so reviewers don't
  expect more.
- Reviewer: verify no test contains a realistic secret value; placeholders only.
