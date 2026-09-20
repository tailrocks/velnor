# Independent exact review: schema-2 APT follow-up

Observed 2026-09-20 (Asia/Ho_Chi_Minh). This is a source-only review of
`4c4b7a7dfb8f87a8624821d06c9faac2bc8d12f4` (`dual-lane-apt-schema2`). The
branch worktree was shared and later became dirty in an unrelated release
renderer; all exact source and commands below were checked in the clean
detached tree `/private/tmp/g1-apt-schema2-clean` at that SHA. No source or
remote mutation was made.

## Verdict

**Reject as a complete/G2 APT delivery.** The commit is a valid narrow hardening
step: ordinary symlink paths are rejected, leading-zero stable/preview numeric
forms are rejected, and several JSON tamper fixtures are useful. It does not
close the file-boundary contract, exact release census, provider-backed source
authority, or generated-workflow integration.

## Closed by this commit

- `is_bare_version` and `is_canonical_decimal` reject leading-zero numeric
  components/preview sequences (`apt.rs:317-371`, `1323-1337`). The tests cover
  `v01.2.3`, `v1.02.3`, `v1.2.03`, and preview `.01`
  (`apt.rs:5485-5539`). Normal X.Y.Z and X.Y.Z-preview.N grammar is therefore
  materially tighter than the parent.
- `require_file`/`require_directory` use `symlink_metadata` and reject a
  currently observed symlink or non-regular path (`apt.rs:924-999`). The
  incoming fixture covers `discovery.json`, the product manifest, its sidecar,
  and one `.deb` symlink (`apt.rs:5032-5061`).
- Selection JSON has exact top-level/asset object keys, canonical source/tag
  relationships, release URL grammar, release-ID character grammar, manifest
  identity, uniqueness, and required fixed asset names
  (`apt.rs:1562-1788`). The new hostile fixture exercises release-ID, source
  ref/commit, release URL, component, missing manifest artifact, and persisted
  extra-inventory mutations (`apt.rs:5091-5202`).

These tests are positive evidence for the narrow changes, not proof of the
whole handoff contract.

## Blocking residuals

### 1. Check-then-use remains path based; hard links are accepted

`require_file` only checks one `symlink_metadata` result and then drops it;
`sha256_file`, `read_json`, and `sidecar_digest` reopen the path with
`std::fs::read` (`apt.rs:866-922`, `924-976`). `regular_file_size` stats the
path again. Preview `SHA256SUMS`/sidecar reads are also direct path reads after
the check (`apt.rs:2771-2833`, `2854-2887`), and `.deb` readers reopen the
path through `dpkg-deb`/`ar` (`apt.rs:1952-2040`). A concurrent rename can put
a symlink, a different file, or a different parent directory between the
check and the read/tool invocation. `run_fetch_selection` has the same shape:
`dir.exists()` and `dir.join(...).is_file()` follow links before later writes
(`apt.rs:1794-1859`).

There is no `nlink`/inode policy. A hard link to an outside regular file passes
`metadata.is_file()` and every current size/hash check. A writer through the
outside link can race later reads, and the sentinel writer can mutate an
outside inode. This is not closed by rejecting symlinks.

Required regression matrix: start from one genuinely passing fixture, replace
each selected file and the incoming directory with a symlink, then with a
hard link to an outside file; race/replace the path between validation and
read; assert rejection and assert the outside target is unchanged. The fix
belongs in one descriptor-based boundary: open the directory/file with
`O_NOFOLLOW`/`openat`-style semantics, validate metadata and (where policy
requires) `nlink == 1` on that same handle, and consume the held descriptor.
Do not keep a `require_*` boolean check followed by a second path open.

### 2. Sentinel is allowed but not verified as a regular, fresh proof

`verify_discovery_incoming` adds `.reprepro-ok` to its allowed-name set but
never requires or validates it (`apt.rs:1870-1903`). A sentinel symlink or
hard link therefore passes this gate. After a successful suite check,
`verify_suite` arms it with `std::fs::write`, which follows an existing
symlink/hard link (`apt.rs:2187-2230`); `publish_suite` later accepts any
following regular file via `.is_file()` (`apt.rs:3011-3031`). A pre-existing
sentinel can thus redirect the write or satisfy publication without a fresh
descriptor-bound proof. Existing tests only use a regular sentinel for the
positive path (`apt.rs:4963-4989`) and start failure fixtures without one
(`apt.rs:6207-6214`); no sentinel symlink, hard link, stale, or replacement
case exists.

The regression must include a passing control, then a sentinel symlink, a
sentinel hard link, and a stale/pre-existing sentinel during a failed verify;
all must fail closed at the intended boundary and leave the outside target
unchanged. Use the same verified-handle/atomic-create strategy as finding 1,
and bind the publish proof to the current verified selection rather than
checking only path existence.

### 3. Release census is minimum-only, not exact

`read_discovery_selection` requires every manifest artifact and a fixed union
of eight names, but never computes an exact allowed set or rejects an extra
selected release asset (`apt.rs:1692-1769`). The downstream verifier builds its
expected set from *all* selected assets (`apt.rs:1879-1902`), so an extra asset
in the selected JSON is fetched by ID and accepted. The new “extra inventory”
test mutates only the persisted incoming `discovery.json` while the selected
JSON remains clean (`apt.rs:5173-5201`); it proves the two JSON documents must
match, not that a malicious selected release census is rejected.

The same minimum-only shape leaves component/artifact inventory policy to
producer self-declaration (`apt.rs:1380-1487`). A same-size mutation of a
non-canonical release asset is only size-checked by
`verify_discovery_incoming` (`apt.rs:1893-1903`); canonical manifest and
manifest-listed artifacts get stronger checks, but record/sidecar/other
metadata bytes do not at this boundary. Add a positive selected fixture, then
mutate the selected JSON with an extra asset and with same-size metadata
tamper; assert the authoritative selection gate rejects both (or explicitly
move and document the byte/digest obligation at the later verifier).

### 4. Provider/source authority and workflow integration are absent

The source-ref “proof” is only fields in the input document checked for shape
and equality (`apt.rs:1490-1550`); it does not query GitHub or independently
resolve the tag/branch. `run_fetch_selection` uses the supplied source slug and
asset IDs; it does not re-query release identity or compare the provider
response with the supplied `release_id`, `provider_release_id`, name, URL, and
state (`apt.rs:1794-1859`). Valid arbitrary release IDs still pass the opaque
grammar. The hostile JSON mutations therefore test consistency against a
self-declared document, not provider-backed provenance.

The commit changes only `crates/velnor-workflow/src/apt.rs` (249 lines). The
exact renderer still maps `apt` to generic `render_package_feed`, invoking
`release verify-feed`/`update-feed`, not immutable selection fetch/verify or
the schema-2 handoff (`s2/primitives/release.rs:3135-3157`, `4212-4237`).
Runtime APT selection functions exist, but no generated workflow reaches them.
Producer manifest/digest/census handoff and hidden-sentinel artifact transport
remain external work. No G2 approval follows from this commit.

## Exact detached verification

All commands ran against the clean detached SHA in
`/private/tmp/g1-apt-schema2-clean`, with build output outside source:

- `CARGO_TARGET_DIR=/private/tmp/g1-apt-schema2-target-clean rtk cargo test -p velnor-workflow --lib discovery_`
  — **16 passed**, 1723 filtered.
- `... rtk cargo test -p velnor-workflow --lib apt::tests` — **67 passed**,
  1672 filtered.
- `... rtk cargo test -p velnor-workflow --lib s2::runtime` — **60 passed**,
  1679 filtered.
- `rtk cargo fmt --all -- --check` — passed.
- `... rtk cargo check -p velnor-workflow --all-features --locked` — passed
  (105 crates compiled).
- `... rtk cargo clippy -p velnor-workflow --lib --all-features -- -D warnings`
  — fails with 12 existing diagnostics only in `s2/mod.rs` and `s2/policy.rs`;
  no `apt.rs` diagnostic.
- `... rtk cargo clippy -p velnor-workflow --all-targets --all-features -- -D warnings`
  — fails with the same 12 existing diagnostics plus one warning; no `apt.rs`
  diagnostic.
- `rtk git diff-tree --check 4c4b7a7^ 4c4b7a7` — passed; detached worktree
  clean at the exact SHA.

Source links: [commit](https://github.com/tailrocks/velnor/commit/4c4b7a7dfb8f87a8624821d06c9faac2bc8d12f4),
[APT source](https://github.com/tailrocks/velnor/blob/4c4b7a7dfb8f87a8624821d06c9faac2bc8d12f4/crates/velnor-workflow/src/apt.rs),
[APT renderer](https://github.com/tailrocks/velnor/blob/4c4b7a7dfb8f87a8624821d06c9faac2bc8d12f4/crates/velnor-workflow/src/s2/primitives/release.rs).
