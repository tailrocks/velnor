# Independent exact APT hardening review: f069789a87b20cd15869c5d9cca8fd96db547b2b

Date: 2026-09-20

Scope: exact detached review of `f069789a87b20cd15869c5d9cca8fd96db547b2b`
plus its parent `9dfc936dd2be874c832c437bd959028bb4dc789f`. This review is
source-only and read-only. No source edit, release, install, runtime,
MacDocker, Velnor, dispatch, publication, merge, or approval was performed.

Review tree: `/private/tmp/velnor-apt-f069-review`, detached and clean.
`origin/dual-lane-apt-schema2` resolved to the reviewed tip. The exact
reviewed commits are the two requested commits; f069 is the signer-pinning
delta and 9dfc is the legacy-route/mandatory-selection refactor.

## Verdict

**BLOCKED. No publication approval.**

The refactor removes the schema-1 APT runtime seam and makes the producer
selection mandatory at the schema-2 verify/publish boundaries. The f069 signer
change adds status-fd checks for both the detached publication record and
`InRelease`, and passes an `expect-signer` option through the previous-pointer
runtime. Two blockers remain:

1. The central APT renderer does not pass `--expect-signer` to either live
   stable or live preview `apt-previous-pointer` invocation. Those generated
   commands reach a runtime path that calls `required_option("expect-signer")`
   and fail before rollback verification.
2. The `VALIDSIG` parser compares the first fingerprint (the signing subkey)
   to a configured primary-key fingerprint and ignores the final primary-key
   field. A normal GPG key with a signing subkey therefore fails its own live
   rollback check; no explicit primary/subkey binding policy is implemented.

The independent provider/source authority and native producer-handoff gaps
also remain. This bounded review does not authorize publication.

## Exact identity and hashes

```text
tip                                      f069789a87b20cd15869c5d9cca8fd96db547b2b
parent                                   9dfc936dd2be874c832c437bd959028bb4dc789f
tip tree                                 26d72839d0b6c77bc53c6337a56071703e26d563
origin/dual-lane-apt-schema2             f069789a87b20cd15869c5d9cca8fd96db547b2b
apt.rs                                   42d90e4df6b7ea7c275643ab0d28d4183382fb99ed8e7cd4f8b5e5ef6e332923
runtime.rs                               f309ae52976f83e5ce75dd5e9b5e354fb905b626724bc94eed88c045d666720f
s2/runtime.rs                            17f22068ba01e8634752dd39ec2c9375e9a76dc0dba5bd890c928954f655007d
s2/primitives/release.rs                 1da2bc25a3991a977ec5246a79a513a41da07ca2e9e238f2a660f4fd1f96d987
generated APT release.yml               534080ada0617488d19a95c747e43a10c26b3a98d3dd1cd0af757b1021941086
```

The generated workflow was rendered from the exact tree into
`/private/tmp/apt-f069-now.1JB5dk`. The checked source tree remained clean.

## Findings

### BLOCKER: central renderer omits the new signer option

The schema-2 runtime accepts `expect-signer` in the previous-pointer option
allowlist (`crates/velnor-workflow/src/s2/runtime.rs:4004-4020`) and requires
it in both live paths (`s2/runtime.rs:4042-4052,4085-4095`). The central
renderer remains unchanged in `s2/primitives/release.rs:4351-4365`: its stable
and non-bootstrap preview command strings end after `--keyring`.

The actual generated workflow proves the mismatch:

```text
release.yml:183 ... apt-previous-pointer ... --suite stable ... --keyring 'example.gpg' > previous-pointer.json
release.yml:196 ... apt-previous-pointer ... --suite preview ... --keyring 'example.gpg' > previous-pointer.json
release.yml:205 ... apt-previous-pointer ... --suite preview --bootstrap true > previous-pointer.json
```

The bootstrap preview branch returns before live verification, so its omitted
signer is not the live-path defect. Stable and non-bootstrap preview both call
`required_option("expect-signer")` and reject the rendered command. The
renderer test at `s2/primitives/release.rs:8059-8149` checks the verify command
and its signer argument, but does not assert signer wiring for rollback
recovery. Actionlint cannot detect this runtime CLI contract mismatch.

### BLOCKER: `VALIDSIG` handling rejects normal signing-subkey signatures

The new parser at `crates/velnor-workflow/src/apt.rs:1641-1659` finds a
`[GNUPG:] VALIDSIG ` line, takes only the first whitespace token, normalizes
it, and compares it directly with `expected`. It does not parse the required
status fields, validate the final primary-key fingerprint, or establish an
explicit signing-subkey-to-primary relationship.

I generated a real primary key plus signing subkey in an isolated temporary
GPG home and verified a detached signature with `gpgv --status-fd 1`. The
actual status output was:

```text
primary=5B4A687FDAD827CED5EFD653AD4769AB8F56E4EC
subkey=3A628AA693E9C37B43A0CCCCC15590E813BDEB41
[GNUPG:] VALIDSIG 3A628AA693E9C37B43A0CCCCC15590E813BDEB41 2026-09-20 1789883126 0 4 0 22 10 00 5B4A687FDAD827CED5EFD653AD4769AB8F56E4EC
```

GPG emitted the same subkey-first/primary-last shape when invoked with the
primary identity. The project’s configured expected identity is normally the
primary: `secret_key_fingerprint` returns the first `fpr` record
(`apt.rs:5537-5551`), and the generated workflow derives `live_fpr` from the
first keyring `fpr` (`s2/primitives/release.rs:4307-4311`). Therefore the
current parser compares `3A62...` with `5B4A...` and rejects a valid signature.

The source does correctly invoke gpgv status verification for both the signed
publication record and `InRelease` (`apt.rs:5263-5295`) and separately
compares the signed record’s declared signer (`apt.rs:5300-5305`). Those
checks cannot repair the subkey parser or missing renderer argument. The safe
contract must parse the actual `VALIDSIG` fields, require a valid final primary
fingerprint, bind that primary to the configured publisher, and explicitly
define whether any valid signing subkey under that primary is accepted.

The current negative tests are insufficient. `apt.rs:8846-8870` tests no
status, a foreign fingerprint, duplicate lines, and non-UTF-8 input, but its
accepted fixture repeats the same fingerprint in the first and final positions.
It has no real subkey-first/primary-last case and accepts a short malformed
`VALIDSIG` line so long as its first token matches.

### HIGH/BLOCKER: provider selection remains self-attested

The mandatory selection seam is structurally improved, but it is not an
independent provider-authority proof. `read_discovery_selection` validates the
producer JSON’s exact shape, canonical URL grammar, IDs, asset inventory, and
manifest (`apt.rs:2160-2428`). `run_fetch_selection` then calls
`gh api repos/{source_repository}/releases/assets/{id}` using those
self-declared repository and asset IDs and checks downloaded size
(`apt.rs:2577-2715`); it does not independently retrieve and compare the
provider release object, tag, target commit, asset names, or release ID.
`source_ref_resolution` is field validation, not provider authentication
(`apt.rs:2088-2147`). Thus mandatory selection prevents caller-field mixing,
but does not turn producer-authored metadata into independently authenticated
source/provider authority.

The native producer-to-APT handoff/attestation remains absent. The generated
APT job verifies `.deb` attestations, not a native producer release object or
cross-repository handoff. No native authority is fabricated by this review.

## Structural checks

**Pass: legacy schema-1 seam removed.** The top-level release dispatcher now
rejects the old APT commands (`crates/velnor-workflow/src/runtime.rs:2961-2982`);
the removed `source_git_url`, `resolve_commit_argv`, `run_resolve_commit`,
`download_patterns`, `gh_download_argv`, and `run_fetch` helpers are absent.
The corresponding negative test passed (`runtime.rs:5928-5949`). The remaining
APT commands are schema-2 `s2/runtime` commands, not a second schema-1 route.

**Pass: typed producer selection is mandatory.** `VerifyInputs.selection` is
non-optional and supplies source/package/version/commit
(`apt.rs:3387-3458`). `PublishInputs.selection` and `selection_path` are also
non-optional and are reread/compared with incoming bytes immediately before
staging (`apt.rs:4503-4651`). Schema-2 fetch, verify, publish, and channel
update require `--selection` and revalidate incoming selection
(`s2/runtime.rs:3861-3997,4123-4160`). No `Option<DiscoverySelection>` or
legacy optional-selection construction remains in these paths.

**Pass: no selection boolean bypass.** `apt_flag_bool` only parses
`verify-oci` and `bootstrap` (`s2/runtime.rs:3761-3768`); selection is a
required path, not a boolean gate. `selection_context` cross-checks all
caller-supplied identity fields against the immutable selection
(`s2/runtime.rs:3720-3746`). This pass does not remove the separate
self-attested provider-authority gap above.

## Verification

```text
apt::tests                                              80 passed, 0 failed
apt::tests::gpgv_status_requires_exactly_the_pinned_signer 1 passed
s2::runtime                                             60 passed, 0 failed
runtime::tests::legacy_apt_release_commands_are_removed 1 passed
s2::primitives::release::tests::a_declared_apt_feed_mutates_from_github_only 1 passed
cargo check -p velnor-workflow --locked --offline         passed
cargo fmt --all -- --check                               passed
git diff --check f1c2970..f069789a                       passed
generated APT actionlint + ShellCheck                    passed (exit 0)
cargo clippy -p velnor-workflow --lib -- -D warnings     failed: 12 unchanged non-APT diagnostics
```

Strict clippy’s 12 diagnostics are in unchanged `s2/mod.rs` and
`s2/policy.rs` code (raw-string hashes, argument/line limits, and no-effect
replacements); none are in the APT files changed by these commits. This is not
a green full-clippy result.

No release, installation, remote feed, publication, or runtime execution was
performed. **No publication approval.**


