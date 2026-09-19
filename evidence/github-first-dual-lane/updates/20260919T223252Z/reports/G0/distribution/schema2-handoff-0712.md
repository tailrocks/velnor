# Schema-2 APT handoff — `0712a549`

Observed 2026-09-20. Worktree: `/private/tmp/dual-lane-apt-schema2`, branch
`dual-lane-apt-schema2`. Remote branch equals
`0712a54952ec4a3d5aa979790aa23f842a954e0e`; no generated consumer files,
publication, dispatch, or Mac runtime operation.

## Checkpoints

- `c84d2a9778950cab20ca9be59da6f6b33465b84d`
  ([APT source](https://github.com/tailrocks/velnor/commit/c84d2a9778950cab20ca9be59da6f6b33465b84d)):
  descriptor-validated, no-follow, single-link input reads; private
  materialization before `dpkg-deb`/`ar`; create-new sentinel and staged-file
  writes; exact canonical 18-row/four-target/three-component inventory;
  subordinate parent-digest, sidecar, and two-package `SHA256SUMS` checks;
  hostile symlink/hardlink/stale sentinel and selected-extra fixtures.
- `7b8fb456c1d1eacaa771f8e60f27e973ab964efd`
  ([renderer](https://github.com/tailrocks/velnor/commit/7b8fb456c1d1eacaa771f8e60f27e973ab964efd)):
  schema-2 typed APT completeness/config and generated workflow seam. One
  repository-owned discovery selection is passed through `apt-fetch`,
  `apt-verify`, `apt-publish`, `apt-channel-update`, and previous-pointer
  steps. Hosted-only mutation, hidden sentinel upload, retention/recovery, and
  no generic latest/tag feed path are rendered from typed source.
- `e2e887165f32e276d26e61da13642a04e1a40d70` and
  `0712a54952ec4a3d5aa979790aa23f842a954e0e`: lint-only follow-ups, both
  signed with DCO and Codex trailer.

## Verification

- `rtk cargo check -p velnor-workflow --all-features --offline`: pass.
- `rtk cargo test -p velnor-workflow --offline --lib discovery_`: 19 pass.
- `rtk cargo test -p velnor-workflow --offline --lib apt::tests`: 69 pass.
- `rtk cargo test -p velnor-workflow --offline --lib s2::runtime`: 60 pass.
- `rtk cargo test -p velnor-workflow --offline --lib a_declared_apt_`: 4 pass.
- `rtk cargo fmt --all -- --check`, `rtk git diff --check`: pass.
- Full clippy still reports pre-existing generator diagnostics in `s2/mod.rs`,
  `s2/policy.rs`; no APT source diagnostics remain after the lint checkpoint.

## Remaining contract gates

1. Native producer must emit the same authenticated product manifest and
   provider-bound attestation consumed here. Current APT fixture/consumer
   keeps `release_id` (producer grammar) separate from numeric
   `provider_release_id`; canonical release-ID semantics remain an explicit
   cross-owner decision. Do not claim G2 delivery until native/Homebrew/APT
   agree.
2. Native product currently has four targets and 18 rows; APT rejects missing
   Apple targets while consuming the two Linux `apt-package` rows. Native must
   add the APT subordinate package projection (release record, package
   manifest, release-manifest, debs, sidecars, OCI/attestation edges).
3. Generated consumer regeneration and hosted verification remain external;
   no package publication until G1 recovery and independent review pass.

