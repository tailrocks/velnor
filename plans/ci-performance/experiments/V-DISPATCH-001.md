# V-DISPATCH-001: propagate actual dispatched verification outcomes

Status: diagnosis and design pending independent challenge. No experiment
completed, no implementation accepted, no performance claim.

## Structural cause

Both IR renderers model nightly verification as a dispatch request rather than
a verification obligation. `render_nightly_dispatcher` returns after
`gh workflow run`; its alert depends only on the synthetic failure job.
Thus a successful request can conceal a failed child. Historical Parallax
dispatcher 35430906774 succeeded while child 35430912434 failed policy.

Provider selection has a separate defaulting defect: supported providers seed
dispatch defaults, even when automatic execution selects GitHub only.
`apply_provider_generation_config` preserves that mismatch unless consumers
explicitly declare dispatch defaults.

## Alternatives for review

1. Dispatch through the versioned REST API, retain its exact returned run ID,
   verify the child's workflow/event/ref/source/attempt, await terminal status,
   and propagate the real conclusion. Preserve dispatch cache-save semantics.
2. Replace dispatch with a reusable workflow call. Investigate whether this
   preserves the required trusted event, cache-save authority, and status graph
   before accepting it; it changes the execution contract.
3. Correlate a separate `workflow_run` observer. This avoids occupying a runner
   while waiting, but requires explicit identity, trust, stable required-result,
   and missing-event handling. A successful dispatcher remains insufficient.

The first offers a bounded repair; the third may improve waiting overhead.
Neither may select a child merely by newest run or matching display name.

## Current primary API evidence

The [workflow dispatch API](https://docs.github.com/en/rest/actions/workflows#create-a-workflow-dispatch-event)
with `X-GitHub-Api-Version: 2026-03-10` returns HTTP 200 with
`workflow_run_id`, `run_url`, and `html_url`. It requires Actions write access.
The [GitHub announcement](https://github.blog/changelog/2026-02-19-workflow-dispatch-api-now-returns-run-ids/)
also documents explicit return details for older API versions.

Use the exact ID, bounded polling, explicit terminal-result handling, and
retained raw responses. Resolve branch movement explicitly; a dispatched run
must be attributed to its actual source, never the caller's assumed SHA.
Do not retry a POST blindly after an ambiguous network failure.

Scheduled selection should follow automatic provider policy. Manual defaults
and explicit overrides need separate tests, including an explicit default
different from automatic selection. Keep supported providers available.

## Acceptance and dependencies

Before implementation, independently challenge event semantics, cancellation,
timeouts, permissions, default provider behavior, and alert/result propagation.
Tests must cover success, failure, cancellation, missing or malformed child
identity, timeout, API failure, and provider overrides. Real CI must demonstrate
actual child completion, with products and required gates validated.

Local `gh` authentication currently returns HTTP 401. SSH pushes work.
A reviewed generated controller using a scoped Actions token is a feasible
alternative for controlled dispatch measurements, not a proven implementation.
It must constrain workflow/ref/inputs, avoid publishing, prevent recursive
dispatch, and retain every attempted experiment. This capability investigation
remains open; the local token failure does not prove all dispatch impossible.
