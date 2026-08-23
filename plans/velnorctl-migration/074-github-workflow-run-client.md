# Plan 074: Build GitHub workflow-run client and merge service

> **Executor instructions**: Build GitHub API/domain services only. Tasks
> C034–C042 own individual `run` commands.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-tools/src/main.rs crates/velnor-runner/src/protocol.rs crates/velnor-client crates/velnor-model crates/velnor-control`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: Plans 066–070
- **Category**: migration, architecture
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

GitHub owns workflow runs, jobs, steps, queues, cancellations, reruns, dispatch,
artifacts, and completed logs. Velnor only enriches those records with local
placement, timing, events, and infrastructure categories.

## Scope

- paginated GitHub run/job/step/queue/artifact/log reads
- watch/retry/rate-limit/version handling
- cancel, rerun, dispatch, artifact download metadata, run URLs
- correlation with Velnor job/slot history without authority inversion
- safe artifact path validation and exact newly dispatched run-ID detection

No CLI handlers, scheduler, workflow execution/parser, local-kill cancellation,
or movement of unrelated maintainer audits/comparisons.

## Steps

1. Define GitHub-owned run/job/step/artifact/queue DTOs separately from local
   resources.
2. Extract authenticated API operations from maintainer tooling where reusable;
   add pagination, retry, API-version, permissions, and rate-limit errors.
3. Implement deterministic local/GitHub correlation and attempt handling.
4. Implement mutations with GitHub terminal confirmation and broker-driven
   local cancellation observation.
5. Harden artifact destinations against traversal, duplicate entries, symlinks,
   corruption, and unintended overwrite.
6. Test API fixtures for every read/mutation and partial local-control outage.

**Verify**: client nextest suites and `rtk mise run check` pass.

## Mandatory fixture integration

Clean pinned `tailrocks/velnor-actions-fixture`; dispatch fresh success, hold,
cancel, failure, rerun, and artifact scenarios using new services. Monitor only
new IDs every at most 60 seconds. Prove GitHub conclusions remain authoritative
and Velnor placement/timing is enrichment only.

## Done criteria

- [ ] C034–C042 use one GitHub client and merge model.
- [ ] Cancel is GitHub-owned; artifacts extract safely.
- [ ] Fixture run lifecycle and attempts match GitHub exactly.

## STOP conditions

Stop if GitHub API lacks a claimed field or operation. Report it unavailable;
never approximate from local Docker state.

