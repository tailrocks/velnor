# Independent exact review: schema-2 APT handoff `0712a549`

Observed 2026-09-20 (Asia/Ho_Chi_Minh). Review tree: detached
`/private/tmp/g1-apt-schema2-0712`, exact
`0712a54952ec4a3d5aa979790aa23f842a954e0e`, clean before and after. No source,
generated-output, or remote mutation.

## Verdict

The c84/7b/0712 source hardening and typed schema-2 seam are real. **Reject as
a complete producer/G2 handoff.** Provider-backed release identity, native
producer integration, and several file-boundary/census contracts remain
unproven or incomplete. The checks below are source-only; no hosted release or
publication claim follows.

## Verified at this exact SHA

- Final incoming-file reads use one descriptor opened with `O_NOFOLLOW`, require
  a regular file, and reject Unix `nlink != 1`; the bytes are consumed from
  that descriptor ([`apt.rs:971-1017`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L971-L1017)).
  `.deb` bytes are copied into a private create-new scratch file before
  `dpkg-deb`/`ar` ([`apt.rs:1139-1151`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L1139-L1151)).
  This closes the old final-component check/reopen and hard-link issue.
- Sentinel proof is fresh and selection-bound: it is
  `selection:<sha256 persisted selection>`, or `verified` for legacy input;
  checking reads the held regular file, and arming uses `create_new`,
  `O_NOFOLLOW`, sync, and a single-link check. Existing/stale proof is refused
  ([`apt.rs:1020-1079`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L1020-L1079)).
- Selection parsing has exact top-level and asset object keys, positive
  `provider_release_id`, canonical release URL, uploaded-state assets, unique
  names/IDs, and an exact allowed release-asset set (manifest artifacts,
  sidecars, records, release manifest, and `SHA256SUMS`), not merely a minimum
  list ([`apt.rs:1833-2065`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L1833-L2065)).
- Product validation enforces three components, all four `PRODUCT_TARGETS`,
  18 rows, 12 binaries, two APT rows, and four archives; subordinate records
  and sidecars bind to the canonical manifest, and `SHA256SUMS` binds the two
  APT artifacts ([`apt.rs:1653-1749`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L1653-L1749), [`apt.rs:2087-2198`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L2087-L2198)).
- Schema-2 renderer resolves the typed APT contract and routes one immutable
  `selection.json` through fetch, verify, publish, channel update, and pointer
  operations ([`release.rs:4221-4237`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/s2/primitives/release.rs#L4221-L4237), [`runtime.rs:3861-4000`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/s2/runtime.rs#L3861-L4000)). Runtime options are compared exactly with selection fields ([`runtime.rs:3720-3747`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/s2/runtime.rs#L3720-L3747)).
- Public schema-2 CLI routing reaches the typed command: `cargo run ... release
  apt-fetch --selection selection.json --dir incoming` exits at the intended
  immutable selection read with `required file missing: selection.json`, not an
  old-command/unknown-option path.

## Hostile regressions reproduced

Against the exact detached tree, each passed (one test, 1741 filtered):

- `discovery_incoming_rejects_symlinked_selection_paths`
- `discovery_incoming_rejects_hardlinks_and_stale_sentinel`
- `discovery_incoming_rejects_hardlinked_sentinel`
- `discovery_selection_binds_asset_ids_and_preserves_hidden_sentinel`
- `discovery_selection_requires_every_manifest_artifact_asset`
- `preview_source_proof_must_bind_main_ancestry`

The complete focused groups also passed: `apt::tests` 69, `discovery_` 19,
`s2::runtime` 60, and `a_declared_apt_` 4.

## Remaining blockers and precise regressions

### 1. Directory boundary is still path/TOCTOU based

`require_directory` opens only the final directory component with
`O_NOFOLLOW`, validates it, drops the descriptor, and `dir_names` then reopens
the path with `read_dir` ([`apt.rs:934-969`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L934-L969), [`apt.rs:1162-1175`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L1162-L1175)). A parent-component symlink or directory replacement can redirect later enumeration and joins. Final file hardlinks/symlinks are closed; this is the remaining directory-handle boundary. Fix with a held directory FD and `openat`/component-by-component `O_NOFOLLOW` traversal, then enumerate/read relative to that handle. Add parent-symlink and swap-after-check regressions.

### 2. Provider/source authority is self-declared, not provider-bound

`parse_discovery_selection` accepts opaque `release_id` grammar and positive
`provider_release_id`, and checks the derived URL, but performs no provider
query ([`apt.rs:1944-2005`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L1944-L2005)). `run_fetch_selection` asks `gh api` only for each supplied numeric asset endpoint and checks byte length; it does not compare the returned release's repository, release ID/tag, asset name, URL, or state ([`apt.rs:2201-2262`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L2201-L2262)). The actual generated shell performs shallow `jq` checks and verifies attestations only for downloaded `.deb` subjects; it does not independently authenticate canonical manifest/subordinate metadata or the release identity ([`release.rs:4288-4311`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/s2/primitives/release.rs#L4288-L4311)).

Required source-owned fix: a trusted collector/API step must fetch and compare
immutable release metadata (repository, numeric ID, tag, asset IDs/names,
URLs, uploaded state, source/ref/commit, and producer workflow conclusion),
then bind the signed/attested manifest and every subordinate/package edge to
that result. A self-authored JSON `provider_release_id` is not evidence.

### 3. The 18-row check does not make the two APT targets exact

All artifact targets must be one of the four product targets, and archive
rows are exact per target. APT rows are only counted as two and checked with
`ends_with("-unknown-linux-gnu")` ([`apt.rs:1724-1748`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L1724-L1748)). A valid-shape mutation can make both APT rows claim the same Linux target while preserving 18 rows, names, digests, and all four component targets. Regression: mutate one APT row to duplicate the other and require rejection; enforce exactly one `x86_64-unknown-linux-gnu` and one `aarch64-unknown-linux-gnu`, with artifact names/arch fields agreeing.

### 4. Non-UTF-8 incoming entries disappear from the census

`dir_names` silently skips directory entries whose names do not convert to
UTF-8 ([`apt.rs:1168-1172`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L1168-L1172)). An unexpected non-UTF-8 extra file is therefore not compared against the exact expected set. Reject conversion failure (or enumerate raw names) and add a passing-control-plus-extra-entry regression.

### 5. Archive extraction needs a separate no-follow regression

Verified `.deb` bytes are safely materialized before external tools, but
`dpkg-deb -x`/`tar -x` populate a pathname tree and extracted identity/binary
checks use final-component `require_file` calls ([`apt.rs:2541-2587`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L2541-L2587), [`apt.rs:3122-3167`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L3122-L3167)). No hostile package with a symlink/hardlink parent or traversal member was exercised here. Add a malicious archive fixture and reject symlink/hardlink/non-regular parents before trusting extracted identity or binary; do not treat the current materialization as proof of confined extraction.

### 6. Typed discovery script is path-safe but not existence/type-safe

`resolve_s2` only validates the configured discovery path grammar; it does not
verify that the repository-relative path exists, is a regular executable file,
or is not a symlink ([`apt.rs:678-766`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs#L678-L766)). The renderer test deliberately declares
`scripts/release-discovery.sh` without creating it and still passes its string
assertions ([`release.rs:8056-8113`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/s2/primitives/release.rs#L8056-L8113)). Generator/source scan must validate the checked-in producer handoff, or generation can emit a workflow that fails at runtime.

### 7. Hosted-only provider gate accepts extra provider names

The generated `admit-provider` and publisher conditions use substring membership
for `github-hosted`; `github-hosted,velnor` or `github-hosted,unknown` is not
rejected ([`release.rs:4273-4277`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/s2/primitives/release.rs#L4273-L4277), [`release.rs:4331-4337`](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/s2/primitives/release.rs#L4331-L4337)). Parse a canonical provider list and reject unknown/extra entries; hosted-only remains the only permitted APT mutation lane.

## Producer/integration disposition

The exact branch contains the generic typed renderer/runtime seam, but the
current project source is still configured as native release rather than a
checked-in APT schema-2 producer contract. Native four-target/18-row output,
the two-Linux APT projection, provider-bound release evidence, generated
consumer regeneration, and hosted execution are not established by this
commit. Keep the G2 gate closed until those source-owned artifacts and live
proof are independently checked.

## Exact verification

All commands used a target directory outside the source tree:

- Focused hostile tests above: all passed.
- `cargo test -p velnor-workflow --lib apt::tests --locked --offline`: **69 passed**.
- `cargo test -p velnor-workflow --lib discovery_ --locked --offline`: **19 passed**.
- `cargo test -p velnor-workflow --lib s2::runtime --locked --offline`: **60 passed**.
- `cargo test -p velnor-workflow --lib a_declared_apt_ --locked --offline`: **4 passed**.
- `cargo check -p velnor-workflow --all-features --locked --offline`: passed.
- `cargo fmt --all -- --check`: passed; `git diff-tree --check 0712^ 0712`: passed.
- Scoped clippy still fails with 12 diagnostics in pre-existing `s2/mod.rs` and
  `s2/policy.rs`; no `apt.rs` diagnostics.

Source links: [exact commit](https://github.com/tailrocks/velnor/commit/0712a54952ec4a3d5aa979790aa23f842a954e0e), [APT source](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/apt.rs), [schema-2 release renderer](https://github.com/tailrocks/velnor/blob/0712a54952ec4a3d5aa979790aa23f842a954e0e/crates/velnor-workflow/src/s2/primitives/release.rs).
