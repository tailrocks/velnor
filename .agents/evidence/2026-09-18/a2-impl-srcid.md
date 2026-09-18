# A2 implementation evidence — G1+G10 (source identity)

Branch: `feat/a2-source-identity` (from `docs/bastion-final-plan` @ 38ffbfd7)
Commit: `65beb2ed` (signed off, pushed to origin)
Worktree: isolated subagent worktree (branch contains only this change)

## G1 — source revision carried in manifest + verified by both consumers

Semantics (deliberate): several commits can share one closure/product, so the
manifest `revision` names the commit the producer built from; consumers check
well-formedness (`^[0-9a-f]{40}$`) plus binary `--revision` == manifest
`revision`. Strict equality with the requested rev would break closure-sharing.

Producer (`crates/velnor-workflow/src/primitives/runtime_products.rs`):
- manifest program gains `revision: $revision`; assembly passes
  `--arg revision "$HEAD_SHA"`; assembled manifest re-checked vs `$HEAD_SHA`
- build prove step: binary `--revision` == `git rev-parse HEAD`
- publish spot check: binary `--revision` == `$HEAD_SHA`
- smoke test: installed binary `--revision` == downloaded manifest `.revision`
- `MANIFEST_ACCEPT_FILTER` gains `(.revision | test("^[0-9a-f]{40}$"))`
- render pin updated 3f8641a7 → f80fa05d (diff verified = only these lines)

Consumers:
- setup action source (`.github-gen/.../action.yml`): both accept filters gain
  the revision clause; PATH step gates `--revision` vs manifest `.revision`
- Velnor provisioner (`lib.rs` `workflow_pinned_policy_runtime_velnor`): same
  filter clause; revision probe+gate after the closure gate (digest-before-exec
  ordering extended, still no probe before the digest comparison)

Regen only (no hand-edited YAML): `velnor-workflow --plain --force .` rewrote
`ci-runtime-products.yml`, `ci-unit-rust.yml`, `release.yml`,
`.github/actions/setup-velnor-workflow/action.yml` (+ generator state); every
hunk reviewed and limited to the intended lines.

Tests:
- extended: `manifest_shape_matches_the_consumer_contract`,
  `binary_closure_is_proven_before_upload`, `smoke_test_installs_the_consumer_layout`,
  `velnor_provisioner_reuses_the_slot_only_on_manifest_digest_match`,
  `setup_action_gates_both_paths_on_digest_and_self_report`
- new: `setup_action_accept_filters_require_a_well_formed_manifest_revision`
- probe (/tmp/a2-probe.sh): `bash -n` clean over all 7 touched run blocks;
  filter ACCEPTs revision-carrying manifest, REJECTs missing + malformed revision

## G10 — daemon/runtime identity separation pinned

New tests in `crates/velnor-workflow/src/primitives/release.rs`:
- `rendered_surfaces_never_select_latest`: scans all 9 rendered surfaces
  (legacy release/preview/maintenance/signer + native release/preview +
  producer + setup action + Velnor provisioner); any `latest` token fails
  except the one known error-prose line ("re-run the LATEST preview run")
- `runtime_product_tags_are_disjoint_from_release_tags`: anchored to the real
  rendered `tags: ["v*"]` trigger + `v[0-9]*` gate; structural proof that the
  product prefix's second byte is a fixed non-digit so NO product tag can ever
  match the gate, plus sampled both-direction checks

## Gates (all observed in-session)

- `cargo test -p velnor-workflow`: 489 lib + 2/6/5/9/33 others, 0 failed
  (incl. `checked_in_workflows_match_the_generator_byte_for_byte`)
- `cargo clippy -p velnor-workflow --all-targets`: 0 warnings
- `cargo fmt -p velnor-workflow -- --check`: clean
- actionlint 1.7.12 on the 3 touched workflows: exit 0, no findings
  (composite action.yml "syntax-check" complaints are pre-existing actionlint
  behavior for non-workflow files — identical on the untouched action)
- `velnor-workflow --plain --dry-run .`: exit 0, "Dry-run: 0 files would change"

## Files changed (9)

- crates/velnor-workflow/src/primitives/runtime_products.rs (producer + tests)
- crates/velnor-workflow/src/lib.rs (Velnor provisioner + consumer tests)
- crates/velnor-workflow/src/primitives/release.rs (G10 tests only)
- .github-gen/sources/actions/setup-velnor-workflow/action.yml (consumer)
- .github/actions/setup-velnor-workflow/action.yml (regen)
- .github/workflows/ci-runtime-products.yml (regen)
- .github/workflows/ci-unit-rust.yml (regen)
- .github/workflows/release.yml (regen)
- .github/ci/.github-actions-generator-state (regen)
