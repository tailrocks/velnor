# Runtime stall successor review: `5b61841d`

Date: 2026-09-20  
Reviewer: independent read-only review (`/root/g1_cache_semantics`)  
Source: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-hosted`  
Branch: `codex/github-first-hosted-g1-security-3ae`  
Reviewed commit: `5b61841dff1607c6dcb0d9ede9b93b1787bcddd6`  
Parent: `c70428c919edc6ab938fcfddd9546ecdedc713c5`

No source, generated, host, or remote state was changed. No Velnor/Docker
payload was run.

## Verification

All of the following were run against the exact commit with a clean worktree:

```text
cargo test -p velnor-workflow --all-features --lib --locked -- \
  run_cmd_stall_tests --nocapture --test-threads=1
15 passed; 0 failed; 0 ignored; 1769 filtered out; 16.95s

cargo clippy -p velnor-workflow --all-features --lib --locked -- -D warnings
pass
cargo check -p velnor-workflow --lib --target x86_64-unknown-linux-gnu --locked
pass
cargo check -p velnor-workflow --lib --target x86_64-pc-windows-gnu --locked
pass
cargo fmt --all -- --check
pass
git diff --check HEAD^ HEAD
pass
```

The Linux tests exercised the real Unix process/poll path. The Windows check
was compile-only; the existing non-Unix threaded-pump path is not behaviorally
covered by these tests.

## Prior residuals assessed

* **EPIPE/write failure:** fixed for the Unix path. `flush_pending_output`
  returns all non-`INTR`/`WOULDBLOCK` write errors, and
  `closed_output_sink_fails_instead_of_reporting_success` passes (runtime.rs
  around lines 3088-3110 and 3870-3900).
* **Child read errors:** fixed for the Unix path. `ReadError` is distinct from
  EOF and is converted to a command error; the synthetic read-error test
  passes (around lines 2905-2980 and 3900-3925).
* **Direct-child PID/PGID reuse:** materially improved. A live, unreaped
  keeper is spawned as a process-group leader (around lines 2816-2875), and
  the command is assigned to that keeper PGID. Cleanup signals the keeper PGID
  rather than a reaped direct-child PID (around lines 3131-3157).
* **Surviving captured pipes / output after kill:** materially improved.
  `wait_for_streams_closed` drains only to detect post-kill bytes and rejects
  open streams or observed output (around lines 3214-3255 and 3521-3568).
  The existing descendant-output and stalled-descendant tests pass.

## Blocking finding: keeper descendant leak

The new keeper is not a single process. It runs:

```text
bash -euo pipefail -c 'while sleep 3600; do :; done'
```

`group_keeper.wait()` reaps the shell, but does not itself prove that the
shell's `sleep` child exited. The normal cleanup paths have no post-cleanup
check for keeper descendants; their pipe-closure check only observes the
command's stdout/stderr.

This was observed after a passing exact-commit test, not inferred from a
timeout:

```text
17244  PPID 1  PGID 17241  S  Sun Sep 20 13:19:47 2026  sleep 3600
cwd: /private/var/folders/8p/h376l_nn3375kyj72czdq2x80000gn/T/velnor-workflow-output-closed-33017553980205595034105726040404918272

41507  PPID 1  PGID 41504  S  Sun Sep 20 13:02:16 2026  sleep 3600
cwd: /private/var/folders/8p/h376l_nn3375kyj72czdq2x80000gn/T/velnor-workflow-output-closed-33017534591116259593712371728100884480
```

PID 17244 was created during the isolated
`closed_output_sink_fails_instead_of_reporting_success` run at this exact
commit. The test reported `1 passed`, yet the `sleep 3600` remained orphaned
under PID 1 after the test returned. PID 41507 was an earlier identical
residue. Neither process was killed during this review. The matching
`velnor-workflow-output-closed-*` cwd ties both processes to the fixture, not
to an unrelated host payload.

Likely enabling race: killing the shell process group while the shell is
between loop iterations can kill the shell and allow a newly forked `sleep`
to appear after the group signal. Regardless of the exact OS race, the
passing test demonstrates that `finish_group_keeper` does not establish
keeper-descendant termination. This is a cleanup/lifecycle blocker, not a
cosmetic test leak. Prefer a keeper that does not fork a child (for example a
single-process long sleep/pause leader), plus a bounded no-residue regression;
also make cleanup explicitly prove/reject unresolved keeper descendants.

There is a second lifecycle weakness in the error fallbacks. In
`finish_group_keeper` and `abort_child` (around lines 3159-3190), if group
kill fails and direct keeper kill also fails, the code may only perform a
`try_wait` probe (or no wait in `finish_group_keeper`) and return an error.
That correctly fails closed for the command result, but it cannot prove the
keeper was reaped and can leave a live child behind. The error path needs an
explicit ownership/liveness policy, not only an error string.

## Output-gate limitation (not fully no-block)

`RUN_CMD_OUTPUT_GATE` (around lines 2722, 3196-3212) serializes poll-plus-
flush only when a runtime loop already has pending output. This closes the
stale-readiness race among participating `run_command_with_polling` calls,
but it does not protect:

* `run_unit`'s `println!("::group::...")`/`println!("::endgroup::")` calls
  (around lines 3653-3660);
* other threads/processes writing the same stdout/stderr file descriptors;
* a runtime loop's first child-pipe read, before pending output causes the
  gate to be acquired.

Therefore the one-byte nonblocking-intent write is still not an absolute
no-block guarantee: an external writer can change sink state between poll and
`rustix::write`. The gate also serializes the whole poll/read/flush section,
so a busy unit can delay other participating units and make their child pipes
fill. Existing tests cover one blocked UnixStream and closed sink, but no
external/concurrent writer adversary or gate starvation case.

## Verdict

The Unix core changes for EPIPE, read errors, live PGID ownership, and
captured-pipe survivor detection are supported by source inspection and 15
passing focused tests. Do **not** approve the cleanup claim or integrate this
runtime successor as fully proven yet: the exact passing closed-output path
leaves a keeper `sleep 3600` orphan. Require a single-process keeper or
equivalent race-proof lifecycle, a regression that checks keeper descendants
after every abort/normal cleanup, and explicit handling of cleanup/reap
fallbacks. Treat the output mutex as intra-runtime serialization only unless
the contract is narrowed; it does not prove safety against external writers.
