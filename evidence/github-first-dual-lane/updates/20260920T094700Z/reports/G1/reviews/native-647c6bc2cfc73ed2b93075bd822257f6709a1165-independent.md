# Independent native 647c6bc2 review

## Verdict

REJECT for publication. No publication, install, dispatch, hosted, Docker, or
G1 approval follows from this review.

The delta fixes the stable native publisher's `source_ref` nounset failure and
the stable component-census slurp rendering. The exact stable rendered census
now passes with fake provider/tool responses. The exact rendered preview
nominal census still fails before assembly because its generated `jq` newline
split is over-escaped. The shared native-product signer boundary and generated
tree drift also remain unresolved.

## Exact scope

- HEAD: `647c6bc2cfc73ed2b93075bd822257f6709a1165`
- Branch: `codex/github-first-native-product-v3`
- Remote: `origin/codex/github-first-native-product-v3` resolves to HEAD.
- Worktree: `/private/tmp/dual-lane-native-product-v3`; clean at review end.
- Parent reviewed: `f37827e09415ba5733b37e97a16aef716e553322`.
- Prior report: `G1/reviews/native-f37827e0-independent.md`, SHA-256
  `07158373e35acced2328038565ae061123137241a2588a1ca89af7344b295a30`.
- Delta from f378: one source file, `release.rs`, `+163/-1`.
- `git diff --check` from f378 to HEAD: pass.
- No source edits, commit, or push were made by this review.

Fresh external render succeeded at `/tmp/native-647-render.NOEfea` using the
locked generator. Rendered hashes:

```text
73fa4504b2d3a73691b7daf2cecfc718dc8947c798818c78a0fe6180ff46b52a  .github/workflows/release.yml
1393b06aa3278d5b42cf98b0f67c80289796d5603f26e59508abe4572dcd7005  .github/workflows/preview.yml
65f39b828b5bc136f6b41be7591a0972b6f703147cfebcbc2a61ba3fcfb0adca  .github/workflows/native-product.yml
968134de5ff83c75ff287151bfead8762a936a851bf3c4be3570b33775611ef1  .github/workflows/native-product-preview.yml
89634fe2b088394166771389c85da8be0f8c4d156595871d6e78f037dfcf42e5  .github/ci/.github-actions-generator-state
```

## Delta validated

1. `crates/velnor-workflow/src/s2/primitives/release.rs:2422-2430` now binds
   `source_ref` from the downloaded contract before checking it against
   `GITHUB_REF`. This removes the f378 `bash: source_ref: unbound variable`
   failure under `set -u`.
2. `release.rs:2655` changes the stable component census to `jq -sc`, so the
   command-substitution result is compact JSON comparable with the compact
   expected census.
3. The new Unix test at `release.rs:7471` renders the real release shell,
   extracts the real census step, runs it with `bash -eu -o pipefail`, fake
   `gh`/`cargo` commands, source-bound fixture contracts, and harmless census
   files. It asserts provider `release_id=12345` and the returned contract.
   This is executable generated-shell coverage, not a string-presence test.

Stable actual rendered path: **pass**.

- `cargo test --locked -p velnor-workflow --lib rendered_native_product
  -- --nocapture`: 3 passed.
- `cargo test --locked -p velnor-workflow --lib native_identity -- --nocapture`:
  20 passed.
- `cargo test --locked -p velnor-workflow --lib release -- --nocapture`:
  263 passed.
- The strict release-ID rerun accepted JSON string `"12345"` and rejected
  numeric `12345` through the actual rendered command. The rendered serializer
  and stable assembly fixtures passed; no candidate binary was executed.

## Preview actual rendered path: blocker

I built an offline nominal preview fixture from the actual generated
`preview.yml` step: `blocked_targets=[]`, four target contracts, complete
component/artifact JSONL rows, and harmless executable sibling files whose
digests were computed into the rows. I then ran the extracted actual shell
with `bash -eu -o pipefail`.

It fails with rc=1 at generated `preview.yml:1024`:

```sh
actual_targets="$(jq -Rsc 'split("\\n") | map(select(length > 0)) | sort' native-product-target-census.txt)"
```

The source is `release.rs:2071`. The double backslash makes jq split on a
literal backslash-plus-`n`, yielding one string containing all four newline
separated targets. The expected value is four target strings, so the exact
shell emits:

```text
::error::native product target census differs from source
```

The stable generated equivalent at `release.yml:4341` uses `split("\n")` and
passes. Thus preview rendering/actionlint pass, but preview nominal executable
coverage is **fail**, not pass; preview cannot reach assembly in this fixture.

## Remaining blockers and boundaries

1. **Preview target-census escaping.** Fix the source template at
   `release.rs:2071`, regenerate, and add an actual rendered preview census
   fixture. Do not count the current preview render as nominal proof.
2. **Generated drift.** The required check
   `mbx run --locked --manifest-path Cargo.toml -- --plain --check ../..`
   fails exactly on:

   ```text
   .github/workflows/native-product-preview.yml
   .github/workflows/native-product.yml
   .github/workflows/preview.yml
   .github/workflows/release.yml
   .github/ci/.github-actions-generator-state
   ```

   The full library test therefore reports `1744 passed; 1 failed`, solely
   `checked_in_workflows_match_the_generator_byte_for_byte` for generated
   drift.
3. **Native signer boundary remains unresolved from f378.** Stable native
   assets are directly attested by `actions/attest-build-provenance` at
   generated `release.yml:4538-4541` and verified only with
   `gh attestation verify ... --repo` at `4542-4550`. Preview directly attests
   and names `preview.yml` as signer at `preview.yml:1188-1198`. Debian uses
   the separate shared package signer. No common native-to-package cryptographic
   handoff is proven; the manifest/release attestation remains a producer
   self-report.
4. The fake provider/API responses, fixture digest rows, and any test-only
   image-digest override are offline test inputs only. They are not provider,
   source, image, attestation, upload, or security proof.

## Checks

- `cargo check --locked -p velnor-workflow`: pass.
- `cargo fmt --all -- --check`: pass.
- `cargo clippy --locked -p velnor-workflow --lib --tests -- -D warnings`: pass.
- Configured `actionlint@1.7.12` over all 16 freshly rendered workflow YAML
  files with the generated `.github/actionlint.yaml`: pass, rc=0, no
  diagnostics.
- Fresh locked generator render: pass; checked-in `--check`: fail as listed
  above.
- No hosted provider, release, artifact upload, install, Docker, Mac Docker,
  candidate binary, hostile probe, or publish endpoint was contacted or run.
