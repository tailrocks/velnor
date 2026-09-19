# G2 preview-publication checkpoint

- Branch: `codex/github-first-preview-publication`
- Base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`
- Runtime: `gpt-5.6-luna`, reasoning `max`
- Scope: generator source only; no checked-in generated workflow, dispatch, merge, release, or publication action.

## Publication contract

The native and schema-1 compatibility renderers now emit the same staged
publication shape:

1. Resolve one commit-bound, orderable preview version and immutable tag
   `preview-v<version>`.
2. Create/re-read a draft prerelease by tag; reject mismatched name, commit,
   tag, or prerelease identity.
3. Enrich the consumer manifest only after the release id exists. It carries
   schema, product id, channel, version, source repository/ref/commit, release
   tag/id, typed artifact rows, and typed component rows.
4. Reconcile each asset by listing and downloading existing bytes. Upload only
   absent assets, never clobber. An upload error re-lists and confirms provider
   bytes before retrying. A digest mismatch fails closed.
5. Require the exact complete asset set before publishing the draft.
6. Retain the `preview` channel release and append uniquely named
   `preview-channel-<orderable-version>.json` assets. Paginated history is
   scanned; only a newer candidate advances. Equal versions confirm immutable
   identity/bytes; an older run is superseded and cannot overwrite the head.
   Other channel state is untouched.

Pure fixtures cover partial upload, provider-stored/API-error retry, equal
version rerun and digest conflict, old/new race, other-channel retention, and
deterministic identity naming in
`crates/velnor-workflow/src/s2/primitives/preview_publication.rs`.

## Known coordination seam

`ReleaseSpec` currently supplies the package/binary/target row but not a
first-class product-id or complete component inventory. The native renderer
derives the generic product id from `GITHUB_REPOSITORY` and emits the current
typed row. `g2_native_packages` must align the final canonical product id,
component inventory, and consumer schema before the first recovery publication.

## Verification

- `cargo fmt --all`: pass.
- Targeted release suites: `269 passed`.
- Pure preview-publication fixtures: `6 passed`.
- Full crate suite: `1740 passed, 1 failed`; the sole failure is the expected
  checked-in generated-workflow byte-drift assertion because this checkpoint
  intentionally leaves generated `.github` output unchanged.
- `git diff --check`: pass.
- Legacy destructive `gh release delete preview --cleanup-tag --yes` and
  `gh release upload preview dist/* --clobber` source strings: absent.
