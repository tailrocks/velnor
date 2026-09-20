# Schema-2 APT handoff — `0712a549`

Observed 2026-09-20. Worktree: `/private/tmp/dual-lane-apt-schema2`, branch
`dual-lane-apt-schema2`. Remote branch equals
`c678b6397e2941f16ba42bed8b6d68dd98296c42`; no generated consumer files,
publication, dispatch, or Mac runtime operation.

## Revision-stamped follow-up — `91bdf6c` (2026-09-20)

Remote `origin/dual-lane-apt-schema2` now equals
`91bdf6cc1d0a5c429c5c01f17bf15dbb153c661b` (clean). This section supersedes
the remote revision stated above; earlier checkpoint descriptions remain
historical.

`91bdf6c`
([verified-byte regression](https://github.com/tailrocks/velnor/commit/91bdf6cc1d0a5c429c5c01f17bf15dbb153c661b))
adds a hostile same-size candidate replacement after sentinel arming; the
sentinel recomputation rejects it before publication.

The preceding `83e7ab4` archive checkpoint remains intact. `f8b0e22`
([filesystem-test correction](https://github.com/tailrocks/velnor/commit/f8b0e22c2836eba7a03666a708dc15f83f058831))
removes runtime skipping from the real invalid-byte test: APFS/macOS runs the
direct conversion boundary only, while non-macOS Unix creates an actual
invalid-byte directory entry and asserts held-FD enumeration rejects it.

- `e4a6edd158946d81c3ca1dc1a4b08f864d930ea0`
  ([generated workflow repair](https://github.com/tailrocks/velnor/commit/e4a6edd158946d81c3ca1dc1a4b08f864d930ea0)):
  closes the generated `verify_args=(...)` shell array before execution and
  exports `publish.outputs.channel`, which deploy consumes. Renderer coverage
  asserts both emitted fragments.
- `6eed883bc0ce28d65c6accb30ec6edf8f60c3e21`
  ([verified-byte publication binding](https://github.com/tailrocks/velnor/commit/6eed883bc0ce28d65c6accb30ec6edf8f60c3e21)):
  binds the schema-2 sentinel to every selected asset's digest, re-reads and
  compares the immutable selection at publication, and checks candidate deb
  bytes against the canonical product-manifest SHA-256 immediately before
  staging. Legacy runtime remains selection-free by design.
- `83e7ab4807824417734ffd96b6f0277c2a21234a`
  ([archive confinement and filesystem fixtures](https://github.com/tailrocks/velnor/commit/83e7ab4807824417734ffd96b6f0277c2a21234a)):
  routes `dpkg-deb` through `--fsys-tarfile`, preflights archive paths and
  regular/dir-only member types, creates extraction roots with component-wise
  no-follow `mkdirat`, rejects destination symlinks, and adds traversal,
  archive-link, destination-link, and real filesystem non-UTF-8 regressions.

Verification at this revision: `cargo check -p velnor-workflow --all-features
--offline` passed; APT suite `76 passed`; renderer APT generation test passed;
focused archive traversal/destination/link, non-UTF-8, and same-size candidate
replacement tests passed; format and diff checks passed. An exact rendered APT
workflow snapshot from the renderer test passed the installed default
`actionlint -oneline` run (and `actionlint -shellcheck '' -`). No generated
consumer file was written or dispatched. Native producer attestation and
canonical product-manifest authority remain external dependencies; no G2
delivery or package publication is claimed.

## Revision-stamped follow-up — `362f2b4c` (2026-09-20)

Remote `origin/dual-lane-apt-schema2` equals
`362f2b4c806059ab5658da40b47b7bca9f781b2a`; the worktree is clean. This is
an APT-owned source checkpoint only. It does not publish, dispatch, regenerate
consumer workflows, install packages, or operate Mac runtime.

The checkpoint closes the reviewed path-resolution races at the responsible
boundaries:

- `run_fetch_selection` opens/creates the incoming directory relative to a
  held no-follow parent, writes assets with `openat`, and installs them with
  descriptor-relative `renameat`; no `create_dir_all`/pathname rename race.
- `.deb` control/data extraction no longer hands a destination pathname to
  `tar -C` on Unix. Compression is decoded as bytes, then the checked-in Rust
  tar reader creates only regular files/directories through held no-follow
  descriptors. Archive traversal, symlink, hardlink, and destination-parent
  swaps stay inside the opened extraction root.
- Publication captures the entire incoming handoff through one held
  directory descriptor, validates the captured sentinel, canonical manifest,
  subordinate sidecars/parent digest, exact artifact inventory, and APT
  `SHA256SUMS` census, then stages candidate bytes from the private immutable
  snapshot rather than reopening incoming paths. The snapshot is captured
  before publication checks; path-based sentinel/discovery rechecks are not
  publication authority. Retained rollback inputs are still sourced from the
  explicit retained directory.
- Generated APT shell quotes the comma-separated architecture scalar and
  hyphenated deploy filenames. The exact renderer fixture passes default
  `actionlint` (ShellCheck enabled): prior SC2054 and both SC2100 warnings are
  gone.

Verification at `362f2b4c`: `cargo check -p velnor-workflow --lib --offline`,
APT tests `76 passed`, focused schema-2 publication test passed, renderer APT
test passed, `cargo fmt --all -- --check`, `git diff --check`, and generated
APT snapshot `actionlint -oneline` passed. Clippy reports the same 12
pre-existing generator diagnostics outside APT; no APT diagnostics remain.

Remaining gates are unchanged and blocking: native producer must emit and
authenticate the shared product manifest/provider release handoff; native
must provide the complete four-target/18-row source projection and exact
provider binding; generated consumer regeneration and hosted verification
remain external; no package publication is claimed before G1.

## Checkpoints

- `c84d2a9778950cab20ca9be59da6f6b33465b84d`
  ([APT source](https://github.com/tailrocks/velnor/commit/c84d2a9778950cab20ca9be59da6f6b33465b84d)):
  descriptor-validated, no-follow, single-link input reads; private
  materialization before `dpkg-deb`/`ar`; create-new sentinel and staged-file
  writes; exact canonical 18-row/four-target/three-component inventory;
  subordinate parent-digest, sidecar, and two-package `SHA256SUMS` checks;
  hostile symlink/hardlink/stale sentinel and selected-extra fixtures.
- `7b8fb456c1d1eacaa771f8e60f27e973ab964efd`
  ([renderer](https://github.com/tailrocks/velnor/commit/7b8fb456c1d1eacaa771f8e60f27e973ab964efd)):
  schema-2 typed APT completeness/config and generated workflow seam. One
  repository-owned discovery selection is passed through `apt-fetch`,
  `apt-verify`, `apt-publish`, `apt-channel-update`, and previous-pointer
  steps. Hosted-only mutation, hidden sentinel upload, retention/recovery, and
  no generic latest/tag feed path are rendered from typed source.
- `e2e887165f32e276d26e61da13642a04e1a40d70` and
  `0712a54952ec4a3d5aa979790aa23f842a954e0e`: lint-only follow-ups, both
  signed with DCO and Codex trailer.
- `f68d79d6e83a2ff4ff1590ea2c75315c088e03cb`
  ([APT boundary hardening](https://github.com/tailrocks/velnor/commit/f68d79d6e83a2ff4ff1590ea2c75315c088e03cb)):
  component-wise `openat`/`O_NOFOLLOW` traversal with held-FD directory
  enumeration, non-UTF-8 rejection, exact one-per-Linux-target APT census,
  and malicious archive symlink/hardlink-parent regressions.
- `180471bc7400b01bfc37804ef03121f7ec33618f`
  ([handoff/config hardening](https://github.com/tailrocks/velnor/commit/180471bc7400b01bfc37804ef03121f7ec33618f)):
  checked-in discovery script must be a no-follow regular executable, and
  generated hosted-only provider admission rejects every extra/unknown name.
- `ced72eb3c512a4fc97a323fc1a45ddf375834198`
  ([declared handoff coverage](https://github.com/tailrocks/velnor/commit/ced72eb3c512a4fc97a323fc1a45ddf375834198)):
  applies discovery-script existence/type validation to both `[release]` and
  `[[declare]]` APT contracts, with a missing declared-script regression.
- `c678b6397e2941f16ba42bed8b6d68dd98296c42`
  ([Debian suffix reconciliation](https://github.com/tailrocks/velnor/commit/c678b6397e2941f16ba42bed8b6d68dd98296c42)):
  binds each Linux APT target to its architecture suffix while accepting the
  two canonical `-arch.deb`/`_arch.deb` producer spellings.

## Verification

- `rtk cargo check -p velnor-workflow --all-features --offline`: pass.
- `rtk cargo test -p velnor-workflow --offline --lib discovery_`: 22 pass.
- `rtk cargo test -p velnor-workflow --offline --lib apt::tests`: 72 pass.
- `rtk cargo test -p velnor-workflow --offline --lib extracted_archive_symlink_and_hardlink_parents_fail_closed`: pass.
- `rtk cargo test -p velnor-workflow --offline --lib s2::config::tests::apt_discovery_script`: 4 pass.
- `rtk cargo test -p velnor-workflow --offline --lib s2::primitives::release::tests::a_declared_apt_feed_mutates_from_github_only`: pass.
- `rtk cargo test -p velnor-workflow --offline --lib s2::runtime`: 60 pass.
- `rtk cargo test -p velnor-workflow --offline --lib a_declared_apt_`: 4 pass.
- `rtk cargo fmt --all -- --check`, `rtk git diff --check`: pass.
- Full clippy still reports pre-existing generator diagnostics in `s2/mod.rs`,
  `s2/policy.rs`; no APT source diagnostics remain after the lint checkpoint.

## Remaining contract gates

1. Native producer must emit the same authenticated product manifest and
   provider-bound attestation consumed here. Current APT fixture/consumer
   keeps `release_id` (producer grammar) separate from numeric
   `provider_release_id`; canonical release-ID semantics remain an explicit
   cross-owner decision. Do not claim G2 delivery until native/Homebrew/APT
   agree.
2. Native product currently has four targets and 18 rows; APT rejects missing
   Apple targets while consuming the two Linux `apt-package` rows. Native must
   add the APT subordinate package projection (release record, package
   manifest, release-manifest, debs, sidecars, OCI/attestation edges).
3. Component-wise no-follow closes the reviewed parent-symlink boundary for
   verifier reads, but provider metadata/producer identity must still be
   bound by the native producer handoff; APT intentionally does not self-
   attest provider identity or query a second discovery authority.
4. Generated consumer regeneration and hosted verification remain external;
   no package publication until G1 recovery and independent review pass.
