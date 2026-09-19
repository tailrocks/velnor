# G1 generated recovery review — PR955 (historical exact head)

Date: 2026-09-20

## Identity and scope

- PR: `tailrocks/velnor#955`
- Exact reviewed head: `f890999cfbcc966bc5ab42aad48b1afcc0a79722`
- Exact reviewed base: `e713841bdb9c33d853b7a9af88ceac924af1b3b6`
- Historical checkout: clean detached worktree `/tmp/g1-pr955-exact.6aXRF4`
- PR diff is exactly four generated files (40 additions/40 deletions):
  `.github/ci/.github-actions-generator-state`,
  `.github/workflows/ci-unit-rust.yml`,
  `.github/workflows/preview.yml`, and
  `.github/workflows/release.yml`.
- `git diff HEAD^ HEAD -- .github-gen .github/ci/project.toml crates .github/actions .github/workflows/ci-main.yml .github/workflows/ci-pr.yml .github/workflows/ci-policy.yml .github/workflows/ci-runtime-products.yml` is empty: source, generator config, provider config, bootstrap action, and runtime-product publisher are untouched.
- Candidate generated file SHA-256 values:
  - state `4dec0c99f70219b8d71c77be6ed4aaf790a3434d2285d984948367ab24bcdcaf`
  - `ci-unit-rust.yml` `727805be6d345ed24f4b028756135ae889f21c7e931f3fa5f37ef9ab93acc657`
  - `preview.yml` `69b8bdc42acd50162e4b7d2c5cf7b5b6064eff681131180d3be50170da7a5d48`
  - `release.yml` `5e3a8f4826f8074ed8d8b1394c898d3b3bd9febb06a38644b43ef54e50d9bc3f`

## Provenance and deterministic rendering

The declared generator pin is `fdeed261bd2247a38db6922a7726cd45d3d6f31e` (state/config at `.github/ci/.github-actions-generator-state:4-6` and `.github-gen/velnor-workflow.toml:5-9`). The published release was independently inspected:

- tag `velnor-workflow-runtime-v1-81ba31f87a699c4e` is non-draft, non-prerelease and targets `fdeed261bd2247a38db6922a7726cd45d3d6f31e`;
- published `manifest.json` SHA-256 is `f62da45ca4559249a222214c8ff2bf2d7f009b367f21ab78b47984d1ee3ee205`;
- manifest closure is `81ba31f87a699c4e24e68baa1cf7cd0b5d765e5970934ec72c38b83772b274dc`, revision is fdeed, profile is release, features are empty;
- the three manifest asset digests match the GitHub release asset digests: Linux-X64 `7e51258a4c670df88d4fd2984a8cc175206ada24a3c27d75aeba8ecab853b05c`, Linux-ARM64 `46c28001bf4f3ee50bdfc498c0e5aa7b375c3f4dd6ca702f675d959c409d14e4`, macOS-ARM64 `626cd30e77f2161db2e2771bdefe25f61eed4ae8b92d2b5213a44530daa4431a`;
- local renderer `/private/tmp/velnor-fdeed-generator-published-target/release/velnor-workflow` reports revision fdeed and closure 81ba; SHA-256 is `f45dab7c8c174b87aeaaddc62ea9d24dae512f1cfb54f73efe22a9c0a60a2fcf`;
- exact pinned renderer command `/private/tmp/velnor-fdeed-generator-published-target/release/velnor-workflow --plain --check /tmp/g1-pr955-exact.6aXRF4` passes with no generated drift;
- exact source test `rtk cargo test -p velnor-workflow --no-default-features --lib s2::tests::checked_in_workflows_match_the_generator_byte_for_byte -- --exact`, run in the clean f890 checkout, fails on `release.yml` because current source renders the newer `a20e1ffc...` action pin while the fdeed pinned renderer emits `7234d3dd...`. This is the known pin-boundary failure, but it is not waivable for G1.

Thus the four bytes are reproducible from the declared published renderer, but they are not compatible with the current source/runner contract.

## Exact behavior changed

The workflow content changes are only Mr. Boxington action refs plus generator-owned state:

- `.github/workflows/ci-unit-rust.yml:335,819`, `.github/workflows/preview.yml:303,517,658`, and every corresponding release job in `.github/workflows/release.yml` change `jdx/mr-boxington-action@a20e1ffc... # v1.3.1` to `@7234d3dd... # v1.3.0`.
- `.github/ci/.github-actions-generator-state:5-6,22,25-26` records the fdeed renderer's scan/generator ownership and output hashes.
- No provider, publisher, release target, runtime bootstrap, credentials, or source behavior is changed by this PR.

The source contract contradicts that generated action ref: `crates/velnor-workflow/src/s2/mod.rs:268-275` renders/adopts `a20e1ffc... # v1.3.1`, and `crates/velnor-runner/src/manifest.rs:452-460` admits only that same ref. The compiled-manifest guard at `manifest.rs:2024-2085` rejects any release action ref absent from the manifest.

## Independent checks and hosted evidence

- `rtk actionlint -config-file .github/actionlint.yaml`: pass.
- `rtk git diff --check HEAD^ HEAD`: pass.
- Exact PR hosted run `35466800154` (`CI / PR`) completed failure: 16 checks pass, 4 fail, 48 skip. Failing jobs are `ci-required`, `Control / Required`, `rust-velnor-workflow / GitHub hosted`, and `rust-velnor-runner / GitHub hosted`; Policy and DCO are green but do not override required CI.
- `rust-velnor-workflow` hosted log ends `error: generated files differ: .github/workflows/ci-unit-rust.yml, .github/workflows/preview.yml, .github/workflows/release.yml, .github/ci/.github-actions-generator-state; rerun generate`.
- `rust-velnor-runner` hosted log fails `manifest::tests::release_workflow_action_refs_are_compiled_into_the_manifest` with `release action ref is absent from manifest: jdx/mr-boxington-action@7234d3dd...`; 1,219 tests passed, 1 failed, 5 skipped in that job.

## Bootstrap, hosted-only policy, native preservation, and DAG

The intended architecture is present but does not rescue this head:

- `.github-gen/velnor-workflow.toml:18-20` keeps both providers and makes only `github-hosted` automatic; the Velnor lane remains dispatch-capable. The PR does not modify this config.
- `release.yml:29-55` admits hosted publishing and rejects Velnor-only/native release dispatch. `preview.yml:185-213` and `release.yml:57-79` bootstrap through the local setup action and the pinned fdeed revision.
- `ci-runtime-products.yml:47-75` restricts product publication to `refs/heads/main`, computes a canonical closure, and names the immutable release tag. `:90-107` builds Linux X64, Linux ARM64, and macOS ARM64 natively; `:201-311` requires closure/build, verifies transport digests, self-reported closure/revision, attestations, and only then creates the release.
- The consumer setup action resolves the full revision to a closure, downloads the immutable closure tag, verifies attestations/manifest/digest/self-report, and never compiles (`.github/actions/setup-velnor-workflow/action.yml:1-26,102-184`).
- D19 shallow pin acquisition is explicit at `.github/workflows/ci-unit-rust.yml:498-523`: it extracts the full SHA and uses `git fetch --no-tags --depth 1` only when absent. Candidate generation similarly fetches PR/base/pin shallowly at `:559-583`.
- Preview's `identity -> guest-payload/metadata -> debian -> sign-deb -> publish` and runtime producer's `closure -> build -> publish` are acyclic. The producer is independent of consumer pin adoption, so the valid adoption sequence is: publish the current source's e713 closure from protected main, then regenerate/update the pin in a later commit/PR, then rerun byte and hosted gates.

## PR feedback inventory

All REST collections were read with `gh api --paginate`:

- one Codex review (`5257493920`, `COMMENTED`) and one inline thread (`4054570434`, `.github/workflows/release.yml:2171`, P1);
- one issue summary comment (`5744988771`), no human reviews, no replies or additional threads.

The inline P1 is actionable and independently confirmed. Therefore unresolved feedback is **not empty**; it identifies the same generated/source pin mismatch proven by hosted CI.

## Verdict

**REJECT PR955 for both source and generated scope. Do not merge and do not mark G1 complete.** The pinned output is reproducibly rendered and the recovery DAG design is structurally sound, but f890 emits an action ref that the current runner manifest rejects, and the exact hosted source renderer also fails the generated-byte gate. A replacement candidate must use the newly published current-source/e713 runtime closure, regenerate all outputs from that authoritative renderer, pass the source byte test and runner manifest test, then obtain green hosted PR and post-merge main evidence. This review does not approve merge or substitute for hosted gates.
