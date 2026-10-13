# V-ROLLING-LOOKUP-001 — rolling release lookup

Status: published as `27bfb54bdecce09a71f320885bb64f5492083744`; exact-head
PR and policy CI pass. Final independent review and consumer/package validation
remain pending. No measured optimization acceptance or completed iteration credit.

## Root cause and alternatives

PR [#973](https://github.com/tailrocks/velnor/pull/973), inspected head
`5d3ec73f5424db4059906b2f834d27e0aa397572`, addresses rolling release recovery
and generated asset quoting. A tag lookup returning 404 does not establish that
an authenticated release listing has no matching draft. The existing boundary
conflates lookup transport with publication state. A pre-existing tag can be
reused only when it resolves to the already verified source commit.

1. Resolve the release through a complete authenticated listing on tag-lookup
   404, validate its full existing contract, then permit mutation. Chosen.
2. Reject every orphan tag and require manual recovery. Preserves refusal but
   leaves a verified exact-source recovery case unavailable.
3. Delete and recreate release/tag state. Rejected: destroys existing identity
   and can lose recovery information. Rollback retains a pre-existing tag and
   publication lock if safe recovery cannot be proved.

Portable asset names have a strict validated alphabet. Double quoting inside
rendered publication shell preserves the enclosing script syntax without
admitting expansion characters. Existing foreground verification and typed
process-group cancellation remain intact.

## Independent findings and intervention

The first adapted patch set `rolling_body_ready=1` after fetching the listed
release, then entered the HTTP-200 branch, which unconditionally overwrote that
body from the original 404 response. Parent review rejected it. A helper-only
fixture had missed the complete 404 → listing → validation transition.

The repaired transition consumes the already fetched body and tests valid
listed drafts, failed listing, ambiguous matches and failed validation before
mutation. Parent applied the compatible PR adaptation and separate transition
repair onto `413458df`; no stale whole-file replacement was used.

Final generated output, strict lint, complete tests and real CI remain required.
These local shell fixtures do not publish a release or prove production recovery.

## Deterministic integration evidence

Parent executed all 50 package-release tests successfully, retaining rollback
and process cancellation cases. Strict lint initially rejected the new
transition fixture size and a nested helper after statements. The independent
agent extracted named fixture helpers without dropping cases; parent strict
all-target/all-feature Clippy and formatting then passed. Actionlint passes.

S2 rendering revision advances 65 → 66. Velnor itself does not declare this
optional package-release primitive, so its regenerated workflow bytes remain
unchanged; ownership state records the new renderer and experiment file.
Jackin/Parallax package consumers must still regenerate with the exact tested
new runtime. Full-suite, exact committed build and real CI results follow.

Primary contract checked 2026-09-20: [GitHub release listing](https://docs.github.com/en/rest/releases/releases#list-releases)
includes draft releases for callers with push access and supports pagination.
The fallback requests every page; lookup errors and multiple matching releases
fail closed. No new production release was created for these tests.

Final parent suite: 1,952 tests passed, zero skipped. Publication awaits exact
committed generator verification; consumer runtime convergence and package CI
remain open obligations.

## Published revision evidence

Exact remote revision rebuilt with default features; generation check passed
against pinned runtime `4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d`.
[PR CI 35511815559](https://github.com/tailrocks/velnor/actions/runs/35511815559)
and [policy CI 35511814394](https://github.com/tailrocks/velnor/actions/runs/35511814394)
succeeded. Linux generator tests: 1,952 passed, zero skipped, including the
complete draft lookup transition. Trigger to required result: 559s; aggregate
execution: 1,359s. This is one observation of changed source and coverage, not a
controlled speedup or packaging/release-promotion measurement. Raw observations
are retained under `observations/velnor-35511815559-*`.
