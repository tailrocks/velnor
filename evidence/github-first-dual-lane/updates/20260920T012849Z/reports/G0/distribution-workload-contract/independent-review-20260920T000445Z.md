# Independent G0 distribution workload-contract cross-check

Review target: `workload-contract-20260919T233308Z.json` and `report.md`.
Read-only review; no build, helper, workflow, dispatch, install, publication, or
source-edit operation was performed.

## Integrity result

The typed record is structurally valid and source pins are reproducible.

- JSON SHA-256:
  `3926b5ff26a5fe317f13018d1a0cc614ae928b8f91d323b7511aed4f2e7e615f`
- Existing report SHA-256:
  `1de9df1de50e0b6cc7b5c0389aa5667e4cd0099dfe4d3c49eb1c4d0ca209bbb1`
- All eight current `refs/heads/main` values still equal the recorded closing
  SHAs. All reviewed local clone HEADs equal those SHAs.
- Source tree entry counts/listing SHA-256 values match all eight JSON rows.
- All 109 recorded source dependency path/blob pairs resolve to the same blob
  at `HEAD:<path>`.
- Committed workflow counts match source: Velnor APT 9, Holla APT 1, each
  generic Homebrew tap 6, Velnor Homebrew 0, Jackin 6. Current Actions API
  counts match the record: 10, 1, 6, 6, 6, 6, 0, 6 respectively.
- Velnor APT's tenth API workflow is exactly the API-only
  `dynamic/pages/pages-build-deployment`; it is not present in the committed
  tree and is correctly not treated as a source dependency.
- Eight rows contain nonempty expected IDs/jobs, validation/install/upgrade/
  channel tasks, and platform fields. `actual_jobs` is null in every row and no
  `plan_state` contains a pass/green/success claim.
- All returned required-check records retain null `integration_id` and `app_id`.
  `homebrew-velnor` has no required-check records because it has no rulesets.
  Classic branch protection is recorded as HTTP 404 and inherited policy as
  unknown; neither is promoted to approval.

## Source contract cross-check

- `velnor-apt`: `conf/distributions` declares `amd64 arm64`; package-state and
  preview records, release/update scripts, signer fingerprint, and release
  workflow admit/verify/publish/deploy/result chain are present. The workflow
  rejects Velnor feed mutation and uses GitHub-hosted publication conditions.
- `holla-apt`: package-state, architecture declaration, and update/test scripts
  are present; `project.toml` explicitly disables release and the only committed
  workflow is docs-only `ci-unit-docs.yml`. Publisher, feed, clean-client,
  upgrade, and service work remain absent/unknown.
- Generic Homebrew taps: each source has the six-workflow generated stack,
  `ci-unit-homebrew.yml`, Ubuntu `brew audit --strict --online`, GitHub automatic
  lane, and explicit formula target branches. TableRock casks explicitly require
  macOS Tahoe/arm64; Ruxel stable is mutable `main.tar.gz` without checksum;
  Parallax has a stable-only manifest updater and `serve` is only a formula
  contract; Holla has four target formulas and stable-only updater coverage.
- `homebrew-velnor`: the two-file source tree has an immutable Velnor tag archive,
  SHA-256, `crates/velnorctl` build path, and README scope separation from the
  Linux runner. It has no workflow/config/action or preview channel source.
- Jackin: current `c501e90d...` tree has preview `refs/heads/main` identity,
  six payload assets plus supporting assets in the updater, explicit preview
  tag guard, generic scan exclusions for `Formula/**` and `Casks/**`, and no
  current `Casks/` tree. Stable cask generation is conditional and unexecuted.

## Findings

### F1 — reuse-count prose is wrong (minor, report-only)

The report says “six unchanged rows” were reused from
`G0/distribution-consumers/consumer-inventory.json`. The JSON truth is five:

`holla-apt`, `homebrew-tablerock`, `homebrew-ruxel`, `homebrew-parallax`, and
`homebrew-holla` have `reused_existing_record: true`. The prior sixth record,
Jackin, was at `f1669391582f92c95da1aa5958de4397dfedca09`; current main is
`c501e90d014c207234ed94ea41f7a1c9b6ea0c7c` and is correctly marked fresh.
Velnor APT and Velnor Homebrew were outside the old six-row inventory. This is a
provenance prose defect; the JSON reuse flags are correct.

### F2 — `homebrew-velnor` arm64 requirement is not source-grounded (material)

The row's `velnorctl-*` expected jobs and observed contract hard-code
`macos/arm64`. At the pinned source, `Formula/velnorctl.rb` has no
`depends_on arch`, `on_arm`, or `on_intel` clause, and `README.md` says only
“macOS”; neither file declares arm64-only support. The formula builds from
source, so architecture cannot be inferred from the URL either. Keep the
required native macOS task, but mark architecture `unknown` (or separately
record an independently sourced arm64 host policy); do not present arm64 as a
source-derived fact.

### F3 — Holla APT package-state job points at a docs-only child (material)

`holla-apt` expected job `holla-apt-package-state` sets
`child_workflow: ci-unit-docs.yml`, but that reusable workflow only runs the
`docs` unit from `project.toml`; it does not invoke `scripts/package-update.sh`
or validate `package-state.json`. The row correctly says
`source_consistency_observed_not_execution` and has no publisher, so this does
not create a false pass. Still, the dependency edge can mislead the collector
into treating docs CI as package-state coverage. Set the child to null or add an
explicit `not_wired_source_contract` state/dependency.

## Verdict

Source-tree integrity, pin closure, API-only Pages distinction, null execution
records, null App bindings, and non-success states are independently confirmed.
The supplement is not gate approval. Correct F1's report text and resolve F2/F3
before treating the record as a clean checker input; no source or original
evidence file was modified by this review.
