# C1 ansible-configs re-resolve — 2026-09-18 (READ-ONLY)

Method: `git ls-remote` + `gh api` only. Zero clones, zero repo writes, zero bastion
touches, zero PRs. Spec paths read from `plans/bastion-three-provider-ci/spec.md`
§6.1 (read-only). Raw bytes retained in `/tmp/c1-ansible/` (read-only fetches).

## 1. Head verification — NO DRIFT

`git ls-remote` and `gh api repos/ChainArgos/java-monorepo/commits/main` agree:

- Live `main`: `218a44b28984acf4ceee24cd1d9d6ccb8c38ae37`
  ("docs: record single-branch delivery policy", 2026-09-17T12:09:13Z)
- Task-assumed head `218a44b2`: CONFIRMED, no movement.
- Audited SHA (evidence §1): `235e479b150aeb949bc8a5190fba5b84f6303c80`

Reference root:
`https://github.com/ChainArgos/java-monorepo/tree/218a44b28984acf4ceee24cd1d9d6ccb8c38ae37/ansible-configs`

## 2. §6.1 path re-resolve — ALL 9 BYTE-IDENTICAL (C-5 re-confirmed)

| # | Path in `ChainArgos/java-monorepo` | Live blob SHA (short) | Size | Audited blob | Verdict |
| --- | --- | --- | --- | --- | --- |
| 1 | `ansible-configs/install-base.yml` | `6c1e2ecf` | 7171 | identical | HOLD |
| 2 | `ansible-configs/install-docker.yml` | `85b9a2c1` | 1772 | identical | HOLD |
| 3 | `ansible-configs/install-docker-selene.yml` | `b30a027e` | 3370 | identical | HOLD |
| 4 | `ansible-configs/hosts.ini` | `cc0eda31` | 357 | identical | HOLD |
| 5 | `ansible-configs/requirements.yaml` | `e8053df3` | 96 | identical | HOLD |
| 6 | `ansible-configs/README.md` | `9de32f6b` | 16173 | identical | HOLD |
| 7 | `ansible-configs/docs/upgrade-debian.md` | `7f887ad9` | 1715 | identical | HOLD |
| 8 | `ansible-configs/update-packages.yml` | `f3dd8199` | 601 | identical | HOLD |
| 9 | `ansible-configs/upgrade-debian.yml` | `6680c698` | 945 | identical | HOLD |

Full blob SHAs (live = audited for every row):

```text
6c1e2ecf12a5161da12ae4f5c4068b7fc8621cff  install-base.yml
85b9a2c18a64409db7c86e1864c0ffea88ff3e94  install-docker.yml
b30a027ee070f7afdaf2e5d40d0bbe0476c82ae0  install-docker-selene.yml
cc0eda3102c0a3d1127eeaa0f4a88cb041f579e7  hosts.ini
e8053df307f05c45164c78c520013b88c66f4f3c  requirements.yaml
9de32f6b82eed7e7e51f93dcbb3fbbc01d99cd4e  README.md
7f887ad95fbda7bb729469fd27a8ccb0b2912fea  docs/upgrade-debian.md
f3dd8199f0e92ddbeed9d43acc3e45e9dcde9d4b  update-packages.yml
6680c698f0ae4b2b9aca5a184c737d95b087db48  upgrade-debian.yml
```

Old observation (baseline C-5) → new evidence → consequence: pins were
identical at baseline; they are still identical at live head `218a44b2`.
C1 may treat audited content as live content; no re-read delta.

### 2a. §6.1 set is NOT self-contained (companion deps, all verified to exist at live head)

| Companion (outside §6.1) | Required by | Live blob | Size |
| --- | --- | --- | --- |
| `ansible-configs/config/zshrc` | `install-base.yml` (`copy: src={{ zshrc_file }}`) | `d7034139…` | 1179 |
| `ansible-configs/debian/sources.list.d/debian.sources` | `upgrade-debian.yml` loop | `f5fc54da…` | 411 |
| `ansible-configs/debian/sources.list.d/hetzner-mirror.sources` | `upgrade-debian.yml` loop | `1504fc84…` | 247 |
| `ansible-configs/debian/sources.list.d/hetzner-security-mirror.sources` | `upgrade-debian.yml` loop | `99c86d4a…` | 233 |
| `ansible-configs/debian/sources.list.d/docker.list` | `upgrade-debian.yml` (conditional) | `9e837b70…` | 111 |
| `ansible-configs/setup-sentry.yml` (35254 bytes, `1069695c…`) | referenced by `install-base.yml` comment + README "Sentry and Velnor" | — | — |

Consequence: a C1 plan that runs `install-base.yml` verbatim needs `config/zshrc`;
a plan that runs `upgrade-debian.yml` verbatim needs the four `debian/` sources.
`setup-sentry.yml` (the actual Velnor-host playbook) is out of §6.1 scope —
flag for a follow-up re-resolve, do not assume its content.

## 3. Minimal idempotent host-setup step list derivable from §6.1 files

Derived only; no plan written into the repo. Order follows file order; each step
notes its idempotency guard as written.

### A. Controller prerequisites (from `requirements.yaml`, `hosts.ini`, `README.md`)

- A1. `ansible-galaxy collection install -r requirements.yaml`
  (`community.general`, `community.postgresql`, `ansible.posix`).
- A2. Inventory entry for bastion in the `hosts.ini` pattern (`[nodes]` group,
  `ansible_ssh_user=root`, `StrictHostKeyChecking=accept-new`). Bastion is not
  in the live inventory (6 hosts: pegasus/delorean/titan/sentry/postgresql-nova/
  clickhouse-selene) — C1 must add it; the file gives the shape, not the entry.
- A3. No `op`/`fnox` needed for the §6.1 subset itself (secret providers only
  enter via out-of-scope `setup-*` playbooks).

### B. Base server (from `install-base.yml`)

- B1. Timezone UTC (`community.general.timezone` — idempotent).
- B2. APT base set (`state: present`, idempotent): gpg, sudo, wget, curl, zsh,
  git, git-lfs, unzip, tmux, iotop, bat, ncurses-term, build-essential,
  pkg-config, libssl-dev.
- B3. xterm-ghostty terminfo: guarded by `infocmp` probe + `creates:` on
  `tic`; fails closed if neither control-host terminfo source exists
  (step aborts the play — C1 must ensure a control host with terminfo or drop
  the step; it is cosmetic).
- B4. `git lfs install` (`changed_when: false`), batcat→bat symlink
  (`state: link` — idempotent).
- B5. Nushell repo + package (third-party `apt.fury.io`; key lands in legacy
  `/etc/apt/trusted.gpg.d/` — see flag F5).
- B6. mise repo (signed-by `/etc/apt/keyrings/mise.asc` — good pattern) +
  package.
- B7. Shell cosmetics, all guarded: Oh My Zsh (`creates: /root/.oh-my-zsh`,
  curl|sh), zsh-autosuggestions (`git` `version: master, update: yes` —
  UNPINNED, see F5), root shell → `/bin/zsh` (safe ordering: zsh installed
  at B2 first), Starship (`creates: /usr/local/bin/starship`, curl|sh),
  `.zshrc` from companion `config/zshrc` (outside §6.1, see §2a).
- B8. Toolchains via `mise use -g` (`command:` module, NO `changed_when`/
  `creates` — re-runs and always reports changed; converge-safe via mise
  itself but noisy): `java@oracle-graalvm-25.0.1` (pinned), `rust@stable`
  (FLOATING, see F5), `cargo-binstall`, `cargo:rust-script`, `cargo:just`,
  `cargo:shellfirm`, `cargo:tirith`, `cargo:bottom`, `cargo:zellij`.
- B9. holla APT repo (`holla-apt.tailrocks.com`, key via curl|tee with
  exists-guard) + `holla` at `state: latest` (UNPINNED, see F5).

### C. Docker Engine (from `install-docker.yml`, compared with `install-docker-selene.yml`)

- C1. Prereqs: apt-transport-https, ca-certificates, curl, gnupg, lsb-release.
- C2. Docker GPG key → `/etc/apt/keyrings/docker.asc`; repo
  `deb [arch=amd64 signed-by=…] https://download.docker.com/linux/debian
  {{ distribution_release }} stable` (`filename: docker`); remove stale
  `docker-ce.list`. (Key fetched over HTTPS with no fingerprint check — F5.)
- C3. Install `state: present` (UNPINNED versions — F5): docker-ce,
  docker-ce-cli, containerd.io, docker-buildx-plugin, docker-compose-plugin.
- C4. `docker` service started + enabled.
- C5. `/etc/docker/daemon.json` (`mode: 0644`, restart handler): log
  `max-size: 10m` in BOTH variants; `default-address-pools 172.30.0.0/16 /24`
  ONLY in the generic variant — the selene variant deliberately omits pools
  and documents keeping the default `/var/lib/docker` data-root. C1 decision
  point: adopt or drop the 172.30/16 pool AFTER reading host routes (§6:
  public route never altered merely to add container networks).
- C6. Selene-only extras, bastion-irrelevant as written: `hosts:
  clickhouse-selene` pinning, conditional systemd ordering drop-in tied to a
  selene mount-guard unit (fires only if that unit file already exists —
  inert on bastion), plus the data-root non-migration note (aligns with the
  second-NVMe-untouched invariant).

### D. Routine refresh (from `update-packages.yml`) — CONDITIONAL, see flag F2

- D1. `apt update` → `apt upgrade: dist` + `autoremove` → `mise upgrade` →
  `systemd daemon_reload`. No drain, no holds, no guards. Derivable but NOT
  recommended verbatim for a live CI host.

### E. Release upgrade (from `upgrade-debian.yml` + `docs/upgrade-debian.md`) — NOT FOR C1

- E1. Sources rewrite (removes `/etc/apt/sources.list`, installs Hetzner-mirror
  `debian.sources` set + conditional `docker.list`). Bastion is already
  Debian 13; no release upgrade is in scope. Requires the §2a companion files.
- E2. Runbook manual sequence (apt upgrade/full-upgrade, GRUB, reboot) and its
  "Drain Docker" block are EXCLUDED — see flag F1 (direct §6 violation).

### F. Explicitly NOT derivable from §6.1 (no users/groups/dirs/sysctl content)

- No non-root users or groups are created anywhere in the 9 files (only
  `user: name=root shell=/bin/zsh`).
- No product directories (`/etc/velnor`, `/var/lib/velnor`, `/run/velnor`,
  `/var/cache/velnor`) — those come from the Velnor package per spec §6.
- No `sysctl`, no kernel/module tuning, no cgroup-driver setting in these files
  (cgroup v2/systemd driver verification is a C1 execution check, not a file
  step). No firewall/route changes. No `docker group` membership step
  (root-only SSH model).

## 4. Campaign-invariant review

Grep over all 9 live files for `libvirt|qemu|kvm|virsh`,
`CPUQuota|MemoryMax|MemoryHigh|NanoCpus|cpuset|blkio`,
`nvme|mkfs|fdisk|parted|lvm|fstab|mount:`,
`sshd_config|authorized_keys|iptables|nft|ufw|PermitRoot`: see findings below.

| # | Invariant / check | Finding |
| --- | --- | --- |
| F0a | No libvirt/KVM/QEMU (§6) | CLEAN — zero matches in all 9 files. |
| F0b | No CPU/RAM quotas (§4.3) | CLEAN — zero quota strings; selene drop-in is ordering-only (`After`/`Requires`), no `[Service]` limits; `daemon.json` sets no resource defaults. |
| F0c | Second NVMe untouched (§6) | CLEAN in prescriptive files — no format/partition/LVM/mount/swap. Two README *mentions* only (descriptive): selene "two-tier NVMe" role line and the drive-init (`init-*-drives.yml`) playbook catalog — those playbooks are OUT of §6.1 scope and must never be added to a bastion run. Selene docker comment explicitly keeps default `/var/lib/docker` data-root. |
| F0d | SSH lockout risk (§6) | NO VECTOR in §6.1 files — no `sshd_config`, key, or firewall changes; `hosts.ini` uses `root` + `accept-new` (sane for provisioning); root-shell change is ordering-safe (zsh installed earlier in the same play; apt failure aborts before the `user` task). Note: SSH keys live in out-of-scope `setup-*` playbooks (README) — C1 must handle bastion access separately. |
| F1 | **CONFLICT — `docker system prune` prescribed** (§6 bans host-wide prune) | `docs/upgrade-debian.md` "Drain Docker" runs `docker stop/rm $(docker ps -qa)`, `rmi --force $(docker images -qa)`, `network rm`, `docker system prune --force`, `volume prune --force`, `volume rm`. Kills all containers, deletes all images/volumes/networks. Directly violates "No host-wide `docker system prune`" + non-destructive provisioning + running-job preservation. MUST NOT enter C1. |
| F2 | **CONFLICT — `update-packages.yml` is destructive-adjacent** | Unconditional `dist` upgrade + `autoremove` + `mise upgrade`, no drain, no holds on `docker-ce`/`velnor-runner`, no running-job check. Violates "read running jobs first" / non-destructive posture if run verbatim on a live CI host. C1 needs drain + holds + pinned targets instead. |
| F3 | **CONFLICT — Hetzner-mirror + release-upgrade assumptions** | `upgrade-debian.yml` hard-installs `hetzner-mirror.sources`; runbook does full-upgrade + reboot. Release upgrades are out of C1 scope (bastion already Debian 13). Mirror choice must be verified against live bastion APT sources at execution, never assumed. |
| F4 | **Container-network vs host-route risk** | Generic `daemon.json` injects `default-address-pools 172.30.0.0/16`; selene omits it. §6 forbids altering the host public route merely to add container networks — C1 must read live routes/FIB first and resolve the pool choice explicitly. |
| F5 | **Pinning/trust gaps vs §7 posture** (not invariant violations, but C1 must close them) | `docker-ce*`/`containerd.io`/plugins `state: present` (floating); `rust@stable` (floating); `holla state: latest`; zsh-autosuggestions `master + update: yes`; Nushell key in legacy `trusted.gpg.d` without `signed-by`; all APT keys (docker/mise/nushell/holla) fetched over HTTPS with NO fingerprint authentication against a trusted reference (§7 requires it); two curl\|sh installers (oh-my-zsh, starship). Spec §6.1 table says "Pinned Docker APT repository" — the *repo* is pinned to `distribution_release`, the *package versions* are not. |
| F6 | Idempotency blemishes | `mise install`/`mise use -g` via `command:` without `changed_when` (always "changed"); cosmetic steps (B3/B7) depend on control-host state (terminfo) and external installers — rerun-safe but noisy/fragile. Core APT/service/file steps are properly idempotent. |
| F7 | Do-not-copy descriptive patterns in README (out-of-scope context) | README "Sentry and Velnor" documents per-repo slot reservations (4/2/4) — contradicts spec §4 one global N / no reservations; it describes sentry, not bastion, but C1 authors must not copy the shape. PAT files in RAM-backed `/run/chainargos-secrets` tmpfs require playbook rerun after reboot — contrasts with spec §6 token-refresh-in-provider. |

## 5. Handoff notes for C1 execution

1. Pins: use live head `218a44b28984acf4ceee24cd1d9d6ccb8c38ae37`; §6.1 content
   = audited content (table §2). Re-verify head at C1-gate; drift procedure is
   `old observation → new evidence → consequence` per spec §6.1.
2. Usable verbatim-pattern core: A1–A2, B1–B2, B4 (minus cosmetics if desired),
   B6-mise-keyring pattern, C1–C4, C5-log-size; C5-pools and C6 need explicit
   decisions (§4/F4).
3. Never carry into C1: F1 drain block, F2 unguarded dist-upgrade, F3
   release-upgrade path, out-of-scope `init-*-drives.yml` / `setup-*` /
   `setup-sentry.yml` (last one needs its own re-resolve first).
4. Close before/at C1: exact version pins for docker-ce set (F5), key
   fingerprint authentication (F5), bastion inventory entry (A2), SSH/access
   handling outside §6.1 (F0d), route read before pool choice (F4).
5. This note + `/tmp/c1-ansible/` raw files are the full output. No repo writes
   were made; spec/evidence files were read only.
