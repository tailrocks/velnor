# C-Scout Report — Bastion Campaign Phase C De-risk (READ-ONLY)

Date: 2026-09-17. Scout scope: work-plan STEP C1/C2 + checklist C + spec §6.1,
all read from `tailrocks/velnor` `origin/main` @ `58a94b12d0139c303ddf1dfdd57a36260c39fa02`.
No bastion writes; no SSH at all (GitHub-side sources + local git objects only).
Pre-B/C recon `/tmp/prebc-recon.md` read: bastion is zero-state (Debian 13,
no docker, no velnor residue, second NVMe virgin).

## 1. ANSIBLE PATHS (§6.1 × live ChainArgos/java-monorepo main)

Live main: `218a44b28984acf4ceee24cd1d9d6ccb8c38ae37`.
Evidence-doc audited SHA: `235e479b150aeb949bc8a5190fba5b84f6303c80`.
Blob comparison audited→main per path via `gh api .../contents/<path>?ref=<sha>`:

| # | Path | Exists on main | Blob SHA (main) | Drift vs audited |
|---|------|----------------|-----------------|------------------|
| 1 | `ansible-configs/install-base.yml` | YES | `6c1e2ecf…cff` (242 lines) | SAME |
| 2 | `ansible-configs/install-docker.yml` | YES | `85b9a2c1…e94` (75 lines) | SAME |
| 3 | `ansible-configs/install-docker-selene.yml` | YES | `b30a027e…2ae` (111 lines) | SAME |
| 4 | `ansible-configs/hosts.ini` | YES | `cc0eda31…9e7` (11 lines) | SAME |
| 5 | `ansible-configs/requirements.yaml` | YES | `e8053df3…f4c` (4 lines) | SAME |
| 6 | `ansible-configs/README.md` | YES | `9de32f6b…d4e` (361 lines) | SAME |
| 7 | `ansible-configs/docs/upgrade-debian.md` | YES | `7f887ad9…fea` (108 lines) | SAME |
| 8 | `ansible-configs/update-packages.yml` | YES | `f3dd8199…d4b` (28 lines) | SAME |
| 9 | `ansible-configs/upgrade-debian.yml` | YES | `6680c698…b48` (34 lines) | SAME |

Result: 9/9 exist, ZERO drift (all blobs identical audited→main). No successor
search needed. Note: §6.1 names playbooks/docs/inventory only — there are no
`roles/` in the list, so "setup roles" below = the playbooks themselves.

## 2. HOST SETUP IDEMPOTENCY (per-path verdicts)

Grep over all 9 files for `sshd|ssh_config|libvirt|kvm|qemu|nvme|parted|fdisk|
mkfs|mdadm|raid|lvm|format`: only hits are "GraalVM"/"install" substrings.
**None of the 9 touch SSH config, libvirt/KVM/QEMU, or any disk/NVMe.**
(Caveat: `setup-*.yml`/`init-*-drives.yml` playbooks NOT in §6.1 do manage SSH
keys/LVM — must stay out of C1 scope.)

1. `install-base.yml` — MOSTLY IDEMPOTENT, DO NOT RUN VERBATIM. Safe: `timezone`,
   `apt state:present`, guarded terminfo (`creates:`), `creates:`-guarded
   oh-my-zsh/starship, idempotent copy/file/user/git. NOT clean: (a) 9×
   `mise install/use -g` (L177–204) lack `creates:`/`changed_when` → run and
   report changed every time; (b) `holla state: latest` (L239–242) upgrades on
   every run — unpinned; (c) flips root shell to zsh + control-host `.zshrc`
   (low SSH-automation risk if zshrc errors; BatchMode still spawns the shell).
   SCOPE: full playbook installs GraalVM/Rust/mise/nushell/holla/starship —
   beyond §6.1's bastion use ("UTC timezone, APT base packages,
   shell/terminfo baseline"). C1 must run a NAMED TASK SUBSET, not the playbook.
2. `install-docker.yml` — IDEMPOTENT, SAFE PATTERN. apt/get_url/apt_repository/
   file-absent/service/copy+notify all converge; re-run restarts docker only if
   `daemon.json` changed (fine pre-job). GAPS: (a) `state: present`, NO version
   pin — spec/C1 say "pinned Docker", playbook doesn't pin; (b) `arch=amd64`
   hardcoded (OK for bastion x86_64); (c) `daemon.json` sets
   `default-address-pools 172.30.0.0/16` — needs bastion route check vs spec §6
   ("host public route never altered merely to add container networks").
3. `install-docker-selene.yml` — REFERENCE ONLY, DO NOT RUN. `hosts:
   clickhouse-selene` (wrong host; bastion absent from their inventory) +
   selene mount-guard drop-in (no-ops off-selene via `when:`). Use only to
   compare the minimal `daemon.json` shape, per spec.
4. `hosts.ini` — PATTERN ONLY. 6 hosts (`pegasus/delorean/titan/sentry/
   postgresql-nova/clickhouse-selene`), `ansible_ssh_user=root`, no bastion
   entry. Bastion needs its own inventory/limit.
5. `requirements.yaml` — SAFE PREREQUISITE. `community.general/postgresql,
   ansible.posix`; `ansible-galaxy install -r` idempotent.
6. `README.md` — DOCS ONLY. Run commands, secret bootstrap (`fnox`/`op`),
   "Sentry and Velnor" precedent (velnor via apt, PATs in `/run` tmpfs,
   `setup-sentry.yml --tags velnor`) — sentry-specific, not the C1 path.
7. `docs/upgrade-debian.md` — EXCLUDE FROM C1. Manual release-upgrade runbook;
   "Drain Docker" block (`docker stop/rm/rmi/network rm/system prune/volume
   prune+rm`) is destructive and violates spec §6 (no host-wide prune,
   owned-only reclaim). Bastion already Debian 13 — not applicable.
8. `update-packages.yml` — EXCLUDE FROM C1 SETUP. Converges but executes full
   `dist-upgrade + autoremove + mise upgrade + daemon-reload` EVERY run —
   kernel/libc churn, not a safe "idempotent re-run proof" vehicle.
   Operator-gated maintenance only.
9. `upgrade-debian.yml` — NOT APPLICABLE TO C1. File ops idempotent but
   semantically a release-upgrade step (Hetzner-mirror sources rewrite, needs
   `debian/sources.list.d/*` files outside the §6.1 list). Bastion is trixie.

## 3. PACKAGE VERBS (tailrocks/velnor origin/main — implemented surface only)

Two-binary estate (`crates/velnor-runner/Cargo.toml:9-11`): `/usr/bin/velnor-runner`
= daemon/service plumbing; `/usr/bin/velnorctl` = ONLY operator-facing CLI.
Package version on main: `0.1.275` (Cargo.toml:3).

- `release verify-installed` — EXISTS on the SERVICE binary:
  `velnor-runner release verify-installed [--record --deployed --binary --arch]`
  (`crates/velnor-runner/src/service.rs:290-292`, args `service.rs:351-365`;
  defaults `/var/lib/velnor/release/active/{record,deployed}.json`,
  `/usr/bin/velnor-runner` per `args.rs:14-18`). Invoked no-arg by 4 units'
  `ExecStartPre` under shared flock: `velnor-controller@.service:19`,
  `velnor-daemon.service:40`, `velnor-daemon@.service:36`,
  `velnor-guardian.service:16`. C1 action 6 command is valid as written.
- ACTIVATION — `sudo velnor-runner release activate --record <record.json>`
  (`service.rs:293-294`; `ReleaseActivateArgs service.rs:367-375`;
  atomic handler `release.rs:1554-1580`, demotes current→previous).
  Documented by the package itself in `postinst` tail echo:
  `activate … then systemctl enable --now velnor-daemon@<name>`.
  STABLE record is NOT shipped in the deb (circular — records the deb digest);
  staged out-of-band at deploy time (`Cargo.toml` Plan-010 comment). Preview
  installs may use shipped `/usr/share/velnor/package-record.json`.
  postinst NEVER activates/restarts (header comment) — explicit operator step.
  ⚠ postinst header comment names `velnorctl release activate` — STALE/WRONG:
  `velnorctl`'s `Command` enum (`crates/velnorctl/src/lib.rs:237-326`) has NO
  `Release` variant (`release` merely "reserved", lib.rs:234-235).
- DRAIN (package procedure) — `systemctl stop` the exact velnor fleet units.
  `preinst` (`install` AND `upgrade` cases) and `postinst configure` FAIL CLOSED
  unless every `velnor*.service/timer` is inactive/failed (lock-serialized
  maintenance oneshots exempt); error text: "Stop the exact units first."
  Graceful: daemon `TimeoutStopSec=10800` + `KillMode=mixed` finishes running
  jobs on stop (unit comment, 2026-06-11 incident). On zero-state bastion the
  drain is vacuous but the flock-held transaction is still mandatory.
  `velnorctl drain <target> --reason --idempotency-key [--expected-version]`
  (`lib.rs:268-269`, `commands.rs:189-197`) is a CONTROL-API instance mutation
  needing a running daemon — NOT the package-upgrade path. `velnorctl host
  drain` (`commands.rs:485-486`) prints EXPLANATION text only. Never invent
  `velnorctl health`-style verbs: NO `health` subcommand exists.
- HEALTH — `velnorctl status [--json]` (`lib.rs:296`; `--json` = "node health
  vector, not systemd is-active", `runtime.rs:781-783`), `velnorctl doctor
  --url --name --slots` (fleet probe; also `velnor-doctor[@].service/timer`
  units), `velnorctl diagnostics/top/telemetry/events`, plus systemd
  `is-active` and guardian `WatchdogSec=30`.
- Guard test: `crates/velnorctl/tests/packaged_invokers.rs` proves packaged
  files' `/usr/bin/velnorctl` verbs parse (units reference only `velnor-runner`
  verbs + `velnorctl cache gc` via the gc timer per postinst comment).

## 4. DOCKER ON BASTION

YES — docker install is part of C1 host setup, not from the package:
- Spec §6 (`spec.md:216`): "Docker and required Buildx/Compose tooling are
  installed through an approved pinned package setup; cgroup v2/systemd driver
  is verified against native preflight."
- Spec §6.1 (`spec.md:231-232`): `install-docker.yml` = Docker APT repo +
  `docker-ce` + Buildx/Compose plugins + daemon config + enablement.
- Work-plan C1 action 3 (`work-plan.md:268-272`): host setup includes "pinned
  Docker + Buildx/Compose, cgroup v2/systemd driver verified".
- The deb declares only `recommends = "docker.io | docker-ce"`
  (`Cargo.toml:53`), NOT a hard `depends`. Exact provisioning step = **C1
  action 3, BEFORE the action-5 `flock … apt-get install` transaction**:
  add Docker APT repo + install pinned `docker-ce/cli/containerd.io/
  buildx/compose` + write `daemon.json` + `systemctl enable --now docker` +
  verify cgroup v2/systemd driver. ORDERING TRAP: apt installs Recommends by
  default, so skipping action 3 would drag in distro `docker.io` (unpinned,
  wrong shape) during action 5 — C1 must verify `docker-ce` satisfies it.

## VERDICT: C-GAPS (exact list)

- G1 [BLOCKER — B3 precondition]: current main package contradicts spec §4.3
  four ways with FAIL-CLOSED enforcement: postinst writes + verifies a
  host-scaled `CPUQuota` drop-in for `velnor-jobs.slice` (aborts if quota
  ineffective); shipped `velnor-jobs.slice` hard-codes `MemoryHigh=90%`,
  `MemoryMax=95%`, `MemorySwapMax=0`, `TasksMax=4096` + `AssertPathExists` on
  the quota drop-in. C2 quota-free proof is IMPOSSIBLE on this package; B3 must
  remove all four per §0.7 before C1/C2 execute.
- G2 [BLOCKER unless overridden]: packaged `velnor-daemon@.service` defaults
  `VELNOR_JOB_CPUS=4` + `VELNOR_JOB_MEMORY=12g` → per-job `--cpus/--memory`
  ceilings (§4.3 bans Docker CPU/memory ceilings). B3 must drop the defaults
  or C2 must clear them in the instance env (`VELNOR_JOB_CPUS=`,
  `VELNOR_JOB_MEMORY=`; instance `EnvironmentFile` comes after `Environment`
  so override wins — verify at execution).
- G3 [C1 INPUT]: stable activation record source unnamed — full release record
  is staged out-of-band (not in deb). C-plan must name the exact B4-artifact
  fetch path yielding the `--record` file before C1 runs.
- G4 [C1 INPUT]: bastion fleet unit set undefined — postinst documents only
  `velnor-daemon@<name>`; package also ships guardian/controller@/slot@/job@/
  doctor units, all verify-installed-gated. C1 must name the exact units +
  instance names + `/etc/velnor/<name>.env` contents.
- G5 [C1 INPUT]: Docker version pin missing — reference playbook uses
  `state: present`. C1 must choose exact `docker-ce/cli/containerd/buildx/
  compose` versions ("pinned Docker" per spec/C1).
- G6 [C1 INPUT]: `install-base.yml` scope — full playbook installs
  Java/Rust/mise/nushell/holla/oh-my-zsh + flips root shell; §6.1 bastion use
  is timezone/base-packages/shell-terminfo only. C1 must name the task subset.
  (`holla state: latest` unpinned; mise tasks always-report-changed.)
- G7 [DOC, minor]: postinst header comment's `velnorctl release activate` is a
  nonexistent verb — B3 should fix to `velnor-runner release activate` to
  prevent operator error.
- G8 [ORDERING — plan already correct, verify at execution]: action 3
  (docker-ce) MUST precede action 5 (apt install) or apt's default
  Recommends install pulls distro `docker.io`; C1 must assert `docker-ce`.
- G9 [SCOPE — make explicit in C1]: do NOT run `install-docker-selene.yml`
  (wrong host), `update-packages.yml` (full dist-upgrade), `upgrade-debian.yml`
  + runbook (release-upgrade; runbook's docker-prune block violates §6).

**C-GAPS**
