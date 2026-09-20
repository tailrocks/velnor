# Independent exact APT rereview: 71a8a5391e5596c5c37a90376b0c505419dae196

Date: 2026-09-20

Scope: exact detached rereview of `71a8a5391e5596c5c37a90376b0c505419dae196`,
parent `19062e656c13b54a4ff27b5082805357a0318e05`, against the prior f069
blockers. Source-only and read-only. No source edit, Velnor runtime, release,
installation, remote feed, publication, dispatch, merge, or approval was
performed.

Review tree: `/private/tmp/velnor-apt-71a8-review`, detached and clean.
`origin/dual-lane-apt-schema2` resolved exactly to the reviewed tip.

## Verdict

**BLOCKED. No publication approval.**

The two f069 findings requested for repair are addressed in source:

- `19062e656` wires the configured signer into stable, preview, and preview
  bootstrap generated recovery commands.
- `71a8a539` parses the complete ten-field `VALIDSIG` shape and binds the
  signing fingerprint to the final primary-key fingerprint, with malformed,
  duplicate, foreign-primary, and non-UTF-8 negatives.

A fresh real GPG primary-plus-signing-subkey fixture produced the expected
subkey-first/primary-last status and the new parser’s field contract matches
that shape. However, the actual live verifier still passes the unsupported
`--no-default-keyring` option to `gpgv`. GnuPG 2.5.22 rejects that command
before emitting status, so both live publication and live `InRelease`
verification fail before the repaired parser runs. Provider/source authority
and native producer handoff remain explicitly unresolved.

## Exact identity and hashes

```text
tip                                      71a8a5391e5596c5c37a90376b0c505419dae196
parent                                   19062e656c13b54a4ff27b5082805357a0318e05
tip tree                                 ca6a080cf3a2b762c87ea1c66bc7faa978b6a5cb
origin/dual-lane-apt-schema2             71a8a5391e5596c5c37a90376b0c505419dae196
apt.rs                                   1d8464f1dc76d9235973ead96a6253c3ab03bfcf1ab04e5cc8175ee7456d10b1
s2/primitives/release.rs                 c7660b6dccd6a5d379ea0dfa978cccf037dfcadc3d45f9313e25fbee6158efd8
generated APT release.yml                77d7b9a53aada1861f64dd950aeb3e54851f689838660a505b37305bf968da35
```

The generated workflow was rendered from the exact tree into
`/private/tmp/apt-71a8-generated.kzFVsp`.

## Prior f069 blockers

### PASS: renderer signer wiring

The runtime still allows and requires `expect-signer` for live stable and
preview recovery (`crates/velnor-workflow/src/s2/runtime.rs:4004-4095`).
The renderer now appends the configured signer after the keyring for stable and
non-bootstrap preview (`s2/primitives/release.rs:4351-4371`), and inserts it
into preview bootstrap (`release.rs:4479-4486`).

Exact generated commands:

```text
release.yml:183 ... apt-previous-pointer ... --suite stable ... --keyring 'example.gpg' --expect-signer '0123456789ABCDEF0123456789ABCDEF01234567' > previous-pointer.json
release.yml:196 ... apt-previous-pointer ... --suite preview ... --keyring 'example.gpg' --expect-signer '0123456789ABCDEF0123456789ABCDEF01234567' > previous-pointer.json
release.yml:205 ... apt-previous-pointer ... --suite preview --expect-signer '0123456789ABCDEF0123456789ABCDEF01234567' --bootstrap true > previous-pointer.json
```

The renderer test asserts stable signer wiring, preview selection wiring, and
bootstrap signer wiring (`s2/primitives/release.rs:8071-8185`). Generated
actionlint plus explicit ShellCheck passed. No generated recovery command
retains the removed legacy APT discovery calls.

### PASS: full `VALIDSIG` primary/subkey binding

The parser now requires exactly ten fields, validates both the signing and
primary fingerprints as full 40-hex values, and compares the final primary
fingerprint with the normalized expected publisher identity
(`crates/velnor-workflow/src/apt.rs:1641-1674`). Production normalizes the
expected identity before invoking it and checks both the detached publication
record and `InRelease` status streams (`apt.rs:5235-5309`).

The new unit test covers:

- real `VALIDSIG` field shape with a signing subkey first and pinned primary
  last;
- direct primary signatures;
- foreign primary;
- duplicate `VALIDSIG` lines;
- malformed short status;
- non-UTF-8 status

(`apt.rs:8860-8905`). The test’s subkey value is synthetic, so I independently
checked the shape with real GPG.

## Independent real GPG fixture

In temporary GPG home `/private/tmp/apt-71a8-gpg3.jjsMiF`, GnuPG generated:

```text
gpgv (GnuPG) 2.5.22
primary=5AFC1449601BE986DB34649F0F2075E9AFFA3554
subkey=1313F18FD9C269CC1472C466D666E894030163EB
```

A detached signature made with the primary identity (GPG selected its signing
subkey) verified with supported `gpgv` arguments and emitted:

```text
[GNUPG:] NEWSIG
[GNUPG:] KEY_CONSIDERED 5AFC1449601BE986DB34649F0F2075E9AFFA3554 0
[GNUPG:] SIG_ID 53Hi7XH9NCW7xplwAbvHnHvE+0I 2026-09-20 1789884712
[GNUPG:] GOODSIG D666E894030163EB APT Review <apt-review@example.test>
[GNUPG:] VALIDSIG 1313F18FD9C269CC1472C466D666E894030163EB 2026-09-20 1789884712 0 4 0 1 8 00 5AFC1449601BE986DB34649F0F2075E9AFFA3554
```

This is ten post-`VALIDSIG` fields: signing fingerprint, date, timestamp,
expiry, version, reserved, public-key algorithm, hash algorithm, class, and
primary fingerprint. The repaired parser accepts that shape by inspection and
the equivalent unit fixture passes.

## New blocker: unsupported gpgv option

The actual production command vectors still include
`--no-default-keyring` for both live checks
(`crates/velnor-workflow/src/apt.rs:5277-5309`):

```text
gpgv --status-fd 1 --no-default-keyring --keyring <keyring> <signature> <data>
gpgv: invalid option "--no-default-keyring"
EXIT=2
```

The exact command above was run against the real primary/subkey fixture. The
same fixture with `--status-fd 1 --keyring <binary-keyring> ...` exits 0 and
emits the `VALIDSIG` shown above. `run_fixed` converts the unsupported-option
exit into a generator error before `gpgv_signer` receives status, so the new
primary/subkey repair is not live-effective. Both the signed publication
record and `InRelease` paths are affected. This is a **BLOCKER** until the
command is corrected or target-tool compatibility is proven on every APT lane.

## Provider and provenance authority remain unresolved

No provider/native binding changed in these two commits.

- `read_discovery_selection` validates producer-authored JSON shape, canonical
  URLs, IDs, assets, and manifest (`apt.rs:2174-2430`).
- `run_fetch_selection` downloads from the self-declared repository and asset
  ID with `gh api repos/{source_repository}/releases/assets/{id}`, checking
  size but not independently retrieving and comparing the provider release
  object, tag, target commit, release ID, and complete asset metadata
  (`apt.rs:2595-2729`).
- `source_ref_resolution` remains field validation, not provider
  authentication (`apt.rs:2102-2161`).
- The generated APT job attests `.deb` subjects only. Native producer release
  handoff and native authority evidence remain absent.

Mandatory typed selection still prevents caller-field mixing, but it does not
convert self-attested discovery metadata into provider authority. **Pending
provider/native binding remains a publication blocker.**

## Selection/bypass review

**Pass: no boolean selection bypass.** `apt_flag_bool` only handles
`verify-oci` and `bootstrap` (`crates/velnor-workflow/src/s2/runtime.rs:3761-3768`);
`--selection` is required by schema-2 fetch, verify, publish, previous-pointer,
and channel-update paths. `selection_context` cross-checks suite, repository,
package, version, ref, tag, and commit against the persisted selection
(`s2/runtime.rs:3720-3746`).

**Pass: mandatory producer selection.** `VerifyInputs.selection`,
`PublishInputs.selection`, and `selection_path` remain non-optional; publish
rereads and compares the selection with incoming bytes before staging
(`apt.rs:3401-3472,4519-4667`). The old top-level schema-1 APT dispatcher and
generic git/tag/download helpers remain removed
(`crates/velnor-workflow/src/runtime.rs:2961-2982`).

## Verification

```text
apt::tests                                              80 passed, 0 failed
apt::tests::gpgv_status_requires_one_pinned_primary       1 passed
s2::runtime                                             60 passed, 0 failed
s2::primitives::release::tests::a_declared_apt_feed_mutates_from_github_only
                                                         1 passed
runtime::tests::legacy_apt_release_commands_are_removed  1 passed
cargo check -p velnor-workflow --locked --offline         passed
cargo fmt --all -- --check                               passed
git diff --check f069789a..71a8a539                       passed
generated APT actionlint + ShellCheck                    passed (exit 0)
cargo clippy strict                                       failed: 14 diagnostics
cargo clippy with the two new style lints allowed        failed: 12 inherited diagnostics
```

The 12 inherited strict-clippy diagnostics are unchanged issues in
`s2/mod.rs` and `s2/policy.rs`. The two new changed-file diagnostics are
non-security quality failures:

- `apt.rs:1642`: `clippy::doc_markdown` wants `GnuPG` in backticks.
- `s2/primitives/release.rs:4482`: `clippy::uninlined_format_args`.

No project runtime, release, installation, remote feed, or publication was
performed. **No publication approval.**

