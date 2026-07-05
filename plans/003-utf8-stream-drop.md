# Plan 003: Stop dropping the rest of a step's output on the first invalid-UTF-8 line

> **Executor instructions**: Follow step by step; run every verification and
> confirm the expected result before continuing. On a "STOP conditions" item,
> stop and report. Update this plan's row in `plans/README.md` when done.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The streaming reader for `run:` steps iterates `BufReader::lines()` and
**breaks** on the first `Err` — which is exactly what happens on an invalid-UTF-8
byte in stdout/stderr. Everything after that byte is silently discarded,
including later workflow commands: `::set-output::`, `::add-mask::`,
`::error::`, `::save-state::`. Consequences: a lost `set-output` silently
changes downstream step behavior; a **lost `add-mask` leaks a secret** into
later log lines; lost `::error::` annotations vanish from the UI. GitHub's
runner reads bytes lossily (replacement char) and keeps going. The exit code is
unaffected (it comes from `child.wait()`), so the job still reports the right
status — the damage is silent data loss. Non-UTF-8 stdout is common (progress
bars, binary dumps, some compilers). This fix reads bytes and splits on newline,
converting each line with `from_utf8_lossy`, so streaming continues.

## Current state

- `crates/velnor-runner/src/executor.rs` — `stream_reader`, excerpt at
  `executor.rs:234-247`:
  ```rust
  fn stream_reader<R: std::io::Read + Send + 'static>(
      reader: R,
      stream: CommandStream,
      sender: mpsc::Sender<(CommandStream, String)>,
  ) {
      for line in BufReader::new(reader).lines() {
          let Ok(line) = line else {
              break;                         // <-- BUG: invalid UTF-8 abandons the rest of the pipe
          };
          if sender.send((stream, line)).is_err() {
              break;
          }
      }
  }
  ```
- The non-streaming sibling path already does the right thing with
  `String::from_utf8_lossy(&output.stdout)` at `executor.rs:228`.
- `stream_reader` is spawned per stdout/stderr on OS threads at
  `executor.rs:153-154`; the consumer loop reads `receiver` at
  `executor.rs:157-170`. Lines are sent **without** a trailing newline (the
  consumer adds `\n`), so preserve that contract: send one `String` per logical
  line, no trailing `\n`.

## Commands you will need

| Purpose   | Command                                                          | Expected        |
|-----------|-----------------------------------------------------------------|-----------------|
| Format    | `cargo fmt --all --check`                                       | exit 0          |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`| exit 0          |
| One test  | `cargo nextest run -p velnor-runner stream_reader --locked`     | new test passes |
| All tests | `cargo nextest run --workspace --locked`                        | all pass        |

## Scope

**In scope**:
- `crates/velnor-runner/src/executor.rs` — rewrite `stream_reader`'s read loop;
  add a unit test.

**Out of scope**:
- The consumer loop (`executor.rs:157-170`) — its contract (one `String` per
  line, no trailing newline) must be preserved, not changed.
- Any masking logic — that is plan 005.

## Git workflow

- Branch: `advisor/003-utf8-stream-drop`
- Commit: `fix(executor): read step output lossily so non-UTF-8 bytes don't drop the stream`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Replace `BufReader::lines()` with a byte-splitting reader

Rewrite `stream_reader` to read bytes and split on `\n` (0x0A), converting each
line to a `String` with `String::from_utf8_lossy(&buf).into_owned()`. Strip a
trailing `\r` if present (to match `lines()` behavior on CRLF). Use
`BufRead::read_until(b'\n', &mut buf)`:

```rust
fn stream_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    stream: CommandStream,
    sender: mpsc::Sender<(CommandStream, String)>,
) {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,                    // EOF
            Ok(_) => {
                // Trim the trailing \n and an optional \r, matching lines().
                if buf.last() == Some(&b'\n') { buf.pop(); }
                if buf.last() == Some(&b'\r') { buf.pop(); }
                let line = String::from_utf8_lossy(&buf).into_owned();
                if sender.send((stream, line)).is_err() { break; }
            }
            Err(_) => break,                   // genuine IO error: stop this pipe
        }
    }
}
```

Keep the `use std::io::BufRead;` import available (it may already be in scope
via `BufReader`; add it if clippy/compile complains).

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy --workspace
--all-targets --locked -- -D warnings` → exit 0.

### Step 2: Unit test — invalid UTF-8 mid-stream does not drop later lines

Add `stream_reader_survives_invalid_utf8` in the `executor.rs` `#[cfg(test)]`
module. Construct a reader from a byte buffer containing:
`b"first\n\xFF\xFEbad\nafter\n"` (an invalid UTF-8 sequence on line 2), wire a
`std::sync::mpsc::channel()`, call `stream_reader(cursor, CommandStream::Stdout,
sender)` (use `std::io::Cursor::new(bytes)` as the reader), then collect all
messages from the receiver. Assert:
- Three lines are received (none dropped).
- The first is `"first"` and the third is `"after"` (the line **after** the bad
  bytes survives — this is the regression the fix addresses).
- The middle line contains the Unicode replacement char `'\u{FFFD}'`.

**Verify**: `cargo nextest run -p velnor-runner stream_reader --locked` →
passes; `cargo nextest run --workspace --locked` → all pass.

## Test plan

- New test `stream_reader_survives_invalid_utf8` as above.
- Optionally assert the no-trailing-newline contract: a line `"x"` arrives as
  `"x"` not `"x\n"`.
- Verification: `cargo nextest run --workspace --locked` → all pass.

## Done criteria

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` exits 0
- [ ] `grep -n "\.lines()" crates/velnor-runner/src/executor.rs` no longer shows `stream_reader` using `BufReader::new(reader).lines()`
- [ ] `cargo nextest run --workspace --locked` exits 0; new test passes and proves the post-bad-byte line survives
- [ ] Only `executor.rs` modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- `stream_reader` doesn't match the excerpt (drift).
- The consumer at `executor.rs:157-170` turns out to depend on receiving a
  trailing newline (it currently adds one itself) — report before changing.

## Maintenance notes

- This makes streaming byte-lossy like the non-streaming path; the two output
  paths now agree on UTF-8 handling.
- Plan 020 (log-pipeline perf) will touch this area; the byte buffer here is a
  natural point to later avoid the per-line `String` allocation, but do not
  attempt that optimization in this plan.
- Reviewer: confirm CRLF inputs still yield a `\r`-free line.
