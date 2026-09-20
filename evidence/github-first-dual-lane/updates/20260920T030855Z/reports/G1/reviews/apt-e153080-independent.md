# Independent APT hardening review: e153080454f539cdd63235540d10c24a615fd714

Date: 2026-09-20

Scope: source-only, read-only review. No source edits, publication, release,
installation, dispatch, Velnor, MacDocker, or merge.

Review tree: `/private/tmp/velnor-apt-e153-review`, detached and clean.
The exact remote `origin/dual-lane-apt-schema2` resolved to the reviewed tip;
`e58f632f8da0c2ecfbe47ba195f9b25cd46412da` is its parent. The delta is only
`crates/velnor-workflow/src/apt.rs`.

## Verdict

**BLOCKED. No publication approval.**

The e153 delta closes the fixed `verified\n` legacy sentinel, makes the
producer selection and full product asset/subordinate census mandatory at
publication, and snapshots incoming bytes, the previous pointer, and retained
rollback `.deb` bytes before key import, staging, or signing. The prior
mutable-pointer/rollback replacement test passes.

Provider/release/source authority and the native producer-to-APT handoff remain
absent. A rollback identity gap also remains: captured rollback bytes are not
checked against hashes from the signed/live `Packages` index. The old runtime
APT verbs and optional `PublishInputs` selection fields still leave an
alternate direct publisher seam. e153 also introduces two APT clippy errors.

## Exact hashes

```text
tip                                      e153080454f539cdd63235540d10c24a615fd714
parent                                   e58f632f8da0c2ecfbe47ba195f9b25cd46412da
tip tree                                 e10737cb0a22f7876f5c865fc5d2f8ce081e97ba
apt.rs                                   cbd5bd4ba38ea0e8c956e2977a51c913b7d02070d51b798f67fdbf72e56198d3
generated APT release.yml                d09925f1e5037a34447e9f436c7c035abeda1f463fa7dfff5e7f0a0ef1a32a72
```

## Fixed in e153

1. **Fixed-marker legacy arm removed.** `expected_sentinel` rejects an
   incoming tree without producer-owned `discovery.json`
   (`apt.rs:1199-1244`). `IncomingSnapshot::capture` requires the selection,
   sentinel, canonical product manifest, every selected asset, subordinate
   sidecar/parent edge, exact package census, and exact captured bytes
   (`apt.rs:4304-4528`). `verify_snapshot_selection` applies those checks from
   owned bytes (`apt.rs:4304-4445`).

2. **Verify-to-publish controls are snapshotted.** `PublicationSnapshot` reads
   the pointer and rollback package bytes before signing/staging
   (`apt.rs:4598-4665,4767-4778`). Stable/preview publishing consumes only
   snapshot values (`apt.rs:5420-5613`); `emit_publication_record` no longer
   reopens the pointer (`apt.rs:6082-6154`). Rollback symlink and hard-link
   inputs are rejected by the held-directory/no-follow reader.

3. **Real hostile regressions passed.** The new snapshot freeze test proves
   same-size pointer and rollback replacement after capture does not change the
   bytes consumed (`apt.rs:9932-10018`). Fixed sentinel, exact manifest asset
   census, pointer defects, archive parent symlink/hardlink, archive
   destination symlink, selection hardlink, stale/hardlinked sentinel, and
   candidate replacement tests passed.

## Blocking findings

1. **Provider/release/source authority remains self-declared.** The selection
   parser validates shape, canonical-looking URLs, positive provider asset IDs,
   names, sizes, and states (`apt.rs:2250-2501`), but never fetches or compares
   the provider release object. `run_fetch_selection` calls only
   `repos/{source}/releases/assets/{id}` and checks response length
   (`apt.rs:2654-2788`). The generated workflow executes the repository-owned
   discovery script, performs a shallow `jq` check, and attests only downloaded
   `.deb` subjects (`s2/primitives/release.rs:4289-4327`); it does not attest or
   independently bind the selection, canonical manifest, subordinate records,
   or provider release metadata.

2. **Native producer-to-APT handoff remains absent.** The producer declaration
   is native and names `tailrocks/velnor-apt`
   (`.github-gen/velnor-workflow.toml:65-75`), while the checked-in native
   release explicitly says it does not push or dispatch the APT consumer
   (`.github/workflows/release.yml:4381-4383`). The generated APT workflow
   starts on schedule/workflow-dispatch (`s2/primitives/release.rs:4270-4289`)
   and contains no trusted producer `workflow_run`/selection handoff.

3. **Rollback bytes are frozen but not identity-bound to signed live state.**
   `capture_retained_debs` accepts the bytes found in `prev_dir` and
   `stage_dir_debs` stages them with no expected external digest
   (`apt.rs:4635-4665,4934-4961`). `derive_previous_pointer` binds only the
   publication/source-record digest and prior tag
   (`apt.rs:5874-5930`). The generated recovery downloads rollback packages by
   URL without checking a digest from the signed/live `Packages` index
   (`s2/primitives/release.rs:4347-4375`). This closes replacement races, but
   not substitution of a different valid same-name/version rollback package.

4. **An alternate direct publisher seam remains.** The legacy runtime still
   exposes `apt-fetch`, `apt-verify`, and `apt-publish`
   (`runtime.rs:3014-3165`). It supplies `selection: None` and
   `selection_path: None`; `publish_suite` permits both options to be absent
   (`apt.rs:4552-4595,4684-4707`) while accepting a producer selection found
   inside `incoming`. Missing selection still fails closed, so the old fixed
   marker is gone, but a direct legacy `apt-publish` invocation can bypass the
   external selection-path binding and rely only on self-authored incoming
   selection bytes. Remove the legacy route or make publication require the
   schema-2 selection object plus its source path as a non-optional type.

5. **e153 adds two APT clippy failures.** `cargo clippy -D warnings` reports
   `needless_borrow` at `apt.rs:5824` and `apt.rs:5840` after the pointer helper
   changed from a path to a `Value`. e58 had 12 errors, all in non-APT
   `s2/mod.rs`/`s2/policy.rs`; e153 has 14. This is a clean, exact regression,
   not part of the baseline.

## Test evidence

```text
apt::tests                                      78 passed, 0 failed
s2::runtime                                     60 passed, 0 failed
config::tests::apt_                              2 passed, 0 failed
release::tests::a_declared_apt                  4 passed, 0 failed
cargo check -p velnor-workflow                  passed
cargo fmt --all -- --check                      passed
git diff --check                                passed
generated APT actionlint + explicit ShellCheck  exit 0
```

Full library is not green. Exact e153 result: `1748 passed, 6 failed`.
The six failures are unchanged policy/generated-drift tests, not APT tests:

```text
s2::policy::tests::owner_entrypoint_pin_ignores_variable_references       policy/tests.rs:610
s2::tests::checked_in_workflows_match_the_generator_byte_for_byte          s2/mod.rs:16140 (ci-unit-docs.yml drift)
s2::tests::owner_policy_job_never_checks_out_workspace_root               s2/mod.rs:15944
s2::tests::policy_candidate_closure_probe_holds_no_token                   s2/mod.rs:16349
s2::tests::policy_candidate_step_binds_manifest_to_head_and_exports_it    s2/mod.rs:16192
s2::tests::policy_trust_comment_states_candidate_execution                 s2/mod.rs:16363
```

`cargo clippy -p velnor-workflow --lib --locked --offline -- -D warnings`
failed with 14 errors: the two APT errors above plus the same 12 non-APT
baseline diagnostics present at e58 (`s2/mod.rs` raw-string/argument/line/
replace/pattern lints and `s2/policy.rs:1483` too-many-arguments).

The generic non-UTF-8 conversion test passed. The real invalid-byte filesystem
test is intentionally excluded on this Darwin/APFS host by
`#[cfg(all(unix, not(target_os = "macos")))]`
(`apt.rs:7500-7516`); no real non-UTF-8 directory-entry result is claimed on
this platform.

No source or checked-in generated file was changed by this review. The owner
worktree was advancing/dirty independently; only the detached review tree was
used.

No publication approval.
