# Independent renderer/bootstrap review

Reviewer: failure_inventory. Inspected current main 97bac4c4 plus `/tmp/velnor-integration/pin-architecture.md`; no implementation diff existed at review time. Design is conditionally sound, not yet verified implementation.

## Proven architectural failures

1. `s2/policy.rs:448-477` makes Candidate render success depend on PR/mainline event. The same source content can pass PR and fail main. Actual Preview failures35520255573/35522583904 exhibit old-pin render mismatch.
2. `s2/mod.rs:4636` only searches ci-pr.yml pull_request runs at HEAD_SHA. Main merge SHA has no such run. Actual main35520255650 waits900seconds then fails; main35522584031 also just became red16:39:24Z (exact latest log pending).
3. setup-velnor-workflow/action.yml only consumes published attested release products; ci-runtime-products producer runs after merge. Thus merely changing pin to an unpublished source commit inside PR creates a bootstrap cycle.

## Approved direction, required conditions

Use existing pin configuration, source commit A then generated-output commit B in the same PR, and one read-only owned-repository root producer before any behavior needing that source renderer. Exact pin renderer must match generated tree on both PR and prospective main; remove event-conditioned Candidate waiver. Published products remain fast path.

- Owner discovery must come from typed repository/source identity, not hardcoded Velnor names. Foreign consumers remain published-product only; unavailable external source is not permission to compile arbitrary code with credentials.
- Root producer must support *every* consuming entry graph: PR, main, merge_group, Preview, release, scheduled/dispatch paths where applicable. A root job only in ci-main does not prevent simultaneous Preview from failing while runtime-products publishes. Cross-workflow waiting is not a reliable dependency.
- Build exact pin source in clean isolated checkout. Bind complete closure, explicit feature/profile, target/platform, repository, source revision, run ID/attempt, artifact identity and binary digest. Existing debug candidate closure uses default `tui`, whereas release runtime closure uses no features; these cannot share cache identity or silently substitute. `expected_closures` permits supported variants, but manifest must state the variant actually compiled.
- Builder has read-only contents and no publication credentials; checkout persist-credentials false. Clear GH_TOKEN/GITHUB_TOKEN and unrelated credential/wrapper/environment inputs before candidate execution. Candidate must not run inside pull_request_target with broad token/secrets. Current old-policy path has only contents/actions read and explicitly blanks tokens for closure probe; retain at least this boundary.
- Consume artifact produced by eligible same run/event/source, or explicitly authorized same-repository PR run for the existing trusted policy adapter. Match exact full closure, not artifact name's16hex prefix; reject expired artifacts, ambiguous duplicates, stale attempts, invalid manifest/profile/platform/source and tampered bytes before executable probes.
- Treat *only explicit product absence* as source-build eligibility. Current `if ! gh release download` conflates404 with auth/network/partial-download failures. New selector must distinguish absence before choosing build. Digest/attestation/provenance/cached-product failure is terminal and must not switch to compilation.
- Keep locally compiled candidate artifacts/cache separate from attested published-runtime cache. A digest paired with a mutable manifest does not establish publication provenance. Never promote candidate to product cache merely because `--closure` self-report matches.
- Verify outputB has unchanged generator closure from pinA and exact render, including scan-input state. Merge SHA may differ harmlessly when closure/tree equal; history-sensitive release identity still needs its independent preflight.
- Final gates need explicit root-producer/planner/unit dependencies and always() verdict. Missing/skipped bootstrap must not be nonapplicable by accident.

## Required adversarial/regression checks

Cold unpublished owned source succeeds; external missing source fails; explicit404 permits owned build while403/5xx/bad signature/hash do not; altered artifact manifest/source/profile/platform/run/attempt rejected before exec; sourceA→outputB closure equality succeeds; stale old pin with candidate-only matching output fails on PR and main; missing candidate fails promptly; all relevant event graphs include producer edges; Preview starting concurrently with runtime publication succeeds without race; no candidate compilation occurs in privileged publisher.

## Limitation

This review establishes direction and constraints from current source and real logs. Implementation, exact trusted-adapter compatibility, rendered graph, cold/warm timings and negative execution tests still require review. Do not label the architecture implemented or green based on this memo.
