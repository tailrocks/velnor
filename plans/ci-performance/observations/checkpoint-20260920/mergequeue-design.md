# Merge queue source contract — approved direction, implementation not started

Status: isolated snapshot `/tmp/velnor-mergequeue-source`, committed base0348729f775e650b37e14e364465a446d51e12a3 plus parent staged bootstrap source. No queue source edits yet; incremental queue patch empty. Frozen at parent request before implementation. Bootstrap snapshot patch saved separately in `/tmp/velnor-integration/mergequeue-snapshot.patch`; it is NOT a queue delta and must not be applied again to integrated source.

## Approved bounded change

Add `merge_group: {types: [checks_requested]}` to existing CI/Main trigger. Main already provides full planning, exact same-run runtime artifact, Policy, strict required aggregation and unique run-ID concurrency. Keep source SHA `github.sha` (queue integration commit), add `github.event.merge_group.base_sha` ahead of mutable branch fallbacks in plan/policy base expressions. Scope resolver already requires full scope for merge_group and rejects affected override. Existing schema2 event trust/provider admission recognizes merge_group; configured automatic provider set selects current owner hosted only. No publication or nightly alert jobs in CI/Main. Existing trusted cache save predicates explicitly exclude merge_group.

Do not add merge_group to standalone pull_request_target Policy: its base-validator/candidate PR-artifact protocol is separate. CI/Main's current-run Policy emits required `Policy` context with the existing app15368. Semantic tests must prove required check names unique within generated queue workflow, Policy depends on plan/runtime, exact queue head/base binding, selected failed/cancelled/skipped obligations fail, full scope cannot narrow, and unsupported/non-requested activity is not triggered.

Schema1 audit pending: its trigger tests explicitly omit merge_group and local trust gates differ. Must preserve provider modes and avoid a trigger that skips required controls. Prefer matching typed CI/Main event behavior in both schemas, with source tests and separate generator revision bumps as needed. No schema1 changes made.

## External required DCO evidence

Live check API for PR979 head0348729 confirms DCO app id974774, slug `dco-2`, conclusion success. Source is cncf/dco2, not older dcoapp/app (app1861). Primary source `dco2/src/dco/event/mod.rs` handles MergeGroup/ChecksRequested and publishes DCO success at `event.merge_group.head_commit.id`, relying on required PR DCO before queue admission. `dco2/src/github/event.rs` recognizes merge_group payload; server verifies webhook HMAC before dispatch. This establishes source capability; deployed service behavior still must be observed on a real group before calling rollout complete.

## Rollout

1. Commit source change, pin exact committed source, generate and prove no drift.
2. Merge trigger delivery under current strict required checks.
3. Only then enable queue and observe all required app-pinned contexts (ci-required, Policy, DCO) on actual group SHA; do not weaken required checks to make queue pass.

## Primary references

- <https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#merge_group> — checks_requested; GITHUB_SHA/ref identify merge group; required workflows need trigger.
- <https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue> — group combines latest base plus queued PRs; required checks must pass on group.
- <https://github.com/cncf/dco2/blob/main/dco2/src/dco/event/mod.rs> — process_merge_group_event.
- <https://github.com/cncf/dco2/blob/main/dco2/src/github/event.rs> — merge_group payload.

No settings mutations, commits, pushes, generated-file changes, or queue implementation edits by this agent.
