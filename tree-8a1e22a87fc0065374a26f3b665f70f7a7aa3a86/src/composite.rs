//! Canonical composite building-block action sources.
//!
//! The two composite actions (`actions/run-gate/action.yml` and
//! `actions/aggregate/action.yml`) are generated artifacts, exactly like the class
//! templates and callable workflows: their canonical bytes live here as static
//! source, [`crate::generate`] writes them, and [`crate::audit`] byte-compares the
//! committed files against these constants. Embedding the bytes as committed Rust
//! source (never `include_str!` of the on-disk action) is deliberate: it makes the
//! audit independent of the file it is auditing, so a hand edit to a composite body
//! — for example neutering the gate exec line into a no-op — fails the audit instead
//! of silently redefining "canonical".

/// Canonical bytes of `actions/run-gate/action.yml` (the lane-neutral single-gate
/// executor). A single trailing newline; LF only; no trailing whitespace.
pub const RUN_GATE_ACTION: &str = r#"name: Run gate
description: >-
  Execute exactly one named CI gate command. The command is identical on either
  lane; only the runner that hosts this composite differs. This is a lane-neutral
  building block: it contains no lane-specific, Velnor-specific, or
  GitHub-hosted-specific logic.
inputs:
  name:
    description: Gate name (install, build, test, lint, or format).
    required: true
  command:
    description: Exact command to execute for this gate. Must be non-empty.
    required: true
runs:
  using: composite
  steps:
    - name: Run gate
      shell: bash
      env:
        GATE_NAME: ${{ inputs.name }}
        GATE_COMMAND: ${{ inputs.command }}
      run: |
        set -euo pipefail
        if [ -z "${GATE_NAME}" ]; then
          echo "run-gate: empty gate name" >&2
          exit 1
        fi
        if [ -z "${GATE_COMMAND}" ]; then
          echo "run-gate: gate '${GATE_NAME}' has an empty command" >&2
          exit 1
        fi
        echo "::group::gate ${GATE_NAME}"
        echo "+ ${GATE_COMMAND}"
        bash -eo pipefail -c "${GATE_COMMAND}"
        echo "::endgroup::"
"#;

/// Canonical bytes of `actions/aggregate/action.yml` (the lane-contract emitter).
/// A single trailing newline; LF only; no trailing whitespace.
pub const AGGREGATE_ACTION: &str = r#"name: Aggregate gates
description: >-
  Emit the lane contract signal. This step runs only after every applicable gate
  in the lane job has already succeeded (each gate fails the job on error), so
  reaching it means the lane genuinely passed. It emits `contract=success`. A lane
  whose gate failed never reaches this step, so its contract output stays empty and
  the lane fails. Skipped work never produces `contract=success`.
outputs:
  contract:
    description: "'success' only when every applicable gate in this lane succeeded."
    value: ${{ steps.emit.outputs.contract }}
runs:
  using: composite
  steps:
    - name: Emit lane contract
      id: emit
      shell: bash
      run: |
        set -euo pipefail
        echo "contract=success" >> "${GITHUB_OUTPUT}"
        echo "lane contract: success (every applicable gate passed)"
"#;

/// The composite building-block action names, in canonical order.
pub const COMPOSITE_NAMES: [&str; 2] = ["run-gate", "aggregate"];

/// The canonical committed bytes for the named composite action, or `None` if
/// `name` is not a recognized building block.
#[must_use]
pub fn canonical(name: &str) -> Option<&'static str> {
    match name {
        "run-gate" => Some(RUN_GATE_ACTION),
        "aggregate" => Some(AGGREGATE_ACTION),
        _ => None,
    }
}
