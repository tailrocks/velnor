# Seven-distribution source-contract review

Review scope: exact inventory objects

* `g0-distribution-inventory-20260920T061750Z.md`, SHA-256
  `495fd21dd1d939a5cb34e4adf997ad83e21a16931c3a2721125d380d05bafe1e`
* `g0-distribution-inventory-20260920T061750Z.json`, SHA-256
  `502c5368612b06669e4524075456c08a3d27c5b027279f190eb612849b1f08c7`

This is a read-only source-contract review. It is not G0, release, install, or
publication approval.

## Verified bindings

* The JSON has exactly seven distribution consumers, with no substitute rows:
  `tailrocks/{velnor-apt,holla-apt,homebrew-tablerock,homebrew-ruxel,homebrew-parallax,homebrew-holla,homebrew-velnor}`.
* Each row's default-branch SHA matches the exact checked-out source clone:
  `b24d7d4`, `0636074`, `7d376ca`, `ce29c81`, `c9c5927`, `cc25db8`, and
  `7af1249` respectively. The 43 declared source-file gitblob IDs all match
  `git rev-parse <sha>:<path>` (`43/43`, zero mismatches).
* The six raw-capture objects named by the inventory rehash to their recorded
  SHA-256 values. Capture metadata is GET-only, read-only, and records 1,304
  workflow bodies, 171 ruleset/check-target references, 2,095 requests, and
  ten request errors. No jobs, artifacts, logs, dispatches, release assets, or
  installs were collected.
* The ten workflow-directory 404s remain raw failures. In particular,
  `homebrew-velnor` has HTTP 404 with body SHA
  `e45906ce02e79f16fb87a838d86bb4e497e37a9591f97608fa013bbcd8b9cbc2`; it is
  not converted to an empty successful workflow inventory.
* The corrected workload contract source rehashes to
  `1132d45af4a583c88925302d06d2c0d330b5eee0e01131a4cc41df94840359d4`.
  Every matching repository row has a nonempty expected-workload list equal to
  the contract's partition: 4 workloads for each APT consumer, 3 for each of
  the four generic Homebrew consumers, and 3 for `homebrew-velnor` (23 total).
  The omitted eighth contract repository is not silently assigned to this
  seven-row partition.

## Source predicates and dependency edges

* `velnor-apt`: source contains stable/preview package state, two-architecture
  asset checks, producer source/ref checks, signer checks, and a release
  verify/publish/deploy DAG. This is source wiring only. Producer-owned release
  discovery/attestation bytes, feed publication, and clean-client
  install/upgrade remain unobserved.
* `holla-apt`: source contains package-state/config/signing material but
  explicitly omits publisher/release workflows. The checked-in package test
  references absent `publish.yml`. Its README/feed/install edge is prose, not
  an executable or observed edge.
* `homebrew-tablerock`: source declares stable/preview formula and cask
  identities, four CLI target branches, arm64 cask targets, and updater
  negatives. The six generated workflows provide generic Ubuntu audit wiring;
  no native macOS child or publisher exists in this source.
* `homebrew-ruxel`: preview has pinned source and four targets; stable is
  disabled and uses a mutable source URL. No updater script exists. This is a
  source defect/unknown, not a waived stable install workload.
* `homebrew-parallax`: stable and preview source identities, four targets, and
  conflict declarations exist; only stable updater wiring is present. Its
  `serve` dependency/smoke path and preview update remain unexecuted.
* `homebrew-holla`: preview has source identity and four targets; stable lacks
  the source identity comment and symmetric conflict declaration. Only static
  audit/updater source exists.
* `homebrew-velnor`: exact source has an immutable tagged Velnor archive and
  SHA for a `velnorctl` source-build formula, plus README install boundaries.
  No workflow was returned (the retained 404 is above), and architecture,
  preview/channel, native install, and upgrade evidence are unknown.

The producer-to-feed/tap-to-install relations are therefore correctly marked
source-declared or README/source-contract-only. None is an observed release or
install edge. Source-declared checksums/package-state rows are not external
asset digest proof.

## Independent fixture signal

`velnor-apt/scripts/test-verify-release.sh` passed all 45 checks in a detached
read-only run. Package-update fixture scripts were attempted for Velnor APT,
Holla APT, TableRock, Parallax, and Holla; each stopped in the fixture copy at
the repository's `mise.toml` trust check before product assertions. This is an
environment/tool trust limitation, not a pass or fail claim for those package
contracts. No source tree was modified.

## Verdict and required follow-up

The seven-way partition, exact source pins, source-file provenance, raw-error
handling, and workload partition are internally consistent and source-accurate.
The inventory must remain `source_derived_not_execution` / `gate_status=not_evaluated`.

Before any G0/G2 claim, bind each required edge to independently acquired
producer release/tag/asset/attestation data and actual run/job/artifact/log
records; add the missing publisher/native/clean-client paths; and resolve the
listed mutable, missing, asymmetric, and unknown channel/architecture cases.
Do not turn any 404, absent workflow, README edge, source-declared checksum, or
static Ubuntu audit into success, exclusion, or N/A.
