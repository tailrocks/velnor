# Runtime 38db provenance review

Status: PASS for the published runtime product and its provenance. Conditional for use with the campaign generator: the product is the exact upstream 38db renderer; it is source-compatible with live main 386a at the generator source level, but it does not contain campaign-local generator changes from 7864.

## Product

- Source revision: `38dbf85e0bbf278cee3ec39ab90a9acc9b5b67a9`.
- Runtime producer: [run 35513670974](https://github.com/tailrocks/velnor/actions/runs/35513670974), `Runtime products · push · main`, event `push`, attempt 1, completed `success`; all five jobs succeeded: closure, Linux ARM64, Linux X64, macOS ARM64, and publish.
- Release: [velnor-workflow-runtime-v1-32a822a584aa93d8](https://github.com/tailrocks/velnor/releases/tag/velnor-workflow-runtime-v1-32a822a584aa93d8), published and non-draft; release target is exactly 38db.
- Closure: `32a822a584aa93d8164fdbcb93220126ad24ac62bbfdef325d48f492146fef72`.
- macOS ARM64 binary: `/tmp/velnor-runtime-38dbf85e/runtime-macos/velnor-workflow-macOS-ARM64`; SHA-256 `51a8029c91a451ec3007dc32152c62ee171337bebba2aa27c61e4e7878ad49a9`.
- Local binary `--revision` returns 38db; `--closure` returns the manifest closure. `closure --rev=38db` returns the same closure. `closure --candidate` is `9e6bb04aed5e5a89197b63de0a328d9c5cd3bd23138e9d24fd339d59d8502c5a`, so candidate and pinned source identities remain distinct.
- Manifest: `/tmp/velnor-runtime-38dbf85e/release/manifest.json`; SHA-256 `187b9f9ffb3b9a1339da5b8d9d32c9d6fe9d10f8ffd0e8ea974c1e972be1cf80`. Manifest lists release profile, empty features, the same closure/revision, and Linux X64, Linux ARM64, and macOS ARM64 asset names/digests. The downloaded macOS binary matches its sidecar and the product digest in the manifest. The separately attested manifest has its own digest listed above.

## Trust evidence

`gh attestation verify` passed for both the macOS asset and `manifest.json`, with signer workflow `tailrocks/velnor/.github/workflows/ci-runtime-products.yml` and source ref `refs/heads/main`.

Both attestations resolve the same source Git commit 38db, workflow SHA 38db, repository `tailrocks/velnor`, push trigger, GitHub-hosted runner, and invocation `35513670974/attempts/1`. The manifest attestation subject is the exact local manifest digest. The binary attestation subject is the exact downloaded macOS binary digest.

The publish log also records artifact transport digest checks for all three platform assets, manifest field checks, binary self-report checks, asset and manifest attestation checks, and installed-layout smoke checks before release creation. Durable raw evidence is stored under `../observations/runtime-38db-*`; producer logs use gzip:

- `runtime-run.json`, `runtime-jobs.json` and the five `job-*.log.gz` files.
- `release.json`, `manifest.json`, `binary-attestation-verify.json`, `manifest-attestation-verify.json` (each with the `runtime-38db-` prefix).

Separate CI/Main and Preview runs for this head were failures; those are not the runtime producer. The dedicated runtime producer is the successful trusted source for this product.

## Live-main compatibility

The source comparison `9e5c0eb2..386a5b63` has only the upstream release-leg seed/D19 changes plus the D19 pin/config regeneration:

- `461cb4e5` adds the shared D19 pin-fetch commands and restores mutable Docker seed handling in release legs.
- `b26eff15` regenerates `release.yml` for those renderer changes.
- `20adffd3` changes the declared D19 revision to 38db.
- `git diff 38db..386a5b63` is empty under `crates/velnor-workflow/src/s2`, `build.rs`, and the generator project/config inputs; it only changes `.github-gen/velnor-workflow.toml` from the staging pin to 38db and refreshes generated ownership hashes. The later 386a changes are runner hardening outside the generator source.

Therefore the 38db binary is compatible with live-main generator source inputs at the upstream level and is the declared live-main base product. It cannot render campaign-local generator edits from 7864 (for example the current IR/release changes and generator revision 67/68); those require the separately built/audited candidate renderer and a new generated-state revision. Do not present this 38db product as proof for campaign-local generator bytes.

## Limits

Direct local asset verification covered macOS ARM64. Linux asset transport and native self-report/attestation checks are evidenced by the successful publish log; Linux binaries were not downloaded independently here. No performance or speedup claim follows from this provenance review.

Release immutability reported by GitHub: `false`. Acquisition verifies digests and attestations on each restore; mutable release metadata is not an immutable storage guarantee.
