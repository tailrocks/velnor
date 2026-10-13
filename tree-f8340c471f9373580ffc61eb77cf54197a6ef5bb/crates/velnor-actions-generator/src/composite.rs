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

/// Canonical bytes of the cache runtime handshake. This action never restores
/// or saves data; it validates that the lane runtime supplied the exact trusted
/// declaration identity, bounded reservation, attribution, and clean lifecycle
/// before a later cache operation is eligible.
pub const CACHE_CONTRACT_ACTION: &str = r#"name: Validate cache contract
description: >-
  Fail closed unless the cache runtime proves the canonical schema, declaration
  identity, scope, quota reservation, byte attribution, and clean lifecycle.
inputs:
  schema-version:
    description: Canonical cache schema version.
    required: true
  declaration-sha256:
    description: SHA-256 reported by runtime for fleet/caches.toml bytes.
    required: true
  expected-declaration-sha256:
    description: Exact SHA-256 embedded by trusted generated callable.
    required: true
  cache-id:
    description: Stable cache identifier attested by runtime.
    required: true
  expected-cache-id:
    description: Exact cache identifier embedded by trusted generated callable.
    required: true
  scope:
    description: Cache authority scope attested by runtime.
    required: true
  expected-scope:
    description: Exact scope embedded by trusted generated callable.
    required: true
  cache-owner:
    description: Cache owner attested by runtime.
    required: true
  expected-cache-owner:
    description: Exact cache owner embedded by trusted generated callable.
    required: true
  reservation-id:
    description: Atomic reservation identity attested by runtime.
    required: true
  expected-reservation-id:
    description: Reservation identity requested by trusted coordinator.
    required: true
  required-peak-bytes:
    description: Full peak bytes requested before admission.
    required: true
  quota-reserved-bytes:
    description: Atomic reservation for full peak cache bytes.
    required: true
  attributed-bytes:
    description: Bytes attributed to this cache owner/materialization.
    required: true
  cleanup-state:
    description: Runtime cleanup state; must be clean.
    required: true
  materialization-id:
    description: Immutable materialization identity attested by runtime.
    required: true
  expected-materialization-id:
    description: Job-local materialization identity requested by coordinator.
    required: true
outputs:
  contract:
    description: success only after every cache authority field validates.
    value: ${{ steps.validate.outputs.contract }}
runs:
  using: composite
  steps:
    - name: Validate cache authority
      id: validate
      shell: bash
      env:
        CACHE_SCHEMA_VERSION: ${{ inputs.schema-version }}
        CACHE_DECLARATION_SHA256: ${{ inputs.declaration-sha256 }}
        EXPECTED_CACHE_DECLARATION_SHA256: ${{ inputs.expected-declaration-sha256 }}
        CACHE_ID: ${{ inputs.cache-id }}
        EXPECTED_CACHE_ID: ${{ inputs.expected-cache-id }}
        CACHE_SCOPE: ${{ inputs.scope }}
        EXPECTED_CACHE_SCOPE: ${{ inputs.expected-scope }}
        CACHE_OWNER: ${{ inputs.cache-owner }}
        EXPECTED_CACHE_OWNER: ${{ inputs.expected-cache-owner }}
        CACHE_RESERVATION_ID: ${{ inputs.reservation-id }}
        EXPECTED_CACHE_RESERVATION_ID: ${{ inputs.expected-reservation-id }}
        CACHE_REQUIRED_PEAK_BYTES: ${{ inputs.required-peak-bytes }}
        CACHE_QUOTA_RESERVED_BYTES: ${{ inputs.quota-reserved-bytes }}
        CACHE_ATTRIBUTED_BYTES: ${{ inputs.attributed-bytes }}
        CACHE_CLEANUP_STATE: ${{ inputs.cleanup-state }}
        CACHE_MATERIALIZATION_ID: ${{ inputs.materialization-id }}
        EXPECTED_CACHE_MATERIALIZATION_ID: ${{ inputs.expected-materialization-id }}
      run: |
        set -euo pipefail
        [[ "${CACHE_SCHEMA_VERSION}" == "1" ]]
        [[ "${EXPECTED_CACHE_DECLARATION_SHA256}" =~ ^[0-9a-f]{64}$ ]]
        [[ "${CACHE_DECLARATION_SHA256}" == "${EXPECTED_CACHE_DECLARATION_SHA256}" ]]
        [[ "${EXPECTED_CACHE_ID}" =~ ^[a-z0-9][a-z0-9-]*$ ]]
        [[ "${CACHE_ID}" == "${EXPECTED_CACHE_ID}" ]]
        [[ "${EXPECTED_CACHE_SCOPE}" == "trusted" || "${EXPECTED_CACHE_SCOPE}" == "unmerged" ]]
        [[ "${CACHE_SCOPE}" == "${EXPECTED_CACHE_SCOPE}" ]]
        [[ "${EXPECTED_CACHE_OWNER}" =~ ^[a-zA-Z0-9][a-zA-Z0-9._:/-]*$ ]]
        [[ "${CACHE_OWNER}" == "${EXPECTED_CACHE_OWNER}" ]]
        [[ "${EXPECTED_CACHE_RESERVATION_ID}" =~ ^[a-zA-Z0-9][a-zA-Z0-9._:-]*$ ]]
        [[ "${CACHE_RESERVATION_ID}" == "${EXPECTED_CACHE_RESERVATION_ID}" ]]
        [[ "${CACHE_REQUIRED_PEAK_BYTES}" =~ ^[1-9][0-9]{0,17}$ ]]
        [[ "${CACHE_QUOTA_RESERVED_BYTES}" =~ ^[1-9][0-9]{0,17}$ ]]
        [[ "${CACHE_ATTRIBUTED_BYTES}" =~ ^(0|[1-9][0-9]{0,17})$ ]]
        (( CACHE_QUOTA_RESERVED_BYTES >= CACHE_REQUIRED_PEAK_BYTES ))
        (( CACHE_ATTRIBUTED_BYTES <= CACHE_QUOTA_RESERVED_BYTES ))
        [[ "${CACHE_CLEANUP_STATE}" == "clean" ]]
        [[ "${EXPECTED_CACHE_MATERIALIZATION_ID}" =~ ^[a-zA-Z0-9][a-zA-Z0-9._:-]*$ ]]
        [[ "${CACHE_MATERIALIZATION_ID}" == "${EXPECTED_CACHE_MATERIALIZATION_ID}" ]]
        echo "contract=success" >> "${GITHUB_OUTPUT}"
"#;

/// The composite building-block action names, in canonical order.
pub const COMPOSITE_NAMES: [&str; 3] = ["run-gate", "aggregate", "cache-contract"];

/// The canonical committed bytes for the named composite action, or `None` if
/// `name` is not a recognized building block.
#[must_use]
pub fn canonical(name: &str) -> Option<&'static str> {
    match name {
        "run-gate" => Some(RUN_GATE_ACTION),
        "aggregate" => Some(AGGREGATE_ACTION),
        "cache-contract" => Some(CACHE_CONTRACT_ACTION),
        _ => None,
    }
}
