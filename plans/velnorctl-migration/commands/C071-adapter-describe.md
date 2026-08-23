# Command Task C071: Implement `velnorctl adapter describe <action>`

> **Executor instructions**: Implement only `velnorctl adapter describe <action>`. Do not combine
> sibling commands. Run every gate; update task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/manifest.rs crates/velnor-runner/src/admission.rs crates/velnor-runner/src/action.rs crates/velnor-runner/src/cli.rs crates/velnor-tools crates/velnor-control crates/velnorctl`
> Compare current implementation and policy before editing; stop on drift.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: Plans 065, 077
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Describe one native adapter contract and evidence.

## Current state

Runtime admission and native adapter data exist, but inspection is limited to old job-dump checking and maintainer audits. Plan 077 creates one no-side-effect query/analyzer service.

## Scope

Implement only `velnorctl adapter describe <action>`: typed parser, thin handler/service entry, versioned output/errors, help/completion, and tests in `crates/velnorctl/src/commands/adapter_describe.rs` and `crates/velnorctl/tests/adapter_describe.rs`.
Use declared shared services. Never spawn old binary, parse sibling output,
duplicate admission/package-observation/masking logic, or expose internal layouts.
Keep operation read-only, side-effect-free, redacted, and consistent with standard output/exit rules.

## Required behavior

- Show supported/unsupported refs/inputs, values/combinations, semantic/effect
  differences, and trust/storage/network behavior only from enforced canonical
  manifest metadata. Never infer history/test coverage/introduced version;
  omit or mark unavailable when absent.
- Return ambiguity/unknown errors with accepted names.
- Read-only; no action download or execution.

## Steps

1. Add exact typed Clap syntax for `velnorctl adapter describe <action>`; use `ValueEnum` for closed choices and reject sibling flags.
2. Call one shared service entry and map invalid/auth/connectivity/rate-limit/timeout/domain failures to documented exits.
3. Render stable human and versioned machine output; warnings stderr, resources stdout, no credential material.
4. Add parser, service, transport, output, exit, redaction, and failure tests under `command_c071`.
5. Update command docs, completion/man metadata, and migration map; retain no old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c071` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before every dispatch, cancel all old pending/in-progress runs, delete only stale validation-owned registrations, and prove clean.
Describe each adapter exercised by fresh fixture compatibility run and compare exact observed inputs/ref with documented contract.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Store sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl adapter describe --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and authority.
- [ ] No sibling command, legacy alias, secret leakage, or direct layout parser exists.

## STOP conditions

- Required authoritative service behavior is missing.
- Work needs unapproved capability/trust/protocol/package-management expansion,
  local package bypass, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
