# Typed validation stages: independent review

Date: 2026-09-20

Scope: `plans/ci-performance/experiments/V-STAGES-001.md` and
`V-RUST-001.md`, current S2 scanner/runtime/renderer, and the staged runtime
rollout contract. This is a design review only. No source or generated
workflow change is accepted by this record.

## Verdict

**HOLD, proceed only with the constraints below.** The typed-constructor
direction is correct. The proposal is incomplete at the execution boundary:

* `CiUnit` still deserializes only `pr_commands`/`full_commands`, and
  `run_layers` still rewrites Clippy into `cargo/mbx check` while stripping
  `-D warnings` and `--no-deps` (`s2/runtime.rs:117-185, 2612-2678`). A stage
  migration must remove this path at the schema cutover; adding stage metadata
  beside it leaves the enabling bug class in place.
* A kind reusable is one static workflow shared by many units. Its current
  renderer collapses member facts into gated blocks and receives one dynamic
  `inputs.unit` (`s2/primitives/ir.rs:4484-4565, 4990-5257`). “One step per
  selected stage” cannot mean dynamic YAML. The generator must emit the union
  of possible stage/setup blocks and pass an explicit per-unit selected-stage
  plan. An unknown or absent selected stage must fail closed; it must never
  become an unconditional successful no-op.
* `StageCommandMatrix`, `modes`, `tools`, `consumes`, and `produces` are names
  without a complete serialized shape or identity rules. Optional parsed
  metadata cannot prove that the final command still carries package, target,
  feature, profile, lock, wrapper, and warning flags.
* Current tool setup is kind-level and runs before `Run unit checks`; it unions
  member needs (`s2/primitives/ir.rs:4990-5184`). Merely annotating stages does
  not move `nextest` or broad Mise installation after format/Clippy. The first
  accepted renderer must place stage-specific setup immediately before the
  stage that owns the tool closure, with verified reuse only after that setup.
* `needs` cannot imply a Cargo product. Same-job order is enough for a
  fail-fast sequence. A receipt/product edge is required only when a result or
  artifact is actually transferred across a job/unit boundary. Do not invent a
  Clippy-to-check or Clippy-to-nextest product relationship.

## Required typed boundary

Construct stages at known scanner/config constructors, after all effective
configuration is resolved, and before serialization. Do not classify shell
text. The current order that must be represented is:

1. scan Rust validation commands (`fmt`, `clippy`, test/nextest);
2. apply typed `[[unit]]` capabilities (`workspace_check`, named `ci_tasks`);
3. materialize declared product preparation and environment;
4. apply Docker seed/context transformations;
5. apply the typed Mr. Boxington command transform; and
6. validate tools, provider support, lockfiles, and stage graph before output.

The stage command stored in the new runtime schema is the exact final shell
command produced by that pipeline. Its constructor, rather than a parser,
owns the semantic kind. A command digest must cover the exact command,
scope, provider/lane, environment, and tool/product identities. If a
structured Cargo invocation is retained, render it once and compare the
rendered command with the serialized command; never parse the serialized
shell string to recover semantics.

The minimum stage record is:

```text
Stage {
  id: unit-scoped stable slug,
  label: deterministic display label,
  kind: closed enum from a constructor,
  scope: affected/full membership (or explicit per-scope command),
  command: exact final command plus execution shell contract,
  tools: locked typed requirements for this stage,
  needs: same-unit stage IDs only,
  consumes/produces: typed product refs only for real transfers,
  task: optional validated Mise task identity,
  command_digest: digest of all effective execution inputs,
}
```

The provider matrix must be explicit. Current S2 commands are provider
independent after MBX conversion, so a common command is valid; a future
provider-specific matrix must contain every admitted provider/scope pair or
fail generation. No missing-entry fallback to another lane is allowed.

Stage IDs are unit-local, unique, non-empty, and deterministic. Rust uses
stable IDs such as `fmt`, `clippy`, `test`/`nextest`, `check`, and explicit
product-preparation IDs. A named task uses a validated task slug with a
collision check. The plan/receipt identity still includes the unit ID, so the
kind reusable can union IDs without conflating equal task names from two
units. Duplicate task declarations either get an explicit ordinal identity or
are rejected; silently deduplicating work is forbidden.

## Exact Rust coverage that must survive

The current scanner is the authoritative fixture at `s2/scan/rust.rs:455-480`:

* `fmt`: `cargo fmt --manifest-path 'Cargo.toml' -- --check`;
* `clippy`: `cargo clippy [--locked] --profile test --no-deps
  --all-targets --all-features [--package or --manifest-path selector] --
  -D warnings`;
* test: `cargo nextest run [--locked] --all-features [selector] --no-tests
  pass`, or the exact `cargo test [--locked] --all-features [selector]`
  fallback when nextest is absent.

The generated unit may prepend a declared product preparation stage. A
`workspace_check` override replaces the normal Rust validation surface with
the explicit `cargo check --workspace --all-targets --locked` Check stage; it
must not leave the default fmt/Clippy/test stages silently active. Named
`ci_tasks` append Task stages in declaration order. Every affected/full scope
must preserve its exact stage sequence and command multiset after platform
resolution, Docker transforms, and MBX conversion.

Acceptance needs a before/after fixture that flattens the new stage plan and
compares it with the final pre-migration command vectors for every scope. It
must cover nextest and cargo-test fallback, locked and unlocked manifests,
workspace-check replacement, package/manifest selectors, all-features,
`--profile test`, `--no-deps`, `-D warnings`, MBX conversion, product
preparation, and a non-Rust command. A passing generated TOML parse alone is
insufficient.

## Mise tasks are opaque but typed at the boundary

`ci_tasks` is already a typed task-name declaration; current materialization
turns it into `mise run <name>` and only checks that the top-level name exists
(`s2/mod.rs:2462-2482, 2828-2915`). The Task stage must carry the name and
static closure, not infer semantics from the command body.

The resolver must read the selected repository `mise.toml` and its matching
`mise.lock` without running Mise or repository code. For each selected task,
resolve declared `depends` transitively, collect task-local tools plus explicit
profile/unit extras, detect missing names/cycles/conflicting selectors, and
prove every effective selector against the lock. Root tools outside that
closure are not installed. Every auto-install setting stays false, including
`MISE_TASK_RUN_AUTO_INSTALL`; an undeclared runtime tool must fail visibly.
Shell text containing `mise run hidden` remains opaque and is not scanned.

Mise task dependencies are part of the task's static tool/validation closure.
They are not automatically separate visible stages: invoking a parent task
already gives Mise its dependency semantics, and rendering dependencies again
would duplicate work. They become separate stages only when the repository
declares that visibility as a typed stage contract with an explicit command
and result policy.

## Receipts, ordering, and fail-closed selection

For a first same-job vertical slice, native ordered Actions steps and exit
status are sufficient for ordinary stage failure. Do not create a fake
artifact receipt to claim Cargo product reuse. A local stage report is still
required because a static kind reusable has a dynamic unit: the frozen plan
must enumerate the exact selected stage IDs and their command/tool digests.

Each rendered stage block must do one of two things:

* execute exactly the selected stage and write a success/failure result; or
* report an explicit, planner-authorized skip because that stage is not in the
  selected scope.

The final stage-coverage check must reject unknown, duplicate, missing,
malformed, blocked, canceled, or unexpectedly executed stage records. A
selected stage whose prerequisite failed is `blocked`/failed evidence, never
an accepted skip. An empty stage plan for a unit with declared work is an
error. Reuse the existing plan/result identity concepts (`plan_digest`,
`command_digest`, unit/provider/run/attempt) instead of introducing a weaker
success marker.

Add `ProductReceipt` fields only when a product crosses a boundary: producer
unit/stage, product name, source/closure digest, profile/features/target,
toolchain/backend ABI, producer run/attempt/job, artifact identity/digest, and
verified status. Caches do not satisfy a stage receipt. Same-job `needs` must
be a topological order check, not proof that Clippy output is a reusable test
product.

## Bounded implementation slice

The first implementation can be bounded while runtime publication is reviewed
in parallel:

1. Add the typed stage model and strict parser/validator, with no old command
   fields in the new runtime schema. Keep command vectors only as transient
   constructor inputs until all unit constructors emit stages.
2. Convert Rust constructors first: fmt, Clippy, nextest/test fallback,
   workspace check, and named Task stages. Convert every non-Rust command
   producer to an explicit opaque Task/Build/Package constructor in the same
   schema cutover; do not leave a generic legacy command-array parser for the
   remaining kinds.
3. Make product preparation, Docker context/seed edits, and MBX conversion
   operate on typed stage commands. Remove `prerequisite_commands` and every
   command-text semantic predicate (`needs_nextest`, Mise/deny/audit
   inference) from the stage path.
4. Render the kind reusable's static union of stage and stage-setup blocks,
   pass a signed/frozen selected-stage plan per unit, and add the final
   coverage result. Install only the stage's verified tool closure immediately
   before it runs; keep Mise runtime bootstrap separate from tool installation.
5. Add focused fixtures for exact command preservation, stage DAG failures,
   selected/missing/unknown stage behavior, task closure and lock failures,
   tool setup order, and deliberate format/Clippy/test failures. Do not time
   the campaign until the generated candidate passes these checks.

This is a single behavior migration, not a dual parser. The old flat fields,
implicit prerequisite rewrite, and old runtime fallback are deleted when the
stage schema is admitted.

## Candidate rollout gate

The new runtime/generator must be built and verified before any generated plan
uses the stage schema. The late post-check candidate producer cannot satisfy
this: historical policy runs timed out while waiting for a product emitted
after the generator check. Use the reviewed pre-plan bootstrap or an immutable
preview product with an explicit receipt. Bind repository, workflow/run/
attempt/job, producer conclusion, source/head and merge/build revisions,
closure, schema/project digest, profile/features/platform, artifact identity,
artifact/binary digests, and successful validation. Do not pretend a branch
candidate is the published main pin.

Velnor's source2 and Jackin/Parallax source1 consumers must be migrated and
regenerated against the same admitted runtime contract. The old pinned
runtime cannot parse the new stage tables. No old/new field alias, parser
fallback, or mixed generated surface is accepted. Candidate planning must
parse the new config before policy/unit fanout starts; fork/untrusted
candidates fail closed.

## Exact current all-target Clippy blockers to send to the Velnor owner

These are outside this stage review and are parent-owned repairs:

* `crates/velnor-workflow/tests/rust_order_render.rs:130`: `clippy::panic`
  (test setup panic path).
* `crates/velnor-workflow/tests/rust_order_render.rs:5`: unfulfilled
  `clippy::expect_used` lint expectation.
* `crates/velnor-workflow/src/s2/primitives/watch.rs:1113`: `expect_used`.
* `crates/velnor-workflow/src/s2/primitives/watch.rs:1130`: `expect_used`.

The identity fixture tests and library Clippy are clean; acceptance still needs
`--all-targets` after those four parent-owned errors are repaired.
