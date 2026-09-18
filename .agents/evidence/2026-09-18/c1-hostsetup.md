# C1 host-setup DESIGN — bastion campaign (read-only design, NO bastion writes)

Date (UTC): 2026-09-17. Status: DESIGN ONLY — no command in this file has been
executed against bastion; execution is a later phase.
Inputs: `/tmp/c1-ansible.md` + fetched bytes `/tmp/c1-ansible/` (source pin
`38a8fb5777fd02fec8b3904d86387aba05940e9a`, zero drift) + spec §6–§7 +
`/tmp/a0-bastion.md` (bare host: no Docker, no Velnor, idle, `nvme1n1` pristine).
Constraints: idempotent, non-destructive, SSH/recovery preserved, second NVMe
untouched, no libvirt/KVM/QEMU, no `docker system prune`, no automated
`full-upgrade` on CI infra (per `upgrade-debian.md` runbook split).

## 0. Phase order

`P0 read-first inventory → P1 SSH/recovery + NVMe guards → P2 base (UTC, APT pkgs,
mise) → P3 Docker pinned install → P4 cgroup/storage verify → P5 managed paths →
P6 Velnor APT install (spec §7 transaction) → P7 drain-verify → evidence record`.
Every phase re-runs cleanly on an already-provisioned host (all writes are
declare-state, never append/assume-absent).

## 1. P0 — read-first inventory (all read-only, run before any write)

Spec §6 requires reading existing state first. One SSH session, fixed order:

```sh
# OS / kernel / CPU / NUMA
uname -a; cat /etc/debian_version; cat /etc/os-release
lscpu; nproc; lscpu | grep -i numa; timedatectl show
# RAM / swap
free -m; swapon --show
# disks / signatures / mounts / inodes (nvme1n1 guard inputs, §3)
lsblk -o NAME,SIZE,TYPE,MOUNTPOINT,MODEL,SERIAL,FSTYPE,PARTUUID
ls -l /dev/disk/by-id/ | grep -i nvme
findmnt -R -o TARGET,SOURCE,FSTYPE,OPTIONS /; df -hT; df -i /
cat /proc/mdstat; pvs; vgs; lvs   # expect: no RAID/LVM (A0: none)
blkid /dev/nvme1n1                # expect: EMPTY — pristine, no signature
# Docker / cgroup (expect: absent at first provision)
command -v docker dockerd containerd podman nerdctl ctr || true
ls -ld /var/lib/docker /var/lib/containerd /run/docker.sock 2>&1 || true
stat -f -c %T /sys/fs/cgroup      # expect: cgroup2fs
systemctl --version | head -1
# systemd / jobs / sockets / routes / firewall
systemctl list-units --type=service --state=running
ps aux | grep -iE 'velnor|runner|actions' | grep -v grep || true
ss -tlnp; ip route show; ip -6 route show; nft list ruleset 2>&1 | head -30 || iptables -L -n 2>&1 | head -30
# existing Velnor state
dpkg -l | grep -i velnor || true
ls -ld /etc/velnor /var/lib/velnor /run/velnor /var/cache/velnor 2>&1 || true
uptime
```

Record all output as `old observation → new evidence → consequence` against A0.
ABORT on: unexpected mounts on `nvme1n1`, any running Velnor/job processes at
first provision (means host state changed since A0 — re-qualify before writing).

## 2. SSH/recovery preservation (hard rules, every phase)

- Never stop/disable/restart `ssh.service`; never rewrite `sshd_config`,
  `authorized_keys`, or firewall rules in a way that drops established tcp/22.
  No firewall change at all during host setup (bastion ships no active ruleset;
  container networks use Docker's own chains only).
- Never reboot without an explicit operator window; upgrades follow the
  `upgrade-debian.md` split (automated source-rewrite only, manual release
  upgrade in a drained window).
- Every destructive-capable step is preceded by its P0 re-check; any Ansible run
  uses `--check --diff` first, then `--limit bastion` (never unscoped `all`
  against shared inventory).

## 3. Second-NVMe-untouched guarantee (`nvme1n1`)

- FORBIDDEN, unconditionally: `mkfs*`, `parted/mkpart`, `fdisk`, `gdisk`,
  `sfdisk`, `pvcreate/vgcreate/lvcreate`, `mdadm --create`, `wipefs`,
  `dd … of=/dev/nvme1n1*`, any `mount` of `nvme1n1`, any fstab entry for it.
  No playbook in this design references `nvme1n1` except in assertions.
- Pre-write assertion (fails closed — any output aborts the run):
  `test -z "$(lsblk -nr -o MOUNTPOINT /dev/nvme1n1 | tr -d ' \n')"` and
  `blkid /dev/nvme1n1` must exit non-zero (no signature); post-run, re-run both
  plus `lsblk` serial compare against A0 (`…52M3P8CGN` unpartitioned).
- The design provisions NOTHING onto `nvme1n1`: Docker data-root stays default
  `/var/lib/docker` on root FS (selene D6 rationale, §4); owned scratch and
  `/var/cache/velnor` live on the existing root filesystem.

## 4. Pinned Docker + Buildx/Compose install (chosen shape)

Base: generic `install-docker.yml` (`85b9a2c1…`, `hosts: all`) + C1 deltas:
keep `default-address-pools 172.30.0.0/16 /24`; DROP the selene
mount-guard drop-in (unit will never exist on bastion; add a bastion-named
drop-in later only if Docker-after-mount ordering is ever needed); keep default
data-root `/var/lib/docker` (no `data-root` key). Close the 3 C1 gaps:

**a. Version pins** (spec §6.1 "pinned package setup"). Resolved 2026-09-17
from live `trixie/stable` `binary-amd64/Packages` (267 entries); re-resolve at
execution start with `apt-cache madison <pkg>` after repo add and record drift
`old pin → live → consequence`:

| Package | Pinned version | Note |
|---|---|---|
| `docker-ce` | `5:29.8.1-1~debian.13~trixie` | engine, keep in lockstep with CLI |
| `docker-ce-cli` | `5:29.8.1-1~debian.13~trixie` | same upstream as engine |
| `containerd.io` | `2.3.5-1~debian.13~trixie` | newest trixie build |
| `docker-buildx-plugin` | `0.37.1-1~debian.13~trixie` | newest trixie build |
| `docker-compose-plugin` | `5.5.1-1~debian.13~trixie` | newest trixie build (v5 line) |

Install as `apt: name: docker-ce=<ver>, …` per package. After install,
`apt-mark hold` all five so routine `update-packages.yml` runs cannot silently
float the engine; bumps are explicit (unhold → re-pin → evidence) in drained
windows. Record `docker version` + `dpkg -l 'docker-*' containerd.io` in C-phase
evidence.

**b. `daemon.json`** (adds rotation count to the source shape):

```json
{
  "log-opts": { "max-size": "10m", "max-file": "5" },
  "default-address-pools": [{ "base": "172.30.0.0/16", "size": 24 }]
}
```

No `exec-opts` cgroup override, no `storage-driver`, no `data-root`, no
`live-restore`, no TCP listeners (Debian 12/13 defaults are correct; §5
verifies rather than overrides). `mode: 0644`, restart handler as in source.
Repo wiring identical to source: GPG key to `/etc/apt/keyrings/docker.asc`,
`signed-by` repo pinned to `distribution_release` codename (`filename: docker`,
legacy `docker-ce.list` absent); prereq pkgs + service started+enabled as in source.

**c. Cgroup driver**: left implicit; asserted in §5.

Base-server head from `install-base.yml`: UTC timezone
(`community.general.timezone`), APT base pkgs (`gpg sudo wget curl git git-lfs
unzip tmux iotop bat ncurses-term build-essential pkg-config libssl-dev`),
mise repo+install. SKIP all cosmetics (oh-my-zsh, starship, `.zshrc`,
GraalVM/Rust/cargo tool installs — headless CI host). Velnor APT wiring follows
the holla-apt block template (`signed-by` keyring + `.list` file), with the key
fingerprint authenticated against a separately trusted project reference per
spec §7 — never blind-trust the download URL. Inventory: bastion host/group
added to `hosts.ini` shape (`root`, `StrictHostKeyChecking=accept-new` for
first provision); `requirements.yaml` all 3 collections.

## 5. cgroup v2 / systemd driver / storage verification (P4)

All must pass before any job runs (native preflight parity):

```sh
stat -f -c %T /sys/fs/cgroup            # == cgroup2fs
docker info --format '{{.CgroupDriver}} {{.CgroupVersion}} {{.Driver}}'
# expect: systemd 2 overlayfs (overlay2 prints as overlayfs on some builds)
docker info --format '{{.DockerRootDir}}'  # == /var/lib/docker
docker buildx version; docker compose version   # match pins (Buildx 0.37.x, Compose v5.5.x)
systemctl is-active docker containerd
```

Failure (e.g. `cgroupfs` driver) blocks: investigate kernel cmdline
(`systemd.unified_cgroup_hierarchy`) — never paper over with `exec-opts`.

## 6. Managed product paths (P5)

Created idempotently (Ansible `file: state=directory`, or `install -d`;
`tmpfiles.d` snippet restores `/run/velnor` across reboots):

| Path | Purpose | Mode/owner |
|---|---|---|
| `/etc/velnor` | config (never secrets in world-readable files) | `0750 root:root` (tighten per package) |
| `/var/lib/velnor` | durable state, lease/store records | `0750 root:root` |
| `/run/velnor` | runtime sockets/locks, incl. `/run/velnor/package-transaction.lock` (spec §7) | `0750 root:root`, tmpfs-backed by `/run` |
| `/var/cache/velnor` | deliberate retained caches (namespaced per §6, never shared-mutable target dirs) | `0750 root:root` |
| owned scratch | job workspaces, `_work`, TMPDIR, DinD data dirs — on existing root FS, per-job ownership identity | per-job |

Never on RAM-backed `/tmp` as disk substitute; never on `nvme1n1` (§3).
Velnor itself arrives ONLY via locked `velnor-runner=${VERSION}` APT install
(spec §7 transaction with `flock` on the package lock + `release
verify-installed` before start) — never `dpkg -i`, never copied binaries.

## 7. Private control APIs

- No Docker TCP API, ever: management socket is the host Unix socket, root-only
  (`0600`-class perms via Docker default group policy — no `docker` group
  membership for job identities). Never bind-mount raw `/var/run/docker.sock`
  (or any alternate unrestricted host socket/proxy) into any job; jobs get only
  job-private DinD sockets (§5.3 semantics).
- Velnor control sockets live under `/run/velnor` with owner-only access; job
  API access is via the job-scoped lease proxy, never management authority.
- No public webhook receiver (Scale Set long-poll design needs none). Outbound
  GitHub/registry/package endpoints permitted; no frozen three-host allowlist
  assumed. The host public route is never altered to add container networks
  (pools in §4 keep Docker off congested ranges without touching host routes).

## 8. Drain procedure

**No-op on idle host (A0 state — expected at first provision).** Prove no-op
validity, then do nothing:

```sh
docker ps -q 2>/dev/null | grep -q . && echo JOBS || echo IDLE-NO-CONTAINERS
ps aux | grep -iE 'velnor|runner' | grep -v grep || echo IDLE-NO-PROCS
ls /run/velnor 2>/dev/null || echo IDLE-NO-RUNTIME
```

A0 evidence (`docker` absent, 10 base units, load 0.00, no velnor paths/procs)
already satisfies this; re-run at execution and record. When all three report
idle, drain = no-op, provisioning proceeds immediately.

**General form** (provisioned host, before Docker restart/upgrade/package work):

1. Stop new admission (controller-side; no new reservations). 2. Let running
   jobs finish within their deadlines; cancel only per queue contract (same-PR
   supersession may cancel same-PR older attempts; never siblings/unrelated).
3. Export job logs/artifacts OUT of disposable containers first. 4. Remove ONLY
   owned inactive resources (recorded ownership identity; active leases
   protected; never host-wide prune). 5. Verify: `docker ps` empty of job
   containers, permits released exactly once, `df`/`df -i` healthy. 6. Perform
   daemon/package op. 7. Post-op: §5 re-verify + one canary real job before
   reopening admission. Shared Docker restart/host reboot uses an isolated
   coordinated window preserving unrelated work and SSH (spec §8).

## 9. Verification checklist (execution evidence)

P0 output archived → SSH reachable throughout (single session held) →
`nvme1n1` pre/post assertions pass → `daemon.json` content matches §4 →
`docker version`/`dpkg -l` match pins → §5 all green → managed paths exist with
modes → drain evidence (idle-proofs or general-form receipts) → Velnor §7
transaction log + `verify-installed` + first real job. Any red item blocks
admission; no partial-provision jobs.
