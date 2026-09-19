# Exact schema-2 APT discovery/runtime review

## Decision

**Reject G2 source/integration approval.** Commit `93efe18e5b7d4218487ce4a628b56cf7293d1aa7`
contains a useful typed schema-2 APT selection/runtime seam, but the renderer and
repository configuration still do not use it. The producer-owned application
manifest/digest handoff is not present, several required hostile cases are not
covered, and incoming selected assets follow symlinks. No publication, install,
upgrade, or G2 delivery claim is authorized.

This is an APT-source review only. It does not approve G1 bootstrap or the mixed
policy-generator changes carried by this branch.

## Exact scope

- Worktree: `/private/tmp/dual-lane-apt-schema2`
- Repository/branch: `tailrocks/velnor`, `dual-lane-apt-schema2`
- Reviewed commit: `93efe18e5b7d4218487ce4a628b56cf7293d1aa7`
- Reviewed parent: `10e31de639e806d350620b2ac6e58647f302956a`
- Worktree state: clean before and after review.
- Cross-contract authority: `G0/distribution/apt-discovery.md`, the
  authoritative dual-lane goal, and the one producer-owned
  `velnor.product-manifest/v1` contract.

## What the typed seam proves

The following narrow source behavior is present:

- `crates/velnor-workflow/src/apt.rs:1512-1738` requires an exact selection
  object shape, canonical product-manifest schema/asset, stable or preview
  source identity, release ID grammar, canonical GitHub release and asset URLs,
  uploaded asset state, manifest-artifact presence, and required subordinate
  asset names.
- `apt.rs:1744-1808` materializes every selected release asset by immutable
  provider asset ID (`repos/<owner>/<repo>/releases/assets/<id>`); it does not
  select `latest`, a mutable preview pointer, or a tag at this boundary.
- `apt.rs:1815-1890` binds persisted `discovery.json`, the selected inventory,
  asset names/sizes, canonical product-manifest bytes/digest/sidecar, and every
  manifest artifact before downstream verification.
- `s2/runtime.rs:3861-3999` routes schema-2 `apt-fetch`, `apt-verify`,
  `apt-publish`, and `apt-channel-update` through the selection. Publication
  passes `Some(&selection)` into the publication record path.
- `s2/dispatch.rs:38-82` routes a schema-2 repository's runtime invocation to
  the schema-2 runtime; the legacy runtime entrypoint is a separate fallback for
  non-schema-2 repositories, not the schema-2 APT implementation.
- `apt.rs:2979-2983` refuses publication without `.reprepro-ok`. Existing APT
  tests also cover successful sentinel arming and a dropped-sentinel publish
  refusal.

These are source-level and fixture-level results only. They are not a package
delivery result.

## Blocking findings

### 1. Schema-2 rendering/configuration is not integrated

`s2/mod.rs:2238-2408` copies release fields into the IR but never calls
`AptContract::resolve_s2` during release application. The only schema-2
`resolve_s2` call is the runtime `apt-publish` path at `s2/runtime.rs:3967-3969`.
`release_contract_complete` at `s2/primitives/release.rs:899-910` treats an APT
contract as complete with only package and consumer repository fields.

The APT renderer remains the generic package-feed workflow at
`s2/primitives/release.rs:4212-4237`; it does not render discovery execution,
selection persistence, ID materialization, selection-bound verification, or
selection-bound publication. The checked-in `.github-gen/velnor-workflow.toml`
declares `[release].kind = "native"` and has no `discovery_script`,
`canonical_manifest_asset`, or `canonical_manifest_schema` fields. The checked-in
`.github/workflows/release.yml` consequently renders the older native release
surface rather than the schema-2 APT contract.

This fails the required typed renderer/config integration gate. It also means
the local selection JSON is not produced by the configured workflow.

### 2. Producer manifest/digest handoff and complete census are absent

No producer release, immutable source attestation, parent digest, or actual
`velnor.product-manifest/v1` asset is present in this branch's evidence. The
discovery fixture at `apt.rs:4741-4870` uses only two APT artifact rows and
placeholder bytes for `release-record.json`, `manifest.json`, and related
subordinate assets. It does not exercise a complete producer release with all
canonical product rows and parent-digest-linked subordinate records.

`validate_product_manifest_selection` (`apt.rs:1294-1437`) checks that component
rows are non-empty, unique, and internally name-aligned, but it does not require
the contract's complete required component set, validate component version
grammar, or establish a component-to-artifact census. It also permits release
asset names beyond the manifest/required set. The two discovery tests therefore
do not prove the required `velnorctl`, `velnor-runner`, and `velnor-workflow`
product inventory or complete APT/Homebrew artifact inventory.

The canonical producer must remain the sole manifest authority. APT must consume
that manifest as a filtered projection; an APT-only synthetic fixture is not a
producer handoff.

### 3. Selected incoming paths accept symlinks

`apt.rs:914-923` implements `require_file` with `Path::is_file()`, which follows
symlinks. `verify_discovery_incoming` then calls `require_file`, `metadata`, and
`sha256_file` on selected asset paths (`apt.rs:1841-1888`). A selected asset
symlink pointing outside `incoming` can therefore pass when its target has the
expected size/digest. There is no `symlink_metadata`, no-follow open, or
realpath-confinement check. The same issue applies to the canonical manifest and
sidecar paths.

The fetch path creates a fresh directory and uses a temporary file plus rename,
but the workflow artifact handoff is a separate trust boundary and can carry
symlinks. A path-symlink hostile fixture is required before this is admissible.

### 4. Required hostile fixtures are incomplete

The three new discovery tests (`apt.rs:4903-5057`) cover immutable provider ID
logging, byte/size tamper, an unknown incoming file, omitted manifest artifact,
component identity drift, canonical release URL drift, and a direct preview
ancestry helper. They do not cover:

- release-ID mutation or invalid release-ID grammar;
- stable source-ref/source-commit/tag mutation after selection;
- an extra release asset in the persisted selection (the parser currently
  accepts additional uploaded assets); or
- a selected asset, manifest, sidecar, or `discovery.json` symlink.

The parser has source/release-ID checks, but untested checks are not hostile
evidence. The preview helper test mutates `merge_base_commit`; it is not a full
selection read/verify/publish mutation test.

### 5. Version grammar permits SemVer leading zeros

`apt.rs:316-323` accepts `01.2.3` and other numeric components with leading
zeros. `parse_preview_version` (`apt.rs:339-355` and the product-preview parser
at `1270-1287`) likewise accepts leading-zero base components and preview
sequence numbers. The tests at `5307-5344` do not reject these cases.

The shared release contract requires canonical SemVer/preview numeric grammar;
otherwise semantically duplicate versions can acquire distinct release
identities. This is the same hostile H1 class previously identified in the
distribution review and must be fixed in the canonical producer/consumer grammar,
not waived per channel.

### 6. Provider-backed source provenance is not established by this runtime

`validate_source_ref_resolution` (`apt.rs:1440-1500`) checks the shape and values
of a locally supplied JSON proof. `read_discovery_selection` then trusts that
document. No discovery script is invoked by the renderer, no GitHub ref/API
response is collected here, and no cryptographic/provider attestation is
verified by the APT runtime. The source proof is useful as a typed contract
field, but without the producer/helper handoff it is self-declared JSON, not
provider-backed provenance.

Likewise, this source contains no same-version release ambiguity/rerun selector:
it accepts one already-selected JSON document. The external producer discovery
authority must reject same-version candidates and bind the rerun to the
canonical manifest digest before this consumer can claim H2 immutability.

## Hidden sentinel and channel state

The lower-level APT publisher correctly requires `.reprepro-ok` before mutation,
and existing tests cover both retained sentinel and dropped sentinel cases.
However, `verify_discovery_incoming` only adds the sentinel to its allowed-name
set (`apt.rs:1827-1839`); it does not itself require the file. The publication
caller currently reaches `publish_suite`, which performs the requirement. The
future generated artifact upload/download path must explicitly include hidden
files and test a missing sentinel at the exact handoff.

`bind_selection_channel_state` (`apt.rs:4522-4571`) carries canonical manifest
digest, product version, release ID, release tag, and provider release ID while
retaining the stable/preview pair. No generated workflow currently invokes this
path.

## Verification

Commands were run in the exact clean worktree:

```text
rtk proxy cargo test -p velnor-workflow --all-features apt::tests::discovery_selection
  3 passed, 0 failed
rtk proxy cargo test -p velnor-workflow --all-features apt::tests::
  66 passed, 0 failed
rtk proxy cargo test -p velnor-workflow --all-features s2::runtime::tests::
  48 passed, 0 failed
rtk cargo check -p velnor-workflow --all-features
  pass
rtk cargo check --workspace --all-features
  pass; one unrelated velnor-runner warning
rtk cargo fmt --all -- --check
  pass
rtk git diff --check HEAD^ HEAD
  pass
```

The full package all-feature test run is not green: `1732 passed, 6 failed`.
The failures are stale checked-in/generated policy/workflow expectations (for
example `s2::tests::checked_in_workflows_match_the_generator_byte_for_byte`),
not a basis for G1 or G2 approval.

### Clippy attribution

`rtk proxy cargo clippy -p velnor-workflow --lib --all-features -- -D warnings`
reports 12 failures in the mixed policy-generator base:

- `s2/mod.rs:4766-4978` is attributed to `c63c1c5d89888b9c93f53d9d11f1f8003804db9b`;
- the candidate-render argument at `s2/policy.rs:1483-1492` was added by
  `52427e82b`; the function predates the APT work;
- the surrounding policy function was introduced by the older `eb0303a34` base.

The latest `93efe18e` commit's APT/runtime edits do not introduce a new library
clippy diagnostic; its `allow` attributes document intentional typed-gate and
error-adaptation ownership. The all-targets command adds three diagnostics from
the new `10e31de6` discovery fixture itself:

- unused `DiscoveryFixture.incoming` at `apt.rs:4729`;
- `format_push_string` at `apt.rs:4892`;
- `expect_used` at `apt.rs:4978`.

These new fixture diagnostics are real and must not be counted as pre-existing
bootstrap warnings. No source edit was made during this review.

## Gate disposition

- Typed S2 APT source seam: **narrow fixture pass only**.
- APT renderer/config integration: **missing**.
- Producer digest/parent-record handoff: **missing**.
- Complete product/component/artifact census: **unproven**.
- Hostile selection/path tests: **incomplete; symlink acceptance found**.
- Full generator/workflow test state: **failed with stale generated output**.
- G1 hosted bootstrap: **separate, not approved by this report**.
- G2 preview/stable publication, install, upgrade, channel-switch, and real
  endpoint evidence: **none**.

No product source files or generated workflows were edited. This report is the
only review artifact created by this bounded read-only review.
