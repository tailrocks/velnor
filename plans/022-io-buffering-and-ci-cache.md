# Plan 022: Buffer the trace/forensic writers and add the missing CI rustup cache

> **Executor instructions**: Follow step by step; run each verification. STOP ⇒
> report. Update `plans/README.md` when done. Three small, independent fixes —
> each can be its own commit.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/telemetry.rs crates/velnor-runner/src/slot_log.rs .github/workflows/ci.yml`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: perf
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

Three low-risk efficiency fixes:

1. **`trace.jsonl` is unbuffered behind one global mutex.** Every daemon slot's
   span-close and forensic event serializes through a single
   `Arc<Mutex<RotatingFile>>` and pays one `write()` syscall per event with no
   buffering. Under load, all slots contend on that lock on the same threads that
   run jobs.

2. **Forensic log append does `create_dir_all` + `stat` + open/close per line.**
   ~5 syscalls per forensic line, re-creating and re-statting the directory
   every time.

3. **CI `fmt` job re-downloads the toolchain every run.** `clippy` and `test`
   cache `~/.rustup/toolchains`; `fmt` does not, so every push/PR re-installs the
   pinned toolchain from scratch on the critical gate.

## Current state

- `crates/velnor-runner/src/telemetry.rs` — `RotatingFile { file: File, ... }`
  (raw `std::fs::File`, no `BufWriter`) at `telemetry.rs:29-33`; `SharedFileWriter::write`
  locks the mutex and calls `inner.file.write(buf)` directly (`telemetry.rs:75-85`).
  Rotation happens in `rotate_if_needed` (`telemetry.rs:55-72`).
- `crates/velnor-runner/src/slot_log.rs` — `append_log_line` (`slot_log.rs:66-75`)
  does `tracing::info!`, then `std::fs::create_dir_all(dir)`, then a size `stat`
  via `rotate_if_large`, then `OpenOptions...open` + `write_all` + close, per call.
- `.github/workflows/ci.yml` — the `fmt` job (`ci.yml:30-41`) installs Rust via
  `mise-action` + `rustup component add rustfmt` with **no** rustup cache step;
  the `clippy` job caches it at `ci.yml:58-64` and `test` at `ci.yml:115-121`:
  ```yaml
  - name: Cache rustup toolchain
    uses: actions/cache@27d5ce7f107fe9357f9df03efb73ab90386fccae # v5
    with:
      path: |
        ~/.rustup/toolchains
        ~/.rustup/update-hashes
      key: rustup-${{ runner.os }}-${{ hashFiles('rust-toolchain.toml') }}
  ```

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|------------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                        | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings` | exit 0          |
| Tests     | `cargo nextest run -p velnor-runner --locked`                   | all pass        |
| Actionlint| `mise exec -- actionlint` (or the repo's actionlint)            | no errors       |

## Scope

**In scope**:
- `crates/velnor-runner/src/telemetry.rs` — buffer the trace writer.
- `crates/velnor-runner/src/slot_log.rs` — avoid per-line `create_dir_all`/`stat`.
- `.github/workflows/ci.yml` — add the rustup cache to `fmt`.

**Out of scope**:
- Switching to `tracing-appender` non-blocking writer — a valid alternative but a
  bigger change; `BufWriter` is enough here. Note it as a follow-up.
- The forensic-log format or rotation policy — unchanged.

## Git workflow

- Branch: `advisor/022-io-buffering-and-ci-cache`
- Commit(s): `perf(telemetry): buffer trace.jsonl writes`,
  `perf(slot-log): stop re-creating the log dir per line`,
  `perf(ci): cache the rustup toolchain in the fmt job`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Buffer the trace writer

Wrap the file in `RotatingFile` with a `BufWriter<File>` (or add an internal
buffer), flushing on rotation and in `SharedFileWriter::flush`. Preserve
JSONL line-atomicity: a full line must be written before rotation, and rotation
must flush first. Keep the size accounting (`written`) correct with buffering.

**Verify**: `cargo nextest run -p velnor-runner --locked` → the telemetry tests
(grep `RotatingFile`/`telemetry` in tests) pass; `cargo clippy ...` → exit 0.

### Step 2: Stop re-creating the forensic log dir per line

In `append_log_line`, ensure the directory once (cache a "dir ensured" flag, or
create it at slot start) and only `stat` for rotation every N writes (or on
write error). On an `ErrorKind::NotFound` write error, lazily recreate the dir
and retry once — preserving the robustness property (survives log-dir removal)
without paying `create_dir_all` + `stat` on every line.

**Verify**: `cargo nextest run -p velnor-runner --locked` → pass; `cargo clippy
...` → exit 0.

### Step 3: Add the rustup cache to the CI `fmt` job

Copy the `Cache rustup toolchain` step (the exact block shown in "Current state")
into the `fmt` job, before the `mise-action` step, matching the key
`rustup-${{ runner.os }}-${{ hashFiles('rust-toolchain.toml') }}` used by the
other jobs.

**Verify**: `mise exec -- actionlint` → no errors; the `fmt` job now contains a
rustup cache step identical to `clippy`/`test`.

## Test plan

- Telemetry: existing tests must pass with buffering; add a test that a written
  line is flushed and readable (if a test seam exists).
- Slot log: existing tests pass; optionally a test that a removed dir is
  recreated on write.
- CI: actionlint clean; no Rust test.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` and `cargo clippy ... -D warnings` exit 0
- [ ] `RotatingFile` writes through a buffer; rotation flushes first; JSONL lines stay intact
- [ ] `append_log_line` no longer calls `create_dir_all` + `stat` on every line
- [ ] The CI `fmt` job caches `~/.rustup/toolchains` with the shared key
- [ ] `mise exec -- actionlint` passes; `cargo nextest run --workspace --locked` exits 0
- [ ] Only the three in-scope files modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- Any excerpt doesn't match (drift).
- Buffering breaks JSONL atomicity (a rotation mid-line) — STOP; correctness of
  the forensic log outranks the perf win.

## Maintenance notes

- Follow-up: `tracing-appender` non-blocking writer would remove the mutex
  entirely; deferred as a bigger change.
- Reviewer: confirm the trace buffer is flushed on rotation and on process exit
  (a lost tail of `trace.jsonl` would hurt post-incident forensics, which the
  stability mandate relies on).
