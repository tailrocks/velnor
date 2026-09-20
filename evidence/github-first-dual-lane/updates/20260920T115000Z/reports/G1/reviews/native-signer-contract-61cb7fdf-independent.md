# Independent native signer-contract review — `61cb7fdf`

Date: 2026-09-20 (Asia/Ho_Chi_Minh)

## Scope and exact source

Reviewed the detached, clean worktree `/private/tmp/velnor-native-signer-61-review`:

- repository: `tailrocks/velnor`
- commit: `61cb7fdfbcac0eb18676f2d4c47189517f832b3a`
- tree: `20703dd6d2d11d697cdebbf81f0e64c55d72cfa3`
- parent comparison: `9908296d28d27e0d5b993d1e48ea7a96bc31db83`
- parent tree: `6c2dcb0805f81cf044cec8016dfd7f58aee2537b`
- signer source SHA-256 (`crates/velnor-workflow/src/s2/primitives/signer_contract.rs`):
  `d9f1702705d8e681661bc451a7bba5a521385a087e416dfa1b423ce2095e0392`

The owner worktree `/private/tmp/dual-lane-native-product-v3` had unrelated
uncommitted `release.rs` changes. It was not used for this review.

This is a bounded source/fixture review. It does not approve a native DAG,
publication, hosted execution, or live evidence authority.

## Verdict

**PASS — pure typed transport contract and preview cross-toolchain delta.**

**NOT APPROVED — generated caller/DAG integration.** The new contract is
registered but intentionally remains dead code until the privileged signer DAG
is wired. The generated caller still uses the older, weaker interface, so the
typed admission binding is not yet an enforced workflow boundary.

## Contract findings

`signer_contract.rs` provides the following correctly typed and fail-closed
boundaries:

- `AdmissionOutputs` denies unknown fields, requires canonical JSON, keeps
  release/provider IDs as strings at the admission boundary, validates exact
  repository/URL/ref/source-commit/phase identity, and only parses the APT
  provider ID as a bounded positive `u64` at its typed boundary.
- Subject inventories and attestation records deny unknown fields, require
  exact source identity, validate digest/size, and aggregate exactly the
  inventory subject set: missing, duplicate, foreign, or nested subject paths
  fail.
- `AptAdmissionHandoff` has an exact field set, binds source/ref/commit/tag,
  release/provider IDs and repository to the admission, and takes the parent
  manifest digest as an external expected value. It has no self-digest field,
  so the APT sidecar is structurally acyclic rather than a self-hash.
- Checksum sidecars are restricted to one exact `sha256  basename` line with a
  safe basename. Transport validators verify sidecar bytes against the exact
  serialized record/handoff bytes.
- `SignerCallInputs::admitted` only accepts safe basenames and renders the
  admitted `source_ref` and `source_commit` expressions from
  `needs.admit-product-release`; the helper forbids `github.sha` in its test.

The pure contract does not itself census downloaded files or independently
hash subject bytes. Inventory/record digest, size, and completeness become
authoritative only when the eventual caller binds them to the producer's
verified bytes and admitted inputs.

## Tests and verification

All commands below ran against the detached exact commit with a separate
`CARGO_TARGET_DIR`:

```text
cargo test --locked --all-features -p velnor-workflow --lib signer_contract -- --nocapture --test-threads=1
4 passed, 1746 filtered out

cargo test --locked --all-features -p velnor-workflow --lib native_identity_build_records_binary_digest_and_debian_reuses_it -- --nocapture --test-threads=1
2 passed, 1748 filtered out

cargo test --locked --all-features -p velnor-workflow --lib native_preview_produces_debs_with_rolling_identity -- --nocapture --test-threads=1
2 passed, 1748 filtered out

cargo clippy --locked --all-features -p velnor-workflow --lib --tests -- -D warnings
No issues found

cargo fmt --all -- --check
pass

git diff --check 9908296d28d27e0d5b993d1e48ea7a96bc31db83...61cb7fdfbcac0eb18676f2d4c47189517f832b3a
pass
```

The four contract tests are meaningful strictness tests: duplicate and
unknown admission keys, string-ID rejection for numeric JSON, complete matrix
aggregation (missing/duplicate/foreign/nested cases), exact APT handoff and
sidecar checks (including provider-ID overflow), and admitted-pair rendering.

Remaining test-coverage gaps, not source-pass claims:

- no dedicated record digest/size mismatch or record unknown-key negative;
- no dedicated wrong/mutated sidecar after an otherwise valid record;
- no explicit whitespace/key-order canonicality negative beyond the generic
  canonical parser behavior;
- no explicit self-hash attack fixture. The implementation has no self-hash
  field and uses an external expected manifest digest, but the integration
  caller must prove that digest came from the separately verified canonical
  manifest.

## Preview cross-toolchain and generator boundary

The exact generated outputs were rendered from the detached commit with the
plain generator into `/private/tmp/native-product-render-61-9t4dfg`.
Configured actionlint (using the source `.github/actionlint.yaml`) passed for
the generated preview and release workflows.

Compared with exact `9908296d` output:

- `release.yml` is byte-for-byte identical;
- the signer workflow is byte-for-byte identical;
- `preview.yml` changes only the arm64 cross-compiler/linker environment and
  the conditional Linux arm64 toolchain installation step.

The preview implementation has actual build meaning: an `ubuntu-24.04` x86
host cross-compiles `aarch64-unknown-linux-gnu`; for that target only, it
installs `gcc-aarch64-linux-gnu`, `libc6-dev-arm64-cross`, and
`linux-libc-dev-arm64-cross`, verifies `aarch64-linux-gnu-gcc` with
`command -v`, binds Cargo's target linker, and passes `--target "$TARGET"`
to both the release build and `cargo-deb`. Stable has no such step and is
unchanged. Unsupported target triples map to no architecture rather than a
silent fallback.

The generator boundary is neutral in this delta: plain rendering preserved
stable output and produced the expected preview-only additions. This does not
establish a hosted runner, publication, or signer-DAG proof.

## Integration blocker

The generated `.github/workflows/ci-release-package-signer.yml` remains
unchanged from `9908296d`: its `workflow_call` accepts `artifact-name`,
`subject-path`, and `source-ref`, but not `source-digest`. Generated preview
and stable sign-deb callers likewise still pass only `source-ref` (preview
`refs/heads/main`; stable `refs/tags/${{ github.ref_name }}`). Release output
still contains the direct build-provenance attestation path.

Therefore the new typed `SignerCallInputs` contract is not yet consumed by the
actual generated caller, and the source-commit binding remains prose/helper
evidence rather than an enforced producer-to-signer DAG edge. Wire the caller
to the typed admitted inputs and bind the emitted subject inventory/record
bytes before treating this commit as signer integration complete.
