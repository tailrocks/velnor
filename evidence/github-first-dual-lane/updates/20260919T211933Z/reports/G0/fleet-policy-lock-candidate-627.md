# Fleet-policy lock candidate-vs-base investigation

Date: 2026-09-20

## Decision

No causal production defect is proven. The reported candidate parallel-suite
failure was observed once, but exact clean base and candidate reruns pass. Do
not patch `fleet_policy.rs` or mask the assertion with a retry.

## Revisions and isolated runs

Candidate tree:

```text
/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-lane-compare
627c36f592437e4aef5f0d7d7f19ec85d391bac3
```

Exact base tree:

```text
/private/tmp/g1-lock-base-abe9ad82
abe9ad82a2d4d01b706bbc6122ab6ccb150faad9
```

Each revision used its own `CARGO_TARGET_DIR`; both worktrees were clean.
Default package test means Cargo's normal parallel test threads:

```text
base:      3/3 runs, 207 passed each
candidate: 3/3 runs, 221 passed each
candidate serialized (`-- --test-threads=1`): 221 passed
candidate focused lock test: 1 passed
```

An earlier candidate default run in the ordinary target reported `220 passed,
1 failed`, with only
`fleet_policy::tests::policy_directory_holds_exclusive_lock_until_file_close`
failing at line 4053 (`left -1`, `right 0`). That observation is real but its
causal source is unknown; the isolated reruns do not reproduce it.

## Production and fixture lifecycle

`PolicyDirectory::open` opens the final directory descriptor, wraps it with
`File::from_raw_fd`, then calls blocking `flock(LOCK_EX)`
(`crates/velnor-tools/src/fleet_policy.rs:1163-1171,1175-1192`). Root,
intermediate, and component descriptors use `O_CLOEXEC`; intermediate/root
descriptors are closed before the final descriptor is returned
(`:1210-1235,1290-1303`). The `PolicyDirectory.file` field is therefore the
owner whose close should release the lock.

The test opens a second directory descriptor, confirms nonblocking contention,
drops the owner, and retries (`:4035-4058`). The temporary test path is only
`tag + SystemTime::now().as_nanos()` and has no PID/random suffix
(`:3178-3187`).

## Darwin evidence

Local authoritative `man 2 flock` says locks are on files, duplicated/forked
descriptors are references to one lock, and contention with `LOCK_NB` returns
`EWOULDBLOCK`. `man 2 close` says the last close of a file holding an advisory
lock releases it.

Scratch probe: `/private/tmp/g1-flock-probe.c`, compiled as
`/private/tmp/g1-flock-probe` on Darwin 27.0 / arm64:

```text
same-process acquire: result=0 errno=0 fd=3
same-process blocked: result=-1 errno=35 (Resource temporarily unavailable) fd=4
same-process after close: result=0 errno=0 fd=4
same-path other process owns lock: result=-1 errno=35 (Resource temporarily unavailable) fd=4
```

The first three lines prove the production sequence's expected close
semantics. The last line is a deterministic same-path race: one process closes
its owner, another process acquires the same path first, and the original
process's post-drop contender still gets `EWOULDBLOCK`. This demonstrates a
possible fixture-path collision mechanism, not that the reported run collided.

## Ranked hypotheses

1. Unattributable concurrent workspace/build state remains possible because the
   original failing log/source timeline is unavailable.
2. Cross-process temporary-path collision is plausible: the fixture lacks a
   PID/random suffix, and the scratch probe reproduces the resulting lock
   assertion shape. No collision was observed in these isolated reruns.
3. Darwin `flock` or descriptor-close incompatibility is unsupported: exact
   base/candidate reruns and the direct probe pass.

No production source or remote state was changed.
