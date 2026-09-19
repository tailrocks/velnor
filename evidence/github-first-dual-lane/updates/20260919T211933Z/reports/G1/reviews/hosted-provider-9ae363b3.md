# G1 hosted-provider exact review — `9ae363b3aceb69b2217880bf371533a55eed4946`

## Scope

- Repository: `tailrocks/velnor`, detached review tree `/private/tmp/velnor-hosted-review-9ae363`.
- Exact parent: `3aecc6ed0bf83b1f6dd1d99a1a8a29884f3d710a`.
- Exact diff: `crates/velnor-workflow/src/s2/primitives/release.rs` and `s2/runtime.rs`; 269 additions/45 deletions.
- Worktree remained clean. No source, generated, branch, or remote state was changed.

## Verdict

**Do not approve the exact source yet.** The previously missing direct `publish-gate` edges are now present across the typed native and tarball producer-preview graphs, and the runtime identity checks are materially covered. A remaining tarball publisher source-binding defect makes the rolling release point at `github.sha`, not the admitted producer source SHA.

## Direct graph and source-pin review

I rendered the real checkout (unbound native release) and two temporary typed producer fixtures from this exact source:

- Real Velnor render: `/private/tmp/hosted-render-9ae`; 21 files. Its checked-in release contract has no producer identity, so its `preview.yml` is correctly source-only and has no `source`/`publish-gate`. This is expected source/config drift context, not producer acceptance evidence.
- Bound native fixture: `/private/tmp/velnor-hosted-fixture-9ae.QkWPxx/rendered`.
- Bound tarball fixture: `/private/tmp/velnor-hosted-fixture-9ae.QkWPxx/rendered-tarball`.

Structural inspection of the bound YAML found every producer consumer directly depends on `publish-gate` and contains `needs.publish-gate.outputs.admitted == 'true'`: native identity, hosted unit verifiers, guest payload, metadata, Debian, signer, and publish; tarball hosted unit verifiers, guest payload, build, and publish. Every source checkout in the bound graphs uses `needs.source.outputs.sha` (or native identity's admitted `needs.identity.outputs.commit`). `needs.*.outputs.*` references are direct dependencies.

The source job exports the validated `run_id`; the gate consumes `SOURCE_RUN_ID: ${{ needs.source.outputs.run_id }}` and passes `--source-run-id`. The gate also carries repository/object IDs, workflow ID/path, event, branch/ref, status/conclusion, and head/run/source SHA into `admit-producer`.

## Blocking finding — bound tarball publish target is not the admitted source

The generic tarball path only rewrites the publish target in `inject_native_preview_bindings` (`s2/primitives/release.rs:2359-2363`). `inject_tarball_preview_bindings` (`:3005-3068`) leaves the generated tarball publisher at:

```yaml
gh release edit preview --target "${{ github.sha }}" --prerelease
```

The exact rendered bound fixture has this at `rendered-tarball/.github/workflows/preview.yml:2592`. Its artifacts were built from `needs.source.outputs.sha`, and its gate exposes that same SHA as `needs.publish-gate.outputs.sha`; `github.sha` is not that admitted producer source in the `workflow_run` path. The create fallback also omits `--target`, so a first preview release has the same moving-default-branch problem. This breaks release/tag identity even though all build jobs are gated.

Fix the tarball binding to use the gate SHA for both initial create and edit (or an env value populated from `needs.publish-gate.outputs.sha`) and add an exact rendered-fixture assertion for both commands. Do not use `github.sha` for a producer-bound tarball publisher.

## Runtime hostile tuple evidence

The exact built runtime admitted the trusted tuple. Independent CLI replays rejected each of these with exit 1: repository object ID; head repository name; head repository object ID; workflow ID; workflow path; producer event; branch; ref; run ID; source run ID; head SHA; run SHA; source SHA. Existing runtime tests additionally cover wrong name/status/conclusion, short/zero IDs, and source-resolution mismatches.

## Verification

- `cargo test -p velnor-workflow --lib s2::runtime --all-features -- --nocapture`: **60 passed**.
- `cargo test -p velnor-workflow --lib s2::primitives::release --all-features -- --nocapture`: **98 passed**.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --locked --profile test --all-targets --all-features --package velnor-workflow -- -D warnings`: passed.
- `actionlint -config-file ...` on real unbound preview/release and both rendered bound fixture preview/release workflows: passed.
- `cargo nextest run --locked --all-features --package velnor-workflow`: **1481 passed, 1 failed**; sole failure is the known checked-in generated-workflow byte-drift test (`s2::tests::checked_in_workflows_match_the_generator_byte_for_byte`). This is expected source-only generated drift and is not approval evidence.
- `git diff --check 3aecc6ed0bf83b1f6dd1d99a1a8a29884f3d710a..9ae363b3aceb69b2217880bf371533a55eed4946`: passed.

No G1 overall approval: generated outputs remain drifted, and the tarball target defect must be corrected and re-reviewed at a new exact commit.
