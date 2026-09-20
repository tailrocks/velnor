# Schema-2 APT hardening review — independent

Date: 2026-09-20

Reviewed commit: `c678b6397e2941f16ba42bed8b6d68dd98296c42`

Review worktree: `/private/tmp/dual-lane-apt-schema2-apt-review` (detached, clean, no source edits)

Scope: source-only. No publish, install, dispatch, merge, Velnor, or MacDocker execution.

## Verdict

**BLOCKED.** The narrow APT hardening tests pass, but a real generated schema-2 APT workflow is not executable: `actionlint` finds a shell parse error and an undefined workflow output. Security handoff is also incomplete: native producer integration is absent, provider/release authority is not independently checked, and verification-to-publication remains TOCTOU vulnerable.

## Exact hashes

Source:

```text
apt.rs                                      726c949a71e6c966f82f76dac9b1ecf62330e60381b74fc8f100d89dfd13b852
s2/config/mod.rs                            cd72513e3584fe21e3bfaa626c4bcc18ad81fb231784bee64cb92e72f343940d
s2/primitives/release.rs                    fe5381f3e83f07b12ca6b6bc2e19ca823f635a35656f27b96ac96cc14980de14
.github-gen/velnor-workflow.toml            9911f537d1621a265ec6037475d8d5f8bf16bf7d918440cb4dd9a0120f3acd54
.github/workflows/release.yml                5e3a8f4826f8074ed8d8b1394c898d3b3bd9febb06a38644b43ef54e50d9bc3f
```

Generated APT fixture (external, from the same binary/revision):

```text
/private/tmp/apt-render-probe/.github/workflows/release.yml
89cf348dc7468ab671e4aeabb73d65f19fd89b517e98dca375b565eb6b3a8442
```

## Verification evidence

Pass:

```text
cargo test -p velnor-workflow --lib apt::tests --locked --offline       73 passed
cargo test -p velnor-workflow --lib s2::runtime --locked --offline      60 passed
cargo test -p velnor-workflow --lib s2::config::tests::apt_ --locked --offline  2 passed
cargo test -p velnor-workflow --lib s2::config::tests::declared_apt_discovery_script_must_exist --locked --offline  1 passed
cargo test -p velnor-workflow --lib s2::primitives::release::tests::a_declared_apt --locked --offline  2 passed
cargo check -p velnor-workflow --all-features --locked --offline        passed
cargo fmt --all -- --check                                           passed
git diff-tree --check 0712a54952ec4a3d5aa979790aa23f842a954e0e c678b6397e2941f16ba42bed8b6d68dd98296c42  passed
```

`cargo clippy -p velnor-workflow --lib --locked --offline -- -D warnings` fails with the same 12 diagnostics recorded in the prior independent baseline (`s2/mod.rs` and `s2/policy.rs` only); no `apt.rs` diagnostic is emitted. Prior baseline: `apt-schema2-followup-4c4b7a7-independent.md`, lines 143–149.

The checked-in native workflow passes `actionlint`.

The generated APT fixture was made from an external schema-2 APT config, then checked with `actionlint` and extracted `bash -n`. It fails before any hosted execution.

## Blocking findings

1. **Generated APT shell is syntactically invalid.** A generated fixture fails `actionlint`/ShellCheck at `release.yml:89` (`SC1073`, `SC1072`); extracted `bash -n` reports `line 31: verify_args+=(--verify-oci true)` because the preceding array assignment is missing `)`. Renderer source is [`release.rs:4323-4327`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/s2/primitives/release.rs#L4323-L4327). Existing renderer coverage only asserts strings, so it missed executable-shell validity.

2. **Generated deploy reads an undeclared output.** `actionlint` reports generated `release.yml:251`: `needs.publish.outputs.channel` is not defined. The publish job has no `outputs:` block ([`release.rs:4331-4347`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/s2/primitives/release.rs#L4331-L4347)); deploy consumes that missing output at [`release.rs:4447-4451`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/s2/primitives/release.rs#L4447-L4451).

3. **Native producer handoff is absent in this source stage.** Checked-in config declares `[release] kind = "native"` and only a generic release declaration (`.github-gen/velnor-workflow.toml:65-78,293-295`); checked-in release explicitly says it does not push or dispatch `tailrocks/velnor-apt` (`.github/workflows/release.yml:4381-4383`). No native producer integration/attestation path produces the schema-2 APT selection. This is a handoff blocker, not a test-only issue.

4. **Provider/release/source authority remains self-declared.** `parse_discovery_selection` validates shape, IDs, canonical URLs, and field consistency ([`apt.rs:2030-2282`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/apt.rs#L2030-L2282)) but performs no provider release lookup. `run_fetch_selection` calls only numeric `gh api repos/{source}/releases/assets/{id}` endpoints and checks returned byte length ([`apt.rs:2401-2471`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/apt.rs#L2401-L2471)); it does not compare live release ID/tag/name/state/URL metadata. Generated shell performs a shallow `jq` check and attests downloaded `.deb` files only ([`release.rs:4289-4327`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/s2/primitives/release.rs#L4289-L4327)); canonical manifest, subordinate records, release identity, and selection are not independently attested.

5. **Verification-to-publication TOCTOU is still exploitable.** `apt_publish` calls `verify_discovery_incoming` once, then `publish_suite` checks only a sentinel containing the selection digest ([`runtime.rs:3959-3966`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/s2/runtime.rs#L3959-L3966), [`apt.rs:1145-1174`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/apt.rs#L1145-L1174), [`apt.rs:3640-3657`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/apt.rs#L3640-L3657)). `stage_package` reopens candidate `.deb` files and checks package/version/architecture, but does not bind bytes to the manifest/selection digest ([`apt.rs:3753-3785`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/apt.rs#L3753-L3785)). Replacing a verified candidate with a new single-link regular `.deb`, or swapping the incoming directory after verification, preserves the selection-bound sentinel while changing published bytes. Fix requires an artifact/inode-bound proof held through publication or an atomic re-verification boundary.

6. **Archive extraction is not confined by code.** `deb_extract_data` creates destination with path-based `create_dir_all` and invokes `dpkg-deb -x` or raw `tar -x -C` without archive-member confinement/filtering ([`apt.rs:2738-2783`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/apt.rs#L2738-L2783)). Control extraction uses the same raw tar path ([`apt.rs:2619-2631`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/apt.rs#L2619-L2631)). Existing hostile archive tests only place symlink/hardlink parents and assert the final identity lookup fails (`apt.rs:7102-7121`); they do not test traversal members, destination symlinks, or write-outside extraction. A BSD-tar probe refused one symlink case on this Mac, but no Linux/GNU-tar proof was run; source remains unconfined across supported backends.

7. **Hostile-entry test coverage is weaker than the requested proof.** Implementation enumerates raw directory names and rejects non-UTF-8, but `incoming_directory_listing_rejects_non_utf8_entries` calls `directory_entry_name(b"invalid-\\xff", ...)` directly rather than creating a non-UTF-8 filesystem entry and running `dir_names`/`verify_discovery_incoming` (`apt.rs:5875-5884`). Likewise, `run_fetch_selection` still uses `dir.exists`/`create_dir_all`/path `rename` ([`apt.rs:2408-2468`](https://github.com/tailrocks/velnor/blob/c678b6397e2941f16ba42bed8b6d68dd98296c42/crates/velnor-workflow/src/apt.rs#L2408-L2468)); the component-wise no-follow reader does not make those writes race-safe.

## Positive evidence / no regression

Canonical product target census, duplicate Linux APT target rejection, canonical `-amd64.deb`/`_amd64.deb` and `-arm64.deb`/`_arm64.deb` identity, no-follow FD reads, hard-link rejection, and exact asset-name census are implemented and covered by the passing 73-test APT slice (`apt.rs:1681-1946`, `5841-5873`, `7102-7121`). The c678 diff touches only Cargo metadata plus APT/config/release renderer files; no platform/Xcode/Intel lane file changed. The checked-in native workflow remains actionlint-clean.

