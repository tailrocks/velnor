# GitHub Action scanner rereview — exact `40ddcc02`

## Disposition

**Changes required. Source-only verdict; no merge, rollout, host runtime, Docker, or fleet operation.**

Reviewed exact revision `40ddcc02dde1ff07aff538ea2ca95da091379e17` on
`codex/github-first-action-scanner` from the clean detached worktree
`/private/tmp/velnor-action-review-40dd`.

Runner protocol comparison uses `actions/runner`
`80bb1fb827fa44d489263061e71ef4adba7ad8cd` at
`/tmp/actions-runner-protocol-review`.

## Findings

1. **P1 — bare Docker images are accepted although the runner rejects them.**
   `crates/velnor-workflow/src/s2/scan/action.rs:1231-1247` only checks that
   `runs.image` is non-empty and then skips host-file lookup for any non-
   Dockerfile value. The test at `:2367-2420` therefore treats `image: ubuntu`
   as a valid Docker action. Runner `ActionManager.cs:1454-1479` accepts only a
   runner-recognized Dockerfile path or `docker://image[:tag]`; plain `ubuntu`
   throws `NotSupportedException`. Require one of those two forms, validate the
   `docker://` image, and change the fixture to `docker://ubuntu` (or assert
   rejection for the plain form).

2. **P1 — a composite action without `runs.steps` passes scan.**
   `ActionRuns.steps` defaults to an empty vector at
   `crates/velnor-workflow/src/s2/scan/action.rs:44-70`; `validate_runs` only
   validates `steps` when present (`:997-1047`), and `local_references` then
   accepts the empty vector (`:1157-1216`). Runner
   `ActionManifestManager.cs:486-501` throws when composite `steps` is absent.
   Preserve an explicit empty sequence if desired, but reject an omitted field
   and add an exact missing-steps test.

3. **P1 — generated consumer fixtures still do not exercise the actual action
   consumer contract.** `crates/velnor-workflow/src/s2/primitives/github_action.rs:101-136`
   renders success/failure/skip-build entries as standalone `bash -- <fixture>`
   commands. The configured fixture test (`:208-338`) executes only those shell
   files; no checked-in project configuration uses the fixture family (the only
   references are the primitive and its tests). The new in-process graph test
   (`:1451-1713`) is useful and passes, including nested `uses`, output mapping,
   failure propagation, and conditional skip-build, but it is test-only and is
   not wired to the generated consumer command. Add a tracked, network-free
   consumer harness/configuration that invokes the scanned action and asserts
   success, each failure path, skip-build/no-build, output propagation, and
   downstream marker behavior.

4. **P2 — the in-process consumer simulator is not shell/working-directory
   runner-equivalent.** `crates/velnor-workflow/src/s2/primitives/github_action.rs:963-1014`
   always launches `bash -euo pipefail -c` in the repository root and does not
   dispatch the declared `step.shell` or apply
   `step.working_directory`. The graph test therefore cannot prove those
   runner paths. Either implement exact shell and working-directory dispatch
   (with safe workspace mapping) or narrow the test’s contract to the subset it
   actually simulates.

## Prior-gap status

- **YAML numeric/boolean/null/empty/collection keys before coercion:** covered
  by `runner_typed_manifest_shapes_fail_closed` (`action.rs:1824-1963`),
  `runner_yaml_rejects_collection_keys_and_anchors` (`:1965-1998`), and
  `runner_scalar_mapping_keys_fail_closed_before_schema_coercion`
  (`:2000-2051`).
- **Traversal and empty `uses` paths:** covered by
  `external_action_reference_rejects_unsafe_paths_and_accepts_valid_subpath`
  (`action.rs:2095-2151`) and local-reference checks. No acceptance issue found
  in this exact review.
- **Plugin runtime:** explicitly rejected by
  `plugin_action_metadata_is_explicitly_unsupported` (`action.rs:2153-2170`).
- **Actual consumer success/failure/skip-build/output fixtures:** partial only;
  the graph test is substantive, but the generated fixture contract remains
  standalone shell and unconfigured (finding 3).

## Verification

Passed on the clean exact checkout:

- `rtk cargo test -p velnor-workflow --locked s2::scan::action::tests -- --nocapture` — **21 passed** (`19` suites; `1869` filtered).
- `rtk cargo test -p velnor-workflow --locked s2::primitives::github_action::tests -- --nocapture` — **9 passed** (`19` suites; `1881` filtered).
- `rtk cargo test -p velnor-runner --all-features --locked action_contract::tests -- --nocapture` — **5 passed** (`18` suites; `2517` filtered).

The detached review worktree remained clean at the reviewed revision. This
report records source findings only; it does not approve publication or any
runtime/fleet change.
