# A0 — Bastion read-only inventory (STEP A0, campaign bastion)

- Host: `root@37.27.110.241` (`bastion`), inspected 2026-09-17 ~01:40 UTC (host clock), over SSH, strictly read-only (no writes, no package changes, no systemctl start/stop, no docker prune/rm — docker absent).
- Spec target: `plans/bastion-three-provider-ci/spec.md` §0 (Debian 13, AMD EPYC 9454P, 48 phys / 96 logical, ~128 GB; operator-reported 128494 MiB ≈ 125.48 GiB).
- Evidence baseline: `plans/bastion-three-provider-ci/evidence.md` §7 (re-resolve host state at execution).

## Verdict: target hardware/OS confirmed, host is bare — no Docker, no Velnor, no jobs

| # | Old observation (spec/evidence) | New evidence (this run) | Consequence |
|---|---|---|---|
| 1 | Spec §0: Debian 13 (trixie) | `6.12.94+deb13-amd64`, `/etc/debian_version` = `13.5`, `os-release` = Debian 13 trixie | CONFIRMED. Debian 13.5, kernel 6.12.94. Proceed. |
| 2 | Spec §0: AMD EPYC 9454P, 48 phys / 96 logical | `lscpu`: AuthenticAMD EPYC 9454P, 1 socket × 48 cores × 2 threads = 96 CPUs, `nproc` = 96, NUMA 1 node | CONFIRMED exactly. |
| 3 | Spec §0: ~128 GB (operator 128494 MiB ≈ 125.48 GiB) | `free -m` total = **128494 MiB** (used 1769, avail 126725); swap 4095 MiB unused | CONFIRMED to the MiB. Host ~99% idle RAM. |
| 4 | Spec §8: second NVMe left untouched | `nvme1n1` 3.5T present, unpartitioned, unmounted, same model `INTEL SSDPF2KX038T1` (serial `…52M3P8CGN`); `nvme0n1` 3.5T holds efi/swap(4G)/boot(1G)/root(xfs 3.5T, 26G used, 1%) | CONFIRMED: two 3.5T NVMe; nvme1n1 pristine. Keep untouched per spec. Split-brain risk: none (no mounts, no RAID). |
| 5 | Spec §8: Docker via pinned APT; work-plan C1 assumes pinned Docker + Buildx/Compose | **No Docker at all**: `docker: command not found` (version/info/buildx/compose all absent); no `/var/lib/docker`, `/var/lib/containerd`, `/run/docker.sock`; no containerd/podman/nerdctl binaries | DIVERGENCE (expected at A0, pre-provision): Docker Engine + Buildx/Compose must be installed in Phase C via pinned ChainArgos Ansible path (`install-docker.yml` vs `install-docker-selene.yml`). Nothing to migrate or preserve. |
| 6 | Spec §8: cgroup v2/systemd driver verified at provision | Not re-checked this run (docker absent, driver N/A); systemd present, 10 base units running | No action at A0; verify cgroup v2 + systemd driver during Phase C Docker install. |
| 7 | Spec §8: existing state read first; `/etc/velnor`, `/var/lib/velnor`, `/run/velnor` | `dpkg -l \| grep -i velnor` empty; all three velnor paths nonexistent; `ps` shows no velnor/runner/actions processes | CONFIRMED clean: no prior Velnor install. Fresh install, no migration. |
| 8 | Spec §8: running jobs preserved | `uptime` 6:27, load 0.00; only sshd listening (tcp/22, dual-stack); services = base 10 (atd/cron/dbus/getty/ssh/journald/logind/timesyncd/udevd/user@0); no containers (no daemon) | CONFIRMED idle: no running jobs, nothing to preserve or drain. Safe provisioning window. |
| 9 | Spec §9: no KVM-runtime VMs on bastion (declared capability only) | `/dev/kvm` present; all 96 CPUs carry `svm` flag; KVM usable but no libvirt/qemu checked (not installed per service list) | No conflict: capability exists in silicon, no VM stack installed. KVM-runtime tests stay separately qualified, never run on bastion. |
| 10 | Evidence §7: refresh host state at execution | This file is the refreshed host-state ledger for A0 | Ledger current as of 2026-09-17. |

## Raw output (full, in run order)

```text
===== uname -a =====
Linux bastion 6.12.94+deb13-amd64 #1 SMP PREEMPT_DYNAMIC Debian 6.12.94-1 (2026-06-20) x86_64 GNU/Linux
===== nproc =====
96
===== lscpu =====
Architecture:                            x86_64
CPU op-mode(s):                          32-bit, 64-bit
Address sizes:                           52 bits physical, 57 bits virtual
Byte Order:                              Little Endian
CPU(s):                                  96
On-line CPU(s) list:                     0-95
Vendor ID:                               AuthenticAMD
Model name:                              AMD EPYC 9454P 48-Core Processor
CPU family:                              25
Model:                                   17
Thread(s) per core:                      2
Core(s) per socket:                      48
Socket(s):                               1
Stepping:                                1
Frequency boost:                         enabled
CPU(s) scaling MHz:                      45%
CPU max MHz:                             3812.1760
CPU min MHz:                             1500.0000
BogoMIPS:                                5499.76
Virtualization:                          AMD-V
L1d cache:                               1.5 MiB (48 instances)
L1i cache:                               1.5 MiB (48 instances)
L2 cache:                                48 MiB (48 instances)
L3 cache:                                256 MiB (8 instances)
NUMA node(s):                            1
NUMA node0 CPU(s):                       0-95
(Vulnerabilities: gather/indirect/itlb/l1tf/mds/meltdown/mmio/regfile/retbleed/srbds/tsx = Not affected; spec-rstack/spec-bypass/spectre-v1+v2/tsa/vmscape = Mitigation — full flags in SSH log)
===== free -m =====
               total        used        free      shared  buff/cache   available
Mem:          128494        1769      127504           4         105      126725
Swap:           4095           0        4095
===== lsblk =====
NAME         SIZE TYPE MOUNTPOINT
nvme0n1      3.5T disk
├─nvme0n1p1  256M part /boot/efi
├─nvme0n1p2    4G part [SWAP]
├─nvme0n1p3    1G part /boot
└─nvme0n1p4  3.5T part /
nvme1n1      3.5T disk
===== lsblk nvme detail =====
NAME         SIZE TYPE MOUNTPOINT MODEL               SERIAL
nvme0n1      3.5T disk            INTEL SSDPF2KX038T1 BTAX340600DS3P8CGN
├─nvme0n1p1  256M part /boot/efi
├─nvme0n1p2    4G part [SWAP]
├─nvme0n1p3    1G part /boot
└─nvme0n1p4  3.5T part /
nvme1n1      3.5T disk            INTEL SSDPF2KX038T1 BTAX3415052M3P8CGN
---nvme list---
bash: line 7: nvme: command not found
---lspci nvme---
c2:00.0 Non-Volatile memory controller: Intel Corporation NVMe DC SSD [3DNAND, Sentinel Rock Controller]
c4:00.0 Non-Volatile memory controller: Intel Corporation NVMe DC SSD [3DNAND, Sentinel Rock Controller]
===== df -hT =====
Filesystem     Type      Size  Used Avail Use% Mounted on
udev           devtmpfs   63G     0   63G   0% /dev
tmpfs          tmpfs      13G  1.4M   13G   1% /run
efivarfs       efivarfs  128K   24K  100K  19% /sys/firmware/efi/efivars
/dev/nvme0n1p4 xfs       3.5T   26G  3.5T   1% /
tmpfs          tmpfs      63G     0   63G   0% /dev/shm
tmpfs          tmpfs     5.0M     0  5.0M   0% /run/lock
tmpfs          tmpfs     1.0M     0  1.0M   0% /run/credentials/systemd-journald.service
tmpfs          tmpfs      63G     0   63G   0% /tmp
/dev/nvme0n1p3 ext3      975M   83M  842M   9% /boot
/dev/nvme0n1p1 vfat      256M  152K  256M   1% /boot/efi
tmpfs          tmpfs     1.0M     0  1.0M   0% /run/credentials/getty@tty1.service
tmpfs          tmpfs      13G  8.0K   13G   1% /run/user/0
===== debian_version =====
13.5
---os-release---
PRETTY_NAME="Debian GNU/Linux 13 (trixie)"
NAME="Debian GNU/Linux"
VERSION_ID="13"
VERSION="13 (trixie)"
VERSION_CODENAME=trixie
DEBIAN_VERSION_FULL=13.5
ID=debian
===== docker version =====
bash: line 10: docker: command not found
===== docker info =====
bash: line 11: docker: command not found
===== docker buildx version =====
bash: line 12: docker: command not found
===== docker compose version =====
bash: line 13: docker: command not found
===== running services =====
  UNIT                      LOAD   ACTIVE SUB     DESCRIPTION
  atd.service               loaded active running Deferred execution scheduler
  cron.service              loaded active running Regular background program processing daemon
  dbus.service              loaded active running D-Bus System Message Bus
  getty@tty1.service        loaded active running Getty on tty1
  ssh.service               loaded active running OpenBSD Secure Shell server
  systemd-journald.service  loaded active running Journal Service
  systemd-logind.service    loaded active running User Login Management
  systemd-timesyncd.service loaded active running Network Time Synchronization
  systemd-udevd.service     loaded active running Rule-based Manager for Device Events and Files
  user@0.service            loaded active running User Manager for UID 0
10 loaded units listed.
===== ss -tlnp =====
State  Recv-Q Send-Q Local Address:Port Peer Address:PortProcess
LISTEN 0      128          0.0.0.0:22        0.0.0.0:*    users:(("sshd",pid=2053,fd=6))
LISTEN 0      128             [::]:22           [::]:*    users:(("sshd",pid=2053,fd=7))
===== dpkg velnor =====
(empty — exit=1, no matches)
===== velnor dirs =====
ls: cannot access '/etc/velnor': No such file or directory
ls: cannot access '/var/lib/velnor': No such file or directory
ls: cannot access '/run/velnor': No such file or directory
===== docker ps =====
bash: line 18: docker: command not found
===== docker ps -a count =====
bash: line 19: docker: command not found
===== uptime/load =====
 01:40:20 up  6:27,  1 user,  load average: 0.00, 0.00, 0.00
(top CPU: systemd transient 15%; sshd-session; rest idle kernel threads)
===== velnor procs =====
(none — no velnor/runner/actions processes)
===== disk by-id nvme =====
nvme-eui.01000000000000005cd2e421a1d25651 -> ../../nvme0n1 (+part1..4)
nvme-eui.01000000000000005cd2e4d487d35651 -> ../../nvme1n1
nvme-INTEL_SSDPF2KX038T1_BTAX340600DS3P8CGN -> ../../nvme0n1
nvme-INTEL_SSDPF2KX038T1_BTAX3415052M3P8CGN -> ../../nvme1n1
===== container runtimes (supplemental, read-only) =====
which containerd/dockerd/podman/nerdctl/ctr/crictl/buildah: no matches
/var/lib/docker, /var/lib/containerd, /run/docker.sock: all absent
apt-cache policy docker-ce docker-ce-cli containerd.io: no installed/known packages
===== kernel virt (supplemental, read-only) =====
/dev/kvm present; svm flag on all 96 CPUs
```
