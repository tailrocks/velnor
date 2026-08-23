# Command Task C016: Implement `velnorctl describe <resource>/<name>`

> **Executor instructions**: Implement only `velnorctl describe <resource>/<name>`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/config.rs crates/velnor-runner/src/capacity.rs crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: Plans 065–071, 074–075, 077
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Provide human-readable explanation for one host, instance, slot, runner, job,
run, reservation, lease, capability, or adapter.

## Current state

Required state is split across runner config, slot loops, in-flight records, systemd, Docker, storage records, logs, and GitHub. Plan 069 provides typed read projections.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl describe <resource>/<name>` in `crates/velnorctl/src/commands/describe.rs` and `crates/velnorctl/tests/describe.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global inspection conventions: versioned table/wide/JSON/YAML/JSONL/name output where meaningful, warnings on stderr, resource data on stdout, useful non-zero exits.

## Required behavior

- For jobs, render exact sections and fields: identity (repository, workflow,
  job, run/attempt, event/ref/SHA); placement (instance, stable slot, ephemeral
  runner, Docker container, trust scope); state (phase, acquired time, current
  step, conclusion, infrastructure category); timing (queue wait, pickup,
  pickup-to-first-step, checkout, container boot, workflow steps, finalize,
  teardown); resources (CPU, memory, disk reservation, active leases); and
  diagnostics (warnings, registration state, GitHub URL, typed log
  source/stream identifiers, and a `velnorctl logs` hint). Never expose absolute
  local filesystem paths as API/resource fields.
- For other resources, render equivalent typed identity, placement, state,
  conditions, source evidence, related objects, and diagnostics sections.
- Accept unambiguous canonical target syntax; return useful not-found/ambiguous errors.
- Human explanation is not a JSON dump; machine output remains versioned resource data.

## Steps

1. Add exact typed Clap shape for `velnorctl describe <resource>/<name>`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c016`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c016` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel every pending/in-progress old fixture run, delete only stale validation-owned runner registrations, and prove both sets clean.
Describe active fixture instance/slot/runner/job/run during fresh hold, then terminal job/run after cancellation; verify every section/source.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl describe --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.
