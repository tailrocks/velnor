# APT hardening rereview: e58f632f

Date: 2026-09-20

Scope: source-only, read-only review. No source edits, release, install,
dispatch, runtime publication, Velnor, MacDocker, or merge.

Reviewed detached snapshots:

- `/private/tmp/velnor-apt362-review` at
  `362f2b4c806059ab5658da40b47b7bca9f781b2a`
- `/private/tmp/velnor-apt-e58-review` at
  `e58f632f8da0c2ecfbe47ba195f9b25cd46412da`

The requested 362 commit is an ancestor of the later owner tip. The remote
`origin/dual-lane-apt-schema2` resolved to
`e58f632f8da0c2ecfbe47ba195f9b25cd46412da` during review. Both review trees
were detached and clean. The owner worktree was not used because it was
dirty/advancing.

## Verdict

**BLOCKED.** The e58 tip closes the prior archive destination race, fetch
parent/write/rename boundary, predictable scratch path, schema-2 incoming
snapshot consumption, and generated APT actionlint failures. Publication is
still not approved: provider/release/source authority remains selection-script
self-authority, the native producer-to-APT handoff remains absent, and the
legacy non-selection path still uses a fixed sentinel that does not bind
verified bytes. Publication-control inputs (`previous_pointer` and rollback
`prev_dir`) also remain mutable path reads outside `IncomingSnapshot`.

## Exact source hashes

```text
tip                                      e58f632f8da0c2ecfbe47ba195f9b25cd46412da
requested review tip                     362f2b4c806059ab5658da40b47b7bca9f781b2a
apt.rs                                   3dc41dd8bed2eb45afb6099bf7cc549ca09bd07382bfb980f9fd90f99e48526e
s2/primitives/release.rs                 814add67baa72320ad8ba70fa2b6ba19a28290f04b7060578d14b02b99697a59
s2/runtime.rs                            6c55c2e7d28819cd53ab132e0ed7a014ea976d4a2d7920f0904ab2702c3bf1f8
s2/config/mod.rs                         cd72513e3584fe21e3bfaa626c4bcc18ad81fb231784bee64cb92e72f343940d
generated APT release.yml                0390d13ee12f83371dbf6e6fa786c1648c8cfaad779a0fdf99ee7f5ac53e52e3
```

## Blocking findings

1. **Provider/release/source authority is still self-declared.**

   `parse_discovery_selection` validates field shape, canonical-looking
   URLs, a positive `provider_release_id`, and asset IDs/state
   (`crates/velnor-workflow/src/apt.rs:2250-2426`), but does not query the
   provider release object or compare release ID, tag, repository, asset name,
   state, size, and URL to the selection. `run_fetch_selection` downloads only
   `repos/{source}/releases/assets/{id}` and checks byte length
   (`apt.rs:2615-2715`). A coherent selection can therefore name an unrelated
   provider release/asset while retaining canonical-looking metadata.

   The generated workflow runs the repository-owned discovery script and a
   shallow `jq` check (`s2/primitives/release.rs:4289-4301`), then attests only
   downloaded `.deb` subjects (`release.rs:4301-4307`). It does not independently
   attest the selection, canonical manifest, subordinate records, or provider
   release metadata. Fix needs live provider metadata binding plus an
   attested producer-owned selection/manifest chain.

2. **Legacy verify-to-publish identity is not byte-bound.**

   Without `discovery.json`, `expected_sentinel` returns the fixed
   `verified\\n` marker (`apt.rs:1199-1235`). `IncomingSnapshot::capture` accepts
   that same fixed marker for the no-selection case
   (`apt.rs:4410-4460,4502-4508`) and does not hash or revalidate the stable or
   preview record, manifest, sidecars, or candidate `.deb` files. Publication
   then stages candidate bytes from the snapshot (`apt.rs:5355-5365`). A
   post-verify replacement with another valid same-name `.deb` leaves the
   marker unchanged and is publishable in legacy mode.

   The generated schema-2 path is materially better: it hashes selected asset
   bytes in the sentinel and validates the captured selection/inventory before
   consuming the in-memory snapshot. This finding remains because the
   non-selection path is still present and callable.

3. **Publication control inputs remain outside the immutable incoming
   snapshot.**

   `IncomingSnapshot` covers only `incoming` (`apt.rs:4419-4491`). Stable and
   preview publication still read rollback files from mutable `prev_dir`
   through `stage_dir_debs` (`apt.rs:4825-4862,5345-5353,5454-5465`), with no
   digest binding to the signed live package index. The previous pointer is
   checked and later re-read for record emission (`apt.rs:5740-5751,6012`), so
   a concurrent replacement can change the retained source pointer after its
   validation. Bind these controls to a private descriptor/snapshot before
   any mutation/signing, and verify rollback bytes against signed live hashes.

4. **Native producer-to-APT handoff remains absent.**

   The checked-in producer declaration is native and names
   `tailrocks/velnor-apt` as consumer (`.github-gen/velnor-workflow.toml:65-78`),
   but the checked-in native release explicitly does not push or dispatch the
   APT publisher (`.github/workflows/release.yml:4381-4383`). The generated APT
   renderer starts on schedule/workflow-dispatch and invokes a repository-owned
   discovery script; no producer-bound `workflow_run` or native selection
   attestation is emitted (`s2/primitives/release.rs:4230-4237,4270-4289`).

## Fixed / verified against 91

- Incoming publication is captured through a held no-follow directory FD and
  consumed from owned bytes (`apt.rs:4410-4491`). Schema-2 selected assets are
  checked against the captured sentinel and canonical inventory.
- Fetch creation and installation use a held directory, `openat` with
  `O_NOFOLLOW|O_EXCL`, and descriptor-relative `renameat`
  (`apt.rs:1316-1426,2619-2740`). This closes the prior parent and rename
  boundary.
- Unix `.deb` control/data handling no longer extracts through pathname `tar
  -C`: validated tar entries are created relative to held destination/parent
  descriptors (`apt.rs:3039-3210,3311-3422`). Existing symlink/hardlink
  parents and destination attacks fail closed.
- Scratch roots are UUID-named and created relative to a held no-follow temp
  parent; materialized inputs are created through a held scratch FD
  (`apt.rs:1463-1484,2963-2985`).
- Generated APT workflow is ShellCheck-clean under actionlint 1.7.12 after the
  renderer quoting fixes. No latest-version rule files changed in the reviewed
  deltas.

## Hostile coverage observed

The 76-test APT suite passed, including archive parent symlink/hardlink and
destination symlink rejection, hardlinked/stale sentinel rejection, symlinked
selection rejection, same-size schema-2 candidate replacement rejection,
non-UTF-8 directory rejection, exact asset-ID/census checks, and both archive
backends. No concurrent race test exists; the fetch/extraction closures above
are structural descriptor checks rather than string-only assertions.

## Verification evidence

```text
362 apt::tests                                      76 passed, 0 failed
362 s2::runtime                                     60 passed, 0 failed
362 config::tests::apt_                              2 passed, 0 failed
362 release::tests::a_declared_apt                  4 passed, 0 failed
362 cargo build -p velnor-workflow                  passed
362 generated APT actionlint + explicit ShellCheck  exit 0
362 extracted generated run blocks | bash -n        exit 0

e58 apt::tests                                      76 passed, 0 failed
e58 s2::runtime                                     60 passed, 0 failed
e58 config::tests::apt_                              2 passed, 0 failed
e58 release::tests::a_declared_apt                  4 passed, 0 failed
e58 cargo check -p velnor-workflow --locked         passed
e58 cargo fmt --all -- --check                      passed
e58 generated APT actionlint + explicit ShellCheck  exit 0
e58 extracted generated run blocks | bash -n        exit 0
e58 git diff-tree --check                           passed
```

`cargo clippy -p velnor-workflow --lib --locked --offline -- -D warnings` on
e58 fails with exactly the 12 recorded non-APT baseline diagnostics in
`G0/distribution/apt-schema2-followup-4c4b7a7-independent.md:143-148`:
`s2/mod.rs` (`needless_raw_string_hashes`, `too_many_arguments`,
`too_many_lines`, `no_effect_replace`, `single_char_pattern`) and
`s2/policy.rs` (`too_many_arguments`). No APT diagnostic appears.

No publication approval is granted.
