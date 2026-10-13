# Command Task C054: Implement `velnorctl config sources`

> **Executor instructions**: Implement only `velnorctl config sources`. Do not combine
> sibling commands. Run every gate; update this task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/cli.rs crates/velnor-runner/src/config.rs crates/velnor-runner/src/runner.rs crates/velnor-runner/debian crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live state before edits; stop on incompatible drift.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plans 065, 068
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

List configuration sources, precedence, availability, ownership, and redaction policy.

## Current state

Configuration currently spans `runner.json`, systemd environment, process environment, and CLI flags; credentials are colocated with settings. Plan 068 provides typed source-aware redacted services.

## Scope

Implement parser, typed arguments, thin handler, output/errors, help/completion, and tests for only `velnorctl config sources` in `crates/velnorctl/src/commands/config_sources.rs` and `crates/velnorctl/tests/config_sources.rs`.
Use Plan 068 services and other declared dependencies. Never spawn old binary or expose source file formats as API.
Inspection/check behavior is read-only, versioned, redacted, and uses standard output/exit conventions.

## Required behavior

- Show client-context sources separately from daemon startup-resolved sources.
  Systemd/process environment is reported only from the daemon's captured
  startup provenance; never inspect live process environment or pretend it is a
  separately re-readable precedence layer.
- Report path/type/mode/owner/precedence without values for secret-bearing sources.
- Read-only and usable when daemon is down.

## Steps

1. Add exact typed Clap syntax for `velnorctl config sources`; reject unknown/sibling flags.
2. Call one typed service method and map invalid/auth/connectivity/timeout/domain failures to stable non-zero exits.
3. Render redacted human/machine output; keep warnings stderr and resources stdout.
4. Add parser, service, transport/file-safety, output, exit, redaction, and idempotency tests under `command_c054`.
5. Update command docs, completion/man metadata, and migration mapping; add no alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c054` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all old active runs, delete only stale validation-owned registrations, and prove clean before dispatch.
Create dedicated fixture instance using every non-secret source layer, run fresh success, and prove source list/preference metadata matches effective config.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl config sources --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and redaction.
- [ ] No sibling command, old alias, inline credential, or direct layout parser exists.

## STOP conditions

- Required service/authority is absent or config ownership is ambiguous.
- Work needs capability/trust expansion, protocol guessing, unsafe credential handling, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
