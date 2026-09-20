# PR962 schemafix independent source review

Verdict: PASS. No findings in the bounded `GenericRelease`/`Preview`
`verification_providers` declaration path.

## Exact revision and hashes

- Review commit: `ea9686f0eb522e402441ecd461bd1f458b731d06`
- Review parent: `da5218686085bd394770cfdabe4f66c9dc5d9e12`
- Commit: `fix(generator): honor declared release verification providers`
- Reviewed tree: detached, clean; no source edits.
- Diff scope: one file, `crates/velnor-workflow/src/s2/primitives/release.rs`
  (`181 insertions, 5 deletions`).
- Reviewed source SHA-256:
  `36f06a58a239b93f45ce4ce1af70594b0f212fbb2584ec79bbb4cf71be1322e8`
- Commit diff SHA-256 (plain `git diff parent commit` bytes):
  `b2979f5ec0eda1a4330450ba596043f8ca75215a8c72c63148f7fac0a69665f6`
- `git diff --check parent commit`: pass.

## Source trace

- `release.rs:155-186` and `233-247`: both declaration schemas expose
  `verification_providers`.
- `release.rs:361-364` and `419-422`: GenericRelease and Preview declarations
  parse the field through the shared helper.
- `release.rs:468-495`: `parse_provider_set` rejects unknown and duplicate
  IDs; `require_non_empty` rejects `[]`; `require_subset` rejects providers
  outside `[workflow] providers`.
- `release.rs:210-221` and `250-264`: declared specs are validated and cloned
  into `ProjectConfig.release` before render, so the declared provider set is
  consumed by the real renderer rather than parsed and discarded.
- `release.rs:2562-2571` and `3839-3871`: renderer selects the explicit release
  set when present, with provider-universe fallback only when the field is
  omitted.

## Focused evidence

Command:

```text
rtk proxy env RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER= CARGO_TARGET_DIR=/private/tmp/velnor-pr962-ea-target cargo test -p velnor-workflow --lib s2::primitives::release::tests::declared_ --locked --offline -- --test-threads=1
```

Result: `10 passed; 0 failed`.

Covered declaration tests include:

- `declared_release_verification_lanes_are_exact`
- `declared_preview_verification_lanes_are_exact`
- `declared_binding_shapes_fail_closed` (empty, duplicate, unknown)
- `declared_verification_providers_must_be_available`

Parsed-config fallback/exact-lane test:

```text
rtk proxy env RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER= CARGO_TARGET_DIR=/private/tmp/velnor-pr962-ea-target cargo test -p velnor-workflow --lib s2::primitives::release::tests::parsed_release_contract_selects_exact_verification_lanes_in_surface --locked --offline -- --test-threads=1
```

Result: `1 passed; 0 failed`; explicit hosted set omits Velnor, omitted field
retains the documented universe fallback.

Additional focused suite:

```text
rtk proxy env RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER= CARGO_TARGET_DIR=/private/tmp/velnor-pr962-ea-target cargo test -p velnor-workflow --lib s2::primitives::release::tests --locked --offline
```

Result: `106 passed; 0 failed`.

Validation:

- `rtk proxy cargo fmt --all -- --check`: pass.
- `rtk proxy env RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER= CARGO_TARGET_DIR=/private/tmp/velnor-pr962-ea-target cargo check -p velnor-workflow --all-features --locked --offline`: pass.
- `rtk proxy env RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER= CARGO_TARGET_DIR=/private/tmp/velnor-pr962-ea-target cargo clippy -p velnor-workflow --lib --all-features --locked --offline -- -D warnings`: pass; no baseline diagnostics.

## Scope holds

- Source-only review. No publish, install, dispatch, merge, MacDocker, or
  Velnor execution.
- No pin/admission or latest-Mac source changed; the commit is confined to the
  declaration/render plumbing above.
- No provider/source/release/attestation self-authority was introduced by this
  change; no native-producer integration was added.
