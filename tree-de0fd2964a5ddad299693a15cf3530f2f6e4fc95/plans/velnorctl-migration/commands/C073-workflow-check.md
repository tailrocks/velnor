# Command Task C073: Implement `velnorctl workflow check --repo <owner/repo> --ref <ref> --workflow <path-or-name>`

> **Executor instructions**: Implement only `velnorctl workflow check --repo <owner/repo> --ref <ref> --workflow <path-or-name>`. Do not combine
> sibling commands. Run every gate; update task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/manifest.rs crates/velnor-runner/src/admission.rs crates/velnor-runner/src/action.rs crates/velnor-runner/src/cli.rs crates/velnor-tools crates/velnor-control crates/velnorctl`
> Compare current implementation and policy before editing; stop on drift.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 068, 074, 077
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Analyze transitive workflow/action graph for exact Velnor compatibility before dispatch.

## Current state

Runtime admission and native adapter data exist, but inspection is limited to old job-dump checking and maintainer audits. Plan 077 creates one no-side-effect query/analyzer service.

## Scope

Implement only `velnorctl workflow check --repo <owner/repo> --ref <ref> --workflow <path-or-name>`: typed parser, thin handler/service entry, versioned output/errors, help/completion, and tests in `crates/velnorctl/src/commands/workflow_check.rs` and `crates/velnorctl/tests/workflow_check.rs`.
Use declared shared services. Never spawn old binary, parse sibling output,
duplicate admission/package-observation/masking logic, or expose internal layouts.
Keep operation read-only, side-effect-free, redacted, and consistent with standard output/exit rules.

## Required behavior

- Resolve exact repo/ref/workflow, all jobs, local composites, nested actions, cycles, and ancestry.
- Report exact blocker field/value/alternatives/manifest version and unprovable GitHub-expanded aspects.
- Never claim compatibility by approximation or execute/download unapproved action runtime.
- Resolve the supplied ref once to a commit SHA and fetch all workflow/action
  metadata at that SHA. Report the resolved SHA. Unresolved expressions,
  matrices, reusable workflows, cycles, or unavailable metadata yield
  `UNKNOWN` with nonzero exit; exact runtime parity applies only to fully
  canonicalized graphs.

## Steps

1. Add exact typed Clap syntax for `velnorctl workflow check --repo <owner/repo> --ref <ref> --workflow <path-or-name>`; use `ValueEnum` for closed choices and reject sibling flags.
2. Call one shared service entry and map invalid/auth/connectivity/rate-limit/timeout/domain failures to documented exits.
3. Render stable human and versioned machine output; warnings stderr, resources stdout, no credential material.
4. Add parser, service, transport, output, exit, redaction, and failure tests under `command_c073`.
5. Update command docs, completion/man metadata, and migration map; retain no old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c073` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before every dispatch, cancel all old pending/in-progress runs, delete only stale validation-owned registrations, and prove clean.
Check every pinned fixture workflow, dispatch positives, run negatives, and prove static/runtime decisions and pre-side-effect rejection match.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Store sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl workflow check --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and authority.
- [ ] No sibling command, legacy alias, secret leakage, or direct layout parser exists.

## STOP conditions

- Required authoritative service behavior is missing.
- Work needs unapproved capability/trust/protocol/package-management expansion,
  local package bypass, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
