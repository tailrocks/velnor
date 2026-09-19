# G2 APT discovery checkpoint

Observed: 2026-09-20. Source worktree: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-apt`.

## Revisions

- Consumer base: `tailrocks/velnor-apt` main
  `b24d7d4370001119cd5ddcb6f9e07aa9007051e7`.
- Discovery implementation: commit
  `8c19fab405b8c608c5db810d4f01d4f3c65b08cd`, branch
  `codex/github-first-apt-discovery`, pushed to `origin` at the identical
  SHA. No dispatch or publication.
- Generator pin currently in the base consumer: `fdeed261bd2247a38db6922a7726cd45d3d6f31e`.
- Canonical Homebrew contract source inspected at local commit
  `b1c94246216d0006e9b7639b1a4a294212ce0bf7`.
- Product/review baseline: `tailrocks/velnor3`
  `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.

## Implemented source prerequisite

`scripts/release-discovery.sh` now:

- reads every page from the GitHub Releases API with `gh api --paginate`;
- selects only immutable application releases, not
  `velnor-workflow-runtime-*` or a mutable `preview` pointer;
- validates channel-specific tag/ref/commit/version identity against one
  canonical `velnor.product-manifest/v1` `product-manifest.json`;
- independently resolves the immutable `refs/tags/<release-tag>` through the
  GitHub ref API (including annotated-tag dereference) and compares its commit
  with release metadata and the canonical manifest;
- enforces exact three-component/binary/target inventory and a shared
  release-ID grammar, exact HTTPS release-asset URLs, complete APT amd64/arm64
  rows, downloaded canonical artifact bytes/sizes, per-deb sidecars, and exact
  `SHA256SUMS` rows;
- verifies subordinate `velnor.release-record/v1` architecture/package/OCI/APT
  census and `velnor.package-release.v1` inventory, exact parent manifest
  digest equality, source provenance, and immutable release identity;
- rejects non-APT canonical artifact byte/digest tampering too, so the output
  cannot claim a full product manifest while silently trusting a Homebrew row;
- emits both canonical `release_id` and provider numeric release ID plus
  manifest digest, source identity, ref-resolution proof, release URL, and
  exact asset provenance;
- never mutates package state. Stable and preview selection are independent;
  stable sorts validated SemVer and preview sorts validated immutable
  `preview.N+sha7` versions, rejecting same-key ambiguity.

`scripts/test-release-discovery.sh` covers mixed paginated pages, a newer
runtime release, prerelease rejection, incomplete/invalid application assets,
wrong source/ref, malformed manifest, explicit preview release, mutable
preview pointer, no eligible release, API listing failure, asset API failure,
hostile component/SHA256SUMS/subordinate/census/non-APT/URL/ref/grammar/tie
mutations, and preservation of retained stable/preview state. The independent
adversarial harness at
`G0/distribution/apt-discovery-adversarial.sh` now reports baseline pass and
nonzero exit for every hostile case. `mise.toml` wires shellcheck and the
fixture test into `check`.

## Remaining integration dependency

This commit is a source/test prerequisite only. The producer must first emit,
for each immutable application release:

1. `product-manifest.json` plus its external `.sha256`, exact canonical fields,
   complete Linux artifact rows and three-component inventory. Every canonical
   artifact row must be uploaded with exact bytes, size, and the producer's
   artifact name; the APT consumer verifies all rows, including non-APT rows,
   before emitting a selection.
2. immutable stable `vX.Y.Z` or preview `preview-<full-source-sha>` GitHub
   release assets, with preview bound to `refs/heads/main` and the exact
   source commit;
3. subordinate `release-record.json`, `release-manifest.json`, and compiled
   `manifest.json` with top-level `parent_manifest_sha256` equal to the
   canonical manifest digest and matching sidecars where consumed. The release
   record must expose exactly amd64/arm64 architecture rows, package/deb
   digests, manifest version/hash, OCI identity, and APT suite/component; the
   package release manifest must expose exactly the two `{name,sha256}` APT
   rows. The current Velnor/APT producer does not yet supply this parent link,
   so old record-only releases must not be accepted as a fallback.

The consumer generator/config still needs a reviewed producer pin and a
regenerated `.github/workflows/release.yml`; generated files were deliberately
not edited here. Integration must replace the current `gh release list --limit
1` and rolling `gh release download preview` path at
`.github/workflows/release.yml:96-115` with this helper, then route the
canonical projection into the existing verify-before-publish flow. It must
also carry the hidden `.reprepro-ok` sentinel through artifact upload and keep
stable/preview retained rollback state. No package publication is authorized
before G1 recovery and independent workflow review.

Cross-consumer contract note: the APT helper now names the producer-owned
release-ID grammar as `^[A-Za-z0-9][A-Za-z0-9._:/-]*$`, which permits
repository-qualified IDs. The Homebrew validator at `b1c9424` still narrows
this to `[A-Za-z0-9._:-]`; its owner must widen it to the shared grammar before
integration. No legacy or alternate manifest path is enabled.

## Verification

- `mise run check` passed at `8c19fab`: actionlint, shellcheck, discovery
  fixtures, package-update fixtures, and verify-release fixtures.
- External adversarial harness passed: baseline `exit=0`; component drift,
  SHA256SUMS tamper, release-manifest inventory tamper, record census omission,
  Homebrew bytes tamper, missing URL, ref mismatch, release-ID grammar, and
  same-version ambiguity all exited nonzero.
- `git diff --check` passed.
- No Mac runtime was operated.

Evidence URLs for the observed base failures and contract are in
`findings.md` and the independent review at
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/distribution-review/report.md`.

## Generated integration design (proposal; not implemented)

The generic renderer must gain a typed APT discovery contract, not a YAML
escape hatch or repository-name special case. The active schema-2 generator
uses provider sets, not the legacy string flags: source definitions are
`dual-lane-generator/crates/velnor-workflow/src/s2/config/mod.rs:172-192`, the
typed provider IR is `dual-lane-generator/crates/velnor-workflow/src/s2/mod.rs:1003-1020`,
and provider resolution is
`dual-lane-generator/crates/velnor-workflow/src/s2/mod.rs:1818-1855`. The owned generator seam should add discovery to
the typed release/provider model and render hosted provider selection from
`github-hosted`; it must not add `runners`, `automatic`,
`default_dispatch_runner`, or `automatic_lanes` as a new migration path.

The typed validator accepts only a safe repository-relative script path and
producer/schema scalars. It invokes the script after checkout with fixed
arguments. The script remains the sole release selector; the renderer never
calls `gh release list`, resolves `latest`, or selects a preview pointer.

The typed workflow step runs the script once, stores stdout as
`incoming/discovery.json`, and downloads only the selected
`.release_assets[].id` values through the GitHub API (including canonical
manifest/sidecar, subordinate records, both debs and sidecars, and
`SHA256SUMS`). The script output carries `source_ref_resolution`, exact asset
IDs/names/sizes/URLs, canonical `manifest_sha256`, and selected source
identity. No later step may rediscover by tag or call the old `apt-fetch`
selector. If a future source-owned output-directory mode is added, it must
preserve this immutable-ID binding rather than create a second selector.

The generated verify step derives only from `discovery.json`:

```text
version       = .version
source_ref    = .source_ref
source_commit = .source_commit
release_tag   = .release_tag
manifest_sha  = .manifest_sha256
```

An optional dispatch commit is an equality assertion against `.source_commit`,
never an override. Attestations use `.source_ref` and `.source_commit`; the
typed `apt-verify` receives the selection path and canonical manifest contract,
checks the canonical digest/identity and APT artifact projection, then runs
the existing record/deb/OCI checks and arms `.reprepro-ok`. `apt-publish` gets
the same immutable `incoming` artifact; `apt-channel-update` also consumes the
selection so its state/publication record carries the canonical manifest
digest/release ID. Existing rollback recovery remains feed-state based and
must retain the other suite; immutable `release_tag` is provenance, never a
source pointer.

### File ownership

- APT worktree: source correction is complete at `8c19fab`; central config and
  generated workflows remain unowned here. Do not add legacy lane flags or
  hand-edit `.github`.
- Generator worktree (separate owner): schema-2 config/IR/provider and APT
  render seam plus typed/render tests. Add discovery to
  `ReleaseSection`/`ReleaseSpec`/`AptContract` only through the active typed
  model; render the fixed invocation, selection-bound verify, canonical channel
  update, and hosted provider choice.
- Producer worktree: emit canonical manifest and subordinate parent links;
  resolve preview package-version projection before consumer integration.

Hosted-only S2 selection is `providers = ["github-hosted"]`,
`automatic_providers = ["github-hosted"]`, and
`default_dispatch_providers = ["github-hosted"]`; exact config ownership and
syntax remain with the schema-2 generator owner.

## Schema-2 integration handoff (observed at `5fe544b77a879d16a2656799f4cd6ddf23a03999`)

The typed seam is pushed at
`https://github.com/tailrocks/velnor/commit/5fe544b77a879d16a2656799f4cd6ddf23a03999`
(`dual-lane-apt-schema2`). It is configuration/IR only: no APT discovery
render, selection-bound verifier, publisher, or schema-2 runtime entrypoint is
implemented yet. `AptContract::resolve_s2` exists at
`crates/velnor-workflow/src/apt.rs:645-713`, but the only `resolve_s2` symbol
in the commit is its definition; it is not called by `apply_release` or the
renderer. Bootstrap must wire that one validator; do not duplicate it.

### Exact old-to-schema-2 mapping

| Old S1 contract/path | Schema-2 owner/required behavior |
| --- | --- |
| `[workflow] runners`, `automatic`, `default_dispatch_runner`, `automatic_lanes` | `[workflow] providers`, `automatic_providers`, `default_dispatch_providers`; each exactly `['github-hosted']`, with the hosted selector. `WorkflowSection` is strict (`s2/config/mod.rs:172-192`); old keys must fail. |
| `[release] kind/package/binary/source_repository/consumer_repository/description` | Same typed `ReleaseSection` and `ReleaseSpec` fields (`s2/config/mod.rs:341-388`, `s2/mod.rs:882-925`). |
| Old `manifest_schema = velnor.package-release.v1` | Retain as subordinate package-release schema (`ReleaseSpec::manifest_schema`); it is not the application manifest authority. |
| Old APT signer/secrets/keyring/origin/identity/feed/retention/arches | Typed fields at `ReleaseSection` lines `371-388` and `ReleaseSpec` lines `908-925`; `AptContract::resolve_s2` delegates generic validation to `resolve`. |
| No old selector (old `gh release list --limit 1`; rolling `gh release download preview`) | Required `discovery_script` (`s2/config/mod.rs:362-369`, `ReleaseSpec:902-907`) executes once after checkout. Its JSON is the sole selection authority; no `latest`, tag rediscovery, or mutable preview pointer. |
| Old `apt-fetch` tag/pointer download | Materialize every selected `release_assets[].id` from the persisted discovery JSON through the GitHub asset API. Bind ID, name, size, URL, and downloaded bytes; never re-resolve by tag. |
| Old `apt-verify` inputs | Read only persisted selection: `version`, `source_ref`, `source_commit`, `release_tag`, `manifest_sha256`, `release_id`, canonical schema, and exact asset census. An optional dispatch commit is an equality assertion, never an override. |
| Old attestation + `apt-publish` + `apt-channel-update` | All consume the same staged `incoming` and selection record. One hosted publisher signs/verifies/publishes; channel state carries canonical digest and release ID while retaining the other suite/rollback pair. |
| Old hidden sentinel upload | Preserve `.reprepro-ok` with hidden-file artifact upload/download and assert it is present before publication. |

The new application fields are `canonical_manifest_asset` and
`canonical_manifest_schema` (`velnor.product-manifest/v1`), distinct from the
subordinate `manifest_schema`. `canonical_manifest_asset` is a required
selected asset, not a second inventory. The discovery output must bind the
canonical manifest digest, full product artifact inventory, subordinate parent
records, immutable source-ref proof, and every downloaded asset ID/URL/size;
publication must reject any post-selection mutation. The manifest has an
external digest and no self-hash field. Note: `resolve_s2` currently checks
only non-empty, whitespace-free canonical schema text and a safe asset name;
the producer contract and integration test must require the exact
`velnor.product-manifest/v1` value and the agreed `product-manifest.json`
asset, not merely pass arbitrary strings.

### Bootstrap implementation requirements

1. Call `AptContract::resolve_s2` from the schema-2 release application/render
   path and fail closed for missing or malformed selector/manifest fields.
2. Extend the APT release renderer from the current generic
   `render_package_feed` call (`s2/primitives/release.rs:4218-4225`) to a
   hosted-only sequence: checkout, run the configured script once, persist
   `incoming/discovery.json`, exact-ID materialization, selection-bound verify,
   then one publisher/channel update. The current generated `verify-feed` and
   `update-feed` stubs (`s2/runtime.rs:3052-3065`, `3714-3764`) only check feed
   directories and cannot implement this contract. Schema-2 dispatch routes
   schema-2 roots to that runtime (`s2/dispatch.rs:49-101`), so port or add
   typed APT commands there; invoking S1 `apt-*` commands is insufficient.
3. Keep typed APT fields generation-time unless the runtime schema is
   deliberately extended: `ProjectConfig::toml` emits provider arrays but
   intentionally omits release manifest/selector details
   (`s2/mod.rs:1118-1210`). The renderer must inline safe values or pass a
   selection path; it must not smuggle an unvalidated raw YAML command.
4. Render no `velnor` provider, `both`, provider fan-out, or second publisher.
   `workflow_dispatch` `channel` may select stable/preview; version/commit
   inputs, if retained, only assert equality with discovery output.

Runtime boundary: the old S1 entrypoints live at
`crates/velnor-workflow/src/runtime.rs:2961-3164` and are not reached after
schema-2 dispatch. The S2 runtime currently exposes only generic
`verify-feed`/`update-feed` (`crates/velnor-workflow/src/s2/runtime.rs:3052-3065`)
and directory/stub checks (`:3714-3764`); the bootstrap seam must therefore
port the immutable APT stages or add one S2-owned typed command, with no S1
fallback.

### Minimal acceptance fixtures

- Strict config: hosted-only provider arrays parse; all four S1 lane keys,
  absolute/`..` selector paths, bad manifest asset/schema, invalid signer or
  secret, incomplete arch census, and retention errors fail.
- Renderer: exactly one `admit-provider`/verify/publish path; no
  `gh release list`, `latest`, mutable preview download, or second selector;
  discovery runs once, exact asset IDs are downloaded, and hidden sentinel is
  retained.
- Binding: mutate persisted asset ID, URL, name, size, bytes, source ref/
  commit/tag, release ID, or manifest digest after discovery; verification and
  publication fail. Reordering releases or moving a latest pointer after the
  JSON is saved must not change the chosen release.
- Feed state: stable publication retains preview and preview retains stable;
  rollback pair and `.reprepro-ok` survive staging; verify failure performs no
  mutation.
- Producer gate: canonical manifest is `velnor.product-manifest/v1` with all
  three components and complete APT/Homebrew rows; external digest and
  subordinate parent links/census exist. No old-only record fallback and no
  publication before the released producer pin/G1 recovery.
