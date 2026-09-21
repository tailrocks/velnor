# V-METADATA-STAGING-001 — release input staging

Status: implementation, complete emitted-step fixtures, and 1,947 local tests
pass. Generated preview/release output uses external staging. Exact committed
build verification and real CI remain pending. No performance acceptance or
completed iteration credit.

## Failure and structural cause

Upstream preview [35504500719](https://github.com/tailrocks/velnor/actions/runs/35504500719),
source `9e5c0eb215d4169578d6f064806e89fe4c793e85`, fails its x64
[job 106063528303](https://github.com/tailrocks/velnor/actions/runs/35504500719/job/106063528303)
because downloaded `metadata/` dirties the source checkout before the release
build verifies source identity. Input artifacts and source share a directory
without an explicit staging boundary. Keep the clean-tree guard. The separate
ARM missing-linker failure remains unresolved by this unit.

PR [#960](https://github.com/tailrocks/velnor/pull/960), inspected head
`c8f7a2b353f9d2d6a60d3ad8bb2dc6299a108ceb`, contains a compatible external
staging correction. Adapt its mechanism in both current source renderers; do
not import stale generated workflows.

## Alternatives and intervention

1. External artifact staging, with one renderer contract supplying Actions and
   shell paths. Chosen: `${{ runner.temp }}/release-metadata` and the quoted
   shell equivalent keep downloaded inputs outside the checkout.
2. Download metadata after compilation. This also avoids pollution but binds
   correctness to stage ordering and fails if earlier build consumers appear.
3. Build in a separate clean worktree. This isolates identity but adds checkout
   and product-transfer work for an input-location defect.

An initial post-render string-replacement patch was rejected: indentation and
statement spelling determined whether a metadata consumer adopted the path.
The final source emits the shared path directly. Every identity/manifest read,
checksum, copy, and release-tool invocation uses the staged location. Preview
source identity checks and package-record checks stay unchanged.

## Validation and limits

Parent applied the two-file source patch onto campaign `6544ad3a` and passed
22 native identity tests plus strict all-target/all-feature Clippy. A separate
probe rendered the real Velnor preview and release workflows, parsed their YAML,
and executed both complete metadata staging steps. Bash 3.2 and 5.3 each cover
valid hostile paths, missing manifests, and preview source mismatch: ten cases,
all expected outcomes. Downloaded inputs leave an empty Git checkout clean
before build/staging. Step hashes and fixture locations are retained in
`observations/metadata-staging-parent-probe.json`.

The package-record step uses an argument-checking release-tool fixture; this
proves path/argument transport, not the production release binary. Actual
packaging, ARM prerequisites, attestations and promotion remain required CI
validation. No production release is created for this probe.

Complete emitted-step fixtures cover both renderers and preserve blank lines
when extracting shell bodies. They verify checkout cleanliness before staging,
hostile temporary paths, missing manifests, preview identity mismatch, and
package-record arguments. Independent review by `velnor_inventory` found the
final fixture changes structurally sound. The complete all-target/all-feature
nextest suite passed 1,947 tests, zero skipped (26.326 seconds execution).
Strict Clippy then exposed fixture-only unchecked unwraps and an oversized test.
Extracted setup/execution/assertion helpers preserve every case and add stable
missing-manifest coverage. Final parent rerun passed all 1,947 tests, zero
skipped (60.330 seconds); strict Clippy, fmt, actionlint and Markdown lint pass.
Local timing varies under shared machine load; neither duration is a CI speedup.
Generated workflow changes are confined to preview/release metadata consumers
and generator ownership state; rendering revisions advance to legacy 63/S2 65.

Next: verify the exact committed generator, publish, and exercise a safe real
packaging path. ARM prerequisites remain a separate required repair.

Primary contracts: [GitHub runner context](https://docs.github.com/en/actions/reference/workflows-and-actions/contexts#runner-context)
defines job-local temporary storage. The pinned
[download-artifact v8.0.1 action](https://github.com/actions/download-artifact/blob/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c/action.yml)
accepts an explicit extraction path; this unit changes that input and its
consumers rather than weakening source identity.

## Published checkpoint

Published as `57e7cafc410020a6d5097b50bfc82b0162b64551`, tree
`07d8bd49b044f0c9249dcb43551192872e0f5a84`. Local signed commit
`521bab5430ee8089fe175ec56fae36aba96dacb5` has identical tree and is retained.
The default-feature generator rebuilt from the exact published revision and
passed `generate . --check --plain` using the verified 4fa runtime for the
declared pin. Candidate closure:
`68a468253f6e968848c1c82eb59a235639613029dce566f3c6af3c001fe97503`.
An earlier no-default-feature build was correctly rejected because it is not
the declared candidate feature contract; no policy guard was relaxed.

Fresh PR run `35510807365` and policy `35510805245` are in progress.
Real packaging and ARM validation remain pending.
