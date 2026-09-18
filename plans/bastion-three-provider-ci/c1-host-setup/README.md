# C1 bastion host-setup (authored, NOT yet executed)

Idempotent, non-destructive host setup for `bastion` (`root@37.27.110.241`,
Debian 13, amd64) — the work-plan STEP C1 action-3 slice ("idempotent
non-destructive host setup per spec §6 … starting from the §6.1
ansible-configs reference paths"). Authoring only: no bastion writes were made
to produce this; execution waits for the B4+C gates and a dedicated C-gate
infra agent.

## Files

| File | Purpose |
| --- | --- |
| `provision-bastion.sh` | Idempotent provisioner (`--check` / apply), all guards inline |
| `pins.env` | Exact docker-ce set pins + key fingerprints + sources + dates |
| `targets.env` | Bastion inventory entry (shell form of the `hosts.ini` pattern) |
| `README.md` | This file: preconditions, execution, decisions, evidence |

A shell provisioner was chosen over an ansible playbook to match repo
conventions (`scripts/*.sh`, `set -euo pipefail`, no ansible tooling in this
repository). Derivation is 1:1 step-mapped to the ansible source (B1…C5 tags
in the script).

## Preconditions for the C-gate infra agent

1. B4 + C gates green (this directory is C1 *prep*; do not execute early).
2. Control host: `bash`, `ssh`, `scp` with the SSH options in `targets.env`.
3. Target: Debian 13 (`trixie`) amd64, reachable as `root@37.27.110.241`;
   the script re-verifies OS/arch itself and refuses anything else.
4. Re-verify pins at gate time: `pins.env` records source URLs + the
   2026-09-18 resolution; if the C-gate date moved on, re-resolve per the
   procedure in `pins.env` (old observation → new evidence → consequence)
   and re-run container validation before touching the bastion.
5. Read-first: run `--check` before apply; review every `would:` line.
6. Drain: the script refuses dockerd-affecting changes while containers run
   unless `VELNOR_C1_ALLOW_RESTART=1` is set — set it only after draining
   jobs per the campaign drain procedure. There is deliberately no
   auto-drain and no prune anywhere in this package.

## Execution (C-gate infra agent)

```sh
C1=plans/bastion-three-provider-ci/c1-host-setup
SSH="ssh -o IdentitiesOnly=no -o StrictHostKeyChecking=accept-new"

# 1. Upload (new directory on target; no existing paths touched).
scp -r -o IdentitiesOnly=no -o StrictHostKeyChecking=accept-new \
  "$C1" root@37.27.110.241:/root/c1-host-setup

# 2. Read-only plan first. Exit 1 with a `would:` list = changes pending.
$SSH root@37.27.110.241 'bash /root/c1-host-setup/provision-bastion.sh --check'

# 3. Apply (default: core only, no cosmetics, no docker pools).
$SSH root@37.27.110.241 'bash /root/c1-host-setup/provision-bastion.sh'

# 4. Re-run --check: must exit 0 ("converged") — the idempotency proof.
$SSH root@37.27.110.241 'bash /root/c1-host-setup/provision-bastion.sh --check'
```

Optional flags (env on the target command line, e.g.
`$SSH root@… 'VELNOR_C1_COSMETICS=1 bash /root/c1-host-setup/provision-bastion.sh'`):

- `VELNOR_C1_COSMETICS=1` — shell cosmetics (terminfo via
  `VELNOR_C1_TERMINFO_SRC`, oh-my-zsh, pinned zsh-autosuggestions, root zsh,
  starship). Default off. Cosmetics never abort the run.
- `VELNOR_C1_DOCKER_POOLS=1` — add the `172.30.0.0/16` daemon pools from the
  generic docker variant. Default off (selene-style omit). The pre-flight
  route read runs before this decision in all modes; a conflicting host
  route refuses pools even when requested.
- `VELNOR_C1_ALLOW_RESTART=1` — confirm dockerd-affecting changes with
  running containers. Set only after draining.

## What the script does (step map)

Pre-flight (read-only, always first): OS/arch gate → route read + pool
conflict record → running-containers/velnor-units read → cgroup/APT/lsblk
baseline. Core: B1 timezone UTC → B2 base APT set present-only (never
upgrade) → B4 git-lfs + bat symlink → C2 docker key (fingerprint fail-closed) +
repo file + stale `docker-ce.list` removal → C3 pinned docker set behind
the drain gate → holds on the docker set (+ `velnor-runner` if installed) →
C5 `daemon.json` (log `max-size: 10m`; pools only if requested and
route-clear) with restart behind the drain gate → C4 service enable+start.
Opt-in cosmetics: B3 terminfo (warn-and-continue, never abort) + B7 shell
setup with a pinned plugin. Post-checks (verify-only): pin/hold/daemon.json
identity, cgroup driver/version via `docker info`, libvirt-absent assert.

## Deliberate exclusions (MUST-NOT compliance)

- Runbook "Drain Docker" block (`docker stop/rm`, `rmi --force`, `system
  prune`, `volume prune`): excluded — host-wide prune violates spec §6.
- Drive-init / second-NVMe handling: excluded — the script contains no
  storage commands; `lsblk` is read-only baseline evidence.
- Per-repo slot reservations: excluded — spec §4 mandates one global N.
- `update-packages.yml` verbatim (`dist` upgrade + `autoremove` + `mise
  upgrade`): replaced by present-only installs + exact pins + holds.
- Release upgrade path (`upgrade-debian.yml`, Hetzner-mirror rewrite,
  full-upgrade + reboot): out of scope, bastion is already Debian 13.
- `mise`/Nushell/`holla` APT repos + `mise` toolchains: excluded from C1 —
  third-party keys without independently published fingerprints (trust gap)
  and developer-shell conveniences, not CI-host requirements (build
  toolchains belong in job images). Re-add procedure: fingerprint-first,
  same as the docker key.
- `.zshrc` copy: skipped — companion `config/zshrc` is outside §6.1 scope.
- `setup-sentry.yml` / `setup-*` / `init-*-drives.yml`: never referenced.

## Validation evidence (authoring host, 2026-09-18)

- `bash -n` clean; `shellcheck` clean (see PR checks / local run).
- MUST-NOT grep over the package: no `apt-get upgrade`, `dist-upgrade`,
  `autoremove`, `system prune`, `volume prune`, `mkfs`, `fdisk`, `parted`,
  `lvm`, `fstab`, storage `mount`, `sshd`/`authorized_keys`,
  `iptables`/`nft`/`ufw` anywhere in code. (`--no-upgrade` appears only as
  the present-only install flag; `libvirt`/`qemu`/`virsh` appear only in
  the assert-absent post-check; `mount` appears only in `MOUNTPOINT` of the
  read-only `lsblk` baseline.)
- Local container dry-run on `debian:13` (docker available locally):
  `--check` reports planned changes without writing (purity verified: no
  files/packages created); full apply converges pinned docker set + holds +
  `daemon.json` from the live repo; second `--check` exits 0 (idempotency
  proof). systemd-dependent steps (enable/start/restart, cgroup-driver
  info) warn-and-defer in containers without systemd and execute on the
  live host. Also exercised in containers: cosmetics opt-in (root zsh,
  pinned plugin rev, starship all land), pools opt-in plan, pool-conflict
  refusal (fail-closed), fingerprint-mismatch refusal (fail-closed), drain
  refusal exit 2 + `VELNOR_C1_ALLOW_RESTART=1` override. The dry-run caught
  and fixed three real bugs (held-package status, `docker ps` pipefail,
  unguarded `git lfs install` pending-count).
