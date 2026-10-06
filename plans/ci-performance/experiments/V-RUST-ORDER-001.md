# V-RUST-ORDER-001: Rust validation order

Status: **candidate implemented; CI performance acceptance pending**.
Date: 2026-09-20.

## Baseline contract

Both `crates/velnor-workflow/src/scan/rust.rs` and
`crates/velnor-workflow/src/s2/scan/rust.rs` emit, in order:

1. `cargo fmt --manifest-path Cargo.toml -- --check`
2. `cargo nextest run --locked --all-features [package] --no-tests pass`
   (or `cargo test` when nextest is unavailable)
3. `cargo clippy --locked --profile test --no-deps --all-targets
   --all-features [package] -- -D warnings`

The current comment says nextest precedes Clippy because test-profile Rust
artifacts can be reused by Clippy. Cargo's profile documentation confirms
that `--profile test` selects the test profile, while nextest builds test
executables and metadata. Clippy's documented `--no-deps` mode still invokes
the compiler through clippy-driver; its output is not a runnable nextest
product. Sources: [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html),
[`cargo clippy`](https://doc.rust-lang.org/cargo/commands/cargo-clippy.html),
and [nextest running](https://nexte.st/docs/running/).

The historical rationale is a source comment introduced with the schema-2
scanner in commit `eb0303a34c9e3c9a0ede921b810158b5a34e34a8`; no separate
benchmark or failure-order evidence was recorded there.

## Controlled fixture

Fixture: `/tmp/velnor-rust-order-fixture-20260920`, a lockfile-backed Rust
library, binary, and two integration tests. Commands retained the generator's
flags, including `--profile test`, `--all-targets`, `--all-features`,
`--no-deps`, and nextest `--no-tests pass`. Five fresh target directories per
sequence were used. This is a mechanism test, not a Velnor CI performance
claim.

Successful sequence medians (five repetitions):

| sequence | total wall median | range | compiler observation |
| --- | ---: | ---: | --- |
| fmt → nextest → clippy | 1.162 s | 1.143–1.204 s | nextest compiled once; Clippy only checked |
| fmt → clippy → nextest | 1.164 s | 1.149–1.239 s | Clippy checked first; nextest compiled afterward |

The order change preserved successful total work within fixture noise, while
moving the test-profile compile after Clippy. This supports the existing
artifact-reuse rationale but does not prove equality on large workspaces.

A second fixture revision added a real Clippy warning (`return 1`). Five fresh
runs per sequence reached the failure at:

| sequence | time to Clippy failure | range | tests executed |
| --- | ---: | ---: | --- |
| fmt → nextest → clippy | median 1.780 s | 1.483–8.984 s | yes |
| fmt → clippy → nextest | median 0.429 s | 0.420–0.453 s | no |

The failed-order result is a clear fail-fast improvement. The successful
fixture result shows the cost risk: Clippy does not produce the nextest test
executable, so reversing order can add a test-profile compilation. The tiny
fixture has high startup noise and cannot estimate Velnor's absolute gain.

## Alternatives

1. **Retain current order.** Keep fmt → nextest → Clippy. This minimizes
   successful compatible work under the measured profile-sharing behavior.
   It delays actionable lint feedback by the complete test execution time.
2. **Reverse within one job.** Use fmt → Clippy → nextest. This gives the
   earliest lint failure while preserving all commands and flags, but accepts
   a possible second test-profile compilation on successful runs. Validate
   with at least ten fresh successful Velnor Rust cohorts and deliberate lint
   failures before accepting.
3. **Parallelize after formatting.** Run Clippy and nextest as separate jobs
   after one formatting/preparation gate. This minimizes user-visible
   failure latency without pretending Clippy output is a test product, but
   adds runner, cache restore, and queue overhead and increases aggregate
   work. Compare end-to-end, billed, and artifact-transfer time.
4. **Build/archive product graph.** Produce a validated `cargo nextest
   archive` once, then execute it in consumers while Clippy runs in parallel.
   The archive needs matching source revision, fixtures, native runtime
   libraries, and nextest version; it cannot replace Clippy. This is the
   architectural option for build-once/run-many, not a flag-only reorder.

The candidate preserves
the package selector, lockfile flag, features, test profile, warning policy,
test-less `--no-tests pass` behavior, and both scanner copies. Record compile
line counts and Cargo timing reports, not just wall time.

## Candidate implementation

Both scanners now emit fmt, Clippy, then nextest (or the existing Cargo test
fallback). The command multiset, selectors, features, profile, lock behavior,
warning policy and empty-test handling remain unchanged. The structural cause
was scanner-owned ordering justified only by an unmeasured reuse assumption;
fixing both scanner constructors propagates the order through regeneration.
This does not yet expose separate Actions steps; that requires the typed-stage
contract tracked in V-STAGES-001.

Scanner tests assert exact order and command identity for both test backends.
An executable generator fixture parses generated runtime TOML for both schemas
and providers, and preserves explicit workspace-check plus Mise-task overrides.
The first isolated full run found old tests asserting the previous order:
1,384 passed, one failed, 534 were not run after fail-fast. Both corresponding
ordering assertions were updated, retaining their empty-test and profile checks.
The complete isolated rerun passed all 1,919 tests across 21 binaries;
all-target, all-feature Clippy passed.

Independent fixture review is recorded in `../reviews/rust-order-review.md`.
Its green-path replay measured 1.373 versus 1.440 seconds on a tiny direct-Cargo
fixture; its lint-failure replay measured 1.447 versus 0.325 seconds. These are
mechanism observations with host-load confounding, not Velnor or MBX performance
acceptance. Real successful-work and failure-feedback comparisons remain open.

## First real CI observations

Candidate `95432856`, run `35492230871`, passed, with 68 recorded jobs in
604 seconds from trigger to last completion, with 2,734 seconds aggregate
execution. Prior fixture revision `bb94bac9`, run `35491248265`, passed in
592 seconds, with 2,705 seconds aggregate execution. Both independent policy
runs failed the existing candidate/pin identity path. These are single,
non-alternated observations, not an accepted improvement.

The collector check interval was approximately 14 seconds before and
21 seconds after. Its actual compiler archive lookup reported no MBX cache;
Cargo/tool layers were warm. The candidate's reporter nevertheless labeled
MBX as `prefix`, a separate defect tracked in V-MBX-TELEMETRY-001. Both runs
reported 104 operations not looked up, one hit, zero misses and five bypasses
across their command summaries. A zero-miss count does not prove reuse.

Exact timestamped excerpts are retained as
`velnor-35491248265-collector-order-cache.txt` and
`velnor-35492230871-collector-order-cache.txt` in the observations directory.
The apparent regression requires controlled repeated observations, including
command intervals and cache state. Planning-job rerun feasibility is tracked
in V-RERUN-MEASUREMENT-001. Neither unrelated subsequent commits nor repeated
same-SHA runs can alone establish normal source-edit performance.
