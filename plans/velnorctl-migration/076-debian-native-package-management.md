# Plan 076: Make Debian package management the only version-management path

> **Executor instructions**: Remove Velnor-owned installed-version management.
> Do not add a replacement command, resource, API, state table, activation
> pointer, or rollback service. Debian package production may remain in
> maintainer CI/tooling; signed apt and dpkg exclusively own installation and
> installed-version transitions.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/release.rs crates/velnor-tools .github/workflows/release-deb.yml systemd packaging debian crates/velnor-control crates/velnor-model crates/velnorctl`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 064, 068, 073
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
- systemd executes `/usr/bin/velnorctl daemon` directly. Startup never resolves
  an active-version symlink or calls Velnor-owned package verification.

## Scope

- Debian control metadata, package names, file ownership, conffiles,
  maintainer scripts, systemd integration, and signed apt publication
- removal of old runner release subcommands and all custom active/previous
  target, activation, rollback, and installed-version history code
- retention or relocation of source-package assembly, provenance, signing, and
  repository publication only where needed by maintainer CI/tooling
- package transition from `velnor-runner` to `velnorctl`

**Out of scope**: any operator package/version command in `velnorctl`, a custom
upgrade orchestrator, direct `.deb` installation, `dpkg -i`, copied binaries,
local-path apt repositories, custom package state in `state.db`, or a second
version selector beside dpkg.

## Steps

### 1. Inventory and classify old release behavior

Trace every call into `release.rs`, release-related Clap variant, active-target
or previous-target file/symlink, history record, startup verification hook,
systemd pre-start check, documentation instruction, and packaging workflow.
Classify each as Debian package production, Debian package consumption, or
obsolete Velnor-owned installed-version management.

**Verify**: the inventory has one disposition per live symbol/path. No custom
installed-version behavior is mislabeled as package production.

### 2. Delete duplicate installed-version services

Delete operator status/verify/activate/rollback/history handlers and their
domain services, DTOs, routes, events, tables, migrations, active-target state,
previous-target state, and startup dependency. Do not move this behavior into
`velnor-control`, `velnor-tools`, or another command name. Preserve only generic
binary version reporting and read-only Debian package observations needed by
existing status/diagnostics tasks.

**Verify**: source, generated help/completion/man pages, API schemas, state DB
migrations, and tests contain no Velnor version mutation or selection surface.

### 3. Make the Debian package self-contained

Package `/usr/bin/velnorctl`, required systemd units, tmpfiles/sysusers/config
assets, and declared dependencies through standard Debian metadata. Ensure
maintainer scripts are idempotent, preserve operator configuration, respect
graceful service stop/start ordering, and never create an alternate release
tree or activation symlink. Express the one-time rename with tested Debian
`Conflicts`, `Replaces`, and any required `Breaks` metadata; install no
compatibility binary.

**Verify**: package-content and ownership tests prove dpkg owns every installed
runtime file and no old/custom activation payload exists.

### 4. Preserve signed package production without operator version logic

Keep source archive, provenance, package build, repository metadata signing,
and publication in maintainer CI/tooling. Publish every supported exact version
to the signed `velnor-apt` repository so apt can perform forward and predecessor
transactions. Do not make immutable build records an installed-version switch.

**Verify**: a clean CI build produces repository metadata and package hashes
whose signatures validate through apt's trust path; maintainer tooling cannot
mutate a host's installed version.

### 5. Prove native install, upgrade, downgrade, recovery, and history

In disposable Debian environments, configure only the signed repository and
exercise fresh install, exact-version upgrade, exact-version downgrade to the
signed predecessor, failed-maintainer-script recovery, reinstall, and purge/
reinstall as applicable. Verify candidate selection, package state, conffile
behavior, unit lifecycle, file integrity, apt/dpkg history, and removal of the
old package.

**Verify**: all transitions use apt; audit spies fail the test on direct
`dpkg -i`, local `.deb`, copied binary, local-path repository, active-target
write, or Velnor-owned rollback call.

### 6. Mandatory fixture integration

Build and publish two test versions to a disposable signed apt repository. Pin
the exact `tailrocks/velnor-actions-fixture` commit. Before each dispatch,
cancel every pending/in-progress old run, delete only stale validation-owned
registrations, prove clean, and capture only the new run ID.

Install version A with exact apt selection, start the packaged service, and run
the full required fixture surface. Gracefully drain, upgrade to version B with
apt, restart/wait Ready, and rerun the same fixture. Then drain and install
exact signed version A through apt's downgrade path, restart/wait Ready, and
rerun again. Check progress within 60 seconds and diagnose stasis before two
minutes. Preserve sanitized non-HTML evidence only.

**Verify**: all three runs pass without workflow changes; dpkg reports the
expected version after each transaction; apt/dpkg logs record each transition;
no custom release state or command participates.

## Done criteria

- [ ] No release command family exists in either product binary.
- [ ] No release resource, API route, event, state table, activation pointer,
      previous pointer, or Velnor rollback service exists.
- [ ] dpkg exclusively owns installed runtime files and installed version.
- [ ] Signed apt install, upgrade, downgrade/rollback, and recovery pass in
      disposable Debian environments.
- [ ] Fresh fixture proof passes on version A, version B, and apt-restored A.
- [ ] `rtk mise run check` passes.

## STOP conditions

- A proposed path bypasses the signed apt repository or dpkg ownership.
- Package transition would overwrite/remove operator configuration or interrupt
  an active fixture job without graceful drain.
- Removing custom state would make current package production impossible and
  the exact build-only dependency has not yet been separated from host version
  management.
