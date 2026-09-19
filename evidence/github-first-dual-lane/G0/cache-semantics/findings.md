# G1 cache-semantics evidence

Task: `G1-cache-semantics`  
Observed: `2026-09-19`  
Source workspace: `/Users/donbeave/Projects/tailrocks/velnor-project/velnor3`  
Source baseline: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

## Boundary and runtime

- Read-only repository/hosted-run investigation. No Velnor source, generated workflow, PR, branch, cache, or remote state was changed.
- Effective child runtime: `gpt-5.6-luna`, reasoning `max`; `gpt-5.6-luna` is present in `/Users/donbeave/.codex/models_cache.json` with `max` supported. `rtk 0.49.0`; configured session cap is 256 concurrent agents.
- Existing untracked `velnor-github-first-dual-lane-goal.md` was preserved.

## Hosted failure

Hosted run: [CI run 35452270126](https://github.com/tailrocks/velnor/actions/runs/35452270126)  
Failing job: [job 105921892451](https://github.com/tailrocks/velnor/actions/runs/35452270126/job/105921892451)  
Pull request: [#954](https://github.com/tailrocks/velnor/pull/954)

The job checked out synthetic merge `ad4fd8ca62c38584475a7a380bdaad5a28edf037`, described in the log as:

```text
Merge 29b7f08de144b92f29022461af275f49ff237f42 into bf2bb9d749d38da3e9c221cdf7db02e6a98c1498
```

The only failing test was:

```text
velnor-workflow-contract::cache_keys::
parameterized_callees_resolve_to_the_pre_parameterization_cache_keys
```

At `crates/velnor-workflow-contract/tests/cache_keys.rs:258`, the exact assertion was:

```text
github-hosted-docker → ci-unit-docker.yml#verify-github-hosted [docker_seed]: primary key changed
left:  velnor-docker-seed-v3-7e67216c9e23-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-trusted-only-docker-${{ hashFiles('Cargo.lock', 'Dockerfile', 'docker/build-mise.lock', 'docker/build-mise.toml', 'rust-toolchain.toml') }}-${{ hashFiles('Cargo.lock', 'deny.toml') }}
right: velnor-docker-seed-v3-743cc362f16c-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-trusted-only-docker-${{ hashFiles('Cargo.lock', 'Dockerfile', 'docker/build-mise.lock', 'docker/build-mise.toml', 'rust-toolchain.toml') }}-${{ hashFiles('Cargo.lock', 'deny.toml') }}
```

The hosted Rust job restored ordinary rustup/mold/Cargo caches and the MBX key, but did not run Docker seed creation. This is a pure generated-key/fixture comparison failure. Velnor jobs in this run were queued; they did not cause the failure.

## Cause and provenance

Confirmed stale fixture, not a package-release defect:

1. [PR revision 29b7f08d](https://github.com/tailrocks/velnor/commit/29b7f08de144b92f29022461af275f49ff237f42) changes only `crates/velnor-workflow/src/s2/primitives/package_release.rs`; it does not change cache fixtures.
2. [Base commit bf2bb9d7](https://github.com/tailrocks/velnor/commit/bf2bb9d749d38da3e9c221cdf7db02e6a98c1498) changes Mr. Boxington `1.11.1 → 1.12.0` and the action pin `v1.3.1 → v1.4.0`; generated compatibility values consequently move from Docker seed `743cc362f16c` to `7e67216c9e23` and MBX values also rotate.
3. [a72d9281](https://github.com/tailrocks/velnor/commit/a72d92811d0d9ae0ff12f15c36c45c289c2ecfba) updates the Docker seed fixture to `7e67216c9e23`; [8cb34ce5](https://github.com/tailrocks/velnor/commit/8cb34ce57cfa10e9bc901f04fb3aab4ef1f4a23f) updates MBX fixture values. Current base [af31b644](https://github.com/tailrocks/velnor/commit/af31b644aa01eb352ba6240fca957e900174d067) includes the Docker fixture update.
4. Therefore the run's merge base predates the fixture rebaseline. Restoring expected `743cc...` would mask a real producer/action compatibility change.

The follow-up head [856a222b](https://github.com/tailrocks/velnor/commit/856a222bd551f71de49468d70952e91a724060bc) has the newer fixture values but was still based on the older generated tree. Its policy run [35453337652](https://github.com/tailrocks/velnor/actions/runs/35453337652) reports generated drift in `ci-unit-rust.yml`, `preview.yml`, `release.yml`, and `.github/ci/.github-actions-generator-state`, plus no candidate within the wait window. This is a separate bootstrap/generation problem.

## Tested integration source

- The contract test reads generated `.github/workflows/ci-pr.yml` and the kind reusable directly, resolves each caller's `workflow_call` inputs, then compares primary keys, restore keys, and paths with the static golden fixture. Source: [`cache_keys.rs`](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow-contract/tests/cache_keys.rs#L1-L274), fixture: [`pre_parameterization_cache_keys.rs`](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow-contract/tests/fixtures/pre_parameterization_cache_keys.rs).
- The key renderer computes `snapshot_compatibility` and digests it before rendering unit keys: [`ir.rs`](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/s2/primitives/ir.rs#L1388-L1463).
- `CompatibilityFacts` documents an exhaustive digest and includes `mbx_version` for all payloads: [`snapshot.rs`](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/s2/primitives/snapshot.rs#L47-L120). Existing unit coverage proves a changed fact changes the digest, but does not distinguish MBX and Docker payload semantics.

## Required recovery

For PR954, the owning implementation should:

1. Update/rebase against current base `af31b644`.
2. Regenerate `.github` through the generator; never hand-edit generated files.
3. Keep the existing `a72d9281` / `8cb34ce5` fixture rebaselines.
4. Rerun the cache contract test and hosted CI on the exact candidate.

No additional fixture patch belongs in this lane.

## Regression design

Keep the parameterization golden test; it protects the invariant that `workflow_call` substitution does not alter a cache key. Add generator-level semantic tests if changing cache identity:

- MBX payload: changing MBX version changes the compatibility digest.
- Docker seed: changing Docker recipe or dependency inputs changes the digest.
- If the intended contract is to retain Docker seeds across MBX-only upgrades, explicitly test that MBX version does *not* change Docker-seed digest after splitting payload-specific facts. That would be a separate compatibility-contract change, not a fixture correction.

The current unconditional `mbx_version` field creates possible safe over-invalidation of Docker seeds, but evidence does not establish it as the hosted failure or as an invalid reuse. Defer that design change unless the cache contract is deliberately revised.

## Hosted refresh: PR953 still fails on MBX fixture

The earlier conclusion that the base fixture rotation was sufficient was too narrow. After stale-run cleanup, [PR953 head `af31b644`](https://github.com/tailrocks/velnor/commit/af31b644aa01eb352ba6240fca957e900174d067) run [35453601367](https://github.com/tailrocks/velnor/actions/runs/35453601367) still failed hosted job [105928872280](https://github.com/tailrocks/velnor/actions/runs/35453601367/job/105928872280).

The exact remaining mismatch is the MBX layer, not Docker:

```text
github-hosted-rust-policy → ci-unit-rust.yml#verify-github-hosted [mbx]: primary key changed
left:  velnor-mbx-v3-fe8982552e68-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-policy-${{ hashFiles('.cargo/**', 'Cargo.lock', 'deny.toml') }}-${{ hashFiles('.cargo/audit.toml', '.cargo/deny.toml', 'Cargo.lock', 'audit.toml', 'deny.toml') }}
right: velnor-mbx-v3-ec62b8b58bab-${{ runner.os }}-${{ runner.arch }}-github-hosted-linux-x64-untrusted-ok-rust-policy-${{ hashFiles('.cargo/**', 'Cargo.lock', 'deny.toml') }}-${{ hashFiles('.cargo/audit.toml', '.cargo/deny.toml', 'Cargo.lock', 'audit.toml', 'deny.toml') }}
```

The hosted log confirms the candidate runtime is valid (`EXPECTED_REVISION=fdeed261...`, manifest/binary/policy closures verified), and D19 pin fetch succeeds. Five of six contract tests pass; only the static cache-key golden comparison fails. The MBX fixture update is carried by [8cb34ce5](https://github.com/tailrocks/velnor/commit/8cb34ce57cfa10e9bc901f04fb3aab4ef1f4a23f), which is not in PR953 base `af31b644`. Therefore:

- Docker fixture rotation in `af31b644` is necessary but insufficient.
- A candidate is not “recovered” because one fixture family matches; all generated compatibility families must match the exact source epoch.
- PR954's `8cb34ce5` MBX rotation remains the intended overlap, but its hosted run must be checked directly before declaring the failure closed.

## Hosted refresh: PR954 has no cache verdict yet

Current PR954 head is [f16592ea](https://github.com/tailrocks/velnor/commit/f16592ea165ced141bf0bb1c43466a95d7df8b2e), and run [35454970877](https://github.com/tailrocks/velnor/actions/runs/35454970877) is the exact hosted run observed. Its `velnor-workflow` job [105928871758](https://github.com/tailrocks/velnor/actions/runs/35454970877/job/105928871758) checked out the synthetic pull-request merge (`refs/pull/954/merge`, merge SHA `2860ef3d961ba269daeafce799387c9918a0a44c`) and failed before the contract test binary ran.

The failure is an unrelated source lint error:

```text
crates/velnor-workflow/src/s2/primitives/package_release.rs:356
error: this `if` statement can be collapsed
clippy::collapsible-if; -D warnings
```

That job reports MBX as cold, but it reports no cache-key assertion and no `parameterized_callees_resolve_to_the_pre_parameterization_cache_keys` result. The run therefore cannot prove or disprove the cache fixture correction. PR954 includes both fixture rebaseline commits (`a72d9281` Docker and `8cb34ce5` MBX); a green contract run is still required.

## Typed release verification review

The current hosted proposal is structurally sound for the stated recovery contract:

- `.github-gen/velnor-workflow.toml` keeps `providers = ["github-hosted", "velnor"]` as the universe and declares `[release].verification_providers = ["github-hosted"]`. This removes Velnor release prerequisites while retaining the provider for later full qualification. Omission remains meaningful: `release_verification_providers` falls back to `config.providers`, preserving normal final dual verification rather than silently following automatic/default dispatch routing.
- `ReleaseSection` has `serde(deny_unknown_fields)` and a typed optional provider list. Validation rejects unknown, duplicate, empty, and outside-universe sets; source tests cover each case at `crates/velnor-workflow/src/s2/config/mod.rs:3880-3929`.
- Stable release rendering selects the explicit set in `render_release_unit_jobs`; selected IDs flow into build/publish `needs`. The renderer's existing provider-universe gate still makes omitted-field configs require the full configured universe. Direct renderer coverage checks hosted-only stable jobs and hosted-only preview publisher behavior at `crates/velnor-workflow/src/s2/primitives/release.rs:5515-5575`.

Review conditions before accepting the implementation:

1. The preview renderer has no release-verification jobs today (`render_preview` emits build/publish plus optional guest payload). Therefore `verification_providers` affects stable verification only; preview coverage must explicitly assert that this is intentional. If “preview+stable” means preview verification prerequisites too, the current helper is incomplete and preview needs its own selected jobs/`needs` wiring.
2. Add one parsed-config/generator-surface test, not only direct `ProjectConfig` rendering, proving `[release] verification_providers = ["github-hosted"]` produces no `release-velnor-*` job or dependency in generated `release.yml`, while omission still emits both providers. Current tests validate parser and direct renderer separately.
3. Keep the field independent of `automatic_providers` and `default_dispatch_providers`; the comments and fallback do this correctly. Do not change the default to automatic/default routing, or ordinary final dual releases become silently single-lane.

## Candidate-bootstrap trust review

The existing producer is coupled to the Rust unit reusable (`ci-unit-rust.yml:559-634`, `candidate_publish` input at `:105-108`, enabled by PR `ci-pr.yml:1113` and main `ci-main.yml:1289`). It runs after the unit command, so a generator test failure prevents candidate publication. Moving it to an independent same-repository PR bootstrap job is the right structural fix, provided these invariants are retained:

- Trigger with `pull_request`, never `pull_request_target`; require immutable same-repository identity (repository ID/full name and exact `head.sha`), and checkout/build the exact head commit. Fork PRs must not execute a candidate producer or receive artifact acceptance.
- Use hosted runners only, `contents: read`/artifact permissions only, no secrets, `id-token`, packages, self-hosted labels, Velnor admission, or PR-local actions/composites. Pin every third-party action by reviewed full SHA; use the trusted base setup/runtime, not a PR action path.
- Build the exact head with `cargo build --locked -p velnor-workflow` in an isolated checkout/environment. Candidate builds execute PR code, so scrub token/credential variables, disable credential persistence and wrappers/caches, and keep the job unprivileged.
- Compute the expected candidate closure with the trusted pinned runtime before accepting the artifact. The manifest must bind `repository` (and immutable repository ID when available), exact `head.sha` as both `revision` and exact-head `build_revision`, selected run/job IDs, full closure, profile/features/platform, and binary SHA-256. The artifact name's 16-hex prefix is only a locator; policy must compare all full fields and the selected run/job to prevent stale same-prefix reuse.
- Verify the digest before executing the candidate; then compare the candidate's `--closure` report to the trusted full closure. A self-reported closure alone is not evidence.
- Remove `candidate_publish` and both old candidate steps completely. Keep `ci-required` wired to every real unit/provider result so a candidate producer can succeed independently while a failing generator test still fails the aggregate. Candidate producer failure must remain a separate fail-closed policy prerequisite when a candidate is required.

The source already documents the closure boundary and candidate/default-feature distinction at [`closure.rs`](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/s2/closure.rs#L64-L177), and policy acceptance binds manifest closure plus pre-execution digest at [`policy.rs`](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/s2/policy.rs#L1458-L1544). The bootstrap change must preserve those trust boundaries while removing the producer's dependency on the unit test result.

Current policy evidence for the source-identity gap: `ci-policy.yml:129` only shape-checks `revision` as a 40-hex string; it does not compare it to `HEAD_SHA`, and it ignores `build_revision`, artifact job identity, and an immutable repository ID. The Rust `CandidateManifest` parser at `crates/velnor-workflow/src/s2/policy.rs:1196-1207` deserializes only `revision`, `closure`, and `binary_sha256`; extra producer fields are not consumed by policy. A bootstrap implementation that merely adds those fields to JSON without extending the consumer gate is not a trust fix.

Cache architecture enabling condition: the parameterization contract intentionally stores a manually maintained pre-parameterization key table, while compatibility digests are derived from mutable generator facts (`MR_BOXINGTON_VERSION`, action pins, provider/platform/trust). The static table's provenance comment is descriptive only; no source-epoch assertion forces a fixture rebaseline when those facts rotate. Keep the table because it protects the caller/callee substitution invariant, but pair any compatibility-fact or tool-pin change with the fixture update and a generator-level payload test. Do not “fix” this failure by reverting the renderer's current digest or by weakening the static comparison.
