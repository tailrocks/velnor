# Command Task C074: Implement `velnorctl diagnostics bundle`

> **Executor instructions**: Implement only `velnorctl diagnostics bundle`. Do not combine
> sibling commands. Run every gate; update task and command index status.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/runner.rs crates/velnor-runner/src/slot_log.rs crates/velnor-runner/src/telemetry.rs crates/velnor-runner/src/job_message.rs crates/velnor-runner/src/storage.rs crates/velnor-runner/src/release.rs crates/velnor-runner/debian crates/velnor-control crates/velnorctl`
> Compare current implementation and policy before editing; stop on drift.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: HIGH
- **Depends on**: Plans 066–078
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Create bounded sanitized `.tar.zst` support bundle for selected instance/time range.

## Current state

Evidence remains distributed across config, systemd, Docker, state, storage,
logs, GitHub, and read-only Debian package data. Plan 078 provides allowlisted,
masked collection.

## Scope

Implement only `velnorctl diagnostics bundle`: typed parser, thin handler/service entry, versioned output/errors, help/completion, and tests in `crates/velnorctl/src/commands/diagnostics_bundle.rs` and `crates/velnorctl/tests/diagnostics_bundle.rs`.
Use declared shared services. Never spawn old binary, parse sibling output,
duplicate admission/package-observation/masking logic, or expose internal layouts.
Apply explicit authorization/reason/timeout, audit, safe rollback, and destructive safeguards appropriate to exact command.

## Required behavior

- Support `--instance`, `--since`, and `--archive <path>`; `--archive` avoids
  collision with global `-o/--output` format. Enforce safe overwrite/symlink
  rules.
- Include redacted effective config; compiled version; `apt-cache policy`,
  `dpkg-query`, bounded `dpkg -V`, and relevant bounded apt/dpkg history;
  systemd status; instance/slot/runner snapshots; Docker version and owned
  inventory; reservations/leases; lifecycle/registry/broker/daemon/warning
  logs; normalized events; timing SLOs; GitHub runner registry; and doctor
  results. Record explicit omissions. This is read-only evidence, not package
  management or a release resource.
- Exclude PATs, OAuth private keys, workflow secrets, endpoint authorization,
  raw job messages, signed URLs, live visitor/channel tokens, and rendered HTML;
  reuse production masking and self-scan archive.
- Collect typed allowlisted projections only—never raw environment, Docker
  inspect, unrestricted journald, or arbitrary paths. Enforce per-source/total
  caps and timeouts; assemble in `0600` temp storage with canonical archive
  metadata and no-clobber/no-follow output. Reopen, decompress, and scan every
  member before atomic publish; clean temporary state on every failure.

## Steps

1. Add exact typed Clap syntax for `velnorctl diagnostics bundle`; use `ValueEnum` for closed choices and reject sibling flags.
2. Call one shared service entry and map invalid/auth/connectivity/rate-limit/timeout/domain failures to documented exits.
3. Render stable human and versioned machine output; warnings stderr, resources stdout, no credential material.
4. Add parser, service, transport, output, exit, redaction, and failure tests under `command_c074`.
5. Update command docs, completion/man metadata, and migration map; retain no old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c074` and `rtk mise run check` pass.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before every dispatch, cancel all old pending/in-progress runs, delete only stale validation-owned registrations, and prove clean.
Run fixture masked-canary success/failure, collect active/completed bundles, verify expected evidence and byte-scan zero canaries/HTML; execution remains unaffected.
Monitor only new run IDs every at most 60 seconds; diagnose stasis before two minutes. Store sanitized non-HTML evidence only.

## Done criteria

- [ ] `velnorctl diagnostics bundle --help` exits 0 with exact syntax.
- [ ] Focused tests and full repository gate pass.
- [ ] Fresh fixture proof covers behavior, safety, and authority.
- [ ] No sibling command, legacy alias, secret leakage, or direct layout parser exists.

## STOP conditions

- Required authoritative service behavior is missing.
- Work needs unapproved capability/trust/protocol/package-management expansion,
  local package bypass, or fixture weakening.
- Two-minute fixture stasis cannot be diagnosed.
