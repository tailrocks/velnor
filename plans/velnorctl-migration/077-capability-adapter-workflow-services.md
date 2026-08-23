# Plan 077: Build capability, adapter, and workflow-check services

> **Executor instructions**: Expose current manifest truth only. Tasks
> C066–C073 own individual commands. No new capability is authorized.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/admission.rs crates/velnor-runner/src/action.rs crates/velnor-runner/src/cli.rs crates/velnor-tools crates/velnor-control crates/velnor-model`

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 064–069
- **Category**: migration, correctness
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

CLI inspection and runtime admission must query one compiled capability manifest
and native adapter registry. Static workflow checking must explain exact nested
ancestry without becoming an execution fallback.

## Scope

- manifest list/explain/check/export services
- adapter list/describe/check services
- transitive workflow/local-composite/nested-action compatibility analysis
- exact field/value/alternatives/ancestry/manifest-version violations

No CLI handlers, manifest mutation, adapter generation, remote JavaScript
fallback, newly accepted ref/input/value/combination/runtime, or side effect.

## Steps

1. Extract manifest query and validation results so admission and inspection call
   the same implementation.
2. Add versioned capability, adapter, violation, ancestry, and compatibility
   resources.
3. Extract transitive action-graph analysis from maintainer tooling where valid;
   detect cycles, depth, ambiguous refs, and unavailable proof.
4. Prove checking performs no checkout, container, cache, service, or credential
   side effect.
5. Test exact positives/negatives and equality between static result and runtime
   admission result.

**Verify**: capability nextest suites and `rtk mise run check` pass.

## Mandatory fixture integration

Run static checks against pinned `tailrocks/velnor-actions-fixture`, then dispatch
accepted cases and deliberate negative capability cases. Clean old state first;
monitor only new IDs every at most 60 seconds. Static/runtime violations must
match exactly and occur before all protected side effects.

## Done criteria

- [ ] C066–C073 share manifest/admission implementation.
- [ ] Fixture positives execute and negatives fail identically before effects.
- [ ] No capability expansion or unknown-action fallback exists.

## STOP conditions

Stop and request explicit operator approval if fixture proof needs a new
capability, ref, input, value, combination, runtime, or behavior.
