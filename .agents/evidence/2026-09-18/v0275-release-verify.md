# Release verification: v0.1.275 (tailrocks/velnor) — INDEPENDENT, read-only

Date (UTC): 2026-09-18. Methods: `gh` REST API + `git ls-remote`/clone only. No writes to repo.

## VERDICT: RELEASE-NOT-PROVEN

The tag exists and points where claimed, and the prerequisite code IS in the tagged
tree — but no release was ever produced: the sole tag-push Release run was
CANCELLED, `Control / Publish` never ran, no GitHub Release object exists (0/14
assets), no OCI image was pushed, and there is therefore nothing to attest.
Same-SHA CI/main is red.

## (1) Tag object — PROVEN

- `refs/tags/v0.1.275` = tag object `8d64cf6191cf69dcd4a048e0274ba049b87debd0` (annotated)
  - `refs/tags/v0.1.275^{}` = `4a7afad0884b5c0c203dd03677f5d4e26178442a` ✔ matches claimed SHA
- Tagger: Alexey Zhokhov <alexey@zhokhov.com>, **2026-09-18T03:46:51Z**
- Message: `B3 bootstrap release: unbounded/global-native prerequisites (#937) + B1 generator fixes (#938) + bootstrap scope (#941)`
- Signature: `verified:false, reason:unsigned` (tag is UNSIGNED — negative)
- Target commit `4a7afad0` (2026-09-18T03:41:51Z):
  `Merge pull request #942 from tailrocks/chore/pin-bump-3e276ad8` /
  `chore(ci): bump generator pin 15c6bc2e → 3e276ad8 (B3 bootstrap scope)` ✔ PR #942

## (2) Tag-push release workflow run(s) — NO SUCCESS; sole run CANCELLED

Search over Release workflow (id 291642051) + all repo runs with
`head_branch==v0.1.275` or `head_sha==4a7afad0…`: exactly ONE Release run.

- Run **35304493150** `Release · v0.1.275` — event `push` (tag push), head_sha `4a7afad0884b`,
  created 2026-09-18T03:46:58Z (7s after tag), status completed, conclusion **CANCELLED**
  - URL: https://github.com/tailrocks/velnor/actions/runs/35304493150
- **No `workflow_dispatch` bootstrap run exists for v0.1.275** (dispatch@tag: ABSENT).
  (Nearby dispatch runs exist only for `v0.0.0-b3probe`, both failure — not this release.)

Per-job verdict (47 jobs total):
- success (4): `Control / Verify release`, `Control / Admit release`,
  `Admit immutable image tag`, `Compile release metadata once`
- failure (19): `Guest payload aarch64` + 18 unit-matrix jobs
  (`velnor`/`github-hosted` × bun/docker/docs/opentofu/rust-*)
- cancelled (6): `Guest payload x86_64`, `Build amd64 GHCR image`,
  `Build arm64 GHCR image`, `Assemble one multi-platform GHCR image`,
  `Build ${{ matrix.arch }} deb (release-build)`, **`Control / Publish`**
- skipped (18): remaining matrix incl. `Sign … Debian package`, `Build / …`

Root-cause evidence (failed logs):
- All 18 unit-matrix jobs: step `Run "<unit>" checks` →
  `error: read CI selection /home/runner/work/velnor/velnor/.velnor-ci-selection/velnor-ci-selection: No such file or directory (os error 2)`
  e.g. https://github.com/tailrocks/velnor/actions/runs/35304493150/job/105473866321
- `Guest payload aarch64` step 18 `Build guest vmlinux and rootfs.ext4`:
  `velnor-guest-image: execution backend microvm failed closed … (mmdebstrap exited 25 …`
  `Failed to fetch https://snapshot.ubuntu.com/ubuntu/20260826T000000Z/dists/noble/InRelease 500 Internal Server Error …)`
  — external mirror outage.
  https://github.com/tailrocks/velnor/actions/runs/35304493150/job/105473774658
- Only run artifact: `release-metadata` (13,364,307 bytes, unexpired) — a workflow
  artifact, NOT release assets.

## (3) Release assets — ABSENT (0/14)

- `GET /repos/tailrocks/velnor/releases/tags/v0.1.275` → **HTTP 404**
- `gh release view v0.1.275` → `release not found`
- Baseline `v0.1.274` (2026-09-06) has exactly 14 assets: `manifest.json(+.sha256)`,
  `release-manifest.json`, `release-record.json(+.sha256)`, `SHA256SUMS`,
  `velnor-runner-0.1.274-{amd64,arm64}.{deb,tar.gz}` each `+.sha256`.
  Expected ~14 for 275, observed **0**. No digests to record.
- Siblings also asset-less: v0.1.271 (run 34050881627 failure) and v0.1.276
  (run 35306258685 failure) both 404 on `/releases/tags/…`.

## (4) Attestations — ABSENT, nothing to verify

- `gh attestation verify "oci://ghcr.io/tailrocks/velnor:v0.1.275" --owner tailrocks`
  → `Error: failed to fetch remote image: … MANIFEST_UNKNOWN: manifest unknown`
  (image-build jobs were cancelled; nothing pushed, so no SLSA/provenance subjects exist)
- No release files exist, so file-based `gh attestation verify <file>` has no subject.
- Limitation: org packages API returned 403 (`read:packages` scope missing), so an
  independent package-version listing was not possible; the direct OCI fetch failure
  above is the primary evidence.

## (5) Prerequisites inside tagged source — PROVEN PRESENT

Fresh `git clone` + `checkout v0.1.275` → HEAD `4a7afad0884b5c0c203dd03677f5d4e26178442a`:
- `native_demand`: `crates/velnor-runner/src/native_demand.rs`,
  `crates/velnor-runner/tests/native_demand.rs`, referenced from `lib.rs`, `runner.rs`,
  `permit_guard.rs`
- permit ledger: `crates/velnor-control/src/permit_ledger.rs`,
  `crates/velnor-runner/src/scaleset/{shared_ledger,capacity,reconcile,allocator,daemon,lane}.rs`,
  plus `service.rs`, `buildkit.rs`, `args.rs`, `runner.rs`; consumers in
  `crates/velnorctl/src/{runtime,host}.rs`; tests `scaleset_{daemon,allocator,worker}.rs`,
  `debian/velnor.env`
- History contains: `05980b7a` Merge PR #937 `feat/c2-native-demand-global-n`,
  `15c6bc2e` Merge PR #938 (B1 generator), `3e276ad8` Merge PR #941 (B3 bootstrap
  scope), `4a7afad0` Merge PR #942 (pin bump) ✔ tag message claims match tree history

## (6) Coherence negatives

- CI/main at the SAME sha `4a7afad0`: run 35304173229, conclusion **failure**
  (5/75 failed: docs, docker trusted, bun-velnor, prepare-cargo, opentofu — velnor side)
  https://github.com/tailrocks/velnor/actions/runs/35304173229
- Preview at same sha: run 35304173200, conclusion **failure**
  https://github.com/tailrocks/velnor/actions/runs/35304173200
- v0.1.276 tag-push run 35306258685: conclusion **failure**, failed job
  `Control / Verify release` (gate failed closed; no 276 release either)
  https://github.com/tailrocks/velnor/actions/runs/35306258685
- Tag v0.1.275 itself is unsigned (`verified:false`).

## Precise gaps blocking RELEASE-PROVEN

1. No successful Release run: sole v0.1.275 run 35304493150 = cancelled.
2. No dispatch@tag bootstrap run for v0.1.275 exists.
3. No GitHub Release object / assets (0 of expected ~14; no digests).
4. No OCI image (`MANIFEST_UNKNOWN`) → no attestations to verify.
5. Same-commit CI/main red → release commit not green on main.

Proven: tag identity/target/date, PR #942 merge, prerequisite code (#937/#938/#941)
present in the tagged tree.
