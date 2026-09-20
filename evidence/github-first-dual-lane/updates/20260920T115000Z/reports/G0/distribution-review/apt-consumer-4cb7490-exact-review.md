# Exact APT consumer review: 4cb7490

Observed 2026-09-20. Read-only review. No source mutation, installation,
publication, release/API write, or workflow dispatch.

## Exact inputs

- Consumer: `tailrocks/velnor-apt`
- Reviewed commit: `4cb749075920fd2abb0d1bdd4c6c6f8480ed9af2`
- Reviewed parent: `f98d3ceb4e72b58fdccb961e206fd5a9264741f4`
- Worktree: `/tmp/velnor-apt-4cb-review` (detached, clean)
- Native producer comparison: `tailrocks/velnor`, branch
  `codex/github-first-native-product-v3`, checkpoint
  `9908296d28d27e0d5b993d1e48ea7a96bc31db83`

## Decision

**Reject producer/consumer integration approval.** The 4cb consumer-side
census is a material improvement and its synthetic suite passes. It does not
prove the required native 990 product bytes, and exact native 990 still has a
four-target/blocked-target contradiction plus a release-ID representation
contradiction. The release-attestation JSON is structurally checked, not
provider-cryptographically verified by this consumer.

## Verification

```text
rtk bash -n scripts/release-discovery.sh scripts/test-release-discovery.sh  PASS
rtk shellcheck scripts/release-discovery.sh scripts/test-release-discovery.sh PASS
rtk bash scripts/test-release-discovery.sh                              PASS
  release discovery checks passed
```

The checked-in manifest has the requested shape: 3 siblings, 4 declared
targets, 12 `binary` rows, 4 archive rows (2 `archive` Linux + 2
`homebrew-archive` macOS), and 2 `apt-package` rows. Positive stable/preview
selection and hostile cases for leading-zero SemVer/release IDs, provider
identity, attestation fields/assets, binary-name drift, archive census,
unknown kind, sidecar/SHA256SUMS, and source resolution all pass.

## Blocking findings

### P0 — no actual native 990 four-target assembly exists

At immutable native checkpoint 990, both native product workflows matrix only
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, and
`aarch64-apple-darwin` (`.github/workflows/native-product.yml` and
`native-product-preview.yml`, matrix around lines 35-43). They declare
`TARGETS_JSON` with four targets but put `x86_64-apple-darwin` in
`BLOCKED_TARGETS_JSON` (lines 55-56). The native release assembler requires
exactly four target contracts and rejects any non-empty blocked-target list
(`.github/workflows/release.yml:4284-4293`).

Therefore the APT fixture's Intel rows cannot be actual rendered output from
990. Do not silently drop Intel; require the producer's real four-target
artifact handoff or an atomically agreed contract that no longer promises it.

### P0 — the claimed native fixture has no producer-byte provenance

The fixture files were introduced only by 4cb (`git log --follow` has no
producer-side predecessor). Its README points at native checkpoint 990, but
the manifest itself contains synthetic identity values, including
`source_commit: 0123456789abcdef0123456789abcdef01234567` and
`release_id: 12345` (`tests/fixtures/native-product/product-manifest.json:6-10`),
not a source/release identity bound to 990.

The test generator explicitly says provider metadata and payload bytes are
synthetic (`scripts/test-release-discovery.sh:246-249`), writes text payloads
with `printf 'native-a8-fixture...'` (`:79-90`), and builds text archives
(`:93-109`). The fixture sidecar only proves that the checked-in manifest
hashes to itself (`:250-262`); no native producer invocation, rendered output
hash, artifact-origin record, or immutable release asset is consumed. Native
990's tree contains contracts/workflows but no matching product-manifest or
rendered product fixture. This is shape testing, not native-byte provenance.

### P1 — native 990 release-ID representation is still internally inconsistent

APT 4cb correctly requires a manifest JSON **string** with positive canonical
decimal grammar and compares it to the provider API's numeric `id` after
stringification (`scripts/release-discovery.sh:429-448, 586-602`). Its local
synthetic positive fixture binds provider `222` to manifest string `"222"`.

Native 990 also emits the product field with `jq --arg release_id`, hence as a
JSON string (`.github/workflows/release.yml:4481`, generated preview template
`crates/velnor-workflow/src/s2/primitives/release.rs:2215`). But the native
published-release revalidation parses that field with
`.release_id | numbers | tostring` (`.github/workflows/release.yml:4638`,
generated template `release.rs:7312`). A real string manifest therefore
cannot pass that producer check; changing it to a JSON number would violate
the producer Rust/string contract and APT's explicit string check. Resolve
one canonical string/provider-u64 contract in producer, Rust verifier, APT,
Homebrew, and subordinate parent IDs, then rerun actual bytes.

### P1 — structural sidecar validation is not cryptographic attestation

`validate_release_attestation` downloads `release-attestation.json` and checks
its keys, source/release identity, manifest digest, and exact artifact array
(`scripts/release-discovery.sh:429-453`). That is useful structural binding,
but it does not invoke `gh attestation verify` or validate a provider-backed
attestation envelope/signature. The APT workflow's crypto check currently
covers fetched `.deb` subjects with the package-signer workflow
(`.github/workflows/release.yml:124-129`); it does not make the consumer's
release-attestation sidecar or all 18 native product assets cryptographically
verified. Do not report the local JSON fixture as provider proof.

### Source-contract pass — `target_commitish` equality and ref resolution

APT requires both the release object and the sidecar's
`target_commitish` to equal the source commit (`release-discovery.sh:447,
695-697`). Native 990's stable and preview publisher paths also require the
provider value to equal the source commit before assembling the product, then
copy that provider value into the sidecar. The equality is therefore aligned
at this exact revision. APT additionally resolves the immutable tag/ref and
checks ancestry; no target-commit-only acceptance was observed.

### P1 — reserved control-asset names are incomplete

The 18-row validator enforces uniqueness, target census, and a safe filename,
but only reserves `discovery.json` and `product-manifest.json`
(`release-discovery.sh:321-327`). It does not reserve the control assets that
the same candidate later assigns semantic roles to: manifest sidecar,
`release-manifest.json`, `SHA256SUMS`, `release-attestation.json`,
`release-record.json` and sidecar, and `manifest.json` and sidecar. No hostile
fixture exercises such collisions. Add one canonical reserved-name set and
negative fixtures before treating the per-target census as complete.

### P1 — helper remains disconnected from the live APT workflow

The 4cb delta changes only the discovery helper, its synthetic test, and the
fixture. `.github/workflows/release.yml` still uses
`velnor-workflow release apt-fetch`/`apt-verify`; it does not invoke
`scripts/release-discovery.sh`. `.github-gen/velnor-workflow.toml` remains
schema 1 and generated `.github/ci/project.toml` schema 2 without a native
product-manifest discovery/publisher pin. The prior disconnected-helper and
schema/workflow-pin blocker therefore remains.

## Narrow disposition

- Consumer schema/census: **source-fixture pass** for 3 siblings, four declared
  targets, 18 unique rows, and archive-kind partition.
- Provider numeric ID ↔ manifest string: **synthetic source-fixture pass**;
  native 990 producer verifier: **reject** until representation is unified.
- `target_commitish` equality plus immutable source resolution: **source pass**.
- Sidecar/hash and hostile local mutations: **pass as integrity checks**;
  provider cryptographic product attestation: **not proven**.
- Native 990 byte origin, Intel artifact, install/upgrade/channel behavior:
  **not proven**; no publication/install claim.
- Live workflow integration: **reject**, helper remains disconnected.
