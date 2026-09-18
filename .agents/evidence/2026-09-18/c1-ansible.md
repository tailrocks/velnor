# C1 provisioning-source analysis — spec §6.1 ansible-configs (bastion campaign)

Date (UTC): 2026-09-17T00:31Z. Read-only: `gh api` GETs only, no repo edits, no bastion writes.
Source: `ChainArgos/java-monorepo`, live `main` = `38a8fb5777fd02fec8b3904d86387aba05940e9a`
(same SHA A0 recorded as live; audited SHA `235e479b150aeb949bc8a5190fba5b84f6303c80`).
Permalink root: `https://github.com/ChainArgos/java-monorepo/tree/38a8fb5777fd02fec8b3904d86387aba05940e9a/ansible-configs`.
Fetched bytes: `/tmp/c1-ansible/` (9 files, layout mirrors repo; API meta in `/tmp/c1-ansible/meta/`).

## 1. Drift check: live blob SHAs vs A0-recorded SHAs

| Path | Live blob SHA (full, API) | A0 short | vs A0 | Bytes |
| --- | --- | --- | --- | --- |
| `ansible-configs/install-base.yml` | `6c1e2ecf12a5161da12ae4f5c4068b7fc8621cff` | `6c1e2ecf` | SAME | 7171 |
| `ansible-configs/install-docker.yml` | `85b9a2c18a64409db7c86e1864c0ffea88ff3e94` | `85b9a2c1` | SAME | 1772 |
| `ansible-configs/install-docker-selene.yml` | `b30a027ee070f7afdaf2e5d40d0bbe0476c82ae0` | `b30a027e` | SAME | 3370 |
| `ansible-configs/hosts.ini` | `cc0eda3102c0a3d1127eeaa0f4a88cb041f579e7` | `cc0eda31` | SAME | 357 |
| `ansible-configs/requirements.yaml` | `e8053df307f05c45164c78c520013b88c66f4f3c` | `e8053df3` | SAME | 96 |
| `ansible-configs/README.md` | `9de32f6b82eed7e7e51f93dcbb3fbbc01d99cd4e` | `9de32f6b` | SAME | 16173 |
| `ansible-configs/docs/upgrade-debian.md` | `7f887ad95fbda7bb729469fd27a8ccb0b2912fea` | `7f887ad9` | SAME | 1715 |
| `ansible-configs/update-packages.yml` | `f3dd8199f0e92ddbeed9d43acc3e45e9dcde9d4b` | `f3dd8199` | SAME | 601 |
| `ansible-configs/upgrade-debian.yml` | `6680c698f0ae4b2b9aca5a184c737d95b087db48` | `6680c698` | SAME | 945 |

Old observation (A0 C-ca-5): all 9 SAME audited→live at `38a8fb57`.
New evidence: live `main` is STILL `38a8fb57`; all 9 full blob SHAs match A0 shorts;
fetched bytes re-verified with `git hash-object` (all 9 match API SHAs).
Consequence: ZERO drift at every level (ref, blob, bytes). C1 pins `38a8fb57` as the §6.1 source.

## 2. install-docker.yml vs install-docker-selene.yml (diff)

Shared core (identical task-for-task): prereq apt pkgs → Docker GPG key to
`/etc/apt/keyrings/docker.asc` → `signed-by` repo pinned to `distribution_release` codename
(`filename: docker`, removes legacy `docker-ce.list`) → `docker-ce, docker-ce-cli,
containerd.io, docker-buildx-plugin, docker-compose-plugin` (`state: present`, UNPINNED
versions) → service started+enabled → `daemon.json` + restart handler.

Deltas (only 3):

| # | `install-docker.yml` (`hosts: all`) | `install-docker-selene.yml` (`hosts: clickhouse-selene`) |
| --- | --- | --- |
| 1 | `daemon.json`: `log-opts.max-size 10m` **PLUS `default-address-pools 172.30.0.0/16 /24`** | `daemon.json`: `log-opts.max-size 10m` ONLY (no pools) |
| 2 | No systemd drop-in | Conditional `docker.service.d/selene-observability-storage.conf` drop-in (`After=`+`Requires=` selene mount-guard unit; no-op unless that unit exists) |
| 3 | Generic | Header comment records D6 data-root decision: keep default `/var/lib/docker` |

Neither playbook sets: package versions, `log-opts.max-file`, cgroup driver,
storage-driver, data-root, live-restore, iptables/IPv6, ulimits. (Docker defaults apply:
json-file `max-file=1`; systemd cgroup driver on Debian 12/13.)

## 3. RECOMMENDATION: bastion Docker shape = generic `install-docker.yml` + 3 deltas

Adopt `install-docker.yml` (`85b9a2c1…`, `hosts: all`) as the base, NOT the selene variant:

1. **Address pools (keep, from generic):** `default-address-pools 172.30.0.0/16 /24` moves
   Docker networks off the congested `172.17–172.29` defaults — matters on a CI bastion
   running many ephemeral job networks. Selene omits pools only because that host's
   networking is fixed; bastion is a fresh multi-network host.
2. **No selene mount-guard (drop):** the `Requires=` drop-in hard-depends on a
   selene-observability unit that will never exist on bastion (conditional no-op there,
   dead weight here). If bastion later needs Docker-after-mount ordering (e.g.
   `/var/cache/velnor`), add a bastion-named drop-in then — do not inherit selene's name.
3. **Data-root (follow selene's D6, already the default):** keep `/var/lib/docker` for
   engine state; bastion job/cache mounts stay explicit bind-mounts. No `data-root` key.

Three gaps to close in the bastion play (spec §6.1 says "pinned" but only the REPO is
pinned — `signed-by` + codename — while `docker-ce` versions float on `state: present`):

- **a. Pin or snapshot versions:** either `docker-ce=<ver>` pins (bump via PR) or record
  `docker version` + `dpkg -l docker-ce*` into C-phase evidence at install time.
  Recommend pinning: bastion is long-lived CI infra, silent major bumps are unacceptable.
- **b. Add `log-opts.max-file` (e.g. 3–5):** both sources set only `max-size: 10m`, so the
  json-file default `max-file=1` keeps a single 10 MB log per container — thin for
  debugging failed CI jobs. Keep `max-size: 10m`, add rotation count.
- **c. Cgroup driver:** leave implicit (Docker on Debian 12/13 defaults to systemd, the
  correct driver); optionally assert `docker info --format '{{.CgroupDriver}}'` ==
  `systemd` in provisioning verification rather than writing `exec-opts`.

## 4. Other 7 paths — C1-relevant notes

- `install-base.yml` (`6c1e2ecf…`): UTC, APT base pkgs, terminfo, mise toolchains,
  zsh/starship cosmetics. Bastion takes the head (timezone, base pkgs, mise) and SKIPS
  the cosmetics (oh-my-zsh, starship, zshrc, GraalVM/Rust/cargo tool installs) — bastion
  is a headless CI host. Most valuable precedent: the **holla-apt repo block**, which the
  comments call "the exact same standard Debian apt pattern used for velnor-runner" —
  this is the template for bastion's Velnor APT wiring.
- `hosts.ini` (`cc0eda31…`): `[nodes]` group, `root`, `StrictHostKeyChecking=accept-new`.
  Bastion adds its own host/group; keep the `accept-new` shape for first-provision.
- `requirements.yaml` (`e8053df3…`): `community.general, community.postgresql,
  ansible.posix`. Bastion needs at least `community.general` (timezone) — install all 3.
- `README.md` (`9de32f6b…`): playbook index + `fnox`/`op` secret bootstrap + **Sentry/Velnor
  section**: Velnor installs ONLY via `velnor-runner` APT from `velnor-apt.tailrocks.com`
  (never cargo-install, never `docker commit`); 3 runner daemons on sentry; GARM-removal
  tags. Direct precedent for bastion's Velnor install shape.
- `upgrade-debian.yml` (`6680c698…`) + `docs/upgrade-debian.md` (`7f887ad9…`): play only
  rewrites Hetzner-mirror + docker `.sources`/`.list` files; the actual release upgrade
  (incl. full Docker drain: stop/rm/rmi/network-prune/volume-prune) is MANUAL per the
  runbook. Bastion adoption: same split — never automate `full-upgrade` on CI infra.
- `update-packages.yml` (`f3dd8199…`): `dist` upgrade + autoremove + `mise upgrade` +
  daemon-reload. Bastion routine-maintenance shape; schedule in drained windows only
  (unpinned Docker floats here — ties back to recommendation 3a).

## 5. C1 consequence summary

- Pin `38a8fb5777fd02fec8b3904d86387aba05940e9a` for all §6.1 content; no drift to reconcile.
- Docker base = generic playbook; selene variant contributes only its D6 data-root rationale.
- Close the pin/log-rotation gaps (3a, 3b) in the bastion play; verify cgroup driver (3c).
- Reuse the holla-apt block as the Velnor APT template and the Sentry/Velnor README section
  as the runner-install precedent. Fetched bytes in `/tmp/c1-ansible/` are the C1 inputs.
