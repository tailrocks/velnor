# Native product exact independent review — `578a3470f4871ada32916429099627823e861126`

Status: **REJECT exact handoff**. The source renderer has strong fail-closed
properties, but the checked-in generated workflows are stale and the exact
Homebrew consumer cross-check still fails. This is a source/integration review;
no GitHub release, publication, macOS installation, Docker operation, or host
runtime was performed.

## Pin and review boundary

- Repository: `tailrocks/velnor`.
- Branch: `codex/github-first-native-product-v3`.
- Exact `HEAD` and remote branch: `578a3470f4871ada32916429099627823e861126`.
- Parent: `8aaa4c835937b9b99e35c643e889a40fd2226091`.
- Worktree: `/tmp/dual-lane-native-product-v3` (clean, remote-equal).
- Candidate diff: one source file, `crates/velnor-workflow/src/s2/primitives/release.rs`,
  3 insertions and 1 deletion. `git diff --check` passes.

The candidate adds a preview tag-ref check at
`crates/velnor-workflow/src/s2/primitives/release.rs:2133-2139` and changes
preview attestation lookup from repository-plus-owner to owner-plus-signer at
`:2242-2246`. The local `gh attestation verify --help` confirms `--owner` is a
valid minimum identity selector and recommends `--signer-workflow`.

## Capability matrix

| Requirement | Evidence | Verdict |
|---|---|---|
| One native-product publisher; build lane cannot publish | Source producer is documented build-only at `crates/velnor-workflow/src/s2/primitives/native_product.rs:479-480`; no release API is emitted by that producer. The native preview workflow has the same declaration at `.github/workflows/native-product-preview.yml:1-2`. | **Source PASS. Exact integration incomplete**: generated preview still uses the old rolling Debian publisher (`.github/workflows/preview.yml:1073-1110`) and has no immutable native-product publisher. |
| Immutable preview release and numeric provider identity | Source preview publisher uses a full-commit candidate tag and serial concurrency at `release.rs:1977-1995`; 404-only create, exact tag/name/target, positive decimal provider ID at `:2121-2148`; product manifest binds the same ID/source/tag at `:2215-2223`. | **Source PASS; exact checked-in preview FAIL**. The checked-in `preview.yml` publishes only Debian assets and `release-manifest.json` (`:1057-1110`). |
| Source/tag binding | Initial provider release and tag-ref checks are present at `release.rs:2135-2137`; final provider verification checks release fields at `:2300-2307`. | **Residual race FAIL**. A tag can move after the first ref read and before/after publication; the final check does not re-read `git/ref/tags/$PRODUCT_RELEASE_TAG`. Release `target_commitish` alone is not an immutable tag-ref proof. |
| Attestation | Every preview product subject is attested and verified against owner, signer workflow, source ref, and full source digest at `release.rs:2235-2246`. Stable product assets are likewise attested before release handling (`:2481-2488`). | **Source PASS; exact preview integration unavailable** because stale generated workflow has no product-assets path or product attestation. |
| Archive parent binding and member census | Preview assembly writes `parent_manifest_id=$PRODUCT_RELEASE_ID` into both archive records and creates a fixed member list at `release.rs:2181-2197`. Runner admission requires root-only regular members, exact set, member digests, parent ID, and complete component tuples at `crates/velnor-runner/src/product.rs:737-942`. | **PASS at source/consumer contract level**. Exact Homebrew evidence independently confirms parent/source/mode checks pass. |
| Executable-mode boundary | Producer restores `0755` before artifact upload and archive assembly restores/checks it (`release.rs:2061-2068`, `:2183-2188`). The prior adjudication remains: downloaded loose-file `0644` normalization is restored; a final archive member that remains non-executable is rejected. | **PASS for mode policy**. Do not weaken the consumer to accommodate a bad final archive. |
| Strict Homebrew consumer | Exact rendered bytes at this SHA pass parent binding, source identity, archive census, and `0755` checks, but the unchanged consumer rejects every synthetic macOS sibling: `filetype 7463726f`/`722d726f`/`772d726f`, not Mach-O `MH_EXECUTE`. Direct consumer exit status is 1. Evidence: `G0/homebrew-contract/native-578-exact-crosscheck.md`, SHA-256 `98a0c5dec7f476c2e4cd17c21a9478390c1818bee7c17c9432af041da6d2bd4`. | **FAIL / blocking**. The fixture writes only a magic/CPU prefix and label; it is not a valid native executable. |
| Intel macOS 27 target | Typed census retains all four targets and declares Intel blocked in `.github/ci/native-product-preview-contract.json:5-7,53-58`. The producer emits an explicit failing blocked job at `native_product.rs:381-390`; generated workflow shows it at `.github/workflows/native-product-preview.yml:17-27`; preview publication rejects nonempty `blocked_targets` at `preview.yml:976-990`. | **Honest fail-closed blocker, not silent success**. No Intel fallback or exclusion is acceptable. Preview product publication cannot pass until a real macOS 27 Intel capability exists or the upstream contract explicitly changes scope. |
| No cycle / canonical sidecar | Preview manifest excludes itself and its sidecar from artifact rows at `release.rs:2215-2218`; sidecar is canonical `sha256  product-manifest.json`; verifier consumes the external checksum. | **PASS for preview source design**. No self-referential manifest cycle found. |
| Race/reconciliation behavior | Preview asset reconciliation compares existing bytes and sizes, rejects published omissions, recovers upload races, requires exact final asset census, then verifies every published byte at `release.rs:2247-2323`. Provider lookup fails closed for non-404 responses (`:2121-2134`). | **Mostly PASS, with two bounded residuals**: final tag-ref recheck is missing (above); stable draft path bulk uploads and flips draft false without an immediate remote byte/census re-read (`release.rs:2727-2743`, generated `.github/workflows/release.yml:4555-4575`). |
| Generated/source agreement | Generator check from the exact worktree: `cargo run --locked --manifest-path Cargo.toml -- ../.. --plain --check` fails with differences in `.github/workflows/native-product-preview.yml`, `.github/workflows/native-product.yml`, `.github/workflows/preview.yml`, and `.github/ci/.github-actions-generator-state`. Source preview inputs exist at `native_product.rs:468-475`, but checked-in native preview has only bare `workflow_call` (`.github/workflows/native-product-preview.yml:6-8`) and the caller passes no inputs (`.github/workflows/preview.yml:252-257`). | **FAIL / blocking**. The exact checked-in CI cannot realize the candidate source renderer. |

## Verification executed

All commands were read-only in the detached exact worktree:

- `cargo fmt --all -- --check`: pass.
- `cargo test --locked -p velnor-workflow --lib native_product -- --nocapture`:
  7 passed, 1,737 filtered.
- `cargo test --locked -p velnor-workflow --lib rendered_native_product_assembly_produces_runner_and_homebrew_contract_bytes -- --nocapture`:
  1 passed, 1,743 filtered.
- `cargo test --locked -p velnor-workflow --lib native_identity_release_wires_release_build_and_deb_publishing -- --nocapture`:
  2 passed, 1,742 filtered.
- `cargo test --locked -p velnor-workflow --lib rendered_native_product_serializer_emits_runner_compatible_sidecar -- --nocapture`:
  1 passed, 1,743 filtered.
- `cargo test --locked -p velnor-runner --lib product -- --nocapture`:
  23 passed, 2,336 filtered.
- Generator `--check`: **failed** as documented above.

The unit/render tests are source-fixture proof only. They do not override the
strict Homebrew consumer result or the generated-workflow drift.

## Required bounded follow-up

1. Regenerate all tracked workflows and generator state from the exact
   candidate renderer, then rerun generator `--check`; the regenerated preview
   caller must pass the three typed product inputs and the publisher must be
   present in the checked-in workflow.
2. Add a final preview tag-ref read immediately before publication and again in
   post-publication verification. Add a rendered race test proving a moved tag
   cannot yield a published candidate. For stable draft publication, reuse the
   immutable asset reconciliation/census before flipping `draft=false`.
3. Replace the six synthetic macOS fixture binaries with real thin
   `MH_EXECUTE` fixtures or actual build artifacts (correct CPU and filetype),
   then rerun the unchanged Homebrew consumer against the exact product bytes.
   Do not bypass `verify-macos-binary.sh`.
4. Keep the Intel target explicitly blocked until a real macOS 27 Intel runner
   is available. Do not turn this blocker into an omitted target or green
   publication.
5. Re-run the cross-lane APT parent-binding review before handoff. The prior
   independent report still records that APT requires `parent_manifest_sha256`
   in its release/compiled manifests while this Velnor candidate only binds
   archive `parent_manifest_id`; 578 changes no APT field.

No approval or publication claim is made by this report.

## Post-review shared-worktree boundary

After the exact-pin inspection and recorded checks completed, the shared
detached directory showed uncommitted fixture-only edits in
`crates/velnor-workflow/src/s2/primitives/release.rs` and a new
`target-jobs2/` build directory. Those bytes are not part of
`578a3470…`; they were not reviewed, tested, or used to change this verdict.
