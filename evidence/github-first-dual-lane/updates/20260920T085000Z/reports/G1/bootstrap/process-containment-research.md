# G1 hosted process-containment research

Observed 2026-09-20 (Asia/Ho_Chi_Minh). Bounded source/design review only.
No Velnor, Docker, OrbStack, hosted job, or hostile payload was run; no host or
source implementation was changed. This is not a G1 approval.

## Decision

The `43ba` runtime has cooperative Unix process-group cleanup. It does not
provide hostile arbitrary-descendant containment. A command such as:

```sh
(setsid sleep 30) >/dev/null 2>&1 & exit 0
```

can leave the keeper process group, let `bash` return zero, and survive cleanup.
That behavior is consistent with the source contract and must not be relabeled
as a PID-safety failure that can be repaired by a process-table scan.

The implementable hosted guarantee is an outer kernel-owned boundary: a fresh
Linux VM/container with a root-owned cgroup v2 (or Docker's PID/cgroup
boundary), a dedicated non-root job UID, no cgroup delegation, no host Docker
socket, and bounded teardown that proves the owned boundary is empty. Same-user
native macOS process APIs cannot provide this hostile guarantee. Windows has a
Job Object equivalent. Fail closed when the boundary cannot be established;
never fall back to `setsid`, `killpg`, PID scans, or a claimed process tree.

## Exact reviewed source

All Velnor source claims below refer to commit
`43ba3b414f245bf3aa9176afce5bf97f2d1e5235` (`fix(workflow): use stable
process-group containment`):

| Surface | Exact evidence | Finding |
| --- | --- | --- |
| Local generator command | `crates/velnor-workflow/src/s2/runtime.rs:2830-2850` | A live `sleep` keeper owns a Unix process group; `setsid`/group escape is explicitly outside the supported cleanup contract. |
| Group creation | `s2/runtime.rs:2964-3036` | `sleep 3600` gets `process_group(0)`; `bash -euo pipefail -c` joins that PGID. |
| Group cleanup | `s2/runtime.rs:3368-3442` | Cleanup kills the keeper PGID and bounded-reaps the keeper; it has no descendant ownership scan. |
| Direct-child completion | `s2/runtime.rs:3969-4032` | After direct child exit, the keeper is cleaned and the direct status is checked; no detached-session census exists. |
| Generator command contract | `.github/ci/project.toml:326-415` | `rust-velnor-workflow`, `rust-velnor-workflow-contract`, `rust-velnorctl`, and production topology are `linux-x64` units; several allow Docker/testcontainers. |
| Daemon job container | `crates/velnor-runner/src/container.rs:267-449` | Separate Docker job lifecycle: writable workspace/temp/cache mounts, per-job network, shell PID1, and no read-only/cap-drop proof in this path. |
| Mac Docker socket | `container.rs:1066-1100` | Native Linux can use a lease proxy; macOS currently resolves/mounts the host Docker socket until a VM-reachable lease transport exists. |
| Docker placement | `crates/velnor-runner/src/execution/docker.rs:232-307` | Existing cgroup preflight is resource-placement validation, not proof that hostile descendants or daemon control are isolated. |

The generator/runtime command lane and daemon job-payload lane are distinct.
Changing the former's process-group wording does not prove the latter's Docker
ownership boundary. Conversely, the daemon's container lifecycle cannot be used
as evidence that a host-shell `velnor-workflow` subprocess is contained.

## Required upstream comparison

The inspected `actions/runner` source is tag `v2.336.0`, commit
`98aabcd429c4e8402406c56ce2d26387fed3b9ce`:

- Unix cancellation calls `Process.Kill()` on the invoked process only:
  [`ProcessInvoker.cs:828-867`](https://github.com/actions/runner/blob/v2.336.0/src/Runner.Sdk/ProcessInvoker.cs#L828-L867).
- Windows builds a best-effort parent-PID tree and ignores scan/kill errors:
  [`ProcessInvoker.cs:667-774`](https://github.com/actions/runner/blob/v2.336.0/src/Runner.Sdk/ProcessInvoker.cs#L667-L774).
- Cancellation escalation and pipe-drain cleanup are separate from ownership:
  [`ProcessInvoker.cs:443-465`](https://github.com/actions/runner/blob/v2.336.0/src/Runner.Sdk/ProcessInvoker.cs#L443-L465).
- Container steps still invoke Docker through the runner process path:
  [`StepHost.cs:206-290`](https://github.com/actions/runner/blob/v2.336.0/src/Runner.Worker/Handlers/StepHost.cs#L206-L290).

The runner is protocol reference, not a hostile-containment proof. PID-tree
scans have reparenting, PID reuse, TOCTOU, and permission gaps. Do not copy that
mechanism as Velnor security evidence.

## Platform mechanisms and limits

### Linux hosted Ubuntu

Use a fresh disposable Ubuntu VM and a root-owned cgroup-v2 subtree. Run the
payload as a dedicated non-root UID with no cgroup-controller access or host
Docker socket. The kernel contract says a child fork remains in its cgroup,
delegated processes cannot move outside the delegated subtree, and
`cgroup.kill=1` kills the cgroup and descendants while handling concurrent forks:
[Linux cgroup v2, `cgroup.kill`](https://cdn.kernel.org/doc/html/latest/admin-guide/cgroup-v2.html#the-cgroup-kill-interface).

The trusted controller must own the lifecycle. A later implementation may use a
transient systemd **service** (not a `--scope`/`--wait` shortcut) with:

```sh
systemd-run --system --unit="$unit" --no-block --service-type=exec \
  --uid=velnor-job --gid=velnor-job \
  -p KillMode=control-group -p SendSIGKILL=yes \
  -p NoNewPrivileges=yes -p RuntimeMaxSec="$deadline" \
  -p TimeoutStopSec=5s \
  /usr/local/libexec/velnor-command-supervisor
```

The supervisor/controller must then:

1. Capture and validate the unit's exact `ControlGroup` and corresponding
   `/sys/fs/cgroup` directory before accepting work.
2. Track the direct command with a stable handle if needed; a pidfd identifies
   only that process, not descendants ([`pidfd_open(2)`](https://man7.org/linux/man-pages/man2/pidfd_open.2.html),
   [`pidfd_send_signal(2)`](https://www.man7.org/linux/man-pages/man2/pidfd_send_signal.2.html)).
3. On direct exit, timeout, or error, kill the owned cgroup (`cgroup.kill=1` or
   an equivalent root-owned systemd control operation) before interpreting the
   command status.
4. Read `cgroup.events` until `populated 0` and verify the owned process list is
   empty. Missing cgroup files, failed kill, unresolved population, or a cgroup
   that disappeared before proof is a hard failure.

`systemd-run --wait` returning a direct status is not enough: the trusted
controller must retain a live ownership/control point or separately prove the
unit's cgroup was killed and emptied. Do not return success from a direct
`exit 0` while a detached child remains.

Docker can supply the outer boundary for the candidate lane when all of these
are admitted from trusted base-owned setup: private PID namespace, digest-pinned
Linux image, `--network=none`, `--read-only`, `--cap-drop=ALL`,
`no-new-privileges`, numeric non-root UID, disposable bounded mounts, and forced
container removal followed by a no-residue check. Docker daemon control itself
is not an untrusted payload boundary; a mounted host socket defeats it.

### Windows hosted VM

Use a Windows Job Object. Create it, set kill-on-close, assign the process before
resume, reject breakaway flags, and verify `IsProcessInJob`; use
`TerminateJobObject` on timeout. Microsoft documents that child processes remain
in the job by default, breakaway flags can escape, and `KILL_ON_JOB_CLOSE` kills
associated processes: [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects).
`CREATE_NEW_PROCESS_GROUP` is only console event grouping, not containment:
[process creation flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags).
No PID-tree fallback is acceptable.

### Native macOS

`posix_spawnattr_setpgroup`/`killpg` and launchd's default
`AbandonProcessGroup=false` provide same-process-group cleanup only:
[`posix_spawnattr_setpgroup`](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man3/posix_spawnattr_getpgroup.3.html),
[`killpg`](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/killpg.2.html),
[`launchd.plist`](https://github.com/apple-oss-distributions/launchd/blob/main/man/launchd.plist.5#L1893-L1900).
An untrusted child can call `setsid`; there is no macOS cgroup equivalent that
meets this hostile same-user contract. Native Mac strong descendant isolation is
therefore unavailable; Velnor payloads requiring this guarantee must run inside
the later Linux Docker/VM boundary.

## Acceptance cases (future hosted canary only)

These are test requirements, not observations made here:

- ordinary fork descendant is killed and the owned cgroup/job is empty;
- `setsid` detached sleeper, double-fork, and reparented child are killed by the
  outer cgroup/container/Job Object, not by a PGID scan;
- direct `exit 0` with a live descendant cannot produce success;
- fork racing teardown cannot repopulate the owned boundary after the final kill;
- missing, foreign, delegated, or unreadable ownership state fails closed;
- Windows breakaway attempt fails and `IsProcessInJob` remains true;
- macOS native process-group result is recorded as cooperative-only, never as a
  hostile containment pass.

## Observation limits

- No hostile probe, Velnor runtime, Docker/OrbStack command, or hosted canary was
  executed on the Mac or anywhere in this review.
- Source evidence is pinned to the Velnor and runner revisions above; it does
  not prove current deployed images, runner versions, daemon configuration, or
  cgroup policy.
- Official platform pages document available mechanisms, not this project's
  deployment. A hosted Linux canary must prove every precondition and failure
  path before any G1/G4 claim.
- Same-user host root, Docker daemon control, cgroup delegation, or equivalent
  privilege can defeat process-level containment. The design must exclude those
  capabilities rather than claim them away.

