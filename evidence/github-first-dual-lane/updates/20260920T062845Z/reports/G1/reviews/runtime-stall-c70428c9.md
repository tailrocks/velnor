# Unix S2 runtime-stall successor review — `c70428c919edc6ab938fcfddd9546ecdedc713c5`

## Scope

- Exact source: `tailrocks/velnor`, branch `codex/github-first-hosted-g1-security-3ae`, commit `c70428c919edc6ab938fcfddd9546ecdedc713c5` (parent `f90515f30c3325a1a5469b1725a202a57a884002`).
- Detached owner path: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-hosted`.
- Worktree clean. No source/generated/remote mutations. No Velnor/Docker payload.

## Verification

```text
rtk proxy cargo test -p velnor-workflow --all-features --lib --locked -- \
  run_cmd_stall_tests --nocapture --test-threads=1
12 passed; 0 failed; 1769 filtered out; 17.02s

same command with --test-threads=4
12 passed; 0 failed; 1769 filtered out; 6.07s

rtk cargo clippy -p velnor-workflow --all-features --lib --locked -- -D warnings
No issues found

rtk cargo fmt --all -- --check
pass

rtk cargo check -p velnor-workflow --lib --target x86_64-unknown-linux-gnu --locked
Finished

rtk cargo check -p velnor-workflow --lib --target x86_64-pc-windows-gnu --locked
Finished

rtk git diff --check HEAD^ HEAD
pass
```

## Prior gaps addressed

1. `spawn_run_command` puts each Unix command in a dedicated PGID (`runtime.rs:2797-2810`). `abort_child` kills that group, then direct child, then waits (`3068-3090`). The cleanup regression proves a delayed descendant cannot create its marker after cancellation (`3559-3601`). The dedicated `process_group(0)` binding prevents ordinary unrelated process-group targeting.
2. `PendingOutput` caps each stream at 64 KiB; child reads and sink writes are both poll-driven (`2845-2907`, `2923-2998`, `3019-3065`). The Unix socket backpressure regression returns promptly (`3603-3645`) instead of blocking the deadline owner.
3. Direct-child exit with descendants retaining captured stdout/stderr is now tested with real stdout/stderr files. Both pre-exit and delayed tail bytes are captured, and the command returns (`3513-3557`).
4. Main-loop non-`EINTR` poll and write failures abort/return diagnostics with cleanup (`3155-3192`). Linux and Windows-GNU library paths compile; Windows retains the pre-existing threaded-pump implementation under `cfg(not(unix))`.

## Remaining findings

### 1. Closed-output `EPIPE` is still silently swallowed

`flush_pending_output` clears the pending bytes and returns `Ok(false)` on
`Errno::PIPE` (`runtime.rs:3039-3045`). Thus a closed stdout/stderr sink can
lose output and the command can still report success. This is inconsistent with
the commit's “non-EINTR write errors surfaced” claim. If closed CI output is an
intentional benign condition, document it and test it; otherwise return a
write-sink error. Likewise, `forward_ready_child_stream` maps every read error
other than `Interrupted` to `Eof` (`2903-2912`), silently dropping unread bytes.

### 2. Process-group cleanup failure can be hidden on successful direct exit

At drain expiry, `kill_child_group`'s error text is computed but only included
when pending bytes remain (`runtime.rs:3305-3325`). A silent descendant-held
pipe can therefore survive a failed group kill while the direct child status is
reported as success. The drain backpressure branches also discard group-kill
errors (`3282-3302`). Cleanup failure should either fail the command or be an
explicitly documented best-effort policy; add a fault-injection/regression path
if cleanup is part of the contract.

### 3. No absolute no-block guarantee for concurrent writers

Non-regular sinks use one-byte writes after `poll(POLLOUT)` (`3050-3065`). This
greatly narrows the blocking window and the socket regression passes, but the
stdout/stderr fds are blocking and multiple `run_layers` threads can poll/write
the same process sink concurrently. A writable result can become stale before
the byte write. Nonblocking sink fds or a serialized bounded writer would be
needed for a strict guarantee; current tests cover one writer only.

### 4. Process-group identity has a post-exit reuse edge

The group is addressed later by `Pid::from_child(child)` even after
`try_wait` has reaped the direct child (`3221-3321`). Normal descendants keep
the original group alive, so this is safe in the tested case. If a descendant
escapes with `setsid`/`setpgid` while retaining a pipe, the original PGID can
disappear and its numeric PID can eventually be reused during the 10-second
drain. A later group kill could then target an unrelated group. This is a rare
Unix PID-reuse edge, but it prevents claiming unconditional “no unrelated
process-group kill” without a platform-specific identity guard.

## Verdict

Successor fixes the prior core gaps: inherited captured-pipe tails, bounded
sink backpressure, and ordinary descendant cleanup are exercised and pass
serial/parallel focused tests. Core Unix behavior is acceptable conditional on
owner disposition of the four residuals above. At minimum, do not describe all
non-`EINTR` write/read errors as surfaced until `EPIPE` and non-`Interrupted`
read errors are intentionally classified and tested; do not claim cleanup is
authoritative while failed group kills can return success.
