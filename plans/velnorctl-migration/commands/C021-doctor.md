# Command Task C021: Implement `velnorctl doctor [<target>]`

> **Executor instructions**: Implement only `velnorctl doctor [<target>]`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/preflight.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnorctl`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 067, 068, 069, 072
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Run read-only diagnosis with stable check results and exit statuses.

## Current state

Old doctor mixes diagnosis and mutation; preflight and repair logic are runner CLI internals. Plan 072 separates read-only checks, preflight, and typed reconciliation.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl doctor [<target>]` in `crates/velnorctl/src/commands/doctor.rs` and `crates/velnorctl/tests/doctor.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global inspection conventions: versioned table/wide/JSON/YAML/JSONL/name output where meaningful, warnings on stderr, resource data on stdout, useful non-zero exits.

## Required behavior

- Host checks: filesystem permissions, clock skew, architecture/label
  consistency, systemd readiness/watchdog, binary/package versions, and config
  ownership/mode.
- Docker checks: daemon, buildx, job image, bind visibility, socket policy,
  stale owned containers/networks, and address-pool exhaustion risk.
- GitHub checks: PAT shape/endpoint access, runner group, rate limit, labels and
  scope, registry consistency, Velnor queue visibility, and stale offline+busy.
- Storage checks: free/emergency bytes, reservations, leases, logical/physical
  cache size, pressure marker, GC eligibility, and unowned paths.
- Host checks may report read-only dpkg package/version/integrity and systemd
  unit consistency. Optional positional target values are host, docker, github,
  fleet, and storage; `--all` runs every target through the same typed check
  graph. There is no release target and doctor never installs, upgrades,
  downgrades, activates, or rolls back a package.
- Use Plan 065 exits with precedence: usage 2; otherwise authorization 3 when a
  requested check could not execute for auth; transport 7 for connectivity;
  conflict 6 for repair-required; condition 1 for degraded checks; healthy 0.
  Preserve partial machine output and test mixed-result precedence.
- Never complete/cancel/delete/GC/restart/rewrite; remediation points to reconcile.

## Steps

1. Add exact typed Clap shape for `velnorctl doctor [<target>]` plus `--all`;
   target uses `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c021`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c021` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel only older pending/in-progress validation runs owned by this iteration; never cancel protected `Release`, `Package update`, `Publish apt repo`, or `workflow_dispatch` release workflows/runs, or unrelated runs. Delete only stale validation-owned registrations, prove both sets clean, and monitor only the new run ID.
Run doctor during fresh hold and after seeded degradation; mutation spies plus fixture/GitHub state prove zero mutation and correct exits.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl doctor --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.
