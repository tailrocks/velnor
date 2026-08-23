# Command Task C075: Implement `velnorctl daemon`

> **Executor instructions**: Implement only `velnorctl daemon`. Do not combine
> sibling commands. Run every gate; update task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/main.rs crates/velnor-runner/src/runner.rs crates/velnor-runner/src/cli.rs crates/velnor-control crates/velnorctl systemd packaging`
> Compare current implementation and policy before editing; stop on drift.

## Status

- **Priority**: P1
- **Effort**: XL
- **Risk**: HIGH
- **Depends on**: Plans 067, 068, 072, 073, 076
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Run Velnor systemd worker/daemon and controlled one-job mode after old binary removal.

## Current state

Old `velnor-runner daemon` and standalone `run` own service execution. Complete removal requires service-only `velnorctl daemon`, with workflow `velnorctl run` reserved for GitHub run resources.

## Scope

Implement only `velnorctl daemon`: typed parser, thin handler/service entry, versioned output/errors, help/completion, and tests in `crates/velnorctl/src/commands/daemon.rs` and `crates/velnorctl/tests/daemon.rs`.
Use declared shared services. Never spawn old binary, parse sibling output,
duplicate admission/package-observation/masking logic, or expose internal layouts.
Apply explicit authorization/reason/timeout, audit, safe rollback, and destructive safeguards appropriate to exact command.

## Required behavior

- Service-only command supports selected instance and `--once`; top-level `run` stays GitHub workflow namespace.
- Own startup config, preflight, state/API, watchdog, JIT slots, broker, strict
  admission, execution, completion, teardown, signals, and graceful drain.
  Execute the dpkg-owned `/usr/bin/velnorctl` directly; never select or activate
  a Velnor-managed version at startup.
- Not an interactive operator shortcut; help/output never exposes credentials. Old daemon/run entrypoints disappear in Plan 079.

## Steps

1. Add exact typed Clap syntax for `velnorctl daemon`; use `ValueEnum` for closed choices and reject sibling flags.
2. Call one shared service entry and map invalid/auth/connectivity/rate-limit/timeout/domain failures to documented exits.
3. Render stable human and versioned machine output; warnings stderr, resources stdout, no credential material.
4. Add parser, service, transport, output, exit, redaction, and failure tests under `command_c075`.
5. Update command docs, completion/man metadata, and migration map; retain no old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c075` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before every dispatch, cancel all old pending/in-progress runs, delete only stale validation-owned registrations, and prove clean.
Run full pinned fixture cold/warm/unchanged, hold/cancel, lifecycle, artifact/log/cache/service/container cases through source-built daemon and `--once`; prove systemd/watchdog/SIGTERM semantics.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Store sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl daemon --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and authority.
- [ ] No sibling command, legacy alias, secret leakage, or direct layout parser exists.

## STOP conditions

- Required authoritative service behavior is missing.
- Work needs unapproved capability/trust/protocol/package-management expansion,
  local package bypass, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
