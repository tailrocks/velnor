# Plan 068: Extract configuration, authentication, and instance services

> **Executor instructions**: Build reusable services only. Do not add Clap
> handlers; command ownership lives in `commands/C051` through `C065`.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-runner/src/runner.rs systemd crates/velnor-control crates/velnor-model`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 064–067
- **Category**: migration, architecture
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Current `runner.json` mixes runner identity, GitHub endpoints, labels, update
policy, and credentials. Systemd environment and process overrides form other
configuration sources. Thin command handlers need one redacted, typed service
for effective configuration, contexts, authentication checks, and instance
lifecycle primitives.

## Scope

- typed desired/effective configuration and source provenance
- protected credential references; never credential values in API resources
- context persistence at `~/.config/velnor/config.toml`
- reusable instance configure/install/apply/delete operations
- desired/live/systemd/GitHub drift calculation

No CLI commands, remote HTTPS transport, dynamic scaling, or capability change.

## Steps

1. Define versioned config, context, auth-report, drift, and instance-operation
   types in `velnor-model`.
2. Extract config merging from built-ins, context, instance files, systemd
   environment, process environment, and explicit overrides. Record source for
   every effective field and redact secrets before serialization.
3. Extract GitHub credential checks, runner-group lookup, JIT-generation probe,
   run/log permissions, and rate-limit reporting behind read-only ports.
4. Extract configure/remove and systemd installation/application primitives
   into idempotent services. Separate planning from mutation; make partial
   failure observable and preserve protected credentials during delete.
5. Add service and transport tests for precedence, file modes, symlink safety,
   redaction, drift, repeated apply/delete, and failed GitHub operations.

**Verify each step**: focused `rtk cargo nextest run --workspace --locked` tests
pass. Final gate: `rtk mise run check` exits 0.

## Mandatory fixture integration

Against pinned `tailrocks/velnor-actions-fixture`, cancel old active runs, remove
only stale validation registrations, apply one dedicated instance through the
service, dispatch a fresh success run, and monitor only its ID at intervals no
longer than 60 seconds. Prove effective config is redacted, drift is empty, auth
checks pass, job succeeds, and delete preserves credential material.

## Done criteria

- [ ] Command tasks C051–C065 can call typed services without parsing files.
- [ ] Secret values never enter config/auth/API output.
- [ ] Apply/delete are idempotent and fixture execution is green.
- [ ] `rtk mise run check` passes.

## STOP conditions

Stop if extraction requires widening credential visibility, accepting a new
GitHub capability, or changing systemd lifecycle semantics.

