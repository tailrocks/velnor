# V-A0 — Independent verifier for A0 bastion inventory

- Source: `/tmp/a0-bastion.md` (A0, run ~01:40 UTC 2026-09-17)
- Verifier run: 2026-09-17 ~01:47 UTC (host clock), SSH strictly read-only (no writes)
- Method: re-ran subset to try to disprove A0 verdict

## Per-probe MATCH/MISMATCH

| # | Probe | A0 claimed | Verifier observed | Result |
|---|---|---|---|---|
| 1 | `uname -a` | `Linux bastion 6.12.94+deb13-amd64 #1 SMP PREEMPT_DYNAMIC Debian 6.12.94-1 (2026-06-20) x86_64 GNU/Linux` | byte-identical | MATCH |
| 2 | `nproc` | 96 | 96 | MATCH |
| 3 | `free -m \| head -2` | total 128494 MiB | total 128494 MiB (used 1796 vs 1769 — normal drift; total invariant) | MATCH |
| 4 | `lsblk -o NAME,SIZE,TYPE,MOUNTPOINT` | nvme0n1 3.5T (efi/swap/boot/root) + nvme1n1 3.5T bare | identical layout | MATCH |
| 5 | `which docker` (expect absent) | absent | absent (exit=1, no output) | MATCH |
| 6 | `dpkg -l \| grep -i velnor` (expect empty) | empty (exit=1) | empty (exit=1) | MATCH |
| 7 | `ls /etc/velnor` (expect missing) | missing | missing (`No such file or directory`, exit=2) | MATCH |
| 8 | `uptime` | 01:40:20 up 6:27, load 0.00 | 01:47:47 up 6:34, load 0.00 (~7 min later, consistent) | MATCH |

## Raw verifier output

```text
===== uname -a =====
Linux bastion 6.12.94+deb13-amd64 #1 SMP PREEMPT_DYNAMIC Debian 6.12.94-1 (2026-06-20) x86_64 GNU/Linux
===== nproc =====
96
===== free -m head-2 =====
               total        used        free      shared  buff/cache   available
Mem:          128494        1796      127467           4         125      126698
===== lsblk =====
NAME         SIZE TYPE MOUNTPOINT
nvme0n1      3.5T disk
├─nvme0n1p1  256M part /boot/efi
├─nvme0n1p2    4G part [SWAP]
├─nvme0n1p3    1G part /boot
└─nvme0n1p4  3.5T part /
nvme1n1      3.5T disk
===== which docker =====
which-docker-exit=1
===== dpkg velnor =====
dpkg-grep-exit=1
===== ls /etc/velnor =====
ls: cannot access '/etc/velnor': No such file or directory
ls-velnor-exit=2
===== uptime =====
 01:47:47 up  6:34,  1 user,  load average: 0.00, 0.00, 0.00
```

## Verdict: CERTIFIED

All 8/8 probes MATCH. No disproven rows. A0 verdict stands: Debian 13.5 / EPYC 9454P / 96 CPU / 128494 MiB, bare host, no Docker, no Velnor, idle.
