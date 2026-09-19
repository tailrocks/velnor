# G1 hosted release configuration

Observed 2026-09-20 Asia/Ho_Chi_Minh. Source worktree: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-hosted`; baseline `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`; source commit `1c5eb2aab1c4785f7915e92b9fd1249ab978c626`.

## Contract

- `.github-gen/velnor-workflow.toml` declares `[release].verification_providers = ["github-hosted"]` while retaining `github-hosted` and `velnor` in the workflow provider universe.
- `ReleaseSection` parses an optional typed provider list. Validation rejects empty, duplicate, unknown, and unavailable IDs.
- Omission preserves historical full `[workflow].providers` release verification. It does not infer from `automatic_providers` or `default_dispatch_providers`.
- Stable release unit jobs select exactly the declared verification set.
- Preview tarball and native paths select the same set. Producer-bound verifier jobs require `publish-gate`, checkout `needs.source.outputs.sha`, and pass that SHA as `HEAD_SHA`; preview build and the singular publisher require the selected verifier IDs. Unbound preview uses the trusted event SHA/branch gate.

## Evidence

- `CARGO_TARGET_DIR=/tmp/velnor3-g1-release-target CARGO_BUILD_JOBS=4 rtk cargo check --locked -p velnor-workflow`: passed.
- Same isolated target: `rtk cargo clippy --locked -p velnor-workflow --all-targets -- -D warnings`: passed.
- Focused config, stable/preview, parsed-surface, producer-SHA, native-preview, and pinned-render tests: passed.
- Source-built `rtk cargo run --locked -p velnor-workflow -- . --plain --dry-run`: passed; reports 12 generated files would change, including `preview.yml` and `release.yml`. No generated output was written pending source/pin freeze.
- Full package run reached 1735 passed; remaining failure was checked-in generated-workflow byte drift awaiting root regeneration after source freeze.

## Ownership correction

`G0/inventory/report.md` is owned by `g0_inventory`. It existed before this report. A prior turn appended the G1 summary there accidentally; that append was removed with `apply_patch`, restoring the prior inventory content. This file is the sole G1 hosted-config report.
