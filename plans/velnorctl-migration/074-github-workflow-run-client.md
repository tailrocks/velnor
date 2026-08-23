# Plan 074: Build the canonical GitHub Actions client and run merge service

> **Executor instructions**: Build GitHub API/domain services only. Tasks
> C034–C042 own individual `run` commands.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-tools/src/main.rs crates/velnor-runner/src/protocol.rs crates/velnor-client crates/velnor-model crates/velnor-control`

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: Plans 065, 066, 068
- **Category**: migration, architecture
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

GitHub owns workflow runs, jobs, steps, queues, cancellations, reruns, dispatch,
artifacts, and completed logs. Velnor only enriches those records with local
placement, timing, events, and infrastructure categories.

## Scope

- paginated GitHub runner-group/registry, run/job/step/queue/artifact/log reads
- read-only permissions/rate-limit checks and explicit JIT permission
  `UNPROVEN` state when no non-mutating endpoint exists
- watch/retry/rate-limit/version handling
- cancel, rerun, dispatch, artifact download metadata, run URLs
- correlation with Velnor job/slot history without authority inversion
- safe bounded artifact extraction and exact workflow-dispatch response run ID

No CLI handlers, scheduler, workflow execution/parser, local-kill cancellation,
or movement of unrelated maintainer audits/comparisons.

## Steps

1. Define GitHub-owned run/job/step/artifact/queue DTOs separately from local
   resources.
2. Extract authenticated API operations from maintainer tooling where reusable.
   At implementation, verify and centralize the latest stable GitHub API version,
   accepted media/content types, bounded redacted error bodies, pagination,
   Actions read/write permission matrix, attempt-aware log endpoints, and rate
   limits. GET/pagination retries are bounded; non-idempotent dispatch/rerun
   requests are never blindly retried after ambiguous transport loss. Reconcile
   authoritative state when possible or return typed `Transport/Ambiguous`.
3. Implement deterministic local/GitHub correlation and attempt handling.
4. Implement mutations with GitHub terminal confirmation and broker-driven
   local cancellation observation. Consume the workflow-dispatch HTTP 200 body
   and its exact run ID/URLs; prohibit before/after list-difference inference.
   Cancel is idempotent only after authoritative state inspection.
5. Stream artifacts into mode-restricted temporary files with compressed,
   expanded, entry-count, nesting, and expansion-ratio caps. Verify available
   metadata digest; reject traversal, duplicate normalized or case-folded paths,
   unsafe modes/types, symlinks, no-follow ancestor violations, corruption, and
   unintended overwrite. Promote atomically only after full validation and
   clean every failure. An explicitly selected unsupported/non-ZIP artifact
   fails typed; it is never silently skipped.
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
