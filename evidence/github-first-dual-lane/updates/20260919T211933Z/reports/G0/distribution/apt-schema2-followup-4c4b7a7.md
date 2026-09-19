# Schema-2 APT follow-up

Observed: `2026-09-20` in `/private/tmp/dual-lane-apt-schema2`.

## Checkpoint

- Branch: `dual-lane-apt-schema2`
- Commit: [`4c4b7a7dfb8f87a8624821d06c9faac2bc8d12f4`](https://github.com/tailrocks/velnor/commit/4c4b7a7dfb8f87a8624821d06c9faac2bc8d12f4)
- Remote: `origin/dual-lane-apt-schema2` equals the commit; worktree clean.
- Commit trailers: `Co-authored-by: Codex <codex@openai.com>` and DCO sign-off.

## Closed review gaps

- Incoming directories, selected assets, canonical manifest, and sidecar are
  checked with `symlink_metadata`; symlinked files/directories are rejected,
  and size checks no longer follow links.
- Stable and preview SemVer base components, plus preview sequence numbers,
  reject leading-zero numeric forms.
- Hostile fixtures cover release-ID grammar, source-ref/source-commit drift,
  canonical release URL drift, persisted extra release inventory, selected
  symlinks for discovery/manifest/sidecar/deb, byte/size tamper, and sentinel
  preservation.

## Verification

- `rtk cargo test -p velnor-workflow --lib discovery_`: 16 passed.
- `rtk cargo test -p velnor-workflow --lib apt::tests`: 67 passed.
- `rtk cargo test -p velnor-workflow --lib s2::runtime`: 60 passed.
- `rtk cargo check -p velnor-workflow --all-features --locked`: passed.
- Full all-target clippy has no new APT fixture diagnostics; remaining errors
  are pre-existing mixed policy-generator diagnostics in `s2/mod.rs` and
  `s2/policy.rs`.

Renderer/config generation, producer manifest/digest/census handoff, hosted
workflow recovery, publication, and Mac runtime validation remain external
dependencies. This checkpoint makes no G2 delivery claim.
