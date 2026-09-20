# Independent exact native successor review — `384e2e4e`

Review timestamp: 2026-09-20T04:55:00Z

## Verdict

**REJECT / changes required.** The exact candidate repairs the d33 typed
component handoff and exercises the rendered stable assembly, but it does not
close the cross-repository APT contract, does not publish an immutable preview
product, has no published f750 runtime product, and fails the current Homebrew
consumer's executable-mode admission in the exact rendered-byte cross-check.

No source, host runtime, install, dispatch, release, or publication operation
was performed. This is an independent source review plus read-only GitHub API
inspection. It is not a G2/G3 gate approval.

## Immutable candidate identity

- Repository: `tailrocks/velnor`
- Branch: `codex/github-first-native-product-v3`
- Candidate: `384e2e4e1ed039d12977547ec9ca0f538803ab54`
- Parent: `f7508567b0df92537d399d33722b2dab26b62ec4`
- `refs/remotes/origin/codex/github-first-native-product-v3` and
  `git ls-remote origin refs/heads/codex/github-first-native-product-v3`
  both resolve to `384e2e4e1ed039d12977547ec9ca0f538803ab54`.
- `git diff --check d33cf621..384e2e4e` passed.

## Capability matrix

| Capability / claim | Exact evidence | Verdict |
|---|---|---|
| Producer carries `feature` and `identity` | Source contract has all three typed components and seven component keys (`.github/ci/native-product-contract.json:9-48`). Generated producer materializes Cargo versions, validates exact keys, selects feature flags and identity command, and emits both fields in each component JSONL row (`.github/workflows/native-product.yml:84-147`, repeated for the siblings at `:148-223`). | **PASS** |
| Release assembly preserves typed fields | Fresh assembly compares component JSONL with `jq -s` against the source-derived contract (`.github/workflows/release.yml:4422-4440`), groups rows while requiring crate/version/binary/feature/identity equality (`:4468-4471`), and serializes both fields into canonical components and archive components (`:4460-4461`, `:4473`). | **PASS** |
| Runner enforces typed fields | `ApplicationComponent`, `ProductComponentContract`, and `ArchiveComponent` all contain `feature` and `identity` (`crates/velnor-runner/src/product.rs:65-112`, `:708-720`). `verify_typed_profile` compares crate, feature, identity, binary, version, and target set (`:536-588`); archive verification compares the same values (`:862-940`). | **PASS** |
| Fresh verifier receives source contract | Generated publish step passes exactly one `--component-contract product-component-contract.json`, and derives target/component arguments from that file (`.github/workflows/release.yml:4509-4526`). The render test now asserts this exact argument and its source (`crates/velnor-workflow/src/s2/primitives/release.rs:6643-6665`). | **PASS** |
| JSONL checks are substantive | `jq -s` compares complete component rows and artifact rows, including exact key sets, names, target, kind, digest, and positive size (`.github/workflows/release.yml:4426-4440`). | **PASS** |
| Archive checksum basename and macOS tar metadata | Assembly hashes from inside `product-assets`, emits basename-only `product-manifest.json` sidecar (`.github/workflows/release.yml:4473-4476`), and uses `COPYFILE_DISABLE=1 tar` (`:4462-4465`). The serializer fixture executes extracted rendered shell and checks the exact sidecar (`crates/velnor-workflow/src/s2/primitives/release.rs:6698-6743`). | **PASS** for checksum; **partial** for archive handoff |
| Rendered assembly → runner → archive fixture | The test extracts and executes the actual rendered canonical assembly, then calls `ApplicationManifest::verify_bytes`, `NativeProductContract::from_bytes`, `verify_typed_profile`, and `verify_artifacts_with_contract`; it lists the Homebrew archive and checks its eight component keys (`crates/velnor-workflow/src/s2/primitives/release.rs:6747-7060`). | **PASS as source fixture only** |
| Homebrew consumer accepts exact fixture bytes | Independent exact cross-check ran the current Homebrew `scripts/package-update.sh` against the produced `product-assets/*` bytes and failed: `archive member is not executable: velnorctl`; all three archive binaries were `0644`. Evidence: [`native-384-exact-crosscheck.md`](../homebrew-contract/native-384-exact-crosscheck.md), SHA-256 `ff6f37e380efc7eeb5721b654436d533ab5e854f179a824c01456a4a93f146a0`. | **FAIL / blocking** |
| Test-only lint allowances | f750 adds `#[allow(...)]` only under `#[cfg(test)]` for runner fixtures (`crates/velnor-runner/src/product.rs:1107-1114`) and workflow fixtures (`crates/velnor-workflow/src/s2/primitives/release.rs:5279-5292`). No new broad production suppression was added by f750. | **PASS for this claim**; does not prove a full clippy run |
| APT parent binding / acyclic handoff | Current dual-lane-apt snapshot `f679a1ce94627281a99e4b887fe26fc1cad33409` requires `.parent_manifest_sha256` in `release-record.json`, `release-manifest.json`, and compiled `manifest.json` (`dual-lane-apt/scripts/release-discovery.sh:379-400`, `:419-430`). Candidate `ReleaseRecord` has no such field (`crates/velnor-runner/src/release.rs:341-351`); its record and consumer-manifest builders emit no parent digest (`crates/velnor-workflow/src/s2/primitives/release.rs:2293-2307`). The candidate's own `manifest_sha256` sidecar is acyclic, but it is not the APT-required parent binding. | **FAIL / blocking** |
| Immutable preview product | Preview downloads and validates native build handoff, but publish assembles only `SHA256SUMS`, `release-manifest.json`, and two preview Debian packages (`.github/workflows/preview.yml:926-1072`); it creates/replaces the rolling `preview` release with those assets only (`:1073-1135`). No preview product manifest, sidecar, native archives, product attestation, or `verify-product` handoff exists. The native preview reusable workflow also contains an unconditional blocked Intel job (`.github/workflows/native-product-preview.yml:17-27`), while preview publish requires that build and rejects nonempty `blocked_targets` (`.github/workflows/preview.yml:926-990`). | **FAIL / blocking** |
| f750 runtime publication | The checked-in setup action resolves f750 to a full closure and fetches immutable tag `velnor-workflow-runtime-v1-<closure16>`; it fails closed when the product is absent (`.github-gen/sources/actions/setup-velnor-workflow/action.yml:81-121`). Reproduced its exact closure algorithm for f750: `16c1a1702764a90c6fb9f8cb19c18984c72d4cb0fe2db074f0be2e15af74062b`, expected tag `velnor-workflow-runtime-v1-16c1a1702764a90c`. Read-only `GET /repos/tailrocks/velnor/releases/tags/velnor-workflow-runtime-v1-16c1a1702764a90c` returned HTTP 404 at review time. f750 itself is a remote commit, but a source pin is not a published runtime product. | **FAIL / integration blocker** |

## Homebrew mode finding

The candidate's fixture writes synthetic binary bytes with `fs::write`
(`crates/velnor-workflow/src/s2/primitives/release.rs:6893-6900`) and never sets
executable permissions before the rendered tar command. The fixture therefore
checks member names and JSON fields but not the consumer's executable-mode
contract. The independent cross-check executed the real consumer parser against
those exact rendered bytes and rejected them. The production assembly only
checks `-f`, not `-x`, before copying the downloaded sibling
(`.github/workflows/release.yml:4441-4447`). Future fix must preserve/validate
0755 for every archive binary on the downloaded-artifact path and make the
fixture exercise that path; do not chmod captured bytes after the fact and call
the producer handoff valid.

## Exact bounded follow-up

1. Fix executable-mode preservation at the producer/download boundary; add a
   rendered fixture that sets source binary modes as production does, asserts
   them before tar, and runs the actual Homebrew parser. Add a negative mode
   case. Do not weaken the consumer requirement.
2. Define one acyclic external product-digest binding accepted by Velnor and
   dual-lane-apt. Update `ReleaseRecord`, `release-manifest`, compiled
   `manifest`, producer verification, and APT acceptance together; a local
   sidecar digest is not enough. Pin the APT work to
   `f679a1ce94627281a99e4b887fe26fc1cad33409` or a newer exact consumer ref.
3. Add preview product publication or explicitly remove preview product scope
   from the contract. If kept, publish/verify preview product manifest,
   basename sidecar, per-target archives, attestation, and Homebrew/APT
   channel metadata. Resolve Intel capability honestly; the current blocked
   target cannot be silently treated as delivered.
4. Publish and attest the f750 runtime product at the exact closure/tag above,
   then rerun the consumer workflow from a cold cache. No build/test result can
   substitute for that immutable release asset.
5. Add a negative runner test mutating `feature` (the existing typed test
   mutates crate/version/identity but not feature: `product.rs:1230-1254`).

No approval, publication, runtime-operability, or gate success is claimed.
