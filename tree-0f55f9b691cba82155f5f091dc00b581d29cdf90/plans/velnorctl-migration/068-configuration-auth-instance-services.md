# Plan 068: Extract configuration, authentication, and instance services

> **Executor instructions**: Build reusable services only. Do not add Clap
> handlers; command ownership lives in `commands/C051` through `C065`.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/config.rs crates/velnor-runner/src/runner.rs crates/velnor-runner/debian crates/velnor-control crates/velnor-model`

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
- bounded sanitized local context-change journal (actor source, old/new context,
  timestamp only), distinct from daemon operational events
- reusable local instance configure/install/apply/delete planning and systemd
  operations; GitHub target adapters arrive in Plan 074
- desired/local/systemd drift calculation; Plan 069 merges GitHub observations

No CLI commands, remote HTTPS transport, dynamic scaling, or capability change.

## Steps

1. Define versioned config, context, auth-report, drift, and instance-operation
   types in `velnor-model`.
2. Extract config merging with one total low-to-high order: built-in defaults,
   persisted context defaults, instance file, captured systemd startup
   environment, captured process startup overrides, then explicit command
   overrides. Define per-field allowed sources, unset versus explicit empty,
   collection replace/merge behavior, conflict errors, and credential-reference
   restrictions. The daemon captures resolved provenance at startup; clients do
   not reconstruct it from `/proc`. Record source for every effective field and
   redact secrets before serialization.
3. Define credential-reference and sanitized cached-auth report services plus
   the required GitHub check specification. Plan 074 alone implements GitHub
   REST operations for group, registry, workflow, log, rate-limit, and
   permission reads. JIT generation is mutating and is never described as a
   read-only probe: report its permission `UNPROVEN` when GitHub exposes no
   non-mutating endpoint. Any future active canary needs separate explicit
   approval, a unique validation prefix, exact returned-ID cleanup in `finally`,
   deletion readback, audit, and orphan recovery.
4. Extract configure/remove and systemd installation/application primitives
   into idempotent local services. Separate planning from mutation; make partial
   failure observable and preserve protected credentials during delete. Define
   GitHub registration operations as injected ports only; Plan 074 implements
   them and command tasks own end-to-end mutation.
5. Add service and transport tests for full pairwise precedence, unset/empty and
   collection rules, file modes, symlink safety,
   redaction, drift, repeated apply/delete, and failed GitHub operations.

**Verify each step**: focused `rtk cargo nextest run --workspace --locked` tests
pass. Final gate: `rtk mise run check` exits 0.

## Mandatory fixture integration

Against pinned `tailrocks/velnor-actions-fixture`, cancel only older
pending/in-progress validation runs owned by this iteration; never cancel
protected `Release`, `Package update`, `Publish apt repo`, or
`workflow_dispatch` release workflows/runs, or unrelated runs. Remove only stale
validation registrations. Produce and repeat a dedicated
instance plan through the new service, then run a fresh success through the
unchanged existing daemon and monitor only its ID at intervals no longer than
60 seconds. Prove effective config/provenance is redacted and stable. End-to-end
apply/delete, GitHub drift, and active auth probes are proved after Plan 074 by
C053, C061, and C064–C065.

## Done criteria

- [ ] Command tasks C051–C065 can call typed services without parsing files.
- [ ] Secret values never enter config/auth/API output.
- [ ] Local plan/apply/delete primitives are idempotent; GitHub mutation remains
      behind an unimplemented injected port until Plan 074.
- [ ] `rtk mise run check` passes.

## STOP conditions

Stop if extraction requires widening credential visibility, accepting a new
GitHub capability, or changing systemd lifecycle semantics.
