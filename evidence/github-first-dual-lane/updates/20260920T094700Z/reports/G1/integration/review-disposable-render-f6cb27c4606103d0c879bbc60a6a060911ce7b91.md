# Disposable render review — `f6cb27c4606103d0c879bbc60a6a060911ce7b91`

**Review type:** bounded, read-only evidence-artifact review. No G1, source, merge, publication, runtime, or authority approval.

**Artifact:** `G1/integration/draft-regeneration-f6.json`  
**Artifact SHA-256:** `dc8f01c18a8ab793bd32fe03cbcb935f2c0c9f607fe46d3d5be3ba62851b57c3`

**Verdict:** **CHANGES REQUIRED for evidence metadata.** The disposable renders, per-file hashes, authority boundary, runner/scanner delta, and clean-original evidence validate. The stored aggregate `render_sha256` does not reproduce from the algorithm declared in the artifact and must be corrected or its algorithm documented precisely.

## Reproduction and authority boundary

- Source commit: `f6cb27c4606103d0c879bbc60a6a060911ce7b91`; recorded remote commit matches.
- Recorded generator source closure and draft candidate closure both equal `9ac962a622605e3edcd5debbc9f4f990abe15cfda1ae1a663c066e00e58c356c`.
- Declared config pin remains `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`; contract remains `54`.
- Draft command uses locked generation with `--output <draft-root>`; no canonical output adoption.
- Recorded candidate `--check --output` exits `0` and explicitly says the draft matches candidate render, not the declared pin.
- Recorded canonical `--check` exits `1` for the four expected paths.
- Recorded actionlint exits `0` on all draft workflows with actionlint `1.7.12`; independently rerun and passed.
- Recorded `deterministic_repeat: true`; independently `diff -qr` found no difference between draft A and draft B, each containing 21 files.
- Recorded canonical worktree clean; independent exact-f6 detached review tree also remained clean.

The artifact correctly preserves the authority boundary: candidate output is disposable, the `0dc79895` pin is unchanged, and there is no generated-file commit, promotion, privileged execution, publication, or pin-adoption claim.

## Stored hashes independently checked

- All 21 draft-A paths were SHA-256 rehashed; every stored per-file hash matched.
- Draft B is byte-identical to draft A; its recorded render/file count is therefore independently corroborated, although the JSON intentionally omits a second per-file list.
- All four `canonical_drift` entries were independently rehashed against the exact f6 canonical tree and draft A:

  - `.github/actionlint.yaml`: stored canonical/draft hashes match; delta removes `macos-26`, adds `xcode-27`.
  - `.github/ci/project.toml`: stored hashes match; candidate scan adds `rust-test-targets:velnor-tools:1`.
  - `.github/workflows/ci-runtime-products.yml`: stored hashes match; ARM64 matrix runner changes `macos-26` to `xcode-27`.
  - `.github/ci/.github-actions-generator-state`: stored hashes match; scan/input/output hashes update while generator contract remains `54`.

These are candidate generated outputs only. No source authority or generated file was changed in f6.

## Aggregate render-hash defect

The artifact declares:

> SHA-256 of sorted `'<file-sha256>  <relative-path>'` lines

Using each independently validated stored file hash, LF-terminated lines, C-locale sort, and SHA-256, the recomputed digest is:

```text
2e164a3d07f64bc608c82e0c96ff4648c88034f0387ca2a8629da00474cfddbd
```

The JSON records:

```text
4151e4af7308a7d4788afa26155afea03868cad0374ad955e31c6d7fb4f94b4d
```

The mismatch is reproducible for draft A; draft B has identical bytes. Individual file hashes and the two-tree deterministic comparison still pass, so this is an evidence-manifest aggregation defect, not evidence that the rendered files differ.

## Required correction

Recompute and replace `render_sha256` using the declared algorithm, or amend the artifact with the exact alternate serialization/ordering used and independently reproducible output. Preserve the current candidate-only boundary and `0dc79895` pin. This review does not approve G1 or generated-output adoption.
