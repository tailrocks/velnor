# Corrected G0 source workload supplement: fleet ten

This is a new immutable correction of
`workload-contract-10-20260919T233341Z.json`; the prior JSON and report remain
unchanged.

- Corrected JSON:
  `G0/fleet/workload-contract-10-20260920T001932Z-corrected.json`
- Corrected JSON SHA-256:
  `eb1ae8fffaa35e7a91124435c7d2bc43aa23eb97e68793635f52bfd814acbf71`
- Superseded JSON SHA-256:
  `910d4db877c4ada0e241ec390546d080b0aed2e34dca1e879f6d6237a628a47b`
- Status remains `source_derived_not_execution`; `gate_status` remains
  `incomplete`.
- No build, helper, workflow, dispatch, install, publication, or source command
  was executed. No source repository was edited.

## Corrections

### Typed Termrock external dependency

`termrock-old-revision` is now an explicit external Git package node, not a
local source unit:

- URL: `https://github.com/tailrocks/termrock.git`
- Commit: `5ff94ee117fd4a1b72fdd0d1b1847815055a93ac`
- Package/version: `termrock` / `=0.11.0`
- Declared at `tools/oldrev-harness/Cargo.toml`, source blob
  `3090b1c377e7bf84588389f5bb562d7423c005a9`
- Code pin at `tools/oldrev-harness/src/main.rs`, source blob
  `5b24e8d2600c63018955a52c48a9ade0287ef550`
- Current source snapshot: `936982e60bce6d19cf33ae09b53545a955f1073d`
- State: source pin only; external contents were not fetched or executed.

The local `rust-oldrev-harness` unit now depends only on local
`rust-termrock`; a typed `unit_to_external_git_package_revision` edge carries
the external node.

### Velnor release/publication platform rows

Three formerly absent expected IDs now have source-pinned declarative rows:

| workload | source blob | source matrix |
|---|---|---|
| `runtime-products-publish` | `.github/workflows/ci-runtime-products.yml` — `c35974071e31f1cbef495776fc5167ab8748908b` | Linux X64 / `ubuntu-24.04`; Linux ARM64 / `ubuntu-24.04-arm`; macOS ARM64 / `macos-26` |
| `release-package-signer` | `.github/workflows/ci-release-package-signer.yml` — `e425cdfa63bbb6c437a670e069e676d64a08430a` | signer `attest` on Ubuntu 24.04 |
| `preview-publication` | `.github/workflows/preview.yml` — `f0a54de82479960b858bfc446ff79d53688ffeff` | guest x86_64/aarch64 on Ubuntu x64/ARM64; Debian amd64/arm64 on Ubuntu 24.04 |

These rows describe source matrices only. They do not imply a successful build,
publication, attestation, or release result.

### Exact per-path object pins

All corrected values are full 40-character lowercase Git object IDs:

- `tailrocks/schemalane` `rust-toolchain.toml`:
  `010f002cf56497ce0a1b4ac124a2e2d86a97bc3d`
  (previous artifact was 39 characters).
- `tailrocks/schemalane` `.github/workflows/ci-main.yml`:
  `50a03003bd3935e3dbde9dd3f033b9a55196257b`
  (previous artifact had the wrong `...393e5d...` bytes).
- `tailrocks/pg-bigdecimal` `rust-toolchain.toml`:
  `010f002cf56497ce0a1b4ac124a2e2d86a97bc3d`
  (previous artifact was 39 characters).

## Independent validation

- JSON parses with `jq empty`.
- All ten repository rows remain present with original closing SHAs and source
  tree listing digests.
- Recomputed source-tree pins: 188 path/blob records; every value is exactly
  40 lowercase hex and equals `git rev-parse HEAD:<path>` in the reviewed
  closing source clones.
- All expected workload IDs still map to logical jobs; every `actual_jobs` value
  remains `null`.
- The original artifact remains preserved at its original path and hash. This
  corrected file is the only replacement candidate for checker/index consumers.

## Index/checker handoff

Index owner/checker should retain the superseded artifact for provenance and
select the corrected artifact by its full path and SHA above. Do not infer any
execution, release, install, platform capability, or external dependency
contents from these source declarations.
