# Command Task C061: Implement `velnorctl auth check`

> **Executor instructions**: Implement only `velnorctl auth check`. Do not combine
> sibling commands. Run every gate; update this task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-runner/src/protocol.rs crates/velnor-control crates/velnor-client crates/velnorctl`
> Compare live state before edits; stop on incompatible drift.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: HIGH
- **Depends on**: Plans 067, 068, 074
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Actively verify GitHub/control authorization through non-mutating operations.

## Current state

GitHub credentials and endpoint access are validated indirectly during configuration/daemon operation. Plan 068 provides read-only credential-source and permission probes.

## Scope

Implement parser, typed arguments, thin handler, output/errors, help/completion, and tests for only `velnorctl auth check` in `crates/velnorctl/src/commands/auth_check.rs` and `crates/velnorctl/tests/auth_check.rs`.
Use Plan 068 services and other declared dependencies. Never spawn old binary or expose source file formats as API.
Inspection/check behavior is read-only, versioned, redacted, and uses standard output/exit conventions.

## Required behavior

- Support `--github-scope`; test GitHub API, runner-group read, workflow run/log
  read, and rate limits through Plan 074.
- Report JIT-generation permission as `UNPROVEN` when GitHub exposes no
  non-mutating permission endpoint. Do not create a JIT configuration or runner
  registration. A future active canary requires a separately approved explicit
  mutating flag and exact returned-ID cleanup contract.
- Map connectivity/auth/permission/rate-limit failures distinctly.

## Steps

1. Add exact typed Clap syntax for `velnorctl auth check`; reject unknown/sibling flags.
2. Call one typed service method and map invalid/auth/connectivity/timeout/domain failures to stable non-zero exits.
3. Render redacted human/machine output; keep warnings stderr and resources stdout.
4. Add parser, service, transport/file-safety, output, exit, redaction, and idempotency tests under `command_c061`.
5. Update command docs, completion/man metadata, and migration mapping; add no alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c061` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Cancel all old active runs, delete only stale validation-owned registrations, and prove clean before dispatch.
Run check before fixture registration and prove the runner-registration set is
byte-for-byte unchanged, then fresh success/list/log checks work; test an
insufficient read-only credential and the explicit `UNPROVEN` JIT result.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Save sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl auth check --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and redaction.
- [ ] No sibling command, old alias, inline credential, or direct layout parser exists.

## STOP conditions

- Required service/authority is absent or config ownership is ambiguous.
- Work needs capability/trust expansion, protocol guessing, unsafe credential handling, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
