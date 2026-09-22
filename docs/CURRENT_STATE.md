# Velnor Bastion — Current Live State

## Authority and capture

This file records observed state. It is the current operational reference for the bastion host.

- Host: `root@37.27.110.241` (`bastion`)
- Repository: `https://github.com/donbeave/velnor-bastion`
- Capture time: `2026-09-23T00:52:43+02:00`
- Method: read-only SSH inspection and read-only local Velnor queries
- Remote changes: none
- Secret handling: secret values were not read into this repository; only safe presence/empty-state facts are recorded

## Bottom line

**Current state: NOT READY / DEGRADED.**

The base installation exists and is internally coherent:

- Debian 13 (`trixie`) on x86_64
- `velnor-runner` `0.1.274` installed from the signed Velnor APT repository
- Docker Engine `29.8.1` active
- Velnor controller and guardian processes active
- 16 local slot processes present
- Docker execution backend configured with `velnor/job-ubuntu:26.04`
- Permit ledger configured for `max_jobs = 16`, with generation reconciled and no active permits/demands

The setup is not a usable full GitHub runner fleet:

- `registered_slots = 0`, `actual_ready_slots = 0`, `executor_ready_slots = 0`
- GitHub reachability is false in the local health vector
- Routing proof and runner-group proof are invalid
- The configured GitHub URL is the placeholder `https://github.com/OWNER/REPO`
- `GITHUB_TOKEN` is empty in `/etc/velnor/velnor.env`; optional `/etc/velnor/secrets.env` is absent
- The active controller journal repeatedly reports HTTP 401 and skips registration because the GitHub URL or PAT is unavailable
- `velnorctl top ...` cannot reach the control API

## Host

| Item | Observed value |
|---|---|
| OS | Debian GNU/Linux 13 (`trixie`) |
| Kernel | Linux `6.12.94+deb13-amd64` |
| CPU | AMD EPYC 9454P, 48 cores / 96 logical CPUs, 1 socket |
| Memory | 125 GiB visible; 4 GiB swap, unused at capture |
| Root storage | `/dev/nvme0n1p4`, XFS, mounted at `/`, ~34 GiB used |
| Secondary storage | `/dev/nvme1n1`, 3.5 TB, no partition, filesystem, or mountpoint observed |
| Network listeners | SSH on TCP/22; no Velnor TCP listener observed |

The secondary NVMe safeguard is currently satisfied by observation. Nothing in this inspection touched it.

## Installed release and package sources

| Item | Observed value |
|---|---|
| Package | `velnor-runner 0.1.274` (`amd64`) |
| `velnor-runner` | `/usr/bin/velnor-runner`, SHA-256 `d5be3751959ec96f849a2a31e6f86dbac882324a6957d7bb744c68c0bc227887` |
| `velnorctl` | `/usr/bin/velnorctl 0.1.0` |
| Release tag/commit | `v0.1.274` / `120f223655587ab0bcf2530cd4b203e0375a9dca` |
| Installed-release check | `velnor-runner release verify-installed` passed |
| Velnor APT source | `https://velnor-apt.tailrocks.com`, `stable/main`, `amd64` |
| Docker APT source | `https://download.docker.com/linux/debian`, Debian `trixie/stable`, `amd64` |
| Velnor APT key fingerprint | `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801` |
| APT candidate | `0.1.274`; `0.1.273` remains listed as an older candidate |

The active release record also identifies the job image index as:
`sha256:42398586433d769982177f7b12d5dad269634eea7a81957012fdca47cf98e690`.

## Docker

- Client/server: `29.8.1 / 29.8.1`
- Storage driver: `overlayfs`
- Docker root: `/var/lib/docker`
- Host capacity visible to Docker: 96 CPUs, ~125.5 GiB
- Containers: `0` total, `0` running
- Local job image: `velnor/job-ubuntu:26.04`, ~5.61 GB
- Docker daemon configuration:

  ```json
  {
    "log-opts": { "max-size": "10m" },
    "default-address-pools": [
      { "base": "172.30.0.0/16", "size": 24 }
    ]
  }
  ```

## Services and process layout

| Unit/process | Current state | Evidence/details |
|---|---|---|
| `velnor-controller@default.service` | active/running; disabled | PID `142341`; controller scope `default`; desired ready `16` |
| `velnor-guardian.service` | active/running; disabled | PID `145737`; local supervision only |
| `velnor-daemon.service` | inactive; disabled | No active daemon unit |
| `velnor-daemon@default.service` | inactive; disabled | No active daemon instance unit |
| `velnor-runner.service` | not found | The installed layout uses controller/guardian/slot units/processes instead |
| `velnor-doctor.timer` | inactive; disabled | No scheduled doctor probe |
| `velnor-fleet-policy-audit.timer` | inactive; disabled | No scheduled fleet-policy audit |
| `velnor-runner` processes | 18 total | 1 controller, 1 guardian, 16 `slot` processes |

The job slice is configured with `CPUQuota=9120%`, `MemoryHigh=90%`, `MemoryMax=95%`, `MemorySwapMax=0`, and `TasksMax=4096`. The 9120% quota corresponds to 95% of 96 logical CPUs.

Active controller and guardian units are disabled rather than enabled. This means persistence across reboot is not proven by the current state.

## Effective configuration observed

### `/etc/velnor/runner.toml`

```toml
max_jobs = 16

[capacity]
max_jobs = 16
```

### `/etc/velnor/execution.toml`

```toml
[execution]
backend = "docker"
max_jobs = 16
```

### `/etc/velnor/velnor.env`

Safe, non-secret values observed:

```text
VELNOR_URL=https://github.com/OWNER/REPO
VELNOR_NAME=velnor
VELNOR_LABELS=velnor,velnor-target-mvp
VELNOR_SLOTS=16
VELNOR_TRUST_SCOPE=untrusted
VELNOR_JOB_CPUS=(empty)
VELNOR_JOB_MEMORY=(empty)
VELNOR_WORK_DIR=/var/lib/velnor/work
VELNOR_GITHUB_HTTP_TRANSPORT=native
VELNOR_CAPABILITY_VALIDATION=strict
MISE_LOCKFILE=1
MISE_LOCKED=1
MISE_LOCKED_VERIFY_PROVENANCE=1
VELNOR_MAX_JOBS=16
max_jobs=16
```

Secret status only: `GITHUB_TOKEN` is present as a variable but empty. `/etc/velnor/secrets.env` and `/etc/velnor/default.secrets.env` are absent. No token value is documented here.

### `/var/lib/velnor/routing-policy.json`

```json
{
  "group": "velnor",
  "selected_repositories": ["tailrocks/velnor"],
  "labels": ["velnor"],
  "trust_scope": "untrusted"
}
```

This is the observed routing policy. It is not the five-consumer rollout described by the historical campaign documents.

### `/var/lib/velnor/daemon-exec.json`

Observed execution values:

- `slots`: `16`
- `max_jobs`: `16`
- `docker_image`: `velnor/job-ubuntu:26.04`
- `permit_ledger`: `/var/lib/velnor/permit-ledger.db`
- `work_dir`: `/var/lib/velnor/work`
- `trust_scope`: `untrusted`
- `execute_scripts`: `true`
- `require_docker_socket`: `true`
- `target_mvp_labels`: `false`
- `target_mvp_arm_label`: `false`
- `replace`: `true`

## Health and capacity

### Controller health file

`/var/lib/velnor/health.json` observed:

```json
{
  "control_live": true,
  "journal_writable": true,
  "github_reachable": false,
  "routing_valid": false,
  "runner_group_valid": false,
  "desired_ready_slots": 16,
  "actual_ready_slots": 0,
  "registered_slots": 0,
  "capacity_permits": 16,
  "executor_ready_slots": 0,
  "external_canary": "unknown",
  "execution_backend": "docker",
  "state": "degraded"
}
```

### Operator CLI

`velnorctl status --json --state-dir /var/lib/velnor --no-color` returned `state=not_ready` with these alerts:

- `control_not_live`
- `github_unreachable`
- `routing_invalid`
- `runner_group_invalid`
- `capacity_shortfall`
- `no_schedulable_capacity`

The CLI returned `registered_slots=0`, `actual_ready_slots=0`, and `executor_ready_slots=0`. It also reported `desired_ready_slots=4`, unlike the 16-slot controller health file and configuration. Treat that mismatch as unresolved configuration/control-plane drift.

`velnorctl top host|instances|slots|jobs|storage --output json` returned:
`control.api.unavailable` / `control socket is unavailable`.

The health socket file exists at `/var/lib/velnor/health.sock`; `/run/velnor/health.sock` is absent. Socket existence does not prove that the operator control API is available.

### Permit ledger

- Path: `/var/lib/velnor/permit-ledger.db`
- File: root-owned, mode `0644`
- `max_jobs`: `16`
- `generation`: `0`
- `reconciled_generation`: `0`
- Active permits: `0`
- Demand rows: `0`

The ledger is empty and internally reconciled. That proves no current permit leak; it does not prove GitHub registration or job execution.

## What is present vs. what is missing

| Area | Present now | Not proven / missing now |
|---|---|---|
| OS/hardware | Debian 13, x86_64, 96 logical CPUs, 125 GiB, secondary NVMe untouched | None observed in this snapshot |
| Package | Signed APT install, `velnor-runner 0.1.274`, release verification passes | Nothing for package coherence |
| Docker | Engine active, expected image present, daemon configured | No job container has run in this snapshot |
| Local Velnor | Controller, guardian, 16 slot processes, ledger | Services are disabled; daemon units inactive |
| GitHub | URL/token fields exist in config | URL is a placeholder; token is empty; zero registered slots; repeated 401 |
| Routing | Local policy file exists | Only `tailrocks/velnor` selected; routing/group proof invalid |
| Control API | Health file and health socket path exist | Operator API unavailable; CLI says control is not live |
| Qualification | Local installation evidence exists | No live five-consumer replay or external canary is proven by this snapshot |

## Resume blockers

To return to the goal of a full, usable bastion setup, the next work must start with current-state repair and re-verification:

1. Set the real GitHub repository/organization/enterprise URL. The current `OWNER/REPO` value is a placeholder.
2. Provide a valid GitHub credential through the intended secret path. Current `GITHUB_TOKEN` is empty and `secrets.env` is absent. Keep the value out of Git and future snapshots.
3. Align runner-group and routing policy with the actual intended consumers. Current policy selects only `tailrocks/velnor` and uses group `velnor`.
4. Resolve the control-plane mismatch: controller health says live while `velnorctl` cannot reach the control API and reports `not_ready`.
5. Decide and verify the intended persistence model. The active controller/guardian units are disabled; both daemon units and health/audit timers are inactive.
6. After repair, prove registration, ready slots, Docker preflight, control API reads, canary execution, and the ordered consumer replay. None of those full-path checks is established by this snapshot.

Do not treat the historical G0–G8 completion claim as proof that these blockers are cleared.

## Historical documents

- [Campaign ledger](../CAMPAIGN_LEDGER.md): historical rollout record; its completion claims are not current host health.
- [Paused handoff](goal-handoffs/velnor-rollout-bastion--20260921T220213Z--antigravity--930b8384.md): historical plan/checkpoint; it also conflicts with the live snapshot.
- [APT repository architecture](APT_REPOSITORY.md): package/repository design and signing reference, not a live readiness report.

## Reproduction commands used

All remote commands were read-only. The important checks were:

```bash
ssh root@37.27.110.241 'hostname; date -Is; systemctl --failed'
ssh root@37.27.110.241 'dpkg-query -W velnor-runner; velnor-runner --version; docker version'
ssh root@37.27.110.241 'systemctl is-active velnor-controller@default.service velnor-guardian.service'
ssh root@37.27.110.241 'pgrep -a -x velnor-runner'
ssh root@37.27.110.241 'cat /var/lib/velnor/health.json'
ssh root@37.27.110.241 'velnorctl status --json --state-dir /var/lib/velnor --no-color'
ssh root@37.27.110.241 'lsblk -o NAME,SIZE,TYPE,FSTYPE,MOUNTPOINTS'
```
