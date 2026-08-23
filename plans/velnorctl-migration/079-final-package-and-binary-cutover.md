# Plan 079: Final package and binary cutover

## Status

`P1 — pending; depends on Plans 063 through 078 and command tasks C001 through C075; starts only after every replacement command has fixture proof.`

## Drift check at implementation start

Re-scan the workspace, package build/publication workflows, Dockerfiles, Debian
metadata, systemd units/timers, mise tasks, scripts, tests, active documentation,
and `tailrocks/velnor-apt` configuration for every executable dependency on
`velnor-runner`. Separately inventory old release subcommands and custom
active/previous-version state so they are deleted, not migrated. Classify
historical prose separately from live interfaces. Do not delete the old crate
until the retained replacement matrix is complete and green.

## Why this task exists

The requested terminal state has no `velnor-runner` worker, daemon, package,
compatibility wrapper, or executable command. C001–C075 move and prove every
retained leaf command one at a time. Plan 076 deliberately gives old release
commands no Velnor replacement: signed apt/dpkg exclusively own installed
version selection and transitions. This task contracts repository and
deployment surface only after all retained behavior is proven.

## Required final state

```text
crates/velnor-model
crates/velnor-control
crates/velnor-client
crates/velnor-render
crates/velnorctl
crates/velnor-tools
```

- `/usr/bin/velnorctl` is the only product binary and is directly owned by the
  Debian package named `velnorctl`.
- systemd executes `/usr/bin/velnorctl daemon` directly.
- install, upgrade, downgrade, rollback, recovery, package integrity, and
  installed-version history use standard apt/dpkg surfaces only.
- no Velnor release command/resource/API/table/event, active-version symlink,
  previous-version pointer, runtime record verification, or activation service
  remains.
- images and Debian artifacts contain `velnorctl`, not an alias, symlink,
  copied old binary, or compatibility subcommand.
- `crates/velnor-runner` is deleted after all reusable modules have moved.
- `velnor-tools` remains maintainer-only and is not the daemon entry point or a
  host package-version manager.

## Command migration gate

Before contraction, verify every row in `commands/README.md` is `DONE`, then
verify this old-surface mapping:

| Old surface | Required final owner |
| --- | --- |
| `status` | `velnorctl get slots` and `describe instance/...` |
| mutating `doctor` | read-only `velnorctl doctor` plus explicit `reconcile` |
| `preflight` | `velnorctl preflight` |
| `storage paths/status` | `velnorctl storage paths/status` |
| `cache du/gc` | `velnorctl storage du/gc` |
| `capabilities check-job` | `velnorctl capability check` |
| old release status/verify/activate/rollback/history | removed; apt/dpkg are the native operator interface |
| package build/sign/publish | maintainer CI/tooling only |
| `configure/remove` | `velnorctl instance init/apply/delete` |
| `daemon` | `velnorctl daemon` |
| single-job `run` | internal service operation `velnorctl daemon --once` |

No old spelling is kept as a hidden alias.

## Implementation steps

1. Move final worker/runtime modules from `crates/velnor-runner` into
   `velnor-control`, `velnor-model`, or `velnorctl` according to ownership.
   Preserve behavior with before/after characterization tests; remove private
   cross-module shortcuts.
2. Verify C075's service-only `velnorctl daemon` and `--once` entry point, then
   switch installed systemd/package entrypoints to `/usr/bin/velnorctl daemon`.
   Keep operator `velnorctl run` reserved for GitHub workflow-run commands.
3. Delete old runner release Clap variants and the custom installed-version
   services/state identified by Plan 076. Do not relocate them. Keep only
   package-production code required to build/sign/publish Debian artifacts.
4. Update workspace members, dependency graph, lockfile, crate imports, feature
   flags, build metadata, compiled version reporting, and test paths. Delete
   `crates/velnor-runner` only when `rg` proves no live source import remains.
5. Rename the Debian package, binary paths, package artifacts, checksums,
   provenance subjects, Docker entrypoints, and OCI labels to `velnorctl`.
   Debian package metadata, not a Velnor runtime record, owns installed files
   and version state.
6. Update systemd units and timers atomically. Preserve graceful SIGTERM drain,
   stop timeout, watchdog, restart policy, user/group, paths, and unprivileged
   execution. Unit names may remain stable where they represent instances
   rather than the removed binary.
7. Update signed apt publication in `tailrocks/velnor-apt`. Use an explicit,
   package-tested Debian transition (`Conflicts`, `Replaces`, and `Breaks` when
   justified) to remove the installed old package during the one-time apt
   transaction; install no compatibility binary.
8. Update all executable workflow, script, fixture, test, documentation, and
   example invocations. Permit old terminology only in immutable historical
   evidence and this migration record; no current instruction may invoke it.
9. Build and publish through the required supply chain: green signed commit,
   signed tag, immutable source evidence, signed Debian package/repository
   metadata, and exact apt candidate verification. Never use local `.deb`,
   `dpkg -i`, copied binaries, or local-path apt.
10. For live cutover, drain the installed service, confirm no active jobs,
    reservations, or leases, install exact `velnorctl=<version>` through apt,
    verify with `apt-cache policy`, `dpkg-query`, `dpkg -V`, binary version,
    unit ownership, and runtime readiness, then run fixture acceptance.
11. Keep the signed predecessor published until acceptance completes. A failed
    cutover is restored only with an exact apt install of that predecessor,
    followed by service readiness and the same fixture proof.
12. Add CI audits that fail on a reintroduced `velnor-runner` crate, binary
    target, package, executable path, systemd command, Docker entrypoint,
    package payload, active-doc invocation, or Velnor version-management
    command/resource/API/state.

## Tests

- Run the complete Rust suite with `cargo nextest run`; never use `cargo test`.
- Build all workspace targets and Debian/OCI artifacts from a clean tree.
- Install, upgrade, downgrade/rollback, recover, and reinstall in disposable
  Debian environments using only signed apt repositories. Verify maintainer
  script ordering, conffile preservation, unit state, dpkg ownership/integrity,
  apt/dpkg history, exact candidate selection, and old-package removal.
- Test SIGTERM drain and watchdog behavior from the packaged systemd unit.
- Inspect package and OCI contents; fail if any executable, symlink, or copied
  payload named `velnor-runner`, or any custom activation state, exists.
- Run `git diff --check`, active-doc link checks, command help snapshots, shell
  completions, command-ID audit, and live-reference audit.

## Mandatory fixture integration validation

1. Pin an exact `tailrocks/velnor-actions-fixture` commit and record the prior
   apt candidate and dpkg-installed package/version.
2. Before dispatch, cancel all pending/in-progress fixture runs and delete only
   stale validation-owned registrations. Confirm the new packaged
   `velnorctl` service alone owns registrations.
3. From the signed apt-installed package, run cold, warm, and unchanged
   executions of every canonical fixture lane required by the estate contract.
   Every job, log, artifact, cache result, service, container, cancellation,
   and teardown check must pass.
4. During fixture work, inspect active slots/jobs/logs; cordon; drain without
   killing a busy job; resume; recycle an idle slot; and reconcile in dry-run
   mode.
5. Exercise run list/view/watch/download, storage status/du, doctor, preflight,
   capability/workflow checks, and diagnostics bundle. Version-transition proof
   comes only from the apt/dpkg checks defined above.
6. Monitor only new run IDs, check within 60 seconds, and diagnose unchanged or
   queued state before two minutes.
7. After green acceptance, prove through the host, dpkg database, apt policy,
   service units, process list, runner registrations, and image/package
   manifests that no old binary/package or custom version state is active or
   installed.
8. Preserve sanitized evidence only; never commit rendered GitHub HTML.

## Rollback and preservation proof

- Before apt cutover, record active jobs, reservations, leases, runner
  registrations, apt candidate, dpkg-installed version, package integrity, and
  relevant unit hashes.
- If readiness or fixture acceptance fails, stop the candidate, install the
  exact signed predecessor through apt's Debian downgrade path, start, wait
  Ready, and rerun the same fixture gate.
- Do not restore a Velnor activation target or custom state database. Rollback
  is a Debian package transaction used for migration safety, not a retained
  Velnor interface.
- Resume contraction only after root cause is fixed and forward proof is green.

## Done when

- The old crate, binary target, installed package, service invocation, image
  payload, and all custom installed-version machinery are absent.
- All retained behavior is owned by `velnorctl` or shared crates; package
  production remains maintainer-only.
- Signed apt install and predecessor downgrade are proven; dpkg is the sole
  installed-version/file authority; final runtime executes only `velnorctl`.
- Full fixture cold/warm/unchanged and operator acceptance is green.

## Stop condition

Stop at complete removal. Do not preserve aliases, wrapper binaries, deprecated
command shims, parallel packages, a second daemon, or any Velnor-native package
activation/version-management layer.
