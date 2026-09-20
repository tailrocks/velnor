# Parallax inventory

Bounded read-only inventory of `tailrocks/parallax`, completed 2026-09-20.

## Revisions and open PRs

- Checkout: `/Users/donbeave/Projects/tailrocks/parallax-project/parallax`.
- Clean `main...origin/main`; work/default SHA:
  `6a12bf47a816b63e848b563aaa45ef9694159c79`.
- [PR #109](https://github.com/tailrocks/parallax/pull/109) head
  `93cee3556b3bf08c9ba38a43aace43312c233c0a` (merge ref used by its
  [historical run](https://github.com/tailrocks/parallax/actions/runs/35300721965):
  `17e5dd138477b8e3dfc6c9d7c51e2e8e26a45a62`).
- [PR #110](https://github.com/tailrocks/parallax/pull/110) head
  `de3fd1c7d347ec8cc98bf4f811b73ec639506236`.
- [PR #111](https://github.com/tailrocks/parallax/pull/111) head
  `6fcc10be6409e73cfb5e9eeb7eb48dbaadbec05d`.

## Products and workflow contract

Parallax is a Rust 2024 workspace with a Bun 1.3.14 TypeScript/React/Vite UI
and two Docker products. Rust units run locked fmt, nextest with all features,
and clippy with all targets and `-D warnings`. The UI unit runs frozen Bun
install, lint, typecheck, build, and test. Current generated `depends_on`
metadata is admission/summary data; unit callers still depend only on `plan`,
so it does not establish execution ordering.

The current release script builds `ui/dist/client` before Cargo's `embed-ui`
build. Current `main` has no server build script. PR #109 adds
`crates/parallax-server/build.rs`, which runs Bun install/build whenever
`embed-ui` is enabled and asserts `_shell.html`. The affected embed-ui
consumers therefore run this hidden build in their isolated Cargo builds. The
inspected #109 generated config adds a `bun-ui` unit and Bun watch/tool inputs,
but contains no
`products`, `prerequisites`, or `embedded-ui` declarations despite the PR body
claiming a typed graph.

Current trust/gates: GitHub `ubuntu-26.04`, Velnor self-hosted labels,
GitHub/Velnor lanes, fork admission, `pull_request_target` policy with
`contents: read`, required contexts `DCO`, `Policy`, and `ci-required`, and
release disabled fail-closed. Nightly is `17 3 * * *`; maintenance is
`31 3 * * *`.

## Historical evidence

Run [`35300721965`](https://github.com/tailrocks/parallax/actions/runs/35300721965)
(PR #109 merge) failed. Server [job
`105794536030`](https://github.com/tailrocks/parallax/actions/runs/35300721965/job/105794536030)
ran for
636 seconds by job timestamps; its structured report said total 631 seconds,
checks 612, setup 3, tool bootstrap 13, cache prep 3, cargo fetch 0. Rustup,
mold, and Cargo caches were exact; MBX was cold. It passed 146 server tests,
then clippy rejected the new build script: two `expect()` on `Option`, one on
`Result`, and `parallax-server (build script)` failed. This is both a lint
failure and evidence that Cargo is hiding a duplicate UI product build inside
every all-features Rust job.

Nightly [scheduler run `35430906774`](https://github.com/tailrocks/parallax/actions/runs/35430906774)
succeeded only in dispatching child
[`35430912434`](https://github.com/tailrocks/parallax/actions/runs/35430912434).
The child plan succeeded, then policy failed before units:
Velnor `b9c3156c...` reported `.github/workflows/ci-policy.yml` differed from
its pinned generated render. No child unit timing exists.

CLI access failed with HTTP 401 (`Requires authentication`) for `gh api user`
and HTTP 403 (public API rate limit) for REST reads. Authenticated connector
GETs succeeded. No `updated_at` field was used as execution completion.

## Structural hypotheses and first experiments

1. UI is a product dependency hidden in Cargo build.rs. That prevents the CI
   graph from ordering/provisioning it, duplicates work, and exposes the build
   script to clippy. Represent the producer/consumer artifact explicitly, then
   compare one Bun build followed by Rust all-features checks against #109's
   build-script path. Keep all checks and coverage.
2. Reconcile generated policy with its pinned runtime first, rerun policy, and
   measure whether units start. Scheduler success is not child health.
3. For every observation, use raw job/step timestamps as baseline. Treat
   `VELNOR_CI_REPORT` as supplemental because marker/report telemetry can be
   incomplete or wrong. Do not call max job duration a critical path without a
   dependency graph; keep execution sum and pre-start latency separate.
