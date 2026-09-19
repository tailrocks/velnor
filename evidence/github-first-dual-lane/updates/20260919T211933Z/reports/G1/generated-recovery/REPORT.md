# G1 generated recovery checkpoint

Observed `2026-09-20` from isolated worktree
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-generated-recovery`.

## Identity and trust boundary

- base/current main: `e713841bdb9c33d853b7a9af88ceac924af1b3b6`
- branch: `codex/g1-current-generated-recovery`
- declared pin: `fdeed261bd2247a38db6922a7726cd45d3d6f31e`
- pin release: `velnor-workflow-runtime-v1-81ba31f87a699c4e`
- published closure: `81ba31f87a699c4e24e68baa1cf7cd0b5d765e5970934ec72c38b83772b274dc`
- exact local renderer source: detached `fdeed261bd2247a38db6922a7726cd45d3d6f31e`
- exact local renderer build: release, `--no-default-features`
- local renderer self-report: revision `fdeed261bd2247a38db6922a7726cd45d3d6f31e`, closure `81ba31f87a699c4e24e68baa1cf7cd0b5d765e5970934ec72c38b83772b274dc`
- local renderer binary SHA-256: `f45dab7c8c174b87aeaaddc62ea9d24dae512f1cfb54f73efe22a9c0a60a2fcf`
- no Velnor host/OrbStack runtime was started; no release, dispatch, merge, or package publication was performed.

The release metadata was independently checked before generation: tag target is
the pinned commit and the release contains the manifest plus Linux-X64,
Linux-ARM64, and macOS-ARM64 assets with manifest-matching digests.

## Generated scope

Generation used the authoritative command semantics from `.github/AGENTS.md`,
first through the exact pinned source and finally through the published-profile
equivalent binary:

```text
CARGO_TARGET_DIR=/tmp/velnor-fdeed-generator-published-target \
  mbx build --locked --release --no-default-features -p velnor-workflow
/tmp/velnor-fdeed-generator-published-target/release/velnor-workflow \
  --plain --force /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-generated-recovery
```

Only four generated files changed. No source, config, hand-maintained file,
secret, or non-generated file changed:

```text
.github/ci/.github-actions-generator-state
.github/workflows/ci-unit-rust.yml
.github/workflows/preview.yml
.github/workflows/release.yml
```

The renderer changed the ownership scan/generator state and reconciled all
Mr. Boxington action literals from the newer unpinned-to-this-renderer
`a20e1ffc…` value to the pinned renderer's reviewed
`7234d3dd…` value. Provider configuration remains `github-hosted, velnor`
with `automatic_providers = ["github-hosted"]`; release/preview publishers
remain hosted-only. The declared runtime pin remains unchanged.

Post-generation SHA-256:

```text
4dec0c99f70219b8d71c77be6ed4aaf790a3434d2285d984948367ab24bcdcaf  .github/ci/.github-actions-generator-state
727805be6d345ed24f4b028756135ae889f21c7e931f3fa5f37ef9ab93acc657  .github/workflows/ci-unit-rust.yml
69b8bdc42acd50162e4b7d2c5cf7b5b6064eff681131180d3be50170da7a5d48  .github/workflows/preview.yml
5e3a8f4826f8074ed8d8b1394c898d3b3bd9febb06a38644b43ef54e50d9bc3f  .github/workflows/release.yml
```

## Validation

- pinned release-profile `--plain --check`: PASS; no generated drift.
- pinned policy command with `HEAD_SHA=BASE_SHA=e713841b`, base revision
  `fdeed261…`, and live contexts `ci-required,DCO,Policy`: PASS, 11 rules,
  0 failures. This includes generated-tree, trusted runners, action pins,
  workflow structure, and required checks.
- `actionlint -config-file .github/actionlint.yaml`: PASS.
- `git diff --check`: PASS.
- exact pinned renderer test
  `s2::tests::checked_in_workflows_match_the_generator_byte_for_byte`: PASS.
- current-main generator source suite: 1,743/1,744 tests passed. The sole
  failure is `s2::tests::checked_in_workflows_match_the_generator_byte_for_byte`
  because current-main generator code is newer than the declared fdeed pin and
  intentionally renders different Mr. Boxington literals. It is not evidence
  against the pinned renderer; the same assertion passes in the exact fdeed
  source checkout. A future source/pin promotion must rerun the current-main
  suite after publishing that new closure.

## Hosted failure relation

Before this checkpoint, main `e713841b` runs failed because workflows and the
sidecar were stale relative to the pinned renderer, and CI candidate
acquisition consequently found no published candidate. The generated delta
addresses that pin-render drift only. PR954 at current head
`7609366f5ad530c12a87addf98d0c49448009362` has an independent hosted
`velnor-workflow` source-test failure and remains open; its source defects are
not included here. This checkpoint is not a fleet or G1 completion claim.

## Review handoff

This is a generated-only WIP checkpoint for independent review and hosted PR
checks. It must not be merged or released until the parent integration owner
reviews the exact diff and confirms current branch/PR state. External evidence
is kept outside the source revision to avoid verification-commit loops.
