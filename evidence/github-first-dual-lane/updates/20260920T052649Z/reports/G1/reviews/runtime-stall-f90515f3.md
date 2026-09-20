# Unix S2 runtime-stall review — `f90515f30c3325a1a5469b1725a202a57a884002`

## Scope

- Exact source: `tailrocks/velnor`, branch `codex/github-first-hosted-g1-security-3ae`, commit `f90515f30c3325a1a5469b1725a202a57a884002` (parent `ea9686f0eb522e402441ecd461bd1f458b731d06`).
- Detached review path: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-hosted`.
- No source/generated/remote mutations. Worktree remained clean.
- Reviewed only the S2 command runner; no Velnor/Docker payload.

## Evidence run

```text
rtk proxy cargo test -p velnor-workflow --all-features --lib --locked -- \
  s2::runtime::run_cmd_stall_tests --nocapture --test-threads=1
5 passed; 0 failed; 1773 filtered out; 6.50s

rtk cargo clippy -p velnor-workflow --all-features --lib --locked -- -D warnings
No issues found

rtk cargo check -p velnor-workflow --lib --no-default-features \
  --target x86_64-unknown-linux-gnu --locked
Finished

rtk cargo check -p velnor-workflow --lib --no-default-features \
  --target x86_64-pc-windows-gnu --locked
Finished

rtk cargo check -p velnor-workflow --lib --target x86_64-pc-windows-gnu --locked
Finished
```

An attempted Linux `--tests` check reached the crate but failed in unrelated cross C build setup: `aws-lc-sys`/`openssl-sys` could not find `x86_64-linux-gnu-gcc`. The no-default-feature library checks above compile both Unix and non-Unix runner paths. No Windows runtime test was possible on this host.

## Correctness findings

1. **The Unix structural change addresses the blocking-reader class.** `run_command_with_polling` (`crates/velnor-workflow/src/s2/runtime.rs:2884-2980`) owns both `ChildStdout`/`ChildStderr`; `poll_child_streams` (`2828-2861`) polls readiness before every read. There is no Unix detached blocking reader. A descendant retaining a captured pipe therefore cannot block the owner thread's `Read` indefinitely.
2. **Stall/cancellation race is handled.** The loop calls `try_wait` before the deadline check, then repeats `try_wait` at expiry (`2897-2929`). A child that exited at the race is treated as completion; a still-running child is killed and synchronously waited/reaped. `try_wait`'s successful exit status is retained for the final status check.
3. **Normal exit drains boundedly and preserves status.** After direct-child exit, the loop drains both ready streams for at most `RUN_CMD_DRAIN_GRACE` (10 s), then drops the pipes and applies `check_unit_command_status` (`2959-2980`). HUP/ERR/NVAL readiness is consumed as EOF/error, so inherited pipe tails cannot create an unbounded wait. Output bytes reset the stall deadline (`2947-2955`).
4. **Non-Unix behavior is intentionally retained.** `#[cfg(not(unix))]` uses the existing threaded pump (`2717-2747`, `2983+`), whose receiver timeout checks direct-child exit and has the same bounded drain. Both macOS/Unix and Windows-GNU library code paths compile.

## Required regression gap

`blocked_child_pipe_is_killed_at_stall_deadline` passes, but its command
`exec 3< <(sleep 2 2>&-); printf 'before\\n'; read <&3` blocks the direct shell
on a process-substitution pipe. It does **not** prove the key descendant case:
the direct shell exits while a descendant still owns the captured stdout/stderr
write end. Add a test shaped like `printf before; sleep 2 & exit 0` (with the
captured stdout/stderr pipe) and assert return is bounded near descendant exit,
not at the full drain cap or an unbounded detached-reader wait. Add a
tail/exit-status assertion for bytes written before
the descendant closes, if the runner is made writer-injectable or otherwise
capturable. Current tests only prove stall kill, chatty heartbeat, and a plain
nonzero exit.

## Remaining risks / owner follow-up

- **Descendant lifecycle is not cleaned up.** Stall/error paths kill and wait
  only the direct `bash` PID (`2919-2924`). A descendant can remain alive after
  its inherited pipes are dropped; Unix process-group/session cleanup is absent.
  A long-lived descendant can leak/orphan and may receive SIGPIPE only when it
  writes. Decide whether the command contract requires process-group kill, and
  add a bounded cleanup/reaping regression if so.
- **Forwarding writes are synchronous.** `forward_ready_child_stream` calls
  `write_all`/`flush` on process stdout/stderr (`2805-2811`) inside the owner
  polling loop. If the runner's output sink backpressures, this write can block
  the stall guard and reintroduce a different deadlock; the old two-pump design
  at least kept the timer thread independent. Add a slow/full-output-sink test
  or make forwarding bounded/decoupled before claiming all pipe stalls are
  structurally eliminated.
- The Unix path swallows non-`EINTR` poll errors into a one-quantum sleep
  (`2937-2944`, `2966-2973`) and eventually reports a generic stall. This is
  bounded but loses the underlying poll error; preserve/report it if diagnosis
  quality matters.

## Verdict

The owner-thread poll loop is a valid bounded fix for reads blocked by an
inherited pipe and passes the focused tests, clippy, and both library target
checks. Do not treat the current regression as proof of inherited captured
stdout/stderr handling. Before final approval, add that exact fixture and make
an explicit decision on descendant process-group cleanup and synchronous output
backpressure.
