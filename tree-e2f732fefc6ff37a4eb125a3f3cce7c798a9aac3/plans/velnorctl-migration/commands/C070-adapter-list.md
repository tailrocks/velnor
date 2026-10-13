# Command Task C070: Implement `velnorctl adapter list`

> **Executor instructions**: Implement only `velnorctl adapter list`. Do not combine
> sibling commands. Run every gate; update task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/manifest.rs crates/velnor-runner/src/admission.rs crates/velnor-runner/src/action.rs crates/velnor-runner/src/cli.rs crates/velnor-tools crates/velnor-control crates/velnorctl`
> Compare current implementation and policy before editing; stop on drift.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: MED
- **Depends on**: Plans 065, 077
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

List native action adapters in installed Velnor.

## Current state

Runtime admission and native adapter data exist, but inspection is limited to old job-dump checking and maintainer audits. Plan 077 creates one no-side-effect query/analyzer service.

## Scope

Implement only `velnorctl adapter list`: typed parser, thin handler/service entry, versioned output/errors, help/completion, and tests in `crates/velnorctl/src/commands/adapter_list.rs` and `crates/velnorctl/tests/adapter_list.rs`.
Use declared shared services. Never spawn old binary, parse sibling output,
duplicate admission/package-observation/masking logic, or expose internal layouts.
Keep operation read-only, side-effect-free, redacted, and consistent with standard output/exit rules.

## Required behavior

- Show action identity, supported refs, native implementation, and only
  canonical manifest-backed descriptive evidence. Introduced version and
  fixture/test coverage are omitted or explicitly unavailable unless Plan 077
  makes them generated/signed canonical data.
- Use runtime registry; no remote action discovery/execution.
- Support standard output and selectors where modeled.

## Steps

1. Add exact typed Clap syntax for `velnorctl adapter list`; use `ValueEnum` for closed choices and reject sibling flags.
2. Call one shared service entry and map invalid/auth/connectivity/rate-limit/timeout/domain failures to documented exits.
3. Render stable human and versioned machine output; warnings stderr, resources stdout, no credential material.
4. Add parser, service, transport, output, exit, redaction, and failure tests under `command_c070`.
5. Update command docs, completion/man metadata, and migration map; retain no old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c070` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel only older pending/in-progress validation runs owned by this iteration; never cancel protected `Release`, `Package update`, `Publish apt repo`, or `workflow_dispatch` release workflows/runs, or unrelated runs. Delete only stale validation-owned registrations, prove both sets clean, and monitor only the new run ID.
List adapters required by pinned fixture, dispatch compatibility workflow, and prove every executed approved action maps to listed adapter.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Store sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl adapter list --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and authority.
- [ ] No sibling command, legacy alias, secret leakage, or direct layout parser exists.

## STOP conditions

- Required authoritative service behavior is missing.
- Work needs unapproved capability/trust/protocol/package-management expansion,
  local package bypass, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
