# Plan 2026-09-13: fast-follow corrections for PR 707 (BuildKit sizing + bench driver)

Adapted from PR #725 onto origin/main tip b91f837d.

## (1) Budget math holes

(a) Partial observability dropped the static fallback for the unknown
dimension in `executor.rs` setup: any derived dimension suppressed the
whole static spelling, silently uncapping the unknown dimension (wider
than pre-PR, which always applied static policy). Fix: per-dimension
fallback — derived wins per dimension, static fills only unknown ones.
Total absence keeps the verbatim static spelling (flag order included);
partial budgets merge via `merge_buildkit_driver_opts`.

(b) The sizing/releasing job's declared limits were multiplied by the
holder count (`SlotBudget::buildkit_size(holders, declared)`), applying
one workflow's policy to strangers. Fix: sum per-holder entitlements.
Each claim records its job's own measured entitlement
(`own_buildkit_entitlement`: slot share narrowed by its own declared
limits); `buildkit_size_summed` adds the claim-file recordings and caps
at the host budget. Unknown holder dimensions, when unobservable at claim
time, contribute nothing; an all-unknown dimension falls back to static
policy. The releaser sums the *remaining* holders' recordings, so its own
limits never touch their ceiling. `builder_holder_count` is replaced by
`builder_holders_for_sizing`; claim documents without the entitlement fields
fail closed instead of silently entering sizing.

## (2) Sizing gaps

Existing daemons got no static fallback while new ones did, and partial
resizes left mixed-holder ceilings (one dimension for the current
holders, the other stale). Fix: the setup resize and the release shrink
use the same per-dimension static fallback as creation
(`BuildkitSize::with_fallback` + `static_buildkit_fallback`, which reads
daemon-wide `resource_options` only — never per-job workflow options —
and stays lenient where the create path is strict). Residual, documented
in `buildkit`/`host_budget` module docs and on `resize_builder_daemon`:
a dimension with neither a derived nor a static value keeps the daemon's
existing ceiling (creation omits it: unconstrained).

## (3) Bench driver honesty

- Trust-partition twins differed by label only. Fix: each twin runs in
  its own trust-scoped partition carrying the same workload files plus a
  round-tripped class marker, mounting only its own class's state.
- Persistent-host warmups retained nothing (workspace deleted,
  container and network removed), so job-100 equaled job-1 and the
  precondition note was false. Fix: each warmup retains its workspace
  and one stopped container, all owned and teardown-removed.
- After-gc's GC removed only synthetic churn (a built-then-removed
  image). Fix: the GC removes exactly the retained warmup state and
  reports what it freed (containers, workspaces, bytes).
- The 512MiB `DOCKER_GROWTH_ALLOWANCE_BYTES` comment claimed it was
  "stated here and on the record" but no record carried it. Fix: every
  `velnor-job` record notes the allowance.
- Soak counted prepare-retained objects as round residue, which would
  fail every retaining row. Fix: soak samples an owned-object baseline
  after prepare and the verdict counts only what rounds added
  (per-kind saturating subtraction, disclosed in the report notes).
- Scenario descriptions corrected: after-gc is a scoped GC, not "full
  disk and image garbage collection"; trust-partition no longer claims
  two records or checkout stages. `Driver::VelnorJob::observable_stages`
  narrows to the 10 locally observable stages (widens back when remote
  dispatch lands); README contract updated.

## (4) Tests

- `local_stages_validate` passed on base (subset-only validation).
  Strengthened with a zero-filled-broker-stage record that must fail
  `StageOutsideDriverCoverage`: verified it FAILS with the narrowing
  reverted and passes with it.
- The executor argv test recomputed its expectation from the same helpers
  on the live host (self-referential, host-dependent). Fix: a
  thread-local `TestBudgetGuard` pins `observe_host` to a synthetic
  16-CPU/16-GiB tree, and the test asserts the literal
  `cpu-period=100000,cpu-quota=400000,memory=12884901888` on the
  `buildx create` argv.
- New focused tests: summation with mixed declared limits (the leak
  regression), unknown dimensions contributing nothing, per-dimension
  fallback (`with_fallback` + driver-opt merge), static fallback reading
  daemon-wide policy only, entitlement round-trip through claims, soak
  baseline verdict.

## Proof

- `cargo fmt`, `cargo clippy -p velnor-runner -p velnor-bench
  --all-targets -- -D warnings` clean.
- 190 bench tests green; runner lib 1867 green with 2 failures in
  `checkout`/`git_mirror` that pass serially — the pre-existing parallel
  flakes 707 documented as reproduced on clean base, in untouched
  modules.
- Live `velnor-bench run` (iterations 3): trust-partition, job-2, and
  after-gc emit valid v2 records under the narrowed validation with the
  new precondition/allowance notes; after-gc reports "2 owned
  container(s) and 2 warmup workspace(s) (76 bytes)"; zero owned-object
  residue (containers/networks) after all three.
