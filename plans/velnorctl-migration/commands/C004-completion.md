# Command Task C004: Implement `velnorctl completion <shell>`

> **Executor instructions**: Implement only `velnorctl completion <shell>`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/main.rs crates/velnor-runner/src/cli.rs crates/velnorctl crates/velnor-model crates/velnor-render`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plans 064–069
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Generate shell completion for command tree plus contexts, resources, instances, and slots.

## Current state

Old binary owns Clap parsing and direct dispatch. New workspace/parser seam comes from Plans 064–065; no stable command exists yet.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl completion <shell>` in `crates/velnorctl/src/commands/completion.rs` and `crates/velnorctl/tests/completion.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global inspection conventions: versioned table/wide/JSON/YAML/JSONL/name output where meaningful, warnings on stderr, resource data on stdout, useful non-zero exits.

## Required behavior

- Use Clap-generated static completion and bounded dynamic lookup for contexts/instances/slots/resources.
- Supported shells are Bash, Zsh, Fish, PowerShell, and Elvish. Dynamic lookup
  uses only context/config and the local control/query API with a 100 ms budget;
  it never calls GitHub directly. Timeout/unavailable falls back to static
  completion without warning text in the completion stream.
- Completion failure must not print credentials or hang when daemon/GitHub is unavailable.
- Write completion to stdout only; shell choice is a typed enum.

## Steps

1. Add exact typed Clap shape for `velnorctl completion <shell>`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c004`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c004` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel every pending/in-progress old fixture run, delete only stale validation-owned runner registrations, and prove both sets clean.
Generate and syntax/source-test every supported shell script in a disposable
environment; complete a live fixture instance/slot during fresh hold and prove
bounded offline fallback and no state mutation.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [ ] `velnorctl completion --help` exits 0 and documents exact accepted syntax.
- [ ] Focused tests and `rtk mise run check` pass.
- [ ] Fixture validation proves live behavior and no secret leakage.
- [ ] No sibling command, compatibility alias, or direct internal-file parser was added.

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.
