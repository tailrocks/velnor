# Native product publication-boundary independent review — `cf5f34b4cf7d2107b989342077e4a65ea90c2177`

Verdict: **reject exact publication/handoff**. The new stable draft boundary is
substantively fail-closed in the source renderer and passed an actual extracted
rendered-shell mock-provider suite. The exact checked-in integration is not
admissible: generated files are stale, and regenerating the exact source emits
an actionlint-invalid preview workflow. The already-published stable rerun path
also does not carry the new exact tag-ref/census policy.

No GitHub API publication, release, dispatch, install, Docker, macOS, or host
runtime was performed.

## Pin and boundary

- Repository: `tailrocks/velnor`.
- Remote branch: `origin/codex/github-first-native-product-v3`.
- Exact `HEAD`: `cf5f34b4cf7d2107b989342077e4a65ea90c2177`.
- Exact parent: `e2195e591177ed899f41517f4346dcb3c88e33b9`.
- Exact tree: `5a4064bcc83d7b6daa05fe87be8859e41f4ef568`.
- Detached review worktree: `/private/tmp/velnor-native-cf5-review`.
- Candidate diff: only `crates/velnor-workflow/src/s2/primitives/release.rs`,
  75 lines (`68` insertions, `7` deletions); `git diff --check` passes.
- Rendered output was generated outside the source tree at
  `/private/tmp/native-cf5-generated` with the exact pinned renderer.

## Findings

### Blocker 1 — checked-in workflows do not contain cf5's boundary

`cargo run --locked -p velnor-workflow -- --plain --check` fails:

```
generated files differ: .github/workflows/native-product-preview.yml,
.github/workflows/native-product.yml, .github/workflows/preview.yml,
.github/workflows/release.yml, .github/ci/.github-actions-generator-state
```

The checked-in release publisher still has the predecessor's bare upload and
flip at `.github/workflows/release.yml:4555-4575`; it has no
`verify_remote_assets`. The exact cf5 render has the new checks at generated
`release.yml:4576-4620`: provider identity, tag-ref commit, exact asset census,
downloaded digest and byte count before the flip, then the same checks after
the flip.

Selected exact-render/current-tree SHA-256 pairs:

| File | exact render | checked-in tree |
|---|---|---|
| `.github/workflows/release.yml` | `47e8434e15f9561c2524d843039b7760a48824c0336de0ea23ac35dde923b424` | `dd220e97673f355225f595a036d53d397e7516d18f2aad6a28a80f0260f180a7` |
| `.github/workflows/native-product.yml` | `65f39b828b5bc136f6b41be7591a0972b6f703147cfebcbc2a61ba3fcfb0adca` | `d6943a19eb2f160cc128489727c0314972540080c4ae25af5500971fa222ec6d` |
| `.github/workflows/native-product-preview.yml` | `6bbf1638c63268da9f6d0c613b9b3e196ddce9b698641e99ae99b333dd0ed52d` | `a00bf895a2f1603b99b18bbdd6592815ff8f9b2cc139919c8f3d2895fb3b7985` |
| `.github/workflows/preview.yml` | `1393b06aa3278d5b42cf98b0f67c80289796d5603f26e59508abe4572dcd7005` | `7000779621c61bb5cf4ad3e969f9dc8e5efbf86fac21a293bbb63706e23e186c` |
| `.github/ci/.github-actions-generator-state` | `8e1f37823ddf433d128b694c97d400df00063287f53160a5c30a9cc2bd028609` | `a019c50dbdb8341122364e073ed4c02c36f0970f73f0f6af9402ab7af1acc151` |

### Blocker 2 — exact source regeneration emits invalid preview YAML

`crates/velnor-workflow/src/s2/primitives/native_product.rs:473-492` builds
`preview_identity_env` with doubled GitHub-expression braces and inserts it by
global `replace("    steps:\\n", ...)`. This inserts the variables into every
job matching that text, including the blocked Intel job's `permissions` map.

The exact generated `.github/workflows/native-product-preview.yml` therefore
has:

- lines `34-36`: `PRODUCT_*_INPUT` interpreted as permission scopes under the
  blocked job;
- lines `72-74`: `${{{{ inputs.* }}}}` instead of `${{ inputs.* }}`.

Configured actionlint (`1.7.12`) rejects both the unknown permission scopes and
the malformed expressions. The existing source unit test only asserts that
the source contains the input names (`native_product.rs:713-729`); it does not
render and lint the workflow.

This is independent of the stale checked-in files: regenerating from the exact
source still fails the workflow syntax gate.

Evidence logs:

- [`native-cf5f34b4-actionlint-preview.txt`](native-cf5f34b4-actionlint-preview.txt), SHA-256 `d220f63b824ad426d024b3c4b26c64bfb0512b02d9e42cdffe241536483954a4`.
- [`native-cf5f34b4-generator-check.txt`](native-cf5f34b4-generator-check.txt), SHA-256 `a3ce96d1f43cdce554ade32edb0a8c847b2c22c18abc43c2100ee472e41cb134`.

### Residual — already-published stable reruns are weaker than the new claim

The `product_existing` shell at
`crates/velnor-workflow/src/s2/primitives/release.rs:2831-2924` checks provider
ID, tag name, target commit, draft/prerelease state, named downloads, and
manifest artifact digest/size. It does not:

- read `repos/$GITHUB_REPOSITORY/git/ref/tags/$tag` and require a commit object
  equal to `COMMIT`; or
- enumerate the provider's complete asset collection and reject extra assets.

Thus the new exact tag-ref/census policy covers the newly published draft path
but not the already-published/idempotent path. A moved tag whose release
`target_commitish` remains unchanged, or an extra unreferenced release asset,
can pass that path. This is a source residual even after generated files are
regenerated.

### Source positives and boundary honesty

The cf5 stable draft path at `release.rs:2773-2820` now:

1. uploads the complete runtime plus native-product asset set;
2. re-reads the release ID, tag, name, target commit, prerelease/draft state,
   and the Git tag's commit object;
3. requires exact remote asset names and unique IDs;
4. downloads every remote asset by ID and compares SHA-256 and byte count;
5. flips draft false once; and
6. repeats identity, tag-ref, census, digest, and byte-count checks after the
   flip.

The renderer explicitly says the provider does not make upload+flip atomic and
only claims an immediate post-flip observation. It makes no false
linearizability claim against an administrator mutating the release after the
last read.

The preview path inherited from e219 has corresponding pre-publication and
post-flip identity/tag-ref/census/digest/size checks at `release.rs:2290-2369`.

The native build producer remains read-only: `native_product.rs:479-498` emits
`contents: read` and documents that it does not mutate provider releases; the
stable release renderer is the sole provider publisher. No second native
publisher or fabricated native attestation authority was found.

## Actual rendered-shell transition test

The harness extracts the `run: |` body from the exact rendered
`/private/tmp/native-cf5-generated/.github/workflows/release.yml`, substitutes
only runtime GitHub expressions with fixture values, and executes that shell
with an exported mock `gh` provider. It does not reimplement
`verify_remote_assets` or rely on string-order assertions.

- Harness: [`native-cf5f34b4-boundary-mock.sh`](native-cf5f34b4-boundary-mock.sh)
- Harness SHA-256: `b437e9f95ef9fd54bbae0b2cfbc3fa2a678690fddb3312cbec9d24162f2600fa`
- Result log: [`native-cf5f34b4-boundary-results.txt`](native-cf5f34b4-boundary-results.txt)
- Result-log SHA-256: `1631d70016838e763078f2204242216089778eff20e43104c47b45fb99377f04`

Observed matrix:

| Mock state | Result | Evidence |
|---|---:|---|
| exact draft → exact published | pass | publish success |
| provider JSON `.size` metadata changed, bytes unchanged | pass | metadata is not trusted; downloaded bytes are checked |
| wrong release ID/source/tag | fail | draft identity error |
| wrong Git tag ref commit/type | fail | draft tag-ref error |
| missing, extra, duplicate asset | fail | draft census error |
| replaced/size-mutated downloaded asset | fail | draft digest error |
| concurrent draft flip before draft admission | fail | draft identity error |
| post-flip provider source mismatch | fail | published identity error |
| post-flip asset replacement | fail | published digest error |

The `metadata_size` pass is deliberate: the implementation uses downloaded
content as the payload authority and does not trust a provider-reported JSON
size field. If the contract requires that untrusted metadata field itself to
match, that would be an additional check; it is not needed to establish the
downloaded bytes' digest and size.

## Verification

All commands were read-only against the detached source tree unless noted as
external generated/evidence output.

- `cargo fmt --all -- --check`: pass.
- `git diff --check e2195e59..cf5f34b4`: pass.
- `cargo test --locked -p velnor-workflow --lib native_identity_release_wires_release_build_and_deb_publishing -- --nocapture`: 2 passed.
- `cargo test --locked -p velnor-workflow --lib rendered_native_product_assembly_produces_runner_and_homebrew_contract_bytes -- --nocapture`: 1 passed.
- `cargo test --locked -p velnor-workflow --lib product_build_lane_has_no_second_publisher_or_old_intel_fallback -- --nocapture`: 1 passed.
- `cargo check --locked -p velnor-workflow`: pass.
- `cargo clippy --locked -p velnor-workflow --lib --tests -- -D warnings`: pass.
- Configured actionlint on exact generated `release.yml`: pass.
- Configured actionlint on exact generated `native-product-preview.yml`: fail as described above.
- Generator `--plain --check`: fail on the five stale generated files listed above.
- Actual rendered-shell harness: all expected pass/fail cases behaved as expected.

No release, publication, dispatch, installation, or publication approval is
made by this report.
