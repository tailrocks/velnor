# B3 release staging (live, updated through the cut)

## Source chain
- 15c6bc2e Merge #938 (B1 generator fixes) — parents 05980b7a (#937 unbounded-N) + 6e0d40b4
- 653d1fbc Merge #939 (B3 pin-bump ec399527 → 15c6bc2e) — parents 15c6bc2e + 91693e68
- PENDING #940 (attestation-order test, render-neutral) → T2 = tag point
- Tag: v0.1.275 at T2 (crate velnor-runner 0.1.275 == Cargo.lock == tag; verify-tag requires tag == origin/main tip)

## Generator product driving the release
- Product: velnor-workflow-runtime-v1-a55ade440d4fd658
- Closure: a55ade440d4fd658975614f688a617032293c003a3449f148590c985ff34677b (revision 15c6bc2e)
- Published by: run 35295222506 (push main 15c6bc2e), all 3 platforms + manifest, attestations verified
- FORBIDDEN: 32565cc6 (pre-fix closure, run on 048a7bda) — must not drive B3/B4
- #940 adds a test-only closure delta (new product will publish); pin 15c6bc2e still renders the tree
  byte-exact (render-neutral, Policy early-exit green) so no second bump is required for B3.

## Expected release identity (v0.1.275)
- Tag: v0.1.275 → T2 (annotated, immutable under protect-tags ruleset)
- Version: 0.1.275 (crate == lock == tag)
- GHCR: ghcr.io/tailrocks/velnor-job-ubuntu:0.1.275 (2-platform index linux/amd64+linux/arm64)
- Assets (14): 2 tarballs + 2 sidecars, 2 debs + 2 sidecars, release-record.json + sidecar,
  manifest.json + sidecar, release-manifest.json, SHA256SUMS
- Attestations: tarballs (build job, actions/attest-build-provenance), debs (ci-release-package-signer.yml),
  OCI provenance+SBOM (image-platform)
- Native arm64: guest-payload aarch64 (ubuntu-24.04-arm) + image-platform arm64 (ubuntu-24.04-arm);
  build/debian cross-compile aarch64 on ubuntu-24.04 (declared, not native)

## BOOTSTRAP PROCEDURE (§0.6; fleet-independent)
T2=ac8a74c1 (tag point was here before bootstrap design). Sequence:
1. #941 (bootstrap scope: legs honor providers on dispatch@tag; build always()+results;
   image adopt-only on dispatch; admit scope notice; policy arm) → merge → T3.
2. Product P3 for T3 closure publishes (auto). Main Policy on T3 fails pin-lag (expected).
3. #942 pin-bump (pin 15c6bc2e → T3, regen with P3) → bump-commit B → merge → T4.
4. REF-TYPE PROBE (derisks dispatch@tag): push junk tag v0.0.0-b3probe at B.
   - push-run: expect policy PASS + verify-tag TIP-fail (proves tag plumbing, zero mutations).
   - dispatch@junk (default inputs): expect policy PASS + verify-tag TIP-fail (proves
     GITHUB_REF_TYPE=tag on dispatch@tag; a "not a tag" error here REDESIGNS the cut).
5. TAG v0.1.275 at T4 (annotated). Main stays quiet from here.
6. Push-run R1: expect github green + velnor rejected + build skipped + image ORPHANS
   the version tag (designed-for-recovery; NO release created). Verify orphan + no release.
7. Read orphan index digest: gh run download R1 -n image-digests → image-index.digest.
8. DISPATCH@TAG: gh workflow run release.yml --ref v0.1.275 -f providers=github-hosted
   -f existing-image-digest=<orphan> → bootstrap publish (admit notice visible;
   velnor legs SKIPPED visibly; build/debian/sign/publish run; image adopts).
9. Verify artifacts (below).

## PRE-TAG GATE (do not tag until ALL hold)
1. T4 = post-#942 main tip; main quiet (verify-tag needs tag == tip).
2. Ref-type probe green (dispatch@tag carries GITHUB_REF_TYPE=tag).
3. No v0.1.275 tag/release exists yet (protect-tags forbids move/delete).

## POST-CUT VERIFICATION
1. Release run green: verify + 34 legs + build + image* + metadata + guest + debian + sign + publish.
2. gh release view v0.1.275: 14 assets; record tag/commit/version/manifest coherence.
3. Digests: tarballs/debs vs record vs sidecars vs SHA256SUMS; binary-in-tarball vs record binary_sha256.
4. OCI: index digest == record oci_index_digest; ref digest-pinned; 2 platforms; per-arch digests match record.
5. Attestations: gh attestation verify on both tarballs (repo) + both debs (--signer-workflow signer).
6. Tamper-negative: flip a byte in a downloaded asset copy → attestation verify MUST fail.
7. Unbounded presence in artifacts: deb contains velnor.env (VELNOR_MAX_JOBS adopt-once docs),
   velnor-jobs.slice WITHOUT ceilings, postinst deleting legacy CPUQuota drop-ins + verifying infinity;
   binary built from sources containing native_demand.rs + adopt_max_jobs; no VELNOR_JOB_CPUS/MEMORY.
8. Coherence negatives: release/tests.rs suites (arch mismatch, missing manifest, bad digests) green in CI;
   attestation order test green; live publish log shows verify-before-create execution.
9. No APT feed mutation (B4), no bastion writes (C), no Jackin/ChainArgos (F/G).
