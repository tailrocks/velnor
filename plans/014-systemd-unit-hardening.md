# Plan 014: Harden the systemd unit (sandboxing) without breaking Docker or bind mounts

> **Executor instructions**: Follow step by step. This plan changes how the
> production daemon runs — **it must be validated against a real job before
> shipping** (see Step 3). Do not merge on unit-test evidence alone. STOP ⇒
> report. Update `plans/README.md` when done. **Security** plan.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/debian/`
> Mismatch against "Current state" ⇒ STOP.

## Status

- **Priority**: P2
- **Effort**: S (change) / M (validation)
- **Risk**: MED
- **Depends on**: none
- **Category**: security
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

The daemon runs as `User=root` with **no** systemd sandboxing. It parses
attacker-influenced input (job messages, JIT payloads, HTTP responses), so any
memory-safety or logic RCE executes as fully-privileged root. There is also no
`PrivateTmp`, so the 0600 curl token temp files live in the shared host `/tmp`,
widening their exposure window. The unit comment even says "Harden ... later if
desired." This plan adds a conservative hardening set that does not break the
daemon's real needs: it manages Docker via the socket and bind-mounts host work
directories into job containers, so the strictest presets (`ProtectSystem=strict`,
full syscall allowlists) are inappropriate — use `ProtectSystem=full` +
explicit `ReadWritePaths` and `PrivateTmp=true`.

## Current state

- `crates/velnor-runner/debian/velnor-daemon.service` — the full unit. The
  relevant part, excerpt at lines 8-43:
  ```ini
  [Service]
  Type=notify
  WatchdogSec=180
  Environment=VELNOR_JOB_CPUS=4
  Environment=VELNOR_JOB_MEMORY=12g
  EnvironmentFile=/etc/velnor/velnor.env
  EnvironmentFile=-/etc/velnor/secrets.env
  ExecStart=/usr/bin/velnor-runner daemon --url ${VELNOR_URL} ... --require-docker-socket
  Restart=always
  RestartSec=5
  TimeoutStopSec=10800
  # ... runs as root ... Harden to a docker-group user later if desired.
  User=root
  StateDirectory=velnor
  WorkingDirectory=/var/lib/velnor
  ```
- There is also a templated unit `velnor-daemon@.service` (per-instance) in the
  same directory — apply the **same** hardening there so single and multi-
  instance deployments match. Read it first.
- The daemon's real filesystem needs: the Docker socket (`/var/run/docker.sock`),
  its state dir (`/var/lib/velnor`, provided by `StateDirectory`), its config
  (`/etc/velnor`), and whatever host work dir it bind-mounts into containers
  (`VELNOR_WORK_DIR` / `VELNOR_DOCKER_HOST_WORK_DIR`). Enumerate these before
  choosing `ReadWritePaths`.

## Commands you will need

| Purpose            | Command                                                                                 | Expected                 |
|--------------------|-----------------------------------------------------------------------------------------|--------------------------|
| Lint unit (if avail)| `systemd-analyze verify crates/velnor-runner/debian/velnor-daemon.service`              | no errors (may warn)     |
| Security score     | `systemd-analyze security velnor-daemon.service` (on a host with the unit installed)    | improved vs baseline     |
| Build (unaffected) | `cargo clippy --workspace --all-targets --locked -- -D warnings`                        | exit 0                   |
| Live smoke         | run a real job on a host with the hardened unit (operator step — see runbook)           | job completes green      |

## Scope

**In scope**:
- `crates/velnor-runner/debian/velnor-daemon.service`
- `crates/velnor-runner/debian/velnor-daemon@.service` (same hardening)

**Out of scope**:
- Switching off `User=root` to a docker-group user — larger change (bind-mount
  ownership, socket group); note as deferred, do not attempt here.
- The doctor units — not the RCE-exposed path.

## Git workflow

- Branch: `advisor/014-systemd-unit-hardening`
- Commit: `chore(deb): sandbox the daemon systemd unit (NoNewPrivileges, PrivateTmp, ProtectSystem)`
- Do NOT push/PR unless instructed.

## Steps

### Step 1: Enumerate the daemon's real read-write paths

From the unit and code, list every path the daemon must write: `/var/lib/velnor`
(state), the host work dir it mounts into containers, `/var/run/docker.sock`
(socket access), and any log dir. Write this list in the PR description — it
determines `ReadWritePaths`.

**Verify**: no code change; list documented.

### Step 2: Add conservative hardening directives

Add to `[Service]` in **both** unit files:

```ini
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=read-only
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictSUIDSGID=true
ReadWritePaths=/var/lib/velnor <the host work dir> <any log dir>
```

Do **not** add `ProtectSystem=strict`, `PrivateDevices=true`, `PrivateMounts`,
or a `SystemCallFilter` in this pass — those risk breaking Docker socket access,
bind-mount propagation, and container operations, and need per-directive live
testing. Keep `User=root` for now (deferred). If the daemon writes outside the
`ReadWritePaths` you listed, add that path — do not loosen `ProtectSystem`.

**Verify**: `systemd-analyze verify crates/velnor-runner/debian/velnor-daemon.service`
→ no errors (if `systemd-analyze` is available; it may not be on macOS — then
this verification is deferred to the live host).

### Step 3: Validate against a real job (REQUIRED before merge)

On a host with the hardened unit installed (or the fixture smoke lane): run a
real job end-to-end and confirm it completes green — the container starts, bind
mounts are visible, Docker operations work, logs stream, artifacts upload. A
hardening directive that breaks any of these must be identified and removed/
adjusted. Record the `systemd-analyze security` score before/after in the PR.

**Verify**: a real job completes successfully under the hardened unit. If you
cannot run a live job, mark this step BLOCKED and hand off to the operator — do
**not** merge unvalidated.

## Test plan

- No Rust tests (packaging change). Validation is the live smoke run in Step 3
  plus `systemd-analyze verify`/`security`.

## Done criteria

- [ ] Both `velnor-daemon.service` and `velnor-daemon@.service` carry the Step 2 directives
- [ ] `ReadWritePaths` covers every path the daemon writes (state, work dir, logs)
- [ ] `systemd-analyze verify` passes (or is explicitly deferred to the host)
- [ ] A real job completes green under the hardened unit (Step 3) — or the plan is BLOCKED pending operator validation
- [ ] `systemd-analyze security` score recorded before/after in the PR
- [ ] Only the two unit files modified (`git status`)
- [ ] `plans/README.md` row updated

## STOP conditions

- The unit doesn't match the excerpt (drift).
- Any hardening directive breaks a real job (container start, bind mounts, Docker
  socket, log stream) — remove/adjust that directive and report which one.
- You cannot run a live validation and no operator is available — mark BLOCKED,
  do not merge on static review alone.

## Maintenance notes

- Deferred: dropping root to a docker-group user, and adding `SystemCallFilter`/
  `PrivateDevices` — each needs its own live-tested PR.
- `PrivateTmp=true` also shrinks the curl token temp-file exposure window
  (relates to plan 008's argv concern) — note the security win.
- Reviewer: the risk is a directive that silently breaks Docker in production;
  insist on the Step 3 live evidence in the PR.
