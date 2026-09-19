# G1 hosted-provider exact review — `de70b482c21b947b78ceae2313f95873aa1e77fe`

## Scope

- Repository: `tailrocks/velnor`.
- Exact detached review tree: `/private/tmp/velnor-hosted-de70-clean`.
- Exact target: `de70b482c21b947b78ceae2313f95873aa1e77fe`.
- Rendered candidate tree: `/private/tmp/velnor-hosted-de70-render`.
- Diff scope: both release renderers, generated `preview.yml`/`release.yml`, and generator state.
- No source, branch, remote, publication, or install state was changed.

The PR moved after this target. GitHub now reports head `5aeda1b7aa53b7a7139fd1e0ad27eaeb78df6f12` (`use generic release metadata path`). That follow-up is outside this exact review and is not approved here.

All PR960 feedback was fetched through paginated REST and GraphQL endpoints. Every relevant connection reported `hasNextPage: false`. The exact target has one unresolved Codex P1 review thread: [#4054931178](https://github.com/tailrocks/velnor/pull/960#discussion_r4054931178).

## Verdict

**Reject the exact target. No merge-ready approval.**

The target fixes the immediate Velnor Debian identity path and preserves the clean-tree gate, but it violates the generic generator contract in both renderers. The focused release suite also fails both pinned identity-surface tests. Actionlint exits zero, but reports unquoted temp-path uses that fail under a hostile path containing spaces.

## Blocking finding — generic renderer hardcodes consumer identity

Both generic renderers emit the estate-specific directory `velnor-release-metadata`:

- `crates/velnor-workflow/src/primitives/release.rs:1530-1545`
- `crates/velnor-workflow/src/s2/primitives/release.rs:1459-1474`

The generated Velnor workflows therefore contain:

```yaml
path: ${{ runner.temp }}/velnor-release-metadata
metadata_dir="$RUNNER_TEMP/velnor-release-metadata"
```

`crates/velnor-workflow/AGENTS.md` requires this renderer to remain repository-neutral. A different repository using the generator receives Velnor-specific artifact paths. This is the unresolved Codex P1 above, not a documentation-only concern.

## Metadata producer/consumer census

- Preview metadata producer uploads `build-identity.json`, `manifest.json`, and the release tool as `preview-metadata`.
- Stable metadata producer uploads the same files as `release-metadata`.
- Preview and stable Debian identity jobs download those artifacts to the runner temp directory and stage identity files from that directory; no `path: metadata` remains in either generated identity job.
- Preview publication intentionally downloads `preview-metadata` to `preview-metadata` and verifies each packaged record with the downloaded release tool.
- Stable publication downloads `release-metadata` into `artifacts` and assembles the release record there.
- The stable image-index job downloads `release-metadata` without a `path`, but it does not check out source or run the release-build clean-tree gate, and it does not consume identity files. This is not evidence that the identity gate accepts stale metadata.

The preview metadata producer and preview Debian consumer still compare `build-identity.json.source_sha` with the admitted preview source commit. The release tool's package-record emission compares the record commit with its embedded binary source SHA, and installed-package checks compare the shipped manifest hash with the installed binary manifest. No new stale-metadata acceptance was demonstrated in this exact target; a real hosted run is still required for the publication path.

## Clean-checkout reproduction

In a fresh worktree at the exact target:

1. Simulating the old artifact download with `metadata/build-identity.json` produced `?? metadata/` from `git status --porcelain=v1`.
2. Moving the same artifact under an explicit runner-temp equivalent (`.../velnor-release-metadata`) left `git status --porcelain=v1` empty.

All three exact review/render/reproduction worktrees were clean after the checks.

## Path-safety finding

The generated identity scripts leave these uses unquoted:

```sh
test -s $metadata_dir/...
jq ... $metadata_dir/...
cp $metadata_dir/... ...
sha256sum $metadata_dir/...
```

`actionlint -config-file .github/actionlint.yaml .github/workflows/*.yml` exits 0 but reports `SC2086` for these lines in both preview and release. A bash fixture with `RUNNER_TEMP=".../runner temp"` returned status 1 from unquoted `sha256sum` and status 2 from unquoted `jq`; the quoted equivalents returned 0. GitHub-hosted paths are normally space-free, but the generic workflow emits a path that is not robust to valid runner-temp paths or glob characters.

## Verification

- Generator render/check, from `crates/velnor-workflow`: passed; candidate tree unchanged. It reports the expected pin drift: candidate render `c99a402b2893537cedf4b878e8fec1155592b731d6f852943ceaf25a5edb2ac5` differs from declared generator revision `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`.
- `actionlint -config-file .github/actionlint.yaml .github/workflows/*.yml`: exit 0, with the `SC2086` findings above.
- `CARGO_TARGET_DIR=/private/tmp/velnor-hosted-de70-target rtk cargo test --locked -p velnor-workflow --lib primitives::release --all-features -- --nocapture`: **212 passed, 2 failed**. Failures: legacy and S2 `identity_rows_render_the_pinned_native_surface`; observed hashes were legacy release `885823130bbfa378588376817b27ef6de85cbe5e25f1b26e57292f23cb862940`, preview `3160639a8576c96556c9ceaa15509c9c8896676faf6c8200d8ccfc04e64efe23`; S2 release `818bb312138c64e16c0e89a42751cf9fb42011021de513b7343949099977166b`, preview `4bbd68634c6662f9d354a8c63bb51638e7907562d29f09763e49638bd4fcbb4a`.
- Focused identity-path test: **2 passed** (`native_identity_build_records_binary_digest_and_debian_reuses_it`).
- `rtk cargo clippy --locked --profile test --all-targets --all-features --package velnor-workflow -- -D warnings`: passed.
- `rtk cargo fmt --all -- --check`: passed.
- `rtk git diff --check b5a4b4afaa6ca807927cacc03659b570a895dd5c de70b482c21b947b78ceae2313f95873aa1e77fe`: passed.

## Required before re-review

1. Use one repository-neutral temp directory name in both renderers and regenerate both workflows; do not preserve `velnor-release-metadata` in generic output.
2. Quote every `$metadata_dir/...` expansion and add a hostile temp-path fixture.
3. Refresh the declared generator pin only after the accepted renderer and generated outputs are finalized; make both pinned identity tests pass.
4. Re-run hosted CI and review the current PR head separately. This exact report does not approve `5aeda1b` or any publication/install result.
