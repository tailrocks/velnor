# Pinned Velnor runtime product verification

Retrieved 2026-09-21 on a Darwin arm64 host with GitHub CLI 2.101.0. Both releases are public, published, non-draft, non-prerelease, and their release tags resolve to the exact declared source commits.

| Declared source pin | Release | Source closure | Platform product SHA-256 |
| --- | --- | --- | --- |
| `38dbf85e0bbf278cee3ec39ab90a9acc9b5b67a9` | [runtime-v1-32a822a584aa93d8](https://github.com/tailrocks/velnor/releases/tag/velnor-workflow-runtime-v1-32a822a584aa93d8) | `32a822a584aa93d8164fdbcb93220126ad24ac62bbfdef325d48f492146fef72` | Linux-X64 `5818b737da606db0194971c1d3c939ce945276ae72a006b424b434b11d15dc52`; Linux-ARM64 `6aa958327f25814914a6e0f96f953db7941cab00f938fcffcc7be770b834a0ef`; macOS-ARM64 `51a8029c91a451ec3007dc32152c62ee171337bebba2aa27c61e4e7878ad49a9` |
| `4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d` | [runtime-v1-af140ad4d8d84326](https://github.com/tailrocks/velnor/releases/tag/velnor-workflow-runtime-v1-af140ad4d8d84326) | `af140ad4d8d84326d5676f626ce4261fd7914ac41d521d851484dcb8253270e7` | Linux-X64 `672de6b171eba0f9e8053e96974cf361a6788af22ca2d498eabba71b1d73c236`; Linux-ARM64 `1df81f61fc3a654e30bd872077085892c57c74e4bfb156f8cf5dbabe8b25b97b`; macOS-ARM64 `7bee5aabdea114e3c7a869101b7bd951619455605addfcb896e69e229e5af49c` |

Downloaded `manifest.json` and all three platform binaries for each tag to this directory. For both pins, the locally recomputed closure matches the product manifest and tag suffix; each binary hash matches both the signed manifest product entry and GitHub Release API asset digest. File inspection identifies Linux-X64 as x86-64 ELF, Linux-ARM64 as AArch64 ELF, and macOS-ARM64 as Mach-O arm64.

All eight attestations (two manifests plus six binaries) pass `gh attestation verify` constrained to owner `tailrocks`, signer workflow `tailrocks/velnor/.github/workflows/ci-runtime-products.yml`, and source ref `refs/heads/main`. The verified SLSA provenance binds each subject digest to the exact source pin and GitHub-hosted producer workflow; each includes a verified Rekor timestamp. Full verifier output is saved beside each artifact as `*.attestation.json`. [product-audit.json](product-audit.json) records the checked fields and digests.

Both macOS ARM64 products execute successfully. The 38db binary reports revision `38dbf85e0bbf278cee3ec39ab90a9acc9b5b67a9` and closure `32a822a584aa93d8164fdbcb93220126ad24ac62bbfdef325d48f492146fef72`; the 4fa binary reports revision `4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d` and closure `af140ad4d8d84326d5676f626ce4261fd7914ac41d521d851484dcb8253270e7`.

Check-only deterministic renders: the 4fa binary reports Jackin's generated files current (exit 0). The 38db binary exits 1 against the current Velnor worktree because its staged generated files differ from the still-declared 38db renderer; the reported drift is in project.toml, ci-main, ci-pr, ci-unit-bun/docker/docs/opentofu/rust, preview, release, and generator state. No repository files were changed by these checks. Logs are [jackin-render-check.log](jackin-render-check.log) and [velnor-render-check.log](velnor-render-check.log).

For regeneration/checking with the declared pin on macOS arm64:

```sh
# Jackin: direct immutable pin check
/Users/donbeave/Projects/work/ci-evidence/runtime-products/4fa7a3a85f141a6bb95bc9bdf0eef9e3ddde165d/velnor-workflow-macOS-ARM64 --plain --check /private/tmp/jackin-pr1007-runtime-refresh

# Velnor current-source D19 check, from crates/velnor-workflow
VELNOR_WORKFLOW_PINNED_BINARY=/Users/donbeave/Projects/work/ci-evidence/runtime-products/38dbf85e0bbf278cee3ec39ab90a9acc9b5b67a9/velnor-workflow-macOS-ARM64 mbx run --locked --manifest-path Cargo.toml -- --plain --check ../..
```

**Publisher gate remains incomplete:** the current `ci-runtime-products.yml` publishes on a main push (and allows manual dispatch) after build identity, attestation, and smoke checks, but does not require successful full `ci-main` validation for that exact source SHA. These signatures prove the named workflow built the artifacts from the pins; they do not prove full main CI passed. Do not claim these are full-CI-admitted products until publisher admission is gated to a successful, repository/branch/event/SHA-matched main run.
