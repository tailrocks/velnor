# Plan 078: Build sanitized diagnostics collection service

> **Executor instructions**: Build bounded collectors and archive assembly only.
> Task C074 owns `velnorctl diagnostics bundle`.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/slot_log.rs crates/velnor-runner/src/telemetry.rs crates/velnor-runner/src/job_message.rs crates/velnor-control crates/velnor-model`

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: HIGH
- **Depends on**: Plans 066–077
- **Category**: security, observability
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Support evidence spans configuration, read-only Debian package state, systemd,
Docker, resources, storage, logs, events, timing, GitHub registry, and doctor
results. Collection must reuse production masking and never sweep arbitrary
host paths or invent a Velnor package-version model.

## Scope

- versioned bundle manifest, hashes, time bounds, omissions, redaction version
- allowlisted bounded collectors for every research-listed component
- shared structured/unstructured redaction
- secure temporary assembly and deterministic `.tar.zst`; command path uses
  `--archive` because global `-o/--output` is reserved for render format
- secret-canary/prohibited-content self-check

No CLI handler, automatic upload, telemetry enrollment, rendered GitHub HTML,
or raw job messages.

## Steps

1. Inventory sensitive fields and extract the exact production masking engine.
2. Implement typed collectors through services, with per-source bounds/timeouts
   and partial-failure manifest entries.
3. Implement safe temp/output paths, member allowlist, stable ordering/hashes,
   permissions, compression, and cleanup.
4. Add final archive scan for injected secrets, authorization canaries, and
   prohibited member types.
5. Test healthy/degraded sources, oversized logs, unavailable journald/GitHub,
   overwrite/symlink safety, permissions, redaction, and read-only behavior.

**Verify**: diagnostics nextest suites and `rtk mise run check` pass.

## Mandatory fixture integration

Use pinned `tailrocks/velnor-actions-fixture` with masked canaries, normal
artifact/log output, and controlled failure. Clean old state; collect active and
completed bundles; monitor only new IDs every at most 60 seconds. Extract and
scan every byte: no canary, authorization value, or `.html` member may exist.

## Done criteria

- [ ] C074 only selects options and calls this collector.
- [ ] Bundle includes all research-listed evidence or explicit omission.
- [ ] Secret/prohibited-content checks and fixture execution pass.

## STOP conditions

Stop if a collector needs unbounded filesystem access or masking cannot prove a
sensitive value absent.
