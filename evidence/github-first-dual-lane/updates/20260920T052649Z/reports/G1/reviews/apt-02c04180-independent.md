# Independent APT successor review: 02c04180a61f23e74cf102cfcccd57adb0567d8d

Date: 2026-09-20

Scope: exact successor review of the schema-2 APT hardening delta. Source-only,
read-only. No source edits, release, installation, runtime, MacDocker, Velnor,
dispatch, publication, merge, or approval was performed.

Review tree: `/private/tmp/velnor-apt-02c-review`, detached and clean. The
remote `origin/dual-lane-apt-schema2` resolved to the reviewed tip
`02c04180a61f23e74cf102cfcccd57adb0567d8d`. `e153080454f539cdd63235540d10c24a615fd714`
is an ancestor. The source worktree was not edited.

## Verdict

**BLOCKED. No publication approval.**

The successor closes the e153 rollback-package substitution gap for the
source/pointer/live-index path and makes the schema-2 selection mandatory at
the `publish_suite` boundary. The hostile selection, snapshot, archive,
package-index, and rollback-digest tests pass; the portable non-UTF-8
conversion test passes. The real invalid-byte directory-entry test is
intentionally cfg-excluded on this Darwin host. This is bounded source
hardening only.

Publication remains blocked because the native producer still does not hand a
trusted release selection/authority to the APT consumer. The provider release
object and source-ref proof remain fields in repository-owned discovery JSON;
the consumer does not independently authenticate those claims. The old
runtime APT verbs and optional `PublishInputs` fields remain, although the
publication boundary now rejects the missing-selection route. The live
rollback signature path also verifies against any key in the supplied keyring
without comparing the record signer to the configured signer fingerprint.
No native producer attestation is fabricated or inferred from the `.deb`
attestation checks.

## Exact review identity

```text
tip                                      02c04180a61f23e74cf102cfcccd57adb0567d8d
parent                                   dde847773591636ff692de2127c55f1a1eb55e70
ancestor comparison                      e153080454f539cdd63235540d10c24a615fd714
tip tree                                 5609771ed6e9f56dd31e4e90439a40a4f21f28c0
apt.rs                                   8327049af3137af105ee4e1b66299fb7c81776d5ac783b286fb025480befccfd
s2/runtime.rs                            ae96f7eac47417e612cf57cb9f341bc619880e9f04e09322568d91778c3a6dc0
s2/primitives/release.rs                 1da2bc25a3991a977ec5246a79a513a41da07ca2e9e238f2a660f4fd1f96d987
checked-in release.yml                   5e3a8f4826f8074ed8d8b1394c898d3b3bd9febb06a38644b43ef54e50d9bc3f
generated APT release.yml                15eaca56ecba988b9eaa4cf2c864996bcfc3960d2e54c8ff98a307fc18c32e8a
```

The successor delta from e153 is four commits: `074d92cf`, `d8f20f9c`,
`dde84777`, and `02c04180`; only `apt.rs`, `s2/primitives/release.rs`, and
`s2/runtime.rs` changed. No latest Mac Xcode 27 / Intel 27 platform rule or
version configuration was touched; no regression was evidenced for that hold.

## Findings

### 1. Rollback package identity is now bound to signed live indexes — PASS

`read_verified_live_publication` reads the publication record, detached
signature, `InRelease`, both architecture `Packages` files, and the keyring,
materializes immutable bytes, runs `gpgv` for the record and `InRelease`, and
compares the signed record's `InRelease`/`Packages` digests before returning
index text (`crates/velnor-workflow/src/apt.rs:5348-5467`). The schema-2
`apt-previous-pointer` path consumes that verified object and binds both
rollback package names and digests from the live indexes
(`crates/velnor-workflow/src/s2/runtime.rs:4047-4083,4089-4118`).

`capture_retained_debs` then reads the held rollback directory and checks the
captured bytes against the typed pointer digests before staging. The tests
`live_packages_bind_exact_rollback_identity_for_both_arches`,
`live_packages_reject_path_or_digest_tampering`, and the publication rollback
digest mismatch regression pass. This closes e153's “different valid same-name
rollback package” gap for this path.

The remaining signer caveat is separate: the live path calls `gpgv` with the
supplied keyring but no expected fingerprint (`apt.rs:5394-5416`), while
`parse_publication_record` only checks that `signer_fingerprint` has full
fingerprint syntax (`apt.rs:6752-6758`). `apt-previous-pointer` passes no
configured signer to this verifier (`s2/runtime.rs:4047-4055`). If the
keyring contains more than the intended signer, a different trusted key can
authorize the live record. This is an authority gap, not a rollback-byte
digest failure.

### 2. Provider/release/source authority remains self-declared — HIGH/BLOCKER

The selection parser now enforces exact shape, provider IDs, canonical-looking
URLs, uploaded state, asset census, release IDs, source commit/ref, and
subordinate digests (`apt.rs:2210-2269,2291-2550`). Those checks validate the
JSON contract but do not authenticate the provider release object or the
source-ref proof. `run_fetch_selection` calls only the asset endpoint built
from the self-declared repository and numeric asset ID, then checks response
length (`apt.rs:2767-2787`); it does not compare the provider release object,
asset name/state/release association, or source/ref metadata returned by the
provider.

The generated workflow runs a repository-owned discovery script, performs a
shallow `jq` shape check, downloads by the selected IDs, and verifies only
downloaded `.deb` subjects with `gh attestation verify` (generated
`release.yml:82-121`; renderer `s2/primitives/release.rs:4289-4327`). The
canonical product manifest, discovery selection, release record, package
manifest, and provider release metadata have no independent native-producer
attestation in this workflow. The new cross-record and digest edges prove
coherence of selected bytes; they do not prove that the selected release was
the canonical provider release.

### 3. Native producer-to-APT handoff remains absent — BLOCKER

The release contract is explicitly native and names `tailrocks/velnor-apt` as
consumer (`.github-gen/velnor-workflow.toml:65-75`). The checked-in producer
workflow explicitly states that it does not push to the consumer or dispatch
its publish and that the consumer pulls the record itself
(`.github/workflows/release.yml:4381-4383`). The generated consumer starts on
schedule or manual dispatch (`release.yml:5-8`; renderer
`s2/primitives/release.rs:4269-4271`) and has no trusted producer `workflow_run`
or equivalent signed handoff.

The `.deb` subject attestations in generated `release.yml:115-117` are not a
native producer release/selection attestation. Treating them as one would
fabricate authority; this review does not do so.

### 4. Optional selection/legacy seam is fail-closed, not removed — HIGH

`PublishInputs` still stores `selection` and `selection_path` as `Option`
(`apt.rs:4601-4645`). `publish_suite` now requires both, re-reads the path,
and compares it with the expected and captured selection before staging
(`apt.rs:4772-4787`). The regression
`publication_requires_bound_schema2_selection` proves that `None`/`None` is
rejected (`apt.rs:11046-11070`). The schema-2 runtime supplies `Some` values
(`s2/runtime.rs:3959-3999`), so the formerly successful publication bypass is
closed at the current boundary.

The legacy runtime still exposes `apt-fetch`, `apt-verify`, and `apt-publish`
(`crates/velnor-workflow/src/runtime.rs:3014-3165`), and that publisher still
constructs `selection: None` and `selection_path: None`
(`runtime.rs:3147-3164`). Therefore the optional API/alternate legacy route
has not been removed as required by the no-legacy rule. It currently fails at
the publication boundary; it is not evidence of publication approval.

### 5. Clippy additions — PASS; baseline remains

`cargo clippy -p velnor-workflow --lib --locked --offline` exits 0 with 12
warnings, all pre-existing non-APT diagnostics. With `-- -D warnings`, it
still exits 101 on exactly 12 non-APT baseline diagnostics in `s2/mod.rs` and
`s2/policy.rs`; no APT lint errors were added. e153's two APT clippy errors
are gone.

## Generated workflow and syntax evidence

The fixture was rendered from the detached tree into
`/private/tmp/apt-02c-generated-new`; generation produced six files. The
generated APT workflow was checked with:

```text
/Users/donbeave/.local/share/mise/installs/actionlint/1.7.12/actionlint \
  -shellcheck=/opt/homebrew/bin/shellcheck \
  /private/tmp/apt-02c-generated-new/.github/workflows/release.yml
exit 0; no output
```

Static output evidence: provider admission is exact `github-hosted`
(`release.yml:42-55`); selection is required by schema-2 `apt-fetch`
(`release.yml:111`); package attestations are the only `gh attestation verify`
calls in the APT verify job (`release.yml:115-117`); stable/preview recovery
downloads signed live publication/index inputs before
`apt-previous-pointer` (`release.yml:159-206`); publication passes the
selection path and previous pointer (`release.yml:210-226`). No workflow was
executed and no remote feed was contacted by this review.

## Test evidence

All scoped tests were rerun sequentially in the detached tree:

```text
apt::tests                                      84 passed, 0 failed
s2::runtime                                     60 passed, 0 failed
config::tests::apt_                              2 passed, 0 failed
release::tests::a_declared_apt                   4 passed, 0 failed
cargo check -p velnor-workflow                  passed
cargo fmt --all -- --check                      passed
git diff --check                                passed
generated APT actionlint + ShellCheck           passed (exit 0)
```

One initial parallel test attempt was SIGKILLed by the host resource limit;
the bounded suites were rerun sequentially and produced the results above.

The full library is not green. Exact result: `1754 passed, 6 failed`, with
the same unrelated policy/generated-workflow failures as the predecessor:

```text
s2::policy::tests::owner_entrypoint_pin_ignores_variable_references
s2::tests::checked_in_workflows_match_the_generator_byte_for_byte
s2::tests::owner_policy_job_never_checks_out_workspace_root
s2::tests::policy_candidate_closure_probe_holds_no_token
s2::tests::policy_candidate_step_binds_manifest_to_head_and_exports_it
s2::tests::policy_trust_comment_states_candidate_execution
```

No APT test failure or APT-specific full-library failure was observed.

The generic non-UTF-8 conversion test passed. The real invalid-byte filesystem
test is intentionally excluded on Darwin by
`#[cfg(all(unix, not(target_os = "macos")))]`
(`apt.rs:8318-8334`); no real non-UTF-8 directory-entry result is claimed on
this host.

## Review disposition

The source delta is materially stronger and closes the e153 rollback digest
and verify-to-publish selection bypasses. It remains a bounded hardening
change, not a publication authority proof. Keep the native producer handoff,
independent provider/source authentication, live signer pin, and removal of
the legacy optional-selection route as release blockers. **No publication
approval.**
