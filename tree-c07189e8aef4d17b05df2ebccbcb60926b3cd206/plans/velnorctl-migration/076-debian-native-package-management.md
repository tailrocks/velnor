# Plan 076: Prepare the Debian-native package transition

> **Executor instructions**: Prepare and prove the Debian transition without
> switching an installed daemon/package entrypoint. Separate maintainer package
> production from Velnor-owned host version logic and record the exact deletion
> set for Plan 079. Do not add a replacement command, resource, API, state table,
> activation pointer, or rollback service.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- Cargo.toml Cargo.lock crates/velnor-runner/Cargo.toml crates/velnor-runner/src/release.rs crates/velnor-runner/debian crates/velnor-tools .github/workflows/release.yml Dockerfile docker crates/velnor-control crates/velnor-model crates/velnorctl`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 064, 068
- **Category**: migration, Debian packaging
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Velnor currently carries release parsing, verification, activation, rollback,
and history behavior that duplicates Debian's installed-package authority. The
operator decision rejects that duplicate control plane. A Debian host must use
normal signed-repository package operations for install, upgrade, downgrade,
rollback, recovery, integrity checks, and version history.

## Required authority boundary

- `apt` selects and installs an exact published package version from the signed
  `velnor-apt` repository.
- `dpkg` owns installed package/file state and maintainer-script execution.
- `apt-cache policy velnorctl`, `dpkg-query -W velnorctl`, `dpkg -V velnorctl`,
  `/var/log/apt/history.log`, and `/var/log/dpkg.log` are the native inspection
  surfaces. Velnor may include bounded, read-only results from them in general
  status or diagnostics, but may not become a second package manager.
- Rollback means installing the exact signed predecessor with apt, including
  explicit Debian downgrade authorization when required. It is an operational
  package transaction, not a Velnor command or domain object.
- Plan 079 will make systemd execute `/usr/bin/velnorctl daemon` directly after
  C075 exists and is green. Startup then never resolves an active-version
  symlink or calls Velnor-owned package verification.

## Scope

- transition-ready Debian control metadata, future package names, file
  ownership, conffiles, maintainer scripts, systemd assets, and signed apt
  publication harness; no installed entrypoint switch in this plan
- inventory and separation of old runner release subcommands and all custom
  active/previous target, activation, rollback, and installed-version history
  code; Plan 079 owns final deletion after C075
- retention or relocation of source-package assembly, provenance, signing, and
  repository publication only where needed by maintainer CI/tooling
- package transition design and disposable metadata proof from `velnor-runner`
  to `velnorctl`

**Out of scope**: any operator package/version command in `velnorctl`, a custom
upgrade orchestrator, direct `.deb` installation, `dpkg -i`, copied binaries,
local-path apt repositories, custom package state in `state.db`, or a second
version selector beside dpkg. Also out of scope: changing the live/installed
package name, switching systemd or Docker entrypoints, deleting the old runtime,
or running live A/B/A Velnor acceptance; Plan 079 owns those atomic actions.

## Steps

### 1. Inventory and classify old release behavior

Trace every call into `release.rs`, release-related Clap variant, active-target
or previous-target file/symlink, history record, startup verification hook,
systemd pre-start check, documentation instruction, and packaging workflow.
Classify each as Debian package production, Debian package consumption, or
obsolete Velnor-owned installed-version management.

**Verify**: the inventory has one disposition per live symbol/path. No custom
installed-version behavior is mislabeled as package production.

### 2. Separate production code and freeze the deletion manifest

Separate source-package assembly, provenance, signing, and publication from
host activation/rollback/status logic. Produce a checked inventory of every
operator handler, domain service, DTO, route, event, table/migration, active or
previous target, and startup dependency that Plan 079 must delete. Do not move
host behavior into `velnor-control`, `velnor-tools`, or another command name.
Preserve generic binary version reporting and read-only Debian observations.
This plan may delete already-unreferenced host-version code, but must not break
the still-installed old daemon before its replacement exists.

**Verify**: maintainer package production has no dependency on host activation
state; every remaining host-version symbol/path is in the Plan 079 deletion
manifest and no new Velnor version surface exists.

### 3. Prepare self-contained Debian assets

Prepare metadata for `/usr/bin/velnorctl`, required systemd units,
tmpfiles/sysusers/config assets (including `velnor` and `velnor-admin` groups),
and declared dependencies. Ensure
maintainer scripts are idempotent, preserve operator configuration, respect
graceful service stop/start ordering, and never create an alternate release
tree or activation symlink. Express the one-time rename with tested Debian
`Conflicts`, `Replaces`, and any required `Breaks` metadata; install no
compatibility binary. Use fixture/stub payloads in package-transition tests
until C075 produces the real daemon; never publish them as product packages.

**Verify**: staged package-content/ownership tests prove the future dpkg mapping
and group/file modes, without switching any installed product entrypoint.

### 4. Preserve signed package production without operator version logic

Keep source archive, provenance, package build, repository metadata signing,
and publication in maintainer CI/tooling. Publish every supported exact version
to the signed `velnor-apt` repository so apt can perform forward and predecessor
transactions. Do not make immutable build records an installed-version switch.

**Verify**: a clean CI build produces repository metadata and package hashes
whose signatures validate through apt's trust path; maintainer tooling cannot
mutate a host's installed version.

### 5. Prove the transition harness with disposable payloads

In disposable Debian environments, configure only a signed HTTP-served apt
repository and
exercise fresh install, exact-version upgrade, exact-version downgrade to the
signed predecessor, failed-maintainer-script recovery, reinstall, and purge/
reinstall as applicable. Verify candidate selection, package state, conffile
behavior, unit lifecycle, file integrity, apt/dpkg history, and removal of the
old package. The signing key is ephemeral, test-only, and never committed. This
proves Debian metadata/harness behavior, not the real daemon or final cutover.

**Verify**: all transitions use apt; audit spies fail the test on direct
`dpkg -i`, local `.deb`, copied binary, local-path repository, active-target
write, or Velnor-owned rollback call.

### 6. Mandatory non-regression integration

Pin the exact `tailrocks/velnor-actions-fixture` commit. Before dispatch,
cancel every pending/in-progress old run, delete only stale validation-owned
registrations, prove clean, and capture only the new run ID.

Run the full required fixture surface once through the unchanged installed
daemon after package-production separation. Check progress within 60 seconds
and diagnose stasis before two minutes. Preserve sanitized non-HTML evidence
only. Plan 079 alone performs real signed-package A/B/A fixture acceptance.

**Verify**: behavior is unchanged and the disposable Debian harness separately
proves exact candidate selection, upgrade, downgrade, recovery, file ownership,
and old-package replacement without direct `dpkg -i` or local-path apt.

## Done criteria

- [ ] Maintainer package production is separated from host version management;
      Plan 079 has an exhaustive deletion manifest.
- [ ] Transition metadata, sysusers/tmpfiles/conffiles, and maintainer scripts
      pass signed HTTP-served disposable apt tests with fixture payloads.
- [ ] No live/installed package, service, Docker entrypoint, or runtime binary
      is switched in this plan.
- [ ] Fresh fixture proof passes unchanged on the existing daemon.
- [ ] `rtk mise run check` passes.

## STOP conditions

- A proposed path bypasses the signed apt repository or dpkg ownership.
- Package transition would overwrite/remove operator configuration or interrupt
  an active fixture job without graceful drain.
- Removing custom state would make current package production impossible and
  the exact build-only dependency has not yet been separated from host version
  management.
