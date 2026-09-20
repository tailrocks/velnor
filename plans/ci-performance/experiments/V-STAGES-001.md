# V-STAGES-001: typed visible validation stages

Status: **design only; unaccepted; no source or generated workflow change**.
Date: 2026-09-20.

## Root cause

The generator currently carries command arrays through `Unit` and runtime
`CiUnit`, then later layers infer stage identity and tools from shell text.
`runtime.rs`/`s2/runtime.rs` derive an implicit Clippy-to-check prerequisite by
rewriting strings and removing flags. The IR searches command strings for
`nextest`, `mise`, and workspace checks. The renderer emits all unit commands
inside one `Run unit checks` Actions step. MBX conversion also runs after this
flat representation exists. Therefore arbitrary opaque Mise tasks can acquire
Rust semantics, prerequisites lose typed identity, flags can change silently,
and Actions cannot expose format/lint/test status separately.

Relevant paths inspected:

- `src/scan/rust.rs`, `src/s2/scan/rust.rs`: Rust command construction.
- `src/lib.rs`, `src/s2/mod.rs`: `Unit`, command materialization, MBX
  conversion, products, and prerequisites.
- `src/config/mod.rs`, `src/s2/config/mod.rs`: typed declarations and current
  command-array rejection rules.
- `src/runtime.rs`, `src/s2/runtime.rs`: flat runtime command execution and
  implicit prerequisite rewriting.
- `src/primitives/ir.rs`, `src/s2/primitives/ir.rs`: command-string tool
  detection and the collapsed `Run unit checks` step.

## Proposed contract

Replace runtime flat command arrays at the migration cutover with a strict
stage graph:

```text
enum ValidationStageKind {
    Format, Lint, Check, Test, Codegen, Build, Package, Install, Task
}

struct ValidationStage {
    id: StageId,
    label: String,
    kind: ValidationStageKind,
    commands: StageCommandMatrix, // exact provider/lane/scope commands
    needs: Vec<StageId>,           // actual same-unit receipt prerequisites
    tools: Vec<ToolRequirement>,
    consumes: Vec<ProductRef>,
    produces: Vec<ProductRef>,
    modes: StageModes,             // PR/full/dependency membership
}
```

Rust scanners construct `fmt`, `clippy`, and `nextest`/`test` stages directly,
preserving package, target, feature, profile, lock, wrapper, and warning flags.
Where a dependent unit really needs a check, the constructor creates an
explicit dependency-only `rust-check` stage with its own valid Cargo/MBX
command. Runtime never derives `check` by editing a Clippy command. MBX
conversion happens once while constructing each typed command; unsupported
flags are handled by the constructor and covered by tests.

`workspace_check` becomes an explicit Check stage. Each `ci_task` becomes a
named Task stage (`task:<validated-name>`) with the exact `mise run` command.
Opaque task bodies remain opaque: no string classifier may infer Clippy,
nextest, products, or prerequisites from them. Task tools must be declared by
the task/unit contract; stage tools are the sole input to tool setup.

`needs` expresses real receipt/product prerequisites. Stage order expresses
fail-fast presentation only. Selecting a dependent stage without a compatible
receipt fails closed; runtime never silently runs or drops prerequisite work.
Cross-unit `depends_on` and existing product transfer contracts remain a
separate graph. Caches never satisfy a stage receipt.

Runtime serializes `[[unit.stage]]` with strict enums, unique IDs, exact
command matrices, tool requirements, product references, and mode membership.
Unknown/empty IDs, duplicate IDs, missing commands, unknown references, cycles,
unsupported modes, and incompatible receipts are errors. The schema migration
removes flat command arrays; no alias or legacy parser remains.

The renderer emits one step per selected stage in the same job and workspace:

```yaml
- name: Format
  run: velnor-workflow run ... --stage fmt
- name: Clippy
  run: velnor-workflow run ... --stage clippy
- name: Nextest
  run: velnor-workflow run ... --stage nextest
```

Checkout, toolchain, cache, and MBX setup stay shared. Separate jobs would
repeat those costs. Required gates consume typed stage receipts and fail for
missing, failed, canceled, skipped, unknown, or malformed obligations.

## Alternatives

1. **Typed runtime stages (recommended).** One source model drives scanner,
   tools, runtime, YAML, receipts, and required gates. It removes classifiers
   and supports visible same-job steps. It requires coordinated runtime and
   generator products plus exact candidate bootstrap before consumers can emit
   the new schema.
2. **Generator-only stage metadata over flat runtime commands.** Smaller first
   patch, but metadata and runtime can diverge; old string rewriting remains,
   prerequisites cannot be validated at the execution boundary, and the
   migration leaves the enabling architecture intact. Reject.
3. **Direct shell commands rendered into YAML.** Avoids runtime migration but
   duplicates lane/provider selection, weakens receipts/provenance, and adds a
   YAML escape path. Reject.
4. **One Actions job per stage.** Clear status and native `needs`, but repeats
   checkout/tool/cache/MBX setup and artifact transfer. It conflicts with
   same-workspace reuse and should be measured only as a comparison, not the
   default architecture.
5. **Grouped typed logs in one runtime step.** Minimal migration, but still
   hides failures behind one Actions step and cannot provide independent stage
   status. Reject.

## Validation and rollout dependency

Add constructor tests for exact Rust command order and flags, explicit
`rust-check`, nextest fallback, opaque Task behavior, and stage tools. Add
runtime tests for invalid DAGs, missing/wrong-scope/source receipts, stage
failure, and no duplicate prerequisite execution. Render both schema paths and
assert separate format, lint, test, check, and task steps in one job. Extend
`tests/rust_order_render.rs` to validate the stage contract. Test gate behavior
for every selected stage and malformed obligations.

Current generation/runtime products require schema 2/3 and reject unknown
fields. The candidate generator/runtime must therefore be built and pinned
for every compatible platform/profile/features identity before any consumer
emits `stage`. Candidate source checks remain independent of candidate-plan
execution. Velnor, Jackin, and Parallax must migrate and regenerate together;
old pinned runtimes cannot parse the new contract. This design has no timing or
performance acceptance yet.
