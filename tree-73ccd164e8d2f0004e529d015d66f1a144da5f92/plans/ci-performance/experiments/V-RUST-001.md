# V-RUST-001: typed visible validation stages

Status: implementation held pending runtime bootstrap, consumer schema migration, and reviewer acceptance; fixture migration is green.

Hypothesis: the active schema-2 generator serializes Rust validation as opaque `pr_commands`/`full_commands`, so generated Actions exposes one `Run unit checks` step and cannot report format, lint, and test failures independently. A typed stage contract can expose the same exact commands without dropping required coverage or changing profile/feature flags.

Baseline: Velnor run 35477566106, Rust workflow job 105989415068, 274 seconds wall time; full trigger-to-required-result approximately 363 seconds. Historical evidence: `plans/ci-performance/observations/velnor-35477566106-{run,jobs-page1}.json` and <https://github.com/tailrocks/velnor/actions/runs/35477566106/job/105989415068>.

Candidate design under independent review:

- Active runtime fields become `[[unit.pr_stages]]` and `[[unit.full_stages]]`; old runtime command fields are removed, with strict unknown-field rejection.
- A stage has required stable `id`, closed `kind`, exact shell `command`, optional parsed `profile`, and explicit prior-stage prerequisites.
- The generator's source command vectors remain internal inputs. The Rust scanner/classifier derives stages deterministically after command transformations, preserving package, target, feature, lock, wrapper, and profile arguments verbatim.
- Runtime stage selection executes one validated stage per visible Actions step. Partial units retain one prerequisite execution; stage steps are gated by the selection plan so prerequisite checks do not run three times.
- Stage output appends to the existing unit log; checks start/end markers and non-zero status propagate through each split step.

Independent challenge questions:

1. Does the schema preserve every source command and exact `--profile test`/feature/target argument, including non-Rust units and lane/provider-independent behavior?
2. Does the split plan preserve affected-unit prerequisite obligations without duplicate execution or false green skipped stages?
3. Does Cargo/nextest reuse remain an experimentally measured choice rather than an inferred equivalence? Clippy metadata must not be treated as a runnable test executable.
4. Do generated stage names, runtime validation, logs, and failure status remain deterministic and fail closed for unknown/empty/duplicate stages?

Validation to record after implementation: focused Rust tests, generated TOML round trip through the runtime parser, generated workflow assertions, deliberate stage failure/exit propagation, and exact consumer regeneration only after parent review. CI and performance acceptance remain pending real runs and independent post-change verification.

## Review hold and bootstrap dependency

Independent review rejected rollout of the current command classifier as the
root architecture: stage identity must come from known scanner/task
constructors with explicit semantics, not arbitrary shell-token inference.
The review also requires removal of implicit Clippy-to-`cargo check` rewriting
and flag stripping from the stage path. Those changes are deferred from this
fixture-only pass.

The current pinned runtime cannot parse `pr_stages`/`full_stages`, and the
existing candidate producer runs after the first `plan` and unit checks. A
schema-changing generated tree therefore needs a candidate runtime before
planning. Three reviewed options are recorded:

1. Add a generator-bootstrap job before `plan`, build one candidate from the
   exact merge tree used by unit jobs, and fan out one verified immutable
   artifact to plan and all units. Retain separate PR-head candidate
   publication for `pull_request_target` when merge and PR-head closures
   differ.
2. Call a reusable runtime-bootstrap workflow before planning. Return exact
   artifact/run/closure identity as outputs, download and verify it in every
   consumer, and admit it to privileged policy only for same-repository,
   successful producer runs.
3. Promote an attested immutable runtime product in a first phase, then pin
   that exact revision and regenerate consumers in a follow-up phase. This
   preserves product-only consumers but requires an explicit candidate path
   before the product is released.

Every option must bind repository, workflow/run/attempt, producer conclusion,
source SHA, build SHA, canonical generator closure, project TOML hash/schema,
platform, profile/features, artifact digest, and binary SHA before execution.
The candidate must self-report the same closure and revision. Artifact name or
latest selection alone is insufficient.

Fixture migration result: all runtime fixtures now use typed stage tables;
`cargo test -p velnor-workflow --lib s2::runtime::` passed 60 tests.
