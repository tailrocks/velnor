# Plan 020: Cut per-line cost on the live log feed — batch frames, hoist the mask set, single-pass masking

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done. Masking is security-critical —
> output must stay equivalent (see Step 4).
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/protocol.rs crates/velnor-runner/Cargo.toml`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: 005 (preserve its multi-line + add-mask semantics) — land 005
  first or carry its semantics forward
- **Category**: perf
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

Velnor's mission is to be **faster** than GitHub-hosted runners, and the live-log
feed pays avoidable per-line cost:

- **One WebSocket frame per output line.** The feed sends one `StepLog` carrying
  a single line, and the consumer serializes + sends one WS frame per message
  with no coalescing — thousands of tiny frames/sec on a verbose build, each
  gated on the WS round-trip in one sequential task. GitHub's runner batches.
- **Job-constant work recomputed per line.** Inside the consumer loop the secret
  mask set is rebuilt (cloning every secret) every message, and the console
  mirror is opened/written/closed **per line** (3 syscalls that block the async
  runtime thread), plus an unconditional per-chunk `eprintln!`.
- **O(secrets × line) masking.** Masking does one full-string `str::replace`
  allocation per secret per line, twice on some paths.

This plan batches the feed, hoists job-constant work out of the loop, and
replaces the multi-pass `str::replace` with a single-pass matcher.

## Current state

- Live emitter sends one line per message: `executor.rs:2545-2562`
  (`lines: vec![line.to_string()]`).
- Consumer loop rebuilds masks per message and opens the console per line:
  `runner.rs:2860` (`let masks = job_secret_mask_values(&job);` inside the loop),
  `runner.rs:2876` → `append_job_console` at `runner.rs:4373` (open/write/close
  per line), `runner.rs:2894` (per-chunk `eprintln!("[feed] Sending ...")`).
- One WS send per message: `runner.rs:2904` → `send_log_lines` → `protocol.rs:2711`
  (`serde_json::to_string(&feed)` + `ws.send(Message::Text(...))`).
- Masking helpers (multi-pass `str::replace`): `runner.rs:3036` (`mask_single_value`),
  `runner.rs:5079` (`mask_value`); timeline path recomputes masks per step at
  `runner.rs:4930-4935`.
- `aho-corasick` is available transitively (via `regex`/`globset`) but not a
  direct dependency; add it to `Cargo.toml` to use it directly.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|------------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                        | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0          |
| Mask tests| `cargo nextest run -p velnor-runner mask --locked`             | pass            |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/runner.rs` — consumer loop batching + mask hoist +
  console handle reuse + drop the per-chunk `eprintln!`; timeline mask hoist.
- `crates/velnor-runner/src/protocol.rs` — allow `send_log_lines` to send a
  batched set of lines in one frame (if not already).
- `crates/velnor-runner/Cargo.toml` — add `aho-corasick`.

**Out of scope**:
- The `bytes::Bytes` zero-copy rebuild (larger, separate P3.2 roadmap item) —
  do not attempt; keep `String` lines.
- Multi-line / add-mask semantics — those come from plan 005; **preserve** them.

## Git workflow

- Branch: `advisor/020-log-feed-batch-and-mask-cost`
- Commit(s): `perf(feed): batch live log frames and reuse the console writer`,
  `perf(mask): single-pass aho-corasick masking, hoisted per job`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Hoist job-constant work out of the consumer loop

Move `job_secret_mask_values(&job)` above the loop (it is immutable for the job).
Open the console mirror file **once** (keep a `BufWriter<File>` or an async file
handle) instead of open/write/close per line. Remove the unconditional per-chunk
`eprintln!("[feed] ...")` (or gate it behind `tracing::debug!`).

**Verify**: `cargo clippy --workspace --all-targets --locked -- -D warnings` → exit 0.

### Step 2: Coalesce the feed

In the consumer, on each wake `recv()` one message then drain with `try_recv()`
until empty; bucket lines by `step_id`; send **one** `send_log_lines` frame per
bucket (preserving per-step `startLine` numbering — read `runner.rs:2893`). If a
short flush interval is easier to reason about, batch by ~50–100 ms / N lines.
Preserve ordering and the reconnect-resend path.

**Verify**: `cargo clippy ...` → exit 0.

### Step 3: Single-pass masking with aho-corasick

Add `aho-corasick` to `Cargo.toml`. Build one `AhoCorasick` automaton from the
hoisted mask set once per job (dedup + longest-match/leftmost semantics), and
replace each line in a single pass into a reused buffer via `replace_all(line,
&["***"; n])` (or the appropriate replace API). Collapse the double/triple
`str::replace` passes (`mask_single_value` + `mask_value`) into one combined
secret+step-mask set. Apply the same hoist to the timeline path
(`runner.rs:4930-4935`).

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy ...` → exit 0.

### Step 4: Equivalence tests (masking must not regress)

Masking is security-critical. Add/extend tests in `runner.rs` `#[cfg(test)]`
asserting the aho-corasick masking produces the **same** output as the old
`str::replace` for representative inputs, including: overlapping secrets, a
secret that is a substring of another, multi-line secrets (from plan 005), and
`::add-mask::` values. Never place a real secret in a test — use placeholders.

Also add `feed_batches_multiple_lines_in_one_frame`: feeding several same-step
lines yields a single batched frame (test the batching helper's output, not the
live WS).

**Verify**: `cargo nextest run -p velnor-runner mask --locked` → pass;
`cargo nextest run --workspace --locked` → all pass.

## Test plan

- Masking equivalence tests (old vs new) covering overlap, substring, multi-line,
  add-mask.
- Batching test: N same-step lines → one frame.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] `job_secret_mask_values` is called once per job on the feed path, not per line
- [ ] The console mirror uses one persistent writer; the per-chunk `eprintln!` is gone
- [ ] The feed sends batched frames (multiple lines per frame), preserving per-step line numbers
- [ ] Masking is single-pass (aho-corasick) and proven output-equivalent to the old logic by tests
- [ ] `cargo nextest run --workspace --locked` exits 0
- [ ] Only the in-scope files modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- Any excerpt doesn't match (drift).
- The aho-corasick output differs from the old masking for any tested case in a
  way that could leak a secret — STOP; masking correctness outranks the perf win.
- Batching breaks per-step line numbering or the reconnect-resend path — report.

## Maintenance notes

- This is a stepping stone toward the P3.2 `bytes::Bytes` zero-copy rebuild;
  keep the batching boundary clean so that later work can swap the line
  representation without re-touching the feed logic.
- Preserve plan 005's semantics (multi-line masks, add-mask in the live feed).
- Reviewer: focus on the masking equivalence tests — the perf change must not
  weaken redaction.
