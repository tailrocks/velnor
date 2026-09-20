# Corrected independent review: G0 distribution workload contract

This is a new immutable corrected artifact. The original JSON and report remain
unchanged.

- Original JSON: `workload-contract-20260919T233308Z.json`
- Original JSON SHA-256: `3926b5ff26a5fe317f13018d1a0cc614ae928b8f91d323b7511aed4f2e7e615f`
- Corrected JSON: `workload-contract-20260920T001239Z-corrected.json`
- Corrected JSON SHA-256: `1132d45af4a583c88925302d06d2c0d330b5eee0e01131a4cc41df94840359d4`
- Scope: eight distribution repositories; exact pinned source objects; no build,
  install, dispatch, publication, helper, or gate operation.

## Corrections

1. `tailrocks/homebrew-velnor` remains a native macOS workload, but its pinned
   source does not establish arm64. At `7af1249f3d69c9f2e548583cdc9f3e737da41b81`,
   `Formula/velnorctl.rb:1-18` has a macOS formula, Rust build dependency, and
   source URL/checksum but no `depends_on arch`, `on_arm`, or `on_intel` clause;
   `README.md:5-15` says macOS and separates `velnorctl` from the Linux daemon,
   without an architecture claim. The corrected JSON therefore sets all three
   Homebrew Velnor job platform rows to `{platform:"macos", architecture:"unknown"}`,
   adds `native_architecture:"unknown"`, and removes arm64 from the install task.

2. `tailrocks/holla-apt` package-state validation is not wired to the docs
   child. At exact closing SHA `0636074d4a16be4771685bd95bf2cec739cf359c`,
   `.github/ci/project.toml:14-32` declares only `ci-unit-docs.yml` and one
   `docs` unit with empty commands. The called workflow is documentation-only
   (`.github/workflows/ci-unit-docs.yml:1-4`), while package-state generation is
   performed by the separate source script (`scripts/package-update.sh:4-21,26-42`)
   and consumes `package-state.json` (`package-state.json:1-16`). The corrected
   JSON clears `holla-apt-package-state.child_workflow` and sets its plan state
   to `not_wired_source_contract`; it does not claim package validation ran.

3. The corrected companion prose records five reused rows, matching the JSON
   `inventory_reuse.reused_existing_record` flags:
   `holla-apt`, `homebrew-tablerock`, `homebrew-ruxel`, `homebrew-parallax`,
   and `homebrew-holla`. Jackin is fresh at its current pin; it is not the
   sixth reused row.

## Retained boundaries

The corrected artifact remains `source_derived_not_execution` with
`gate_status: incomplete`. `actual_jobs` remains null. Source declarations,
missing publisher/workflow states, provider eligibility, required-check App
unknowns, and clean install/upgrade unknowns are preserved. No source or
original evidence file was modified.

## Acceptance checks

- Original SHA remains `3926b5ff...`; corrected SHA is pinned above.
- Corrected Homebrew Velnor rows contain no `arm64` claim and all three
  architectures are `unknown`.
- Corrected Holla APT package-state job has `child_workflow: null` and
  `plan_state: not_wired_source_contract`.
- Corrected JSON has no execution or gate-success claim.

Handoff: `g1_run_operations` (fleet index owner) and the checker should bind to
the corrected JSON SHA above, retain the original artifact as historical input,
and not treat this correction as approval.
