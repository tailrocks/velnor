//! Stage-0 runtime-product producer: the owner-only publisher of immutable
//! `velnor-workflow` binaries.
//!
//! Consumers never compile: the setup action and the Velnor policy provisioner
//! download `velnor-workflow-<RUNNER_OS>-<RUNNER_ARCH>` plus `manifest.json`
//! from the immutable release the closure names, then prove attestation,
//! manifest, digest, and the binary's own `--closure` report. This module
//! renders the workflow that publishes those releases. The consumer contract
//! is the specification: the tag scheme, the manifest shape, the attestation
//! subject, and the acceptance filter below mirror the setup action byte for
//! byte, and the conformance tests pin that agreement against the action
//! source itself.
//!
//! The workflow is owner-only infrastructure. It renders only for the
//! repository that ships the setup action (derived from the action's own
//! coordinate, never spelled out); every other repository gets no file, and a
//! declared row on a non-owner fails closed.
//!
//! Layout: a `closure` job first proves the run targets the default branch
//! (a dispatch from anywhere else fails instead of publishing), then resolves
//! the source closure of `HEAD` and checks whether its tag already exists
//! (unchanged closure, no rebuild); a `build` matrix compiles natively on one
//! runner per consumer platform, proves each binary reports the tag closure,
//! attests it, and uploads it; a `publish` job proves transport integrity,
//! assembles the manifest, attests it, smoke-tests the exact consumer flow
//! against those same bytes, and only then creates the release without ever
//! overwriting — no consumer can see a product whose verification failed.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Args, Primitive, RenderCtx, Rendered};
use crate::s2::closure::{
    product_tag, CI_FEATURES, CLOSURE_PATHS, CLOSURE_VERSION, PRODUCT_TAG_PREFIX, PROFILE_RELEASE,
};
use crate::s2::{
    config_rust_toolchain, workflow_setup_action_repository, yaml_scalar, ActionPin,
    MACOS_HOSTED_RUNS_ON,
    GeneratorError, ProjectConfig, RustToolchain, GENERATED_HEADER, HOSTED_WORKFLOW_RUNTIME_HOME,
};

/// The workflow file the producer renders into. Consumers pin this path in
/// their attestation check, so the name is load-bearing: a rename breaks
/// every verifier until the setup action ships the new pin with it.
pub(crate) const RUNTIME_PRODUCTS_FILE: &str = "ci-runtime-products.yml";

/// Canonical sidecar added to ordinary source releases. The consumer
/// `manifest.json` remains byte-compatible with the setup action.
pub(crate) const RUNTIME_PRODUCTS_RELEASE_MANIFEST_FILE: &str =
    "runtime-products-release.json";
/// Schema for the source-release wrapper that binds runtime assets to the
/// exact release source and consumer manifest.
pub(crate) const RUNTIME_PRODUCTS_RELEASE_MANIFEST_SCHEMA: &str =
    "velnor.runtime-products-release/v1";

/// One native runtime build target shared by the producer and source-release
/// renderers. `asset` is the stable raw-binary release asset name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeProductPlatform {
    pub(crate) os: &'static str,
    pub(crate) arch: &'static str,
    pub(crate) target: &'static str,
    pub(crate) runner: &'static str,
    pub(crate) asset: &'static str,
}

impl RuntimeProductPlatform {
    /// The platform key written in the consumer manifest.
    pub(crate) fn key(self) -> String {
        format!("{}-{}", self.os, self.arch)
    }

    /// The release asset name, returned as owned text for template assembly.
    pub(crate) fn asset_name(self) -> String {
        self.asset.to_owned()
    }
}

/// The source-release product row, with platform identity explicit so
/// publisher verification cannot confuse two binaries with the same digest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeProductsReleaseProduct {
    pub(crate) platform: String,
    pub(crate) asset: String,
    pub(crate) sha256: String,
}

/// Canonical source-release wrapper around the unchanged consumer manifest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeProductsReleaseManifest {
    pub(crate) schema: String,
    pub(crate) repository: String,
    pub(crate) source_ref: String,
    pub(crate) source_sha: String,
    pub(crate) release_tag: String,
    pub(crate) version: String,
    pub(crate) closure: String,
    pub(crate) manifest_sha256: String,
    pub(crate) products: Vec<RuntimeProductsReleaseProduct>,
}

/// Expected identity supplied by the release workflow. The wrapper is never
/// allowed to choose the repository, tag, source revision or closure it
/// claims to verify.
pub(crate) struct RuntimeProductsReleaseExpectation<'a> {
    pub(crate) repository: &'a str,
    pub(crate) source_ref: &'a str,
    pub(crate) source_sha: &'a str,
    pub(crate) release_tag: &'a str,
    pub(crate) version: &'a str,
    pub(crate) closure: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsumerProductManifest {
    closure: String,
    revision: String,
    profile: String,
    features: String,
    products: BTreeMap<String, ConsumerProduct>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsumerProduct {
    binary: String,
    asset: String,
}

/// Native targets and asset names shared with generated source-release jobs.
pub(crate) fn runtime_product_platforms() -> [RuntimeProductPlatform; 3] {
    platforms()
}

/// Build canonical source-release metadata from the exact consumer manifest
/// and asset bytes. Asset bytes are keyed by consumer platform name.
pub(crate) fn build_runtime_products_release_manifest(
    consumer_manifest: &[u8],
    assets: &BTreeMap<String, Vec<u8>>,
    expected: &RuntimeProductsReleaseExpectation<'_>,
) -> Result<Vec<u8>, GeneratorError> {
    validate_release_expectation(expected)?;
    let consumer = parse_consumer_manifest(consumer_manifest)?;
    validate_consumer_identity(&consumer, expected)?;
    let platforms = runtime_product_platforms();
    if consumer.products.len() != platforms.len() || assets.len() != platforms.len() {
        return Err(GeneratorError::usage(
            "runtime product manifest must contain exactly the supported platforms",
        ));
    }
    let mut products = Vec::with_capacity(platforms.len());
    for platform in platforms {
        let key = platform.key();
        let consumer_product = consumer.products.get(&key).ok_or_else(|| {
            GeneratorError::usage(format!("runtime consumer manifest omits {key}"))
        })?;
        if consumer_product.asset != platform.asset {
            return Err(GeneratorError::usage(format!(
                "runtime consumer manifest names the wrong asset for {key}"
            )));
        }
        let bytes = assets.get(&key).ok_or_else(|| {
            GeneratorError::usage(format!("runtime source release omits the {key} asset"))
        })?;
        let digest = sha256_bytes(bytes);
        if !valid_sha256(&consumer_product.binary) || consumer_product.binary != digest {
            return Err(GeneratorError::usage(format!(
                "runtime consumer manifest digest does not match the {key} asset"
            )));
        }
        products.push(RuntimeProductsReleaseProduct {
            platform: key,
            asset: platform.asset.to_owned(),
            sha256: digest,
        });
    }
    let manifest = RuntimeProductsReleaseManifest {
        schema: RUNTIME_PRODUCTS_RELEASE_MANIFEST_SCHEMA.to_owned(),
        repository: expected.repository.to_owned(),
        source_ref: expected.source_ref.to_owned(),
        source_sha: expected.source_sha.to_owned(),
        release_tag: expected.release_tag.to_owned(),
        version: expected.version.to_owned(),
        closure: expected.closure.to_owned(),
        manifest_sha256: sha256_bytes(consumer_manifest),
        products,
    };
    let bytes = serde_json::to_vec(&manifest)
        .map_err(|error| GeneratorError::usage(format!("serialize runtime release manifest: {error}")))?;
    validate_runtime_products_release_manifest(&bytes, consumer_manifest, assets, expected)?;
    Ok(bytes)
}

/// Parse canonical source-release metadata and verify every raw asset byte
/// against both the wrapper and the consumer manifest.
pub(crate) fn validate_runtime_products_release_manifest(
    release_manifest: &[u8],
    consumer_manifest: &[u8],
    assets: &BTreeMap<String, Vec<u8>>,
    expected: &RuntimeProductsReleaseExpectation<'_>,
) -> Result<RuntimeProductsReleaseManifest, GeneratorError> {
    validate_release_expectation(expected)?;
    let manifest: RuntimeProductsReleaseManifest = serde_json::from_slice(release_manifest)
        .map_err(|error| GeneratorError::usage(format!("parse runtime release manifest: {error}")))?;
    let canonical = serde_json::to_vec(&manifest)
        .map_err(|error| GeneratorError::usage(format!("serialize runtime release manifest: {error}")))?;
    if canonical != release_manifest {
        return Err(GeneratorError::usage(
            "runtime release manifest is not canonical JSON",
        ));
    }
    let consumer = parse_consumer_manifest(consumer_manifest)?;
    validate_consumer_identity(&consumer, expected)?;
    if manifest.schema != RUNTIME_PRODUCTS_RELEASE_MANIFEST_SCHEMA
        || manifest.repository != expected.repository
        || manifest.source_ref != expected.source_ref
        || manifest.source_sha != expected.source_sha
        || manifest.release_tag != expected.release_tag
        || manifest.version != expected.version
        || manifest.closure != expected.closure
        || manifest.manifest_sha256 != sha256_bytes(consumer_manifest)
    {
        return Err(GeneratorError::usage(
            "runtime release manifest identity does not match the admitted source",
        ));
    }
    let platforms = runtime_product_platforms();
    if manifest.products.len() != platforms.len()
        || consumer.products.len() != platforms.len()
        || assets.len() != platforms.len()
    {
        return Err(GeneratorError::usage(
            "runtime release manifest must contain exactly the supported platforms",
        ));
    }
    for (entry, platform) in manifest.products.iter().zip(platforms) {
        let key = platform.key();
        let consumer_product = consumer.products.get(&key).ok_or_else(|| {
            GeneratorError::usage(format!("runtime consumer manifest omits {key}"))
        })?;
        let bytes = assets.get(&key).ok_or_else(|| {
            GeneratorError::usage(format!("runtime source release omits the {key} asset"))
        })?;
        let digest = sha256_bytes(bytes);
        if entry.platform != key
            || entry.asset != platform.asset
            || consumer_product.asset != platform.asset
            || !valid_sha256(&entry.sha256)
            || entry.sha256 != digest
            || consumer_product.binary != digest
        {
            return Err(GeneratorError::usage(format!(
                "runtime release asset identity or digest mismatch for {key}"
            )));
        }
    }
    Ok(manifest)
}

fn validate_release_expectation(
    expected: &RuntimeProductsReleaseExpectation<'_>,
) -> Result<(), GeneratorError> {
    if !crate::s2::apt::valid_repository_slug(expected.repository)
        || !crate::s2::apt::valid_commit(expected.source_sha)
        || !crate::s2::closure::is_full_closure(expected.closure)
        || !crate::s2::runtime::is_canonical_semver(expected.version)
        || expected.release_tag != format!("v{}", expected.version)
        || !expected
            .source_ref
            .strip_prefix("refs/heads/")
            .is_some_and(crate::s2::runtime::valid_branch)
    {
        return Err(GeneratorError::usage(
            "runtime release expectation must bind a canonical v* tag and full source SHA to a default-branch ref and source closure",
        ));
    }
    Ok(())
}

fn validate_consumer_identity(
    consumer: &ConsumerProductManifest,
    expected: &RuntimeProductsReleaseExpectation<'_>,
) -> Result<(), GeneratorError> {
    if consumer.closure != expected.closure
        || consumer.revision != expected.source_sha
        || consumer.profile != PROFILE_RELEASE
        || consumer.features != CI_FEATURES
    {
        return Err(GeneratorError::usage(
            "runtime consumer manifest does not match the admitted release source",
        ));
    }
    Ok(())
}

fn parse_consumer_manifest(bytes: &[u8]) -> Result<ConsumerProductManifest, GeneratorError> {
    if bytes.len() > 1_048_576 {
        return Err(GeneratorError::usage(
            "runtime consumer manifest exceeds 1 MiB",
        ));
    }
    serde_json::from_slice(bytes)
        .map_err(|error| GeneratorError::usage(format!("parse runtime consumer manifest: {error}")))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// The producer side-file family and the canonical file it renders.
pub(crate) const RUNTIME_PRODUCTS_SIDE_FILES: &[(&str, &str)] =
    &[(RUNTIME_PRODUCTS_FILE, super::RUNTIME_PRODUCTS)];

/// Whether `primitive` renders the runtime-product producer workflow.
pub(crate) fn is_runtime_products_side(primitive: &str) -> bool {
    RUNTIME_PRODUCTS_SIDE_FILES
        .iter()
        .any(|(_, family)| *family == primitive)
}

/// The canonical filename the runtime-product primitive renders.
pub(crate) fn canonical_runtime_products_side_file(primitive: &str) -> Option<&'static str> {
    RUNTIME_PRODUCTS_SIDE_FILES
        .iter()
        .find(|(_, family)| *family == primitive)
        .map(|(file, _)| *file)
}

/// The platform→builder mapping: which runner natively compiles each
/// consumer platform's product. These labels are product infrastructure fixed
/// in generator source — a closure-covered path — not lane configuration: a
/// mapping change is a generator source change, so it alters the closure and
/// mints a new product tag instead of reusing the stale tag's binaries. Owner
/// selector labels move freely without affecting the builders, and can never
/// silently reselect them.
const LINUX_X64_RUNNER: &str = "ubuntu-24.04";
const LINUX_ARM64_RUNNER: &str = "ubuntu-24.04-arm";

/// The manifest acceptance filter, exactly as the setup action evaluates it:
/// full closure, a well-formed source revision, release profile, empty
/// features, a 64-hex digest for the platform, and the asset name the
/// platform expects. The publish job evaluates this same filter over the
/// assembled manifest, so a manifest no consumer would accept never reaches
/// a release.
///
/// The revision clause is well-formedness, not equality with the requested
/// revision: several commits can share one closure (and therefore one
/// product), so the manifest names the commit the producer built from while
/// the consumer requested another. Both consumers bind the binary to the
/// manifest instead, requiring its `--revision` report to equal the
/// manifest's `revision`.
const MANIFEST_ACCEPT_FILTER: &str = ".closure == $closure and (.revision | test(\"^[0-9a-f]{40}$\")) and .profile == \"release\" and .features == \"\" and (.products[$platform].binary | test(\"^[0-9a-f]{64}$\")) and .products[$platform].asset == $asset";

/// The isolated Cargo home the producer steps build under, as a rendered
/// step-level `env:` value. The `runner` context is unavailable in job-level
/// `env:` (GitHub rejects the workflow at compile time), so each step that
/// needs isolation carries this as step-level `env:`, which does provide
/// `runner`.
const PRODUCER_CARGO_HOME_VALUE: &str = "${{ runner.temp }}/velnor-producer-cargo-home";

/// One natively built consumer platform: the `RUNNER_OS`-`RUNNER_ARCH` pair
/// the setup action resolves its asset name from, and the fixed runner that
/// builds it. Linux X64 serves Linux consumers, Linux ARM64 serves ARM
/// consumers, and macOS ARM64 serves Apple consumers (no repository selects
/// an Intel Mac, so there is no macOS X64 consumer to build for).
/// The consumer platforms, in manifest order. All three builders are the
/// fixed product-infrastructure mapping, independent of selector configuration.
fn platforms() -> [RuntimeProductPlatform; 3] {
    [
        RuntimeProductPlatform {
            os: "Linux",
            arch: "X64",
            target: "x86_64-unknown-linux-gnu",
            runner: LINUX_X64_RUNNER,
            asset: "velnor-workflow-Linux-X64",
        },
        RuntimeProductPlatform {
            os: "Linux",
            arch: "ARM64",
            target: "aarch64-unknown-linux-gnu",
            runner: LINUX_ARM64_RUNNER,
            asset: "velnor-workflow-Linux-ARM64",
        },
        RuntimeProductPlatform {
            os: "macOS",
            arch: "ARM64",
            target: "aarch64-apple-darwin",
            runner: MACOS_HOSTED_RUNS_ON,
            asset: "velnor-workflow-macOS-ARM64",
        },
    ]
}

/// The product owner: the organization of the repository that ships the setup
/// action. Derived from the action's own coordinate so owner routing cannot
/// drift from it.
fn product_owner(repository: &str) -> &str {
    repository
        .split_once('/')
        .map_or(repository, |(owner, _)| owner)
}

/// The tag the zero closure names: the worked example of the tag scheme the
/// workflow header carries. The renderer knows no closure — the workflow
/// resolves it per run — so the example pins the scheme visibly to
/// [`product_tag`]: if the prefix ever changes, the header follows it.
fn example_tag() -> String {
    product_tag(&"0".repeat(64))
}

/// The `ci-runtime-products.yml` content for a config, or `None` for every
/// repository that is not the setup-action owner. The owner check compares
/// against the action coordinate, so fixture repositories (empty or foreign)
/// render no file and their goldens never see this surface.
#[expect(
    clippy::too_many_lines,
    reason = "one workflow family renders its three jobs in one template"
)]
pub(crate) fn runtime_products_content(
    config: &ProjectConfig,
) -> Result<Option<String>, GeneratorError> {
    if config.repository != workflow_setup_action_repository() {
        return Ok(None);
    }
    if config.default_branch != "main" {
        return Err(GeneratorError::usage(
            "the setup action pins runtime attestations to refs/heads/main; the runtime-product publisher requires default_branch = \"main\"",
        ));
    }
    let hosted_runs_on = crate::s2::hosted_runs_on(config)?;
    let repository = workflow_setup_action_repository();
    let owner = product_owner(repository);
    // The toolchain install is file-driven: the checkout's own
    // `rust-toolchain.toml` — itself a closure input — pins the channel, so a
    // config without a scanned Rust unit still provisions exactly the pinned
    // toolchain. The scan guarantees the pin on real repositories; the
    // fallback only serves hand-built configs.
    let toolchain = config_rust_toolchain(config).unwrap_or(RustToolchain {
        channel: String::new(),
        components: Vec::new(),
        targets: Vec::new(),
        profile: None,
    });
    let mut toolchain_steps = String::new();
    super::render_pinned_toolchain_steps(
        &mut toolchain_steps,
        ActionPin::CacheRestore.reference(),
        ActionPin::CacheSave.reference(),
        &toolchain,
        Some(&format!(
            "{} && steps.rustup-toolchain.outputs.cache-hit != 'true'",
            super::default_branch_push_cache_save_expression(&config.default_branch)
        )),
    );
    // Step-level isolation for the provision step only: restore/save touch
    // `~/.rustup` alone, while `rustup toolchain install` must not read an
    // ambient cargo config. The shared renderer stays untouched so no other
    // family gains this env.
    toolchain_steps = toolchain_steps.replace(
        "      - name: Provision Rust toolchain\n        shell: bash\n",
        &format!(
            "      - name: Provision Rust toolchain\n        shell: bash\n        env:\n          CARGO_HOME: {PRODUCER_CARGO_HOME_VALUE}\n"
        ),
    );
    let platforms = platforms();
    let mut matrix = String::new();
    for platform in &platforms {
        let _ = writeln!(
            matrix,
            "          - os: {}\n            arch: {}\n            runner: {}",
            platform.os,
            platform.arch,
            yaml_scalar(platform.runner),
        );
    }
    let mut manifest_products = String::new();
    for (index, platform) in platforms.iter().enumerate() {
        if index > 0 {
            manifest_products.push_str(", ");
        }
        let _ = write!(
            manifest_products,
            "\"{key}\": {{binary: ${var}, asset: \"{asset}\"}}",
            key = platform.key(),
            asset = platform.asset_name(),
            var = platform_variable(platform),
        );
    }
    let manifest_program = format!(
        "{{closure: $closure, revision: $revision, profile: \"release\", features: \"\", products: {{{manifest_products}}}}}"
    );
    let mut manifest_digests = String::new();
    for platform in &platforms {
        let _ = writeln!(
            manifest_digests,
            "            --arg {var} \"$(cat dist/{asset}.sha256)\" \\",
            var = platform_variable(platform),
            asset = platform.asset_name(),
        );
    }
    let platform_list = platforms
        .iter()
        .map(|platform| platform.key())
        .collect::<Vec<_>>()
        .join(" ");
    let release_assets = platforms
        .iter()
        .map(|platform| format!("dist/{}", platform.asset_name()))
        .collect::<Vec<_>>()
        .join(" ");
    let closure_footer = format!(
        "closure-version:{CLOSURE_VERSION}\\nfeatures:{CI_FEATURES}\\nprofile:{PROFILE_RELEASE}\\n"
    );
    Ok(Some(format!(
        r#"{header}# Stage-0 runtime products: the immutable `velnor-workflow` binaries every
# consumer lane installs through the setup action instead of compiling.
#
# One release per source closure: tags name the closure's product tag (worked
# example for the zero closure: `{example_tag}`), so an unrelated monorepo
# change never rebuilds the runtime. Each platform job builds natively with
# `cargo build --locked --no-default-features --release`, proves the binary's
# own `--closure` report equals the tag closure and its `--revision` report
# equals the build commit, attests the asset, and uploads it; the publish
# job proves transport integrity, assembles `manifest.json` (naming the
# source revision it built from), attests it, smoke-tests the exact consumer
# flow (attestation, manifest, digest, self-report) against those same bytes,
# and only then creates the release. Verification precedes exposure: a
# product no consumer would accept never reaches a release.
#
# The workflow never overwrites: when the tag already exists the run skips,
# and the publish job re-checks immediately before creating the release.
name: Velnor workflow runtime products
run-name: Runtime products · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}

on:
  push:
    branches: [{default_branch}]

permissions:
  contents: read

jobs:
  closure:
    name: Resolve runtime closure
    runs-on: {closure_runner}
    timeout-minutes: 10
    permissions:
      contents: read
      attestations: read
    outputs:
      closure: ${{{{ steps.closure.outputs.value }}}}
      tag: ${{{{ steps.closure.outputs.tag }}}}
      head-sha: ${{{{ steps.closure.outputs.head-sha }}}}
      exists: ${{{{ steps.exists.outputs.exists }}}}
    steps:
      - name: Prove the default-branch ref
        shell: bash
        env:
          REF: ${{{{ github.ref }}}}
        run: |
          set -euo pipefail
          [[ "$REF" == "{branch_ref}" ]] || {{ echo "::error::the producer publishes only from {branch_ref}, got $REF" >&2; exit 1; }}
      - name: Checkout
        uses: {checkout}
        with:
          persist-credentials: false
      - name: Resolve source closure
        id: closure
        shell: bash
        run: |
          set -euo pipefail
          command -v sha256sum >/dev/null 2>&1 || {{ echo "::error::sha256sum is required on the closure runner" >&2; exit 1; }}
          head="$(git rev-parse HEAD)"
          [[ "$head" =~ ^[0-9a-f]{{40}}$ ]] || {{ echo "::error::HEAD is not a commit SHA: $head" >&2; exit 1; }}
          listing="$(git ls-tree -r HEAD -- {closure_paths})"
          test "$listing" != '' || {{ echo "::error::HEAD has no closure inputs" >&2; exit 1; }}
          closure="$(printf '%s\n{closure_footer}' "$(LC_ALL=C sort <<<"$listing")" | sha256sum | awk '{{print $1}}')"
          [[ "$closure" =~ ^[0-9a-f]{{64}}$ ]] || {{ echo "::error::closure resolution failed" >&2; exit 1; }}
          tag="{tag_prefix}${{closure:0:16}}"
          {{
            echo "value=$closure"
            echo "tag=$tag"
            echo "head-sha=$head"
          }} >> "$GITHUB_OUTPUT"
      - name: Check for an existing product
        id: exists
        shell: bash
        env:
          GH_TOKEN: ${{{{ github.token }}}}
          TAG: ${{{{ steps.closure.outputs.tag }}}}
          CLOSURE: ${{{{ steps.closure.outputs.closure }}}}
          REPOSITORY: {repository}
        run: |
          set -euo pipefail
          query_error="$(mktemp)"
          trap 'rm -f "$query_error"' EXIT
          if gh api "repos/$REPOSITORY/releases/tags/$TAG" >/dev/null 2>"$query_error"; then
            exists=true
          elif grep -Eq '(^|[[:space:]])HTTP 404([:[:space:]]|$)|\(HTTP 404\)' "$query_error"; then
            exists=false
          else
            cat "$query_error" >&2
            echo "::error::failed to query release $TAG" >&2
            exit 1
          fi
          if [[ "$exists" != true ]]; then
            echo "exists=false" >> "$GITHUB_OUTPUT"
            exit 0
          fi
          temporary="$(mktemp -d)"
          trap 'rm -rf "$temporary" "$query_error"' EXIT
          patterns=(--pattern manifest.json)
          for platform in {platform_list}; do patterns+=(--pattern "velnor-workflow-$platform"); done
          gh release download "$TAG" --repo "$REPOSITORY" --dir "$temporary" "${{patterns[@]}}"
          manifest_revision="$(jq -er '.revision' "$temporary/manifest.json")"
          [[ "$manifest_revision" =~ ^[0-9a-f]{{40}}$ ]] || {{ echo "::error::existing product manifest revision is invalid" >&2; exit 1; }}
          gh attestation verify "$temporary/manifest.json" --owner {owner} --signer-workflow {repository}/.github/workflows/{workflow_file} --source-ref {branch_ref} --source-digest "$manifest_revision"
          for platform in {platform_list}; do
            asset="velnor-workflow-$platform"
            jq -e --arg closure "$CLOSURE" --arg platform "$platform" --arg asset "$asset" '{accept_filter}' "$temporary/manifest.json" >/dev/null
            expected="$(jq -er --arg platform "$platform" '.products[$platform].binary' "$temporary/manifest.json")"
            actual="$(sha256sum "$temporary/$asset" | awk '{{print $1}}')"
            [[ "$actual" == "$expected" ]] || {{ echo "::error::existing product digest mismatch for $asset" >&2; exit 1; }}
            gh attestation verify "$temporary/$asset" --owner {owner} --signer-workflow {repository}/.github/workflows/{workflow_file} --source-ref {branch_ref} --source-digest "$manifest_revision"
          done
          linux_asset="$temporary/velnor-workflow-Linux-X64"
          chmod 0755 "$linux_asset"
          reported_closure="$("$linux_asset" --closure)"
          [[ "$reported_closure" == "$CLOSURE" ]] || {{ echo "::error::existing product reports closure $reported_closure, expected $CLOSURE" >&2; exit 1; }}
          reported_revision="$("$linux_asset" --revision)"
          [[ "$reported_revision" == "$manifest_revision" ]] || {{ echo "::error::existing product reports revision $reported_revision, expected $manifest_revision" >&2; exit 1; }}
          echo "exists=true" >> "$GITHUB_OUTPUT"

  build:
    name: Build runtime (${{{{ matrix.os }}}}-${{{{ matrix.arch }}}})
    needs: closure
    if: needs.closure.outputs.exists != 'true'
    strategy:
      fail-fast: false
      matrix:
        include:
{matrix}    runs-on: ${{{{ matrix.runner }}}}
    timeout-minutes: 60
    permissions:
      contents: read
      id-token: write
      attestations: write
    # The producer builds with an isolated Cargo home: no ambient registry,
    # git checkouts, or cargo config from the runner image can enter the
    # build. The toolchain steps cache `~/.rustup` only, which rustup owns.
    # Each step that needs the isolated home (provision, hermetic proof,
    # build, product proof) carries it as step-level `env:`: the `runner`
    # context is unavailable in job-level `env:`.
    steps:
      - name: Checkout
        uses: {checkout}
        with:
          persist-credentials: false
{toolchain_steps}      - name: Prove a hermetic build environment
        shell: bash
        env:
          CARGO_HOME: {cargo_home}
        run: |
          set -euo pipefail
          test -z "$(git status --porcelain)" || {{ echo "::error::the producer checkout is not clean" >&2; exit 1; }}
          test -z "${{RUSTFLAGS:-}}" || {{ echo "::error::RUSTFLAGS is set: ${{RUSTFLAGS}}" >&2; exit 1; }}
          test -z "${{CARGO_ENCODED_RUSTFLAGS:-}}" || {{ echo "::error::CARGO_ENCODED_RUSTFLAGS is set" >&2; exit 1; }}
          test -z "${{RUSTC_WRAPPER:-}}" || {{ echo "::error::RUSTC_WRAPPER is set: ${{RUSTC_WRAPPER}}" >&2; exit 1; }}
          [[ "${{CARGO_HOME:-}}" == "${{RUNNER_TEMP:?}}/"* ]] || {{ echo "::error::CARGO_HOME is not the isolated producer home: ${{CARGO_HOME:-<unset>}}" >&2; exit 1; }}
      - name: Build release runtime
        shell: bash
        env:
          CARGO_HOME: {cargo_home}
        run: cargo build --locked --no-default-features --release --package velnor-workflow --bin velnor-workflow
      - name: Prove the product closure
        id: prove
        shell: bash
        env:
          CLOSURE: ${{{{ needs.closure.outputs.closure }}}}
          CARGO_HOME: {cargo_home}
        run: |
          set -euo pipefail
          binary=target/release/velnor-workflow
          test -x "$binary" || {{ echo "::error::no release binary at $binary" >&2; exit 1; }}
          reported="$("$binary" --closure)"
          [[ "$reported" == "$CLOSURE" ]] || {{ echo "::error::built binary reports closure $reported, expected $CLOSURE" >&2; exit 1; }}
          head="$(git rev-parse HEAD)"
          reported_revision="$("$binary" --revision)"
          [[ "$reported_revision" == "$head" ]] || {{ echo "::error::built binary reports revision $reported_revision, expected $head" >&2; exit 1; }}
          asset="velnor-workflow-${{RUNNER_OS}}-${{RUNNER_ARCH}}"
          cp "$binary" "$asset"
          if command -v sha256sum >/dev/null 2>&1; then
            digest="$(sha256sum "$asset" | awk '{{print $1}}')"
          else
            digest="$(shasum -a 256 "$asset" | awk '{{print $1}}')"
          fi
          [[ "$digest" =~ ^[0-9a-f]{{64}}$ ]] || {{ echo "::error::digest computation failed" >&2; exit 1; }}
          printf '%s' "$digest" > "$asset.sha256"
          echo "asset=$asset" >> "$GITHUB_OUTPUT"
      - name: Attest runtime asset
        uses: {attest}
        with:
          subject-path: ${{{{ steps.prove.outputs.asset }}}}
      - name: Upload runtime asset
        uses: {upload}
        with:
          name: runtime-${{{{ matrix.os }}}}-${{{{ matrix.arch }}}}
          path: |
            ${{{{ steps.prove.outputs.asset }}}}
            ${{{{ steps.prove.outputs.asset }}}}.sha256
          if-no-files-found: error
          retention-days: 7

  freshness:
    name: Check source freshness
    needs: [closure, build]
    if: needs.closure.outputs.exists != 'true'
    runs-on: {closure_runner}
    timeout-minutes: 5
    permissions:
      contents: read
    outputs:
      fresh: ${{{{ steps.freshness.outputs.fresh }}}}
    steps:
      - name: Check the default branch head
        id: freshness
        shell: bash
        env:
          GH_TOKEN: ${{{{ github.token }}}}
          REPOSITORY: {repository}
          DEFAULT_BRANCH: {default_branch}
          CLOSURE: ${{{{ needs.closure.outputs.closure }}}}
        run: |
          set -euo pipefail
          current="$(gh api "repos/$REPOSITORY/commits/$DEFAULT_BRANCH" --jq '.sha')"
          [[ "$current" =~ ^[0-9a-f]{{40}}$ ]] || {{ echo "::error::the default branch returned an invalid commit SHA: $current" >&2; exit 1; }}
          tree="$(gh api "repos/$REPOSITORY/git/trees/$current?recursive=1")" || {{ echo "::error::could not resolve the current default-branch tree" >&2; exit 1; }}
          [[ "$(jq -r '.truncated // false' <<<"$tree")" != "true" ]] || {{ echo "::error::the current default-branch tree is truncated" >&2; exit 1; }}
          listing="$(jq -er '[.tree[] | select(.type != "tree") | select(.path == "Cargo.toml" or .path == "Cargo.lock" or .path == "rust-toolchain.toml" or .path == "rust-toolchain" or (.path | startswith("crates/velnor-workflow/")) or (.path | startswith(".cargo/"))) | "\(.mode) \(.type) \(.sha)\t\(.path)"] | sort | join("\n")' <<<"$tree")"
          test -n "$listing" || {{ echo "::error::the current default branch has no closure inputs" >&2; exit 1; }}
          current_closure="$(printf '%s\n{closure_footer}' "$(LC_ALL=C sort <<<"$listing")" | sha256sum | awk '{{print $1}}')"
          [[ "$current_closure" =~ ^[0-9a-f]{{64}}$ ]] || {{ echo "::error::current default-branch closure resolution failed" >&2; exit 1; }}
          if [[ "$current_closure" == "$CLOSURE" ]]; then
            echo "fresh=true" >> "$GITHUB_OUTPUT"
          else
            echo "::notice::the default branch now has closure $current_closure; this run built $CLOSURE"
            echo "fresh=false" >> "$GITHUB_OUTPUT"
          fi

  publish:
    name: Publish runtime products
    needs: [closure, build, freshness]
    if: needs.closure.outputs.exists != 'true' && needs.freshness.outputs.fresh == 'true'
    # Serialize publishers by the immutable product tag, even when separate
    # source runs have the same closure. Never cancel a writer mid-publish.
    concurrency:
      group: runtime-products-${{{{ needs.closure.outputs.tag }}}}
      cancel-in-progress: false
    runs-on: {publish_runner}
    timeout-minutes: 20
    permissions:
      contents: write
      id-token: write
      attestations: write
    steps:
      - name: Download runtime assets
        uses: {download}
        with:
          path: dist
          merge-multiple: true
      - name: Assemble and verify the release
        id: assemble
        shell: bash
        env:
          CLOSURE: ${{{{ needs.closure.outputs.closure }}}}
          HEAD_SHA: ${{{{ needs.closure.outputs.head-sha }}}}
        run: |
          set -euo pipefail
          # The build jobs proved each binary's identity on its native runner;
          # here the digests they shipped prove the artifact transport moved
          # the same bytes. Only the native binary runs again, as a spot check.
          for platform in {platform_list}; do
            asset="velnor-workflow-$platform"
            test -s "dist/$asset" || {{ echo "::error::missing product asset $asset" >&2; exit 1; }}
            test -s "dist/$asset.sha256" || {{ echo "::error::missing digest for $asset" >&2; exit 1; }}
            expected="$(cat "dist/$asset.sha256")"
            [[ "$expected" =~ ^[0-9a-f]{{64}}$ ]] || {{ echo "::error::malformed digest for $asset" >&2; exit 1; }}
            actual="$(sha256sum "dist/$asset" | awk '{{print $1}}')"
            [[ "$actual" == "$expected" ]] || {{ echo "::error::transport digest mismatch for $asset" >&2; exit 1; }}
            # Artifact transport strips POSIX execute bits; restore them after
            # the digest proof so the native spot check below can execute.
            chmod 0755 "dist/$asset"
          done
          reported="$(./dist/velnor-workflow-Linux-X64 --closure)"
          [[ "$reported" == "$CLOSURE" ]] || {{ echo "::error::published binary reports closure $reported, expected $CLOSURE" >&2; exit 1; }}
          reported_revision="$(./dist/velnor-workflow-Linux-X64 --revision)"
          [[ "$reported_revision" == "$HEAD_SHA" ]] || {{ echo "::error::published binary reports revision $reported_revision, expected $HEAD_SHA" >&2; exit 1; }}
          jq -n \
            --arg closure "$CLOSURE" \
            --arg revision "$HEAD_SHA" \
{manifest_digests}            '{manifest_products}' \
            > dist/manifest.json
          for platform in {platform_list}; do
            jq -e --arg closure "$CLOSURE" --arg platform "$platform" --arg asset "velnor-workflow-$platform" \
              '{accept_filter}' dist/manifest.json >/dev/null
          done
          manifest_revision="$(jq -er '.revision' dist/manifest.json)"
          [[ "$manifest_revision" == "$HEAD_SHA" ]] || {{ echo "::error::assembled manifest names revision $manifest_revision, expected $HEAD_SHA" >&2; exit 1; }}
      - name: Attest release manifest
        uses: {attest}
        with:
          subject-path: dist/manifest.json
      - name: Smoke-test the release
        shell: bash
        env:
          GH_TOKEN: ${{{{ github.token }}}}
          CLOSURE: ${{{{ needs.closure.outputs.closure }}}}
        run: |
          set -euo pipefail
          # The exact consumer flow, against the local bytes the release will
          # publish: attestation, manifest, digest, and the self-report from
          # the install layout the setup action uses. The release is created
          # only after this flow passes, so a product no consumer would
          # accept fails the publish instead of shipping silently.
          manifest_revision="$(jq -er '.revision' dist/manifest.json)"
          [[ "$manifest_revision" =~ ^[0-9a-f]{{40}}$ ]] || {{ echo "::error::manifest revision is not a full commit SHA" >&2; exit 1; }}
          asset="velnor-workflow-${{RUNNER_OS}}-${{RUNNER_ARCH}}"
          gh attestation verify "dist/$asset" --owner {owner} --signer-workflow {repository}/.github/workflows/{workflow_file} --source-ref {branch_ref} --source-digest "$manifest_revision"
          gh attestation verify "dist/manifest.json" --owner {owner} --signer-workflow {repository}/.github/workflows/{workflow_file} --source-ref {branch_ref} --source-digest "$manifest_revision"
          jq -e --arg closure "$CLOSURE" --arg platform "${{RUNNER_OS}}-${{RUNNER_ARCH}}" --arg asset "$asset" \
            '{accept_filter}' "dist/manifest.json" >/dev/null
          actual="$(sha256sum "dist/$asset" | awk '{{print $1}}')"
          expected="$(jq -er --arg platform "${{RUNNER_OS}}-${{RUNNER_ARCH}}" '.products[$platform].binary' "dist/manifest.json")"
          [[ "$actual" == "$expected" ]] || {{ echo "::error::smoke-test digest mismatch" >&2; exit 1; }}
          runtime="{runtime_home}/$CLOSURE"
          mkdir -p "$runtime/bin"
          cp "dist/$asset" "$runtime/bin/velnor-workflow"
          chmod 0755 "$runtime/bin/velnor-workflow"
          cp "dist/manifest.json" "$runtime/manifest.json"
          chmod 0644 "$runtime/manifest.json"
          reported="$("$runtime/bin/velnor-workflow" --closure)"
          [[ "$reported" == "$CLOSURE" ]] || {{ echo "::error::installed runtime reports closure $reported, expected $CLOSURE" >&2; exit 1; }}
          reported_revision="$("$runtime/bin/velnor-workflow" --revision)"
          [[ "$reported_revision" == "$manifest_revision" ]] || {{ echo "::error::installed runtime reports revision $reported_revision, expected $manifest_revision" >&2; exit 1; }}
      - name: Create the release
        shell: bash
        env:
          GH_TOKEN: ${{{{ github.token }}}}
          CLOSURE: ${{{{ needs.closure.outputs.closure }}}}
          TAG: ${{{{ needs.closure.outputs.tag }}}}
          HEAD_SHA: ${{{{ needs.closure.outputs.head-sha }}}}
          DEFAULT_BRANCH: {default_branch}
          REPOSITORY: {repository}
        run: |
          set -euo pipefail
          current_default_branch_closure() {{
            local current tree listing current_closure
            current="$(gh api "repos/$REPOSITORY/commits/$DEFAULT_BRANCH" --jq '.sha')" || {{ echo "::error::failed to query the default branch head" >&2; return 2; }}
            [[ "$current" =~ ^[0-9a-f]{{40}}$ ]] || {{ echo "::error::the default branch returned an invalid commit SHA: $current" >&2; return 2; }}
            tree="$(gh api "repos/$REPOSITORY/git/trees/$current?recursive=1")" || {{ echo "::error::failed to query the current default-branch tree" >&2; return 2; }}
            [[ "$(jq -r '.truncated // false' <<<"$tree")" != "true" ]] || {{ echo "::error::the current default-branch tree is truncated" >&2; return 2; }}
            listing="$(jq -er '[.tree[] | select(.type != "tree") | select(.path == "Cargo.toml" or .path == "Cargo.lock" or .path == "rust-toolchain.toml" or .path == "rust-toolchain" or (.path | startswith("crates/velnor-workflow/")) or (.path | startswith(".cargo/"))) | "\(.mode) \(.type) \(.sha)\t\(.path)"] | sort | join("\n")' <<<"$tree")" || {{ echo "::error::failed to read current default-branch closure inputs" >&2; return 2; }}
            test -n "$listing" || {{ echo "::error::the current default branch has no closure inputs" >&2; return 2; }}
            current_closure="$(printf '%s\n{closure_footer}' "$(LC_ALL=C sort <<<"$listing")" | sha256sum | awk '{{print $1}}')"
            [[ "$current_closure" =~ ^[0-9a-f]{{64}}$ ]] || {{ echo "::error::current default-branch closure resolution failed" >&2; return 2; }}
            printf '%s' "$current_closure"
          }}
          ensure_fresh_source() {{
            local current_closure
            current_closure="$(current_default_branch_closure)" || return $?
            if [[ "$current_closure" != "$CLOSURE" ]]; then
              echo "::notice::the default branch now has closure $current_closure; this run built $CLOSURE"
              return 1
            fi
          }}
          release_presence() {{
            local query_error
            query_error="$(mktemp)"
            if gh api "repos/$REPOSITORY/releases/tags/$TAG" >/dev/null 2>"$query_error"; then
              rm -f "$query_error"
              return 0
            fi
            if grep -Eq '(^|[[:space:]])HTTP 404([:[:space:]]|$)|\(HTTP 404\)' "$query_error"; then
              rm -f "$query_error"
              return 1
            fi
            cat "$query_error" >&2
            rm -f "$query_error"
            echo "::error::failed to query release $TAG" >&2
            return 2
          }}
          verify_existing_release() {{
            local existing_dir manifest_revision asset platform actual expected reported
            local -a patterns=(--pattern manifest.json)
            existing_dir="$(mktemp -d)"
            for platform in {platform_list}; do
              patterns+=(--pattern "velnor-workflow-$platform")
            done
            if ! gh release download "$TAG" --repo "$REPOSITORY" --dir "$existing_dir" "${{patterns[@]}}"; then
              echo "::notice::release $TAG exists but its assets are not ready; retrying bounded convergence" >&2
              rm -rf "$existing_dir"
              return 2
            fi
            if ! manifest_revision="$(jq -er '.revision' "$existing_dir/manifest.json")" || [[ ! "$manifest_revision" =~ ^[0-9a-f]{{40}}$ ]]; then
              echo "::error::existing release $TAG has an invalid source revision" >&2
              rm -rf "$existing_dir"
              return 1
            fi
            if ! gh attestation verify "$existing_dir/manifest.json" --owner {owner} --signer-workflow {repository}/.github/workflows/{workflow_file} --source-ref {branch_ref} --source-digest "$manifest_revision"; then
              echo "::notice::release $TAG manifest attestation is not ready; retrying bounded convergence" >&2
              rm -rf "$existing_dir"
              return 2
            fi
            for platform in {platform_list}; do
              asset="velnor-workflow-$platform"
              if ! jq -e --arg closure "$CLOSURE" --arg platform "$platform" --arg asset "$asset" '{accept_filter}' "$existing_dir/manifest.json" >/dev/null; then
                echo "::error::existing release $TAG manifest does not accept $asset for closure $CLOSURE" >&2
                rm -rf "$existing_dir"
                return 1
              fi
              expected="$(jq -er --arg platform "$platform" '.products[$platform].binary' "$existing_dir/manifest.json")"
              actual="$(sha256sum "$existing_dir/$asset" | awk '{{print $1}}')"
              if [[ "$actual" != "$expected" ]]; then
                echo "::error::existing release $TAG has a digest mismatch for $asset" >&2
                rm -rf "$existing_dir"
                return 1
              fi
              chmod 0755 "$existing_dir/$asset"
              if ! gh attestation verify "$existing_dir/$asset" --owner {owner} --signer-workflow {repository}/.github/workflows/{workflow_file} --source-ref {branch_ref} --source-digest "$manifest_revision"; then
                echo "::notice::release $TAG attestation for $asset is not ready; retrying bounded convergence" >&2
                rm -rf "$existing_dir"
                return 2
              fi
            done
            reported="$("$existing_dir/velnor-workflow-Linux-X64" --closure)"
            if [[ "$reported" != "$CLOSURE" ]]; then
              echo "::error::existing release $TAG binary reports closure $reported, expected $CLOSURE" >&2
              rm -rf "$existing_dir"
              return 1
            fi
            reported="$("$existing_dir/velnor-workflow-Linux-X64" --revision)"
            if [[ "$reported" != "$manifest_revision" ]]; then
              echo "::error::existing release $TAG binary reports revision $reported, expected manifest revision $manifest_revision" >&2
              rm -rf "$existing_dir"
              return 1
            fi
            rm -rf "$existing_dir"
          }}
          for attempt in 1 2 3 4; do
            # The workflow-level freshness output prevents stale runs from
            # entering this job. Recheck immediately before each possible
            # mutation because the default branch can advance while queued.
            if ensure_fresh_source; then
              :
            else
              freshness_status=$?
              if [[ "$freshness_status" == 2 ]]; then exit 1; fi
              exit 0
            fi
            if release_presence; then
              if verify_existing_release; then
                echo "::notice::release $TAG already exists and has the verified product; leaving it untouched"
                exit 0
              else
                verify_status=$?
                if [[ "$verify_status" != 2 ]]; then exit 1; fi
                if [[ "$attempt" != 4 ]]; then sleep "$attempt"; fi
                continue
              fi
            else
              release_status=$?
              if [[ "$release_status" == 2 ]]; then exit 1; fi
            fi
            if gh release create "$TAG" --repo "$REPOSITORY" --title "$TAG" \
              --notes "Immutable velnor-workflow runtime product for source closure $CLOSURE (built from $HEAD_SHA). Consumers verify the manifest digest, the binary self-report, and the build provenance attestation." \
              {release_assets} dist/manifest.json; then
              exit 0
            fi
            if release_presence; then
              if verify_existing_release; then
                echo "::notice::release $TAG won a concurrent create and has the verified product"
                exit 0
              else
                verify_status=$?
                if [[ "$verify_status" != 2 ]]; then exit 1; fi
              fi
            else
              release_status=$?
              if [[ "$release_status" == 2 ]]; then exit 1; fi
            fi
            if [[ "$attempt" != 4 ]]; then
              sleep "$attempt"
            fi
          done
          echo "::error::release $TAG did not converge after four bounded create attempts" >&2
          exit 1
"#,
        header = GENERATED_HEADER,
        example_tag = example_tag(),
        default_branch = yaml_scalar(&config.default_branch),
        branch_ref = format!("refs/heads/{}", config.default_branch),
        closure_runner = hosted_runs_on,
        publish_runner = hosted_runs_on,
        checkout = ActionPin::Checkout.reference(),
        attest = ActionPin::Attest.reference(),
        upload = ActionPin::UploadArtifact.reference(),
        download = ActionPin::DownloadArtifact.reference(),
        repository = repository,
        owner = owner,
        workflow_file = RUNTIME_PRODUCTS_FILE,
        runtime_home = HOSTED_WORKFLOW_RUNTIME_HOME,
        tag_prefix = PRODUCT_TAG_PREFIX,
        closure_paths = CLOSURE_PATHS.join(" "),
        closure_footer = closure_footer,
        matrix = matrix,
        manifest_products = manifest_program,
        manifest_digests = manifest_digests,
        platform_list = platform_list,
        release_assets = release_assets,
        accept_filter = MANIFEST_ACCEPT_FILTER,
        cargo_home = PRODUCER_CARGO_HOME_VALUE,
    )))
}

/// The `jq` variable holding a platform's digest while the manifest assembles:
/// `linux_x64` for `Linux-X64`, and so on.
fn platform_variable(platform: &RuntimeProductPlatform) -> String {
    platform.key().replace('-', "_").to_ascii_lowercase()
}

/// The declared runtime-product producer.
pub(crate) struct RuntimeProducts;

impl Primitive for RuntimeProducts {
    fn id(&self) -> &'static str {
        super::RUNTIME_PRODUCTS
    }

    fn schema(&self) -> &'static [&'static str] {
        &[]
    }

    fn render(&self, ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        if ctx.config.repository != workflow_setup_action_repository() {
            return Err(GeneratorError::usage(format!(
                "`{}` renders `{RUNTIME_PRODUCTS_FILE}` only for the repository that ships the setup action",
                ctx.family
            )));
        }
        let file = ctx
            .file
            .filter(|file| !file.is_empty())
            .ok_or_else(|| {
                GeneratorError::usage(format!(
                    "`{}` renders `{RUNTIME_PRODUCTS_FILE}` and needs `file`",
                    ctx.family
                ))
            })?
            .to_owned();
        let Some(content) = runtime_products_content(ctx.config)? else {
            return Err(GeneratorError::usage(format!(
                "`{}` renders `{RUNTIME_PRODUCTS_FILE}` only for the repository that ships the setup action",
                ctx.family
            )));
        };
        Ok(Rendered {
            files: std::iter::once((
                std::path::PathBuf::from(".github/workflows").join(file),
                content,
            ))
            .collect(),
            ..Rendered::default()
        })
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]

    use std::collections::{BTreeMap, BTreeSet};
    use std::fmt::Display;
    use std::fs;
    use std::path::{Path, PathBuf};

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::s2::UnitKind;

    const FIXTURE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

    fn must<T, E: Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn must_fail<T, E: Display>(result: Result<T, E>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context} must fail"),
            Err(error) => error.to_string(),
        }
    }

    fn digest_of(content: &str) -> String {
        Sha256::digest(content.as_bytes()).iter().fold(
            String::with_capacity(64),
            |mut output, byte| {
                let _ = write!(output, "{byte:02x}");
                output
            },
        )
    }

    fn runtime_source_release_fixture() -> (
        Vec<u8>,
        BTreeMap<String, Vec<u8>>,
        RuntimeProductsReleaseExpectation<'static>,
    ) {
        const CLOSURE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut products = serde_json::Map::new();
        let mut assets = BTreeMap::new();
        for platform in runtime_product_platforms() {
            let key = platform.key();
            let asset = platform.asset_name();
            let bytes = format!("runtime bytes for {key}").into_bytes();
            let digest = digest_of(std::str::from_utf8(&bytes).expect("fixture bytes are UTF-8"));
            products.insert(
                key.clone(),
                serde_json::json!({"binary": digest, "asset": asset}),
            );
            assets.insert(key, bytes);
        }
        let manifest = serde_json::json!({
            "closure": CLOSURE,
            "revision": FIXTURE_REVISION,
            "profile": PROFILE_RELEASE,
            "features": CI_FEATURES,
            "products": products,
        });
        let consumer_manifest =
            serde_json::to_vec(&manifest).expect("serialize consumer manifest fixture");
        let expected = RuntimeProductsReleaseExpectation {
            repository: "owner/project",
            source_ref: "refs/heads/main",
            source_sha: FIXTURE_REVISION,
            release_tag: "v1.2.3",
            version: "1.2.3",
            closure: CLOSURE,
        };
        (consumer_manifest, assets, expected)
    }

    #[test]
    fn source_release_wrapper_binds_exact_manifest_and_three_asset_bytes() {
        let (consumer, assets, expected) = runtime_source_release_fixture();
        let wrapper = must(
            build_runtime_products_release_manifest(&consumer, &assets, &expected),
            "build source-release wrapper",
        );
        let parsed: RuntimeProductsReleaseManifest = must(
            serde_json::from_slice(&wrapper),
            "parse source-release wrapper",
        );
        assert_eq!(parsed.schema, RUNTIME_PRODUCTS_RELEASE_MANIFEST_SCHEMA);
        assert_eq!(parsed.repository, expected.repository);
        assert_eq!(parsed.source_ref, expected.source_ref);
        assert_eq!(parsed.source_sha, expected.source_sha);
        assert_eq!(parsed.release_tag, expected.release_tag);
        assert_eq!(parsed.version, expected.version);
        assert_eq!(parsed.closure, expected.closure);
        assert_eq!(parsed.manifest_sha256, sha256_bytes(&consumer));
        assert_eq!(parsed.products.len(), runtime_product_platforms().len());
        must(
            validate_runtime_products_release_manifest(
                &wrapper,
                &consumer,
                &assets,
                &expected,
            ),
            "verify source-release wrapper",
        );
    }

    #[test]
    fn source_release_wrapper_rejects_changed_bytes_identity_and_platform_sets() {
        let (consumer, assets, expected) = runtime_source_release_fixture();
        let wrapper = must(
            build_runtime_products_release_manifest(&consumer, &assets, &expected),
            "build source-release wrapper",
        );

        let mut changed_assets = assets.clone();
        changed_assets
            .get_mut("Linux-X64")
            .expect("Linux X64 asset")
            .push(0);
        must_fail(
            validate_runtime_products_release_manifest(
                &wrapper,
                &consumer,
                &changed_assets,
                &expected,
            ),
            "reject asset bytes changed after wrapper creation",
        );

        let mut wrong_source_wrapper: serde_json::Value =
            serde_json::from_slice(&wrapper).expect("parse wrapper fixture");
        wrong_source_wrapper["source_sha"] = serde_json::json!("b".repeat(40));
        let wrong_source_wrapper =
            serde_json::to_vec(&wrong_source_wrapper).expect("serialize tampered wrapper");
        must_fail(
            validate_runtime_products_release_manifest(
                &wrong_source_wrapper,
                &consumer,
                &assets,
                &expected,
            ),
            "reject wrapper for another source SHA",
        );

        let wrong_ref = RuntimeProductsReleaseExpectation {
            source_ref: "refs/tags/v1.2.3",
            ..expected
        };
        must_fail(
            build_runtime_products_release_manifest(&consumer, &assets, &wrong_ref),
            "reject a tag ref as the signer source ref",
        );

        let mut missing_asset = assets.clone();
        missing_asset.remove("Linux-ARM64");
        must_fail(
            build_runtime_products_release_manifest(&consumer, &missing_asset, &expected),
            "reject missing platform asset",
        );
    }

    fn must_some<T>(option: Option<T>, context: &str) -> T {
        option.unwrap_or_else(|| panic!("{context}"))
    }

    fn unit() -> crate::s2::Unit {
        crate::s2::Unit {
            id: "rust-example".to_owned(),
            label: "rust-example".to_owned(),
            kind: UnitKind::Rust,
            root: ".".to_owned(),
            pinned_lockfile: true,
            watch: vec!["Cargo.toml".to_owned()],
            pr_commands: vec!["cargo check".to_owned()],
            full_commands: vec!["cargo check".to_owned()],
            depends_on: Vec::new(),
            cache: None,
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: Some(crate::s2::RustToolchain {
                channel: "1.91.1".to_owned(),
                components: Vec::new(),
                targets: Vec::new(),
                profile: None,
            }),
            services: Vec::new(),
            trust: crate::s2::provider::TrustReq::UntrustedOk,
            platform: crate::s2::provider::Platform::LinuxX64,
            capabilities: crate::s2::provider::Capabilities::default(),
            workspace_check: false,
            products: Vec::new(),
            prerequisites: Vec::new(),
            docker_contexts: Vec::new(),
            env: std::collections::BTreeMap::new(),
            mbx: None,
            prepared_tools: Vec::new(),
        }
    }

    fn config(workflow_files: &[&str]) -> ProjectConfig {
        crate::s2::ProjectConfig {
            repository: String::new(),
            workflow_revision: FIXTURE_REVISION.to_owned(),
            profile: "generic".to_owned(),
            analysis: crate::s2::AnalysisSummary {
                method: "test".to_owned(),
                detected: Vec::new(),
                limitations: Vec::new(),
            },
            verified: true,
            workflow_files: workflow_files
                .iter()
                .map(|file| (*file).to_owned())
                .collect(),
            notes: Vec::new(),
            version_bump_units: Vec::new(),
            default_branch: "main".to_owned(),
            providers: crate::s2::provider::ProviderId::ALL.into_iter().collect(),
            automatic_providers: crate::s2::provider::ProviderId::ALL.into_iter().collect(),
            default_dispatch_providers: crate::s2::provider::ProviderId::ALL.into_iter().collect(),
            selectors: crate::s2::scan::default_selectors(),
            release_enabled: false,
            release_reason: String::new(),
            release: None,
            renovate_enabled: false,
            renovate_reason: String::new(),
            renovate: None,
            docs_enabled: false,
            docs_reason: String::new(),
            docs: None,
            check_profiles: Vec::new(),
            maintenance: crate::s2::MaintenanceSpec::default(),
            units: vec![unit()],
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            rust_needs: crate::s2::RustNeeds::Parallel,
            concurrency_group: None,
            serial_stack_groups: false,
            static_files: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
            github_cache: crate::s2::config::CacheGithubSection::default(),
            velnor_host_cache: crate::s2::config::CacheVelnorSection::default(),
        }
    }

    fn owner_config(workflow_files: &[&str]) -> ProjectConfig {
        let mut config = config(workflow_files);
        config.repository = workflow_setup_action_repository().to_owned();
        config
    }

    fn owner_content(workflow_files: &[&str]) -> String {
        must_some(
            must(
                runtime_products_content(&owner_config(workflow_files)),
                "the owner renders the producer",
            ),
            "the owner renders the producer",
        )
    }

    fn scanned_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-runtime-products-{name}-{}",
            crate::s2::unique_suffix()
        ));
        must(fs::create_dir_all(&root), "create test repository");
        must(
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"example\"\nversion = \"0.1.0\"\n",
            ),
            "write fixture manifest",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.91.1\"\n",
            ),
            "write fixture toolchain pin",
        );
        root
    }

    fn try_generate(
        root: &std::path::Path,
        config: &ProjectConfig,
        rows: &str,
    ) -> Result<super::super::Surface, GeneratorError> {
        let providers: crate::s2::provider::ProviderSet =
            crate::s2::provider::ProviderId::ALL.into_iter().collect();
        let shape = must(
            crate::s2::scan::scan_shape(root, &providers, "main", &[]),
            "scan fixture",
        );
        let directory = root.join(crate::s2::config::GENERATION_CONFIG_PATH);
        must(
            fs::create_dir_all(directory.parent().unwrap_or(root)),
            "create generation config directory",
        );
        must(
            fs::write(
                &directory,
                format!("schema = 2\n\n[generator]\nrepository = \"example/declared\"\n\n{rows}"),
            ),
            "write declared config",
        );
        let generation = must(
            crate::s2::config::discover(root),
            "discover declared config",
        );
        super::super::generate(root, &shape, config, generation.as_ref())
    }

    /// The setup action source: the consumer contract the producer mirrors.
    fn setup_action_source() -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github-gen/sources/actions/setup-velnor-workflow/action.yml");
        must(fs::read_to_string(&path), "read the setup action source")
    }

    #[test]
    fn non_owner_repositories_render_no_producer() {
        assert!(
            must(runtime_products_content(&config(&[])), "render producer").is_none(),
            "a repository without identity renders no producer"
        );
        let mut consumer = config(&[]);
        consumer.repository = "example/consumer".to_owned();
        assert!(
            must(runtime_products_content(&consumer), "render producer").is_none(),
            "a consumer repository renders no producer"
        );
    }

    #[test]
    fn owner_renders_the_producer_on_main_push_only() {
        let config = owner_config(&[]);
        let content = must_some(
            must(
                runtime_products_content(&config),
                "the owner renders the producer",
            ),
            "the owner renders the producer",
        );
        assert!(content.starts_with(GENERATED_HEADER), "{content}");
        assert!(
            content.contains("name: Velnor workflow runtime products"),
            "{content}"
        );
        assert!(
            content.contains("branches: [main]"),
            "the producer follows its fixed attestation source ref: {content}"
        );
        assert!(!content.contains("workflow_dispatch:"), "{content}");
        for event in ["pull_request", "schedule:", "tags:", "merge_group"] {
            assert!(
                !content.contains(event),
                "the producer admits no untrusted or scheduled trigger ({event}): {content}"
            );
        }
    }

    #[test]
    fn release_tags_come_from_product_tag() {
        let content = owner_content(&[]);
        assert!(
            content.contains(&format!("tag=\"{PRODUCT_TAG_PREFIX}${{closure:0:16}}\"")),
            "the shell forms tags from the shared prefix: {content}"
        );
        assert!(
            content.contains(&example_tag()),
            "the header pins the scheme to product_tag: {content}"
        );
        assert_eq!(
            example_tag(),
            format!("{PRODUCT_TAG_PREFIX}{}", "0".repeat(16)),
            "the worked example is the zero closure's product tag"
        );
    }

    #[test]
    fn closure_shell_matches_the_canonical_form() {
        let content = owner_content(&[]);
        let pathspec = CLOSURE_PATHS.join(" ");
        assert!(
            content.contains(&format!("git ls-tree -r HEAD -- {pathspec}")),
            "the closure pathspec derives from CLOSURE_PATHS: {content}"
        );
        assert!(
            content.contains(&format!(
                "closure-version:{CLOSURE_VERSION}\\nfeatures:{CI_FEATURES}\\nprofile:{PROFILE_RELEASE}\\n"
            )),
            "the closure footer derives from the canonical constants: {content}"
        );
        assert!(content.contains("LC_ALL=C sort"), "{content}");
        assert!(
            content.contains("[[ \"$closure\" =~ ^[0-9a-f]{64}$ ]]"),
            "a malformed closure fails the run: {content}"
        );
    }

    #[test]
    fn closure_shell_matches_the_setup_action() {
        let content = owner_content(&[]);
        let action = setup_action_source();
        // The commands differ (the action resolves any rev portably, the
        // producer resolves HEAD on Linux), but the hashed byte stream — the
        // pathspec, the byte sort, and the footer — must agree exactly.
        let action_ls_tree = must_some(
            action
                .lines()
                .map(str::trim)
                .find(|line| line.contains("ls-tree -r") && line.contains("-- ")),
            "the setup action resolves closures from git",
        );
        let action_pathspec = must_some(
            action_ls_tree
                .split_once("-- ")
                .map(|(_, pathspec)| pathspec),
            "the setup action pathspec",
        );
        let action_pathspec = must_some(
            action_pathspec.strip_suffix(")\""),
            "the setup action pathspec end",
        );
        assert_eq!(
            action_pathspec,
            CLOSURE_PATHS.join(" "),
            "the setup action pathspec is the closure paths"
        );
        assert!(
            content.contains(action_pathspec),
            "producer and setup action hash the same pathspec: {content}"
        );
        let footer = format!(
            "closure-version:{CLOSURE_VERSION}\\nfeatures:{CI_FEATURES}\\nprofile:{PROFILE_RELEASE}\\n"
        );
        assert!(
            action.contains(&footer),
            "the setup action hashes the canonical footer"
        );
        assert!(
            content.contains(&footer),
            "the producer hashes the canonical footer: {content}"
        );
        assert!(
            content.contains("LC_ALL=C sort") && action.contains("LC_ALL=C sort"),
            "both sort the listing in byte order"
        );
        assert!(
            content.contains("velnor-workflow-${RUNNER_OS}-${RUNNER_ARCH}")
                && action.contains("velnor-workflow-${RUNNER_OS}-${RUNNER_ARCH}"),
            "producer and consumer name assets identically"
        );
    }

    #[test]
    fn build_is_a_locked_hermetic_release() {
        let content = owner_content(&[]);
        assert!(
            content.contains(
                "run: cargo build --locked --no-default-features --release --package velnor-workflow --bin velnor-workflow"
            ),
            "the build honors the footer contract (locked, no default features, release): {content}"
        );
        for guard in [
            "git status --porcelain",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC_WRAPPER",
        ] {
            assert!(
                content.contains(guard),
                "the hermetic proof covers {guard}: {content}"
            );
        }
        assert!(
            content.contains("CARGO_HOME: ${{ runner.temp }}/velnor-producer-cargo-home"),
            "the build runs under an isolated Cargo home: {content}"
        );
        assert!(
            !content.contains("sccache"),
            "no compiler cache wrapper may enter the producer build: {content}"
        );
        // The `runner` context is unavailable in job-level `env:` (GitHub
        // rejects the workflow at compile time), so the build job carries no
        // job-level `env:` and exactly the four steps that need isolation
        // carry step-level `CARGO_HOME`.
        let build = must_some(content.split("  build:\n").nth(1), "the build job");
        let build = must_some(build.split("\n  freshness:\n").next(), "the build job body");
        assert!(
            !build.contains("\n    env:"),
            "the build job has no job-level env (runner is unavailable there): {build}"
        );
        assert_eq!(
            build
                .matches("          CARGO_HOME: ${{ runner.temp }}/velnor-producer-cargo-home")
                .count(),
            4,
            "exactly four step-level CARGO_HOME entries: {build}"
        );
        let steps: Vec<&str> = build.split("      - name: ").skip(1).collect();
        assert_eq!(steps.len(), 9, "the build job has nine steps: {build}");
        for step in &steps {
            let name = must_some(step.split('\n').next(), "the step name");
            let has_cargo_home = step.contains(PRODUCER_CARGO_HOME_VALUE);
            let needs_isolation = [
                "Provision Rust toolchain",
                "Prove a hermetic build environment",
                "Build release runtime",
                "Prove the product closure",
            ]
            .contains(&name);
            assert_eq!(
                has_cargo_home, needs_isolation,
                "step `{name}` carries step-level CARGO_HOME iff it needs isolation: {step}"
            );
        }
    }

    #[test]
    fn binary_closure_is_proven_before_upload() {
        let content = owner_content(&[]);
        let prove = must_some(
            content.find("Prove the product closure"),
            "the prove step exists",
        );
        let attest = must_some(
            content.find("Attest runtime asset"),
            "the attest step exists",
        );
        let upload = must_some(
            content.find("Upload runtime asset"),
            "the upload step exists",
        );
        assert!(
            prove < attest && attest < upload,
            "prove precedes attest precedes upload: {content}"
        );
        assert!(
            content.contains("CLOSURE: ${{ needs.closure.outputs.closure }}"),
            "the proof compares against the tag closure: {content}"
        );
        assert!(
            content.contains(
                "[[ \"$reported\" == \"$CLOSURE\" ]] || { echo \"::error::built binary reports closure"
            ),
            "a binary that misreports its closure fails the build: {content}"
        );
        assert!(
            content.contains("reported_revision=\"$(\"$binary\" --revision)\""),
            "the build probes the binary's revision stamp: {content}"
        );
        assert!(
            content.contains(
                "[[ \"$reported_revision\" == \"$head\" ]] || { echo \"::error::built binary reports revision"
            ),
            "a binary that misreports its build commit fails the build: {content}"
        );
    }

    #[test]
    fn manifest_shape_matches_the_consumer_contract() {
        let content = owner_content(&[]);
        let action = setup_action_source();
        assert!(
            action.contains(MANIFEST_ACCEPT_FILTER),
            "the acceptance filter is the setup action's own"
        );
        let bodies = step_bodies(&content);
        let assemble_body = must_some(
            bodies.iter().find_map(|(name, body)| {
                (name == "Assemble and verify the release").then_some(body)
            }),
            "assemble body",
        );
        let smoke_body = must_some(
            bodies
                .iter()
                .find_map(|(name, body)| (name == "Smoke-test the release").then_some(body)),
            "smoke body",
        );
        assert_eq!(
            assemble_body.matches(MANIFEST_ACCEPT_FILTER).count()
                + smoke_body.matches(MANIFEST_ACCEPT_FILTER).count(),
            2,
            "assemble and smoke-test each evaluate the consumer filter"
        );
        assert!(
            content.contains("--arg revision \"$HEAD_SHA\""),
            "the manifest names the commit the producer built from: {content}"
        );
        assert!(
            content.contains("revision: $revision"),
            "the manifest program carries the revision field: {content}"
        );
        assert!(
            content.contains("manifest_revision=\"$(jq -er '.revision' dist/manifest.json)\""),
            "the assembled manifest revision is read back: {content}"
        );
        assert!(
            content.contains(
                "[[ \"$manifest_revision\" == \"$HEAD_SHA\" ]] || { echo \"::error::assembled manifest names revision"
            ),
            "a manifest that names the wrong build commit fails the publish: {content}"
        );
        assert!(
            content.contains(
                "[[ \"$reported_revision\" == \"$HEAD_SHA\" ]] || { echo \"::error::published binary reports revision"
            ),
            "the publish spot check binds the binary stamp to the build commit: {content}"
        );
        // The revision checks above read `$HEAD_SHA` under `set -u`: the
        // assemble step must receive the build commit in its own `env:`, or
        // the step dies on an unbound variable before assembling anything.
        let assemble = must_some(
            content.split("Assemble and verify the release").nth(1),
            "the assemble step",
        );
        let assemble = must_some(
            assemble.split("- name: Attest release manifest").next(),
            "the assemble step body",
        );
        assert!(
            assemble.contains("HEAD_SHA: ${{ needs.closure.outputs.head-sha }}"),
            "the assemble step receives the build commit it proves: {assemble}"
        );
        for platform in ["Linux-X64", "Linux-ARM64", "macOS-ARM64"] {
            assert!(
                content.contains(&format!(
                    "\"{platform}\": {{binary: ${}, asset: \"velnor-workflow-{platform}\"}}",
                    platform.to_ascii_lowercase().replace('-', "_")
                )),
                "the manifest binds {platform} digest and asset: {content}"
            );
        }
        assert!(
            content.contains("profile: \"release\", features: \"\""),
            "the manifest footer matches the build: {content}"
        );
        // The Velnor policy provisioner is the second consumer: it must accept
        // the same manifest and the same attestation the setup action does.
        let velnor = crate::s2::workflow_pinned_policy_runtime_local("checkout");
        assert!(
            velnor.contains(MANIFEST_ACCEPT_FILTER),
            "the Velnor consumer evaluates the same filter"
        );
        assert!(
            velnor.contains(&format!(
                "--signer-workflow \"$PRODUCT_REPOSITORY/.github/workflows/{RUNTIME_PRODUCTS_FILE}\""
            )),
            "the Velnor consumer pins the same producer workflow"
        );
        assert!(
            velnor.contains("gh attestation verify \"$temporary/manifest.json\""),
            "the Velnor consumer verifies the manifest it trusts"
        );
        assert!(
            velnor.contains("\"$existing\" != \"$expected\""),
            "the Velnor consumer reuses its slot only on a manifest digest match"
        );
    }

    #[test]
    fn attestation_covers_assets_and_manifest_in_the_pinned_workflow() {
        let content = owner_content(&[]);
        let repository = workflow_setup_action_repository();
        assert_eq!(
            content.matches(ActionPin::Attest.reference()).count(),
            2,
            "the pinned attest action covers the asset and the manifest: {content}"
        );
        assert!(
            content.contains("subject-path: ${{ steps.prove.outputs.asset }}"),
            "the per-platform asset is attested where it is built: {content}"
        );
        assert!(
            content.contains("subject-path: dist/manifest.json"),
            "the manifest is attested where it assembles: {content}"
        );
        let signer =
            format!("--signer-workflow {repository}/.github/workflows/{RUNTIME_PRODUCTS_FILE}");
        let owner_flag = format!("--owner {}", product_owner(repository));
        for flag in [&signer, &owner_flag] {
            assert!(
                content.contains(flag),
                "the smoke test verifies {flag}: {content}"
            );
        }
        let action = setup_action_source();
        for flag in [&signer, &owner_flag] {
            assert!(
                action.contains(flag),
                "the setup action verifies the same {flag}"
            );
        }
        // Subject-level: both consumers verify the manifest as well as the
        // asset, against the same pinned producer workflow.
        let velnor = crate::s2::workflow_pinned_policy_runtime_local("checkout");
        for (name, consumer) in [
            ("setup action", action.as_str()),
            ("velnor", velnor.as_str()),
        ] {
            for subject in [
                "gh attestation verify \"$temporary/$asset\"",
                "gh attestation verify \"$temporary/manifest.json\"",
            ] {
                assert!(
                    consumer.contains(subject),
                    "the {name} consumer verifies {subject}"
                );
            }
        }
    }

    #[test]
    fn all_consumers_pin_the_same_producer_ref() {
        // The setup action, the Velnor provisioner, and the producer smoke
        // test verify the same two subjects against the same signer
        // workflow: an attestation minted anywhere but the runtime-products
        // publisher verifies nowhere.
        let signer = "--signer-workflow tailrocks/velnor/.github/workflows/ci-runtime-products.yml";
        let action = setup_action_source();
        assert_eq!(
            action.matches(signer).count(),
            2,
            "the setup action pins the signer on the asset and the manifest"
        );
        let velnor = crate::s2::workflow_pinned_policy_runtime_local("checkout");
        assert_eq!(
            velnor
                .matches("--signer-workflow \"$PRODUCT_REPOSITORY/.github/workflows/ci-runtime-products.yml\"")
                .count(),
            2,
            "the Velnor provisioner pins the signer on the asset and the manifest"
        );
        assert_eq!(
            velnor.matches("--source-ref \"$source_ref\"").count(),
            2,
            "both Velnor subject checks pin the product main ref"
        );
        assert!(
            velnor.contains("source_ref=\"refs/heads/main\"")
                && !velnor.contains("DEFAULT_BRANCH"),
            "the Velnor product ref is fixed to the producer's main branch"
        );
        let content = owner_content(&[]);
        let smoke = must_some(
            step_bodies(&content)
                .into_iter()
                .find_map(|(name, body)| (name == "Smoke-test the release").then_some(body)),
            "producer smoke-test body",
        );
        assert_eq!(
            smoke.matches(signer).count(),
            2,
            "the producer smoke test pins the signer on the asset and the manifest"
        );
    }

    #[test]
    fn producer_builders_are_fixed_closure_covered_infrastructure() {
        // Hostile lane labels must not move the builders: the matrix is the
        // fixed product-infrastructure mapping, so a label change can never
        // reuse a stale tag's binaries, and a mapping change is a generator
        // source change that mints a new closure and a new tag.
        let mut config = owner_config(&[]);
        for selector in config.selectors.values_mut() {
            selector.runs_on = vec!["self-hosted-spoof".to_owned()];
        }
        let content = must_some(
            must(
                runtime_products_content(&config),
                "the owner renders the producer",
            ),
            "the owner renders the producer",
        );
        let matrix = must_some(
            content
                .split("      matrix:\n        include:\n")
                .nth(1)
                .and_then(|tail| tail.split("\n    runs-on: ${{ matrix.runner }}").next()),
            "the build matrix renders",
        );
        for (os, arch, runner) in [
            ("Linux", "X64", LINUX_X64_RUNNER),
            ("Linux", "ARM64", LINUX_ARM64_RUNNER),
            ("macOS", "ARM64", MACOS_HOSTED_RUNS_ON),
        ] {
            assert!(
                matrix.contains(&format!(
                    "- os: {os}\n            arch: {arch}\n            runner: {runner}"
                )),
                "the matrix builds {os}-{arch} on the fixed {runner}: {matrix}"
            );
        }
        for spoof in ["self-hosted-spoof-x64", "self-hosted-spoof-macos"] {
            assert!(
                !matrix.contains(spoof),
                "lane labels never reselect the builders ({spoof}): {matrix}"
            );
        }
        assert!(
            !content.contains("macOS-X64"),
            "no lane selects an Intel Mac, so no macOS X64 product is built: {content}"
        );
        assert!(
            content.contains("runs-on: ${{ matrix.runner }}"),
            "one job per platform: {content}"
        );
        // The mapping must live under a closure path: only then does a
        // builder change alter the digest and mint a new tag.
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mapping_file = manifest_dir.join("src/s2/primitives/runtime_products.rs");
        let mapping_source = must(
            fs::read_to_string(&mapping_file),
            "read the producer renderer",
        );
        for runner in [LINUX_X64_RUNNER, LINUX_ARM64_RUNNER] {
            assert!(
                mapping_source.contains(&format!("\"{runner}\"")),
                "the fixed mapping lives in the producer renderer: {runner}"
            );
        }
        assert!(
            mapping_source.contains("runner: MACOS_HOSTED_RUNS_ON"),
            "the macOS builder reuses the hosted Apple runner contract: {mapping_source}"
        );
        let root = must(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&manifest_dir)
                .args(["rev-parse", "--show-toplevel"])
                .output(),
            "git present",
        );
        assert!(root.status.success());
        let root = must(
            PathBuf::from(String::from_utf8_lossy(&root.stdout).trim().to_owned())
                .canonicalize()
                .map_err(|error| format!("canonicalize repository root: {error}")),
            "canonicalize the repository root",
        );
        let relative = must(
            mapping_file
                .canonicalize()
                .map_err(|error| format!("canonicalize renderer: {error}"))
                .and_then(|absolute| {
                    absolute
                        .strip_prefix(&root)
                        .map(Path::to_path_buf)
                        .map_err(|_| "the renderer is outside the repository".to_owned())
                }),
            "locate the renderer under the repository root",
        );
        assert!(
            CLOSURE_PATHS
                .iter()
                .any(|covered| { relative == Path::new(covered) || relative.starts_with(covered) }),
            "the builder mapping is closure-covered ({}): {relative:?}",
            CLOSURE_PATHS.join(", ")
        );
    }

    #[test]
    fn existing_tags_skip_and_never_overwrite() {
        let content = owner_content(&[]);
        assert!(
            content.contains("exists: ${{ steps.exists.outputs.exists }}"),
            "the closure job reports tag existence: {content}"
        );
        assert_eq!(
            content
                .matches("if: needs.closure.outputs.exists != 'true'")
                .count(),
            3,
            "build, freshness, and publish skip when the tag exists: {content}"
        );
        assert!(
            content.contains(
                "release $TAG already exists and has the verified product; leaving it untouched"
            ),
            "the publish job verifies an existing tag before converging: {content}"
        );
        for overwrite in ["--clobber", "--overwrite", "release delete", "release edit"] {
            assert!(
                !content.contains(overwrite),
                "the producer never overwrites ({overwrite}): {content}"
            );
        }
    }

    #[test]
    fn actions_are_sha_pinned_with_least_privilege() {
        let content = owner_content(&[]);
        let mut pinned = 0;
        for line in content.lines().filter(|line| line.contains("uses:")) {
            let reference = must_some(line.split("uses:").nth(1), "the uses reference").trim();
            let sha = must_some(reference.split('@').nth(1), "the action pin");
            let sha = must_some(sha.split_whitespace().next(), "the bare pin");
            assert_eq!(sha.len(), 40, "SHA-pinned actions only: {line}");
            assert!(
                sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "SHA-pinned actions only: {line}"
            );
            pinned += 1;
        }
        assert_eq!(
            pinned, 8,
            "every step the producer needs, pinned: {content}"
        );
        assert!(
            content.contains("permissions:\n  contents: read\n"),
            "the workflow defaults to read-only: {content}"
        );
        let build = must_some(content.split("  build:\n").nth(1), "the build job");
        let build = must_some(build.split("\n  publish:\n").next(), "the build job body");
        assert!(build.contains("id-token: write"), "{build}");
        assert!(build.contains("attestations: write"), "{build}");
        assert!(!build.contains("contents: write"), "{build}");
        let publish = must_some(content.split("\n  publish:\n").nth(1), "the publish job");
        assert!(publish.contains("contents: write"), "{publish}");
        assert!(publish.contains("id-token: write"), "{publish}");
        assert!(publish.contains("attestations: write"), "{publish}");
    }

    #[test]
    fn registry_resolves_the_producer() {
        assert!(is_runtime_products_side(super::super::RUNTIME_PRODUCTS));
        assert!(!is_runtime_products_side(super::super::RELEASE));
        assert_eq!(
            canonical_runtime_products_side_file(super::super::RUNTIME_PRODUCTS),
            Some(RUNTIME_PRODUCTS_FILE)
        );
        let registry = super::super::registry();
        let producer = must_some(
            registry
                .iter()
                .find(|primitive| primitive.id() == super::super::RUNTIME_PRODUCTS),
            "the producer is registered",
        );
        assert!(
            producer.schema().is_empty(),
            "the producer takes no arguments"
        );
    }

    #[test]
    fn declared_row_renders_for_the_owner_and_fails_closed_elsewhere() {
        let rows = format!(
            "[[declare]]\nprimitive = \"{}\"\nfile = \"{RUNTIME_PRODUCTS_FILE}\"\n",
            super::super::RUNTIME_PRODUCTS
        );
        let root = scanned_root("declared-owner");
        let surface = must(
            try_generate(&root, &owner_config(&["maintenance.yml"]), &rows),
            "the owner declares the producer",
        );
        let path = PathBuf::from(".github/workflows").join(RUNTIME_PRODUCTS_FILE);
        assert_eq!(
            surface.files.get(&path).map(String::as_str),
            must(
                runtime_products_content(&owner_config(&["maintenance.yml"])),
                "render producer",
            )
            .as_deref(),
            "declared and legacy paths render identical bytes"
        );
        assert!(
            surface
                .added_files
                .contains(&RUNTIME_PRODUCTS_FILE.to_owned()),
            "declaring the producer owns its file: {:?}",
            surface.added_files
        );
        let _ = fs::remove_dir_all(&root);

        let root = scanned_root("declared-consumer");
        let error = must_fail(
            try_generate(&root, &config(&["maintenance.yml"]), &rows),
            "a consumer must not declare the producer",
        );
        assert!(error.contains("only for the repository"), "{error}");
        let _ = fs::remove_dir_all(&root);

        let root = scanned_root("declared-wrong-file");
        let rows = format!(
            "[[declare]]\nprimitive = \"{}\"\nfile = \"other.yml\"\n",
            super::super::RUNTIME_PRODUCTS
        );
        let error = must_fail(
            try_generate(&root, &owner_config(&["maintenance.yml"]), &rows),
            "a wrong file must fail closed",
        );
        assert!(
            error.contains(RUNTIME_PRODUCTS_FILE),
            "the error names the canonical file: {error}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn owner_auto_add_through_generated_files() {
        let files = must(
            crate::s2::generated_files(&owner_config(&[])),
            "generate the owner surface",
        );
        let path = PathBuf::from(".github/workflows").join(RUNTIME_PRODUCTS_FILE);
        assert!(
            files.contains_key(&path),
            "the owner owns the producer without declaring it: {:?}",
            files.keys().collect::<Vec<_>>()
        );
        let project = must_some(
            files.get(&PathBuf::from(".github/ci/project.toml")),
            "the project contract",
        );
        assert!(
            project.contains(RUNTIME_PRODUCTS_FILE),
            "the owned surface records the producer: {project}"
        );
        let files = must(
            crate::s2::generated_files(&config(&[])),
            "generate the consumer surface",
        );
        assert!(
            !files.contains_key(&path),
            "a consumer surface carries no producer"
        );
        let mut adopted = owner_config(&[]);
        adopted.adopted_workflow_surface = true;
        let files = must(
            crate::s2::generated_files(&adopted),
            "generate the adopted surface",
        );
        assert!(
            !files.contains_key(&path),
            "an adopted surface owns its files as reviewed templates"
        );
    }

    #[test]
    fn toolchain_fallback_stays_file_driven() {
        let mut config = owner_config(&[]);
        config.units[0].toolchain = None;
        let content = must_some(
            must(
                runtime_products_content(&config),
                "the owner renders without a scanned toolchain",
            ),
            "the owner renders without a scanned toolchain",
        );
        assert!(
            content.contains("rustup toolchain install"),
            "the install stays file-driven: {content}"
        );
        assert!(
            !content.contains("--profile"),
            "no profile is invented: {content}"
        );
    }

    #[test]
    fn smoke_test_installs_the_consumer_layout() {
        let content = owner_content(&[]);
        assert!(
            content.contains(&format!(
                "runtime=\"{HOSTED_WORKFLOW_RUNTIME_HOME}/$CLOSURE\""
            )),
            "the smoke test installs the setup action's layout: {content}"
        );
        assert!(
            content.contains("\"$runtime/bin/velnor-workflow\" --closure"),
            "the smoke test self-reports from the install layout: {content}"
        );
        assert!(
            content.contains("manifest_revision=\"$(jq -er '.revision' \"dist/manifest.json\")\""),
            "the smoke test reads the revision from the local manifest: {content}"
        );
        assert!(
            content.contains("\"$runtime/bin/velnor-workflow\" --revision"),
            "the smoke test probes the installed revision stamp: {content}"
        );
        assert!(
            content.contains(
                "[[ \"$reported_revision\" == \"$manifest_revision\" ]] || { echo \"::error::installed runtime reports revision"
            ),
            "the smoke test binds the installed stamp to the manifest: {content}"
        );
    }

    #[test]
    fn producer_runs_only_on_the_default_branch() {
        let content = owner_content(&[]);
        let closure = must_some(content.split("  closure:\n").nth(1), "the closure job");
        let closure = must_some(closure.split("\n  build:\n").next(), "the closure job body");
        // The guard is the first step and confirms the trusted source ref
        // before checkout, closure resolution, or build.
        let first = must_some(
            closure.split("      - name: ").nth(1),
            "the first closure step",
        );
        let name = must_some(first.split('\n').next(), "the first step name");
        assert_eq!(name, "Prove the default-branch ref", "{closure}");
        assert!(
            first.contains("REF: ${{ github.ref }}"),
            "the guard reads the run ref: {first}"
        );
        assert!(
            first.contains("[[ \"$REF\" == \"refs/heads/main\" ]]"),
            "the guard compares against the default-branch ref: {first}"
        );
        assert!(
            first.contains("the producer publishes only from refs/heads/main"),
            "the failure names the expected ref: {first}"
        );
        let mut config = owner_config(&[]);
        config.default_branch = "trunk".to_owned();
        let error = runtime_products_content(&config).expect_err("non-main source ref is unsafe");
        assert!(
            error.to_string().contains("requires default_branch = \"main\""),
            "the producer and consumers share refs/heads/main: {error}"
        );
    }

    #[test]
    fn publish_verifies_before_creating_the_release() {
        let content = owner_content(&[]);
        let publish = must_some(content.split("\n  publish:\n").nth(1), "the publish job");
        let assemble = must_some(
            publish.find("Assemble and verify the release"),
            "the assemble step",
        );
        let attest = must_some(
            publish.find("Attest release manifest"),
            "the manifest attestation step",
        );
        let smoke = must_some(
            publish.find("Smoke-test the release"),
            "the smoke-test step",
        );
        let create = must_some(publish.find("gh release create"), "the release creation");
        assert!(
            assemble < attest && attest < smoke && smoke < create,
            "assemble precedes manifest attestation precedes the smoke test precedes release creation: {publish}"
        );
        assert_eq!(
            publish.matches("gh release create").count(),
            1,
            "exactly one creation, after every verification: {publish}"
        );
        assert!(
            !content.contains("skipped"),
            "no skipped output remains: verification gates the create directly: {content}"
        );
        let recheck = must_some(
            publish.find("already exists and has the verified product; leaving it untouched"),
            "the pre-create release verification",
        );
        assert!(
            recheck < create,
            "an existing tag is checked before creation: {publish}"
        );
        assert!(
            publish.contains("for attempt in 1 2 3 4"),
            "bounded conflict retries: {publish}"
        );
        assert!(
            publish.contains("gh release create \"$TAG\" --repo \"$REPOSITORY\" --title \"$TAG\""),
            "creation has no protected-target override: {publish}"
        );
        assert!(
            !publish.contains("--target"),
            "release creation is targetless: {publish}"
        );
        let bodies = step_bodies(&content);
        let smoke = must_some(
            bodies
                .iter()
                .find_map(|(name, body)| (name == "Smoke-test the release").then_some(body)),
            "smoke-test body",
        );
        assert!(
            !smoke.contains("gh release download"),
            "the pre-exposure smoke test uses local build bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn freshness_output_gates_publish_and_the_create_step_rechecks_the_branch() {
        let content = owner_content(&[]);
        let freshness_job = must_some(
            content.split("\n  freshness:\n").nth(1),
            "the freshness job",
        );
        let freshness_job = must_some(
            freshness_job.split("\n  publish:\n").next(),
            "the freshness job body",
        );
        let publish = must_some(content.split("\n  publish:\n").nth(1), "publish job");
        assert!(
            freshness_job.contains("fresh: ${{ steps.freshness.outputs.fresh }}"),
            "freshness is a job output: {freshness_job}"
        );
        assert!(
            freshness_job.contains("gh api \"repos/$REPOSITORY/git/trees/$current?recursive=1\""),
            "the job compares the complete current tree, not commit identity: {freshness_job}"
        );
        assert!(
            freshness_job.contains(".truncated // false")
                && freshness_job.contains("failed to query the current default-branch tree"),
            "tree truncation and query failure fail closed: {freshness_job}"
        );
        assert!(
            publish.contains("needs: [closure, build, freshness]"),
            "publish waits for the freshness decision: {publish}"
        );
        assert!(
            publish.contains("if: needs.closure.outputs.exists != 'true' && needs.freshness.outputs.fresh == 'true'"),
            "a stale freshness output skips the complete publish job: {publish}"
        );
        assert!(
            publish.contains("group: runtime-products-${{ needs.closure.outputs.tag }}"),
            "publish concurrency is keyed by the immutable product tag: {publish}"
        );

        let freshness_body = step_body(&content, "Check the default branch head");
        let create_body = step_body(&content, "Create the release");
        let root = std::env::temp_dir().join(format!(
            "velnor-runtime-freshness-stale-{}",
            crate::s2::unique_suffix()
        ));
        must(
            fs::create_dir_all(&root),
            "create stale flow test directory",
        );
        let output_file = root.join("github-output");
        let log_file = root.join("gh.log");
        let stale_tree_file = root.join("stale-tree.json");
        let stale_blob = "2".repeat(40);
        let current_blob = "1".repeat(40);
        let closure = closure_for_api_tree("Cargo.toml", &current_blob);
        must(
            fs::write(&stale_tree_file, api_tree("Cargo.toml", &stale_blob)),
            "write stale branch tree",
        );
        publisher_gh_stub(
            &root,
            r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
if [[ "$1" == api ]]; then
  if [[ "${GH_FAIL_API:-}" == true ]]; then echo 'API unavailable' >&2; exit 1; fi
  case "$2" in
    repos/*/commits/*) echo "$GH_FIRST_SHA" ;;
    repos/*/git/trees/*) cat "$GH_TREE_FILE" ;;
    *) echo "unexpected API path: $2" >&2; exit 1 ;;
  esac
  exit
fi
exit 1
"#,
        );
        must(fs::write(&output_file, ""), "create stale job output");
        let head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let advanced = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let stale_result = run_rendered_shell(
            &freshness_body,
            &root,
            &[
                (
                    "GITHUB_OUTPUT".to_owned(),
                    output_file.display().to_string(),
                ),
                ("GH_LOG".to_owned(), log_file.display().to_string()),
                ("GH_FIRST_SHA".to_owned(), advanced.to_owned()),
                ("GH_TREE_FILE".to_owned(), stale_tree_file.display().to_string()),
                (
                    "REPOSITORY".to_owned(),
                    workflow_setup_action_repository().to_owned(),
                ),
                ("DEFAULT_BRANCH".to_owned(), "main".to_owned()),
                ("HEAD_SHA".to_owned(), head.to_owned()),
                ("CLOSURE".to_owned(), closure.clone()),
            ],
        );
        assert!(
            stale_result.status.success(),
            "stale run writes a false freshness output: {}",
            String::from_utf8_lossy(&stale_result.stderr)
        );
        let freshness = must(fs::read_to_string(&output_file), "read stale job output");
        assert_eq!(freshness.trim(), "fresh=false");
        let publish_job_runs = rendered_publish_job_runs(publish, "false", freshness.trim());
        assert!(
            !publish_job_runs,
            "the rendered publish condition skips the whole job after its executed freshness step"
        );
        let stale_log = must(fs::read_to_string(&log_file), "read stale gh log");
        assert_eq!(stale_log.lines().count(), 2, "only the commit and tree queries ran");
        assert!(
            stale_log.contains("api repos/"),
            "only the freshness query ran: {stale_log}"
        );

        must(fs::write(&output_file, ""), "reset output for query failure");
        let failed_freshness = run_rendered_shell(
            &freshness_body,
            &root,
            &[
                ("GITHUB_OUTPUT".to_owned(), output_file.display().to_string()),
                ("GH_LOG".to_owned(), log_file.display().to_string()),
                ("GH_FIRST_SHA".to_owned(), advanced.to_owned()),
                ("GH_TREE_FILE".to_owned(), stale_tree_file.display().to_string()),
                ("GH_FAIL_API".to_owned(), "true".to_owned()),
                ("REPOSITORY".to_owned(), workflow_setup_action_repository().to_owned()),
                ("DEFAULT_BRANCH".to_owned(), "main".to_owned()),
                ("CLOSURE".to_owned(), closure.clone()),
            ],
        );
        assert!(!failed_freshness.status.success(), "API errors fail the freshness job");
        assert!(must(fs::read_to_string(&output_file), "read failed freshness output").is_empty());
        let failed_create = run_rendered_shell(
            &create_body,
            &root,
            &[
                ("GH_LOG".to_owned(), log_file.display().to_string()),
                ("GH_FIRST_SHA".to_owned(), advanced.to_owned()),
                ("GH_TREE_FILE".to_owned(), stale_tree_file.display().to_string()),
                ("GH_FAIL_API".to_owned(), "true".to_owned()),
                ("REPOSITORY".to_owned(), workflow_setup_action_repository().to_owned()),
                ("DEFAULT_BRANCH".to_owned(), "main".to_owned()),
                ("HEAD_SHA".to_owned(), head.to_owned()),
                ("CLOSURE".to_owned(), closure.clone()),
                ("TAG".to_owned(), product_tag(&closure)),
            ],
        );
        assert!(
            !failed_create.status.success(),
            "the final API error fails instead of returning stale success"
        );
        let failed_log = must(fs::read_to_string(&log_file), "read failed query log");
        assert!(
            !failed_log.contains("release create "),
            "API failure cannot reach release mutation: {failed_log}"
        );

        let root = std::env::temp_dir().join(format!(
            "velnor-runtime-freshness-race-{}",
            crate::s2::unique_suffix()
        ));
        must(fs::create_dir_all(&root), "create race flow test directory");
        let output_file = root.join("github-output");
        let log_file = root.join("gh.log");
        let count_file = root.join("gh-commit-count");
        let same_tree_file = root.join("same-tree.json");
        let changed_tree_file = root.join("changed-tree.json");
        let changed_blob = "3".repeat(40);
        must(
            fs::write(&same_tree_file, api_tree("Cargo.toml", &current_blob)),
            "write same-closure tree",
        );
        must(
            fs::write(&changed_tree_file, api_tree("Cargo.toml", &changed_blob)),
            "write changed-closure tree",
        );
        publisher_gh_stub(
            &root,
            r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
if [[ "$1" == api ]]; then
  case "$2" in
    repos/*/commits/*)
      count=0
      if [[ -f "$GH_COUNT" ]]; then count="$(cat "$GH_COUNT")"; fi
      count=$((count + 1))
      printf '%s' "$count" > "$GH_COUNT"
      if [[ "$count" -le 2 ]]; then echo "$GH_FIRST_SHA"; else echo "$GH_LATER_SHA"; fi
      ;;
    repos/*/git/trees/*)
      if [[ "$2" == *"$GH_FIRST_SHA"* ]]; then cat "$GH_FIRST_TREE"; else cat "$GH_LATER_TREE"; fi
      ;;
    *) echo "unexpected API path: $2" >&2; exit 1 ;;
  esac
  exit
fi
exit 1
"#,
        );
        must(fs::write(&output_file, ""), "create race job output");
        let fresh_result = run_rendered_shell(
            &freshness_body,
            &root,
            &[
                (
                    "GITHUB_OUTPUT".to_owned(),
                    output_file.display().to_string(),
                ),
                ("GH_LOG".to_owned(), log_file.display().to_string()),
                ("GH_COUNT".to_owned(), count_file.display().to_string()),
                ("GH_FIRST_SHA".to_owned(), advanced.to_owned()),
                ("GH_LATER_SHA".to_owned(), "c".repeat(40)),
                ("GH_FIRST_TREE".to_owned(), same_tree_file.display().to_string()),
                ("GH_LATER_TREE".to_owned(), changed_tree_file.display().to_string()),
                (
                    "REPOSITORY".to_owned(),
                    workflow_setup_action_repository().to_owned(),
                ),
                ("DEFAULT_BRANCH".to_owned(), "main".to_owned()),
                ("HEAD_SHA".to_owned(), head.to_owned()),
                ("CLOSURE".to_owned(), closure.clone()),
            ],
        );
        assert!(fresh_result.status.success());
        assert_eq!(
            must(fs::read_to_string(&output_file), "read fresh output").trim(),
            "fresh=true"
        );
        let freshness = must(fs::read_to_string(&output_file), "read fresh output");
        assert!(
            rendered_publish_job_runs(publish, "false", freshness.trim()),
            "the rendered job enters publish only for a fresh, missing product"
        );
        assert!(
            !rendered_publish_job_runs(publish, "true", freshness.trim()),
            "the rendered job skips publish when the immutable tag already exists"
        );
        let create_result = if rendered_publish_job_runs(publish, "false", freshness.trim()) {
            run_rendered_shell(
                &create_body,
                &root,
                &[
                    ("GH_LOG".to_owned(), log_file.display().to_string()),
                    ("GH_COUNT".to_owned(), count_file.display().to_string()),
                    ("GH_FIRST_SHA".to_owned(), advanced.to_owned()),
                    ("GH_LATER_SHA".to_owned(), "c".repeat(40)),
                    ("GH_FIRST_TREE".to_owned(), same_tree_file.display().to_string()),
                    ("GH_LATER_TREE".to_owned(), changed_tree_file.display().to_string()),
                    (
                        "REPOSITORY".to_owned(),
                        workflow_setup_action_repository().to_owned(),
                    ),
                    ("DEFAULT_BRANCH".to_owned(), "main".to_owned()),
                    ("HEAD_SHA".to_owned(), head.to_owned()),
                    ("CLOSURE".to_owned(), closure.clone()),
                    ("TAG".to_owned(), product_tag(&closure)),
                ],
            )
        } else {
            panic!("fresh rendered workflow job should enter publish");
        };
        assert!(
            create_result.status.success(),
            "the final freshness race exits without publishing: {}",
            String::from_utf8_lossy(&create_result.stderr)
        );
        let race_log = must(fs::read_to_string(&log_file), "read final freshness log");
        assert_eq!(
            race_log.lines().filter(|line| line.starts_with("api ")).count(),
            6,
            "the workflow gate and two publish checks each query commit and tree"
        );
        assert_eq!(
            race_log
                .lines()
                .filter(|line| line.starts_with("release create "))
                .count(),
            1,
            "the later closure change prevents a second create"
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_product_is_a_hit_only_after_attested_assets_and_identity_verify() {
        let content = owner_content(&[]);
        let body = step_body(&content, "Check for an existing product");
        let blob = "e".repeat(40);
        let closure = closure_for_api_tree("Cargo.toml", &blob);
        let revision = FIXTURE_REVISION;

        for mode in [
            "valid",
            "incomplete",
            "tampered",
            "wrong-source-digest",
            "query-error",
            "absent",
        ] {
            let root = std::env::temp_dir().join(format!(
                "velnor-runtime-existing-product-{mode}-{}",
                crate::s2::unique_suffix()
            ));
            must(fs::create_dir_all(&root), "create existing-product test directory");
            let fixture = release_fixture(&root, &closure, revision, mode == "tampered");
            let output = root.join("github-output");
            let log = root.join("gh.log");
            must(fs::write(&output, ""), "create existing-product output");
            publisher_gh_stub(
                &root,
                r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
case "$1" in
  api)
    [[ "$2" == "repos/$REPOSITORY/releases/tags/$TAG" ]] || { echo "unexpected API path: $2" >&2; exit 1; }
    if [[ "$GH_MODE" == query-error ]]; then echo 'HTTP 403: resource forbidden' >&2; exit 1; fi
    if [[ "$GH_MODE" == absent ]]; then echo 'Not Found (HTTP 404)' >&2; exit 1; fi
    echo '{"tag_name":"$TAG"}'
    ;;
  release)
    [[ "$2" == download ]] || exit 1
    if [[ "$GH_MODE" == incomplete ]]; then echo 'asset not uploaded yet' >&2; exit 1; fi
    destination=''
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == --dir ]]; then destination="$2"; shift 2; else shift; fi
    done
    cp -R "$GH_FIXTURE"/. "$destination"/
    ;;
  attestation)
    [[ "$2" == verify ]] || exit 1
    if [[ " $* " == *" --source-ref refs/heads/main --source-digest $GH_EXPECTED_REVISION "* ]]; then
      [[ "$GH_MODE" != wrong-source-digest ]] || { echo 'attestation source digest mismatch' >&2; exit 1; }
      exit 0
    fi
    echo "attestation source binding mismatch: $*" >&2
    exit 1
    ;;
  *) exit 1 ;;
esac
"#,
            );
            let expected_revision = if mode == "wrong-source-digest" {
                "f".repeat(40)
            } else {
                revision.to_owned()
            };
            let result = run_rendered_shell(
                &body,
                &root,
                &[
                    ("GITHUB_OUTPUT".to_owned(), output.display().to_string()),
                    ("GH_LOG".to_owned(), log.display().to_string()),
                    ("GH_FIXTURE".to_owned(), fixture.display().to_string()),
                    ("GH_EXPECTED_REVISION".to_owned(), expected_revision),
                    ("GH_MODE".to_owned(), mode.to_owned()),
                    ("REPOSITORY".to_owned(), workflow_setup_action_repository().to_owned()),
                    ("TAG".to_owned(), product_tag(&closure)),
                    ("CLOSURE".to_owned(), closure.clone()),
                ],
            );
            let result_output = must(fs::read_to_string(&output), "read existing-product output");
            if mode == "absent" {
                assert!(
                    result.status.success(),
                    "404 means a new closure product may be built: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
                assert_eq!(result_output.trim(), "exists=false");
                let logged = must(fs::read_to_string(&log), "read absent-product log");
                assert_eq!(logged.lines().count(), 1, "404 cannot download assets");
            } else if mode == "valid" {
                assert!(
                    result.status.success(),
                    "complete attested product is accepted: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
                assert_eq!(result_output.trim(), "exists=true");
                let logged = must(fs::read_to_string(&log), "read existing-product log");
                assert_eq!(
                    logged.lines().filter(|line| line.starts_with("attestation verify ")).count(),
                    4,
                    "the manifest and all three assets bind provenance to manifest revision"
                );
            } else {
                assert!(
                    !result.status.success(),
                    "{mode} existing release must fail closed"
                );
                assert!(
                    result_output.is_empty(),
                    "{mode} release cannot become a cache hit"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn release_create_conflicts_converge_only_on_an_authenticated_matching_product() {
        let content = owner_content(&[]);
        let create_body = step_body(&content, "Create the release");
        let blob = "c".repeat(40);
        let closure = closure_for_api_tree("Cargo.toml", &blob);
        let revision = FIXTURE_REVISION;

        for tamper in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "velnor-runtime-release-conflict-{tamper}-{}",
                crate::s2::unique_suffix()
            ));
            must(fs::create_dir_all(&root), "create conflict test directory");
            let fixture = release_fixture(&root, &closure, revision, tamper);
            let created = root.join("release-created");
            let log = root.join("gh.log");
            let tree_file = root.join("default-tree.json");
            must(
                fs::write(&tree_file, api_tree("Cargo.toml", &blob)),
                "write matching default-branch tree",
            );
            publisher_gh_stub(
                &root,
                r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
case "$1 $2" in
  "api "*)
    case "$2" in
      repos/*/commits/*) echo "$GH_REMOTE_SHA" ;;
      repos/*/git/trees/*) cat "$GH_TREE_FILE" ;;
      "repos/$REPOSITORY/releases/tags/$TAG")
        if [[ -f "$GH_CREATED" ]]; then
          printf '{"tag_name":"%s"}\n' "$TAG"
        else
          echo 'Not Found (HTTP 404)' >&2
          exit 1
        fi
        ;;
      *) exit 1 ;;
    esac
    ;;
  "release create"*) touch "$GH_CREATED"; exit 1 ;;
  "release download"*)
    destination=''
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == --dir ]]; then destination="$2"; shift 2; else shift; fi
    done
    cp -R "$GH_FIXTURE"/. "$destination"/
    ;;
  "attestation verify"*)
    if [[ " $* " == *" --source-digest $GH_EXPECTED_REVISION "* ]]; then exit 0; fi
    echo "attestation source digest mismatch: $*" >&2
    exit 1
    ;;
  *) exit 1 ;;
esac
"#,
            );
            let result = run_rendered_shell(
                &create_body,
                &root,
                &[
                    ("GH_LOG".to_owned(), log.display().to_string()),
                    ("GH_REMOTE_SHA".to_owned(), revision.to_owned()),
                    ("GH_TREE_FILE".to_owned(), tree_file.display().to_string()),
                    ("GH_CREATED".to_owned(), created.display().to_string()),
                    ("GH_FIXTURE".to_owned(), fixture.display().to_string()),
                    ("GH_EXPECTED_REVISION".to_owned(), revision.to_owned()),
                    (
                        "REPOSITORY".to_owned(),
                        workflow_setup_action_repository().to_owned(),
                    ),
                    ("DEFAULT_BRANCH".to_owned(), "main".to_owned()),
                    ("HEAD_SHA".to_owned(), revision.to_owned()),
                    ("CLOSURE".to_owned(), closure.clone()),
                    ("TAG".to_owned(), product_tag(&closure)),
                ],
            );
            let logged = must(fs::read_to_string(&log), "read create conflict log");
            assert_eq!(
                logged
                    .lines()
                    .filter(|line| line.starts_with("release create "))
                    .count(),
                1,
                "the concurrent tag conflict gets one create attempt"
            );
            let create_line = must_some(
                logged
                    .lines()
                    .find(|line| line.starts_with("release create ")),
                "targetless release create command",
            );
            assert!(
                !create_line.contains("--target"),
                "the create uses GitHub's default-branch tag target: {create_line}"
            );
            if tamper {
                assert!(
                    !result.status.success(),
                    "mismatched existing bytes fail convergence"
                );
                assert!(
                    String::from_utf8_lossy(&result.stderr).contains("digest mismatch"),
                    "the existing asset is checked against signed manifest metadata: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
            } else {
                assert!(
                    result.status.success(),
                    "same-tag conflict converges on the attested product: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
                assert_eq!(
                    logged
                        .lines()
                        .filter(|line| line.starts_with("release download "))
                        .count(),
                    1,
                    "the competing product is downloaded and verified"
                );
            }
            assert!(
                created.exists(),
                "the stub exposes the raced release after create fails"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn existing_release_convergence_waits_for_assets_and_rejects_bad_attestation() {
        let content = owner_content(&[]);
        let create_body = step_body(&content, "Create the release");
        let blob = "a".repeat(40);
        let closure = closure_for_api_tree("Cargo.toml", &blob);
        let revision = FIXTURE_REVISION;

        for mode in ["delayed-assets", "wrong-source-digest"] {
            let root = std::env::temp_dir().join(format!(
                "velnor-runtime-release-convergence-{mode}-{}",
                crate::s2::unique_suffix()
            ));
            must(fs::create_dir_all(&root), "create release-convergence directory");
            let fixture = release_fixture(&root, &closure, revision, false);
            let tree_file = root.join("default-tree.json");
            let log = root.join("gh.log");
            let download_count = root.join("download-count");
            must(
                fs::write(&tree_file, api_tree("Cargo.toml", &blob)),
                "write matching source tree",
            );
            publisher_gh_stub(
                &root,
                r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
case "$1 $2" in
  "api "*)
    case "$2" in
      repos/*/commits/*) echo "$GH_REMOTE_SHA" ;;
      repos/*/git/trees/*) cat "$GH_TREE_FILE" ;;
      "repos/$REPOSITORY/releases/tags/$TAG") printf '{"tag_name":"%s"}\n' "$TAG" ;;
      *) exit 1 ;;
    esac
    ;;
  "release download"*)
    count=0
    if [[ -f "$GH_DOWNLOAD_COUNT" ]]; then count="$(cat "$GH_DOWNLOAD_COUNT")"; fi
    count=$((count + 1))
    printf '%s' "$count" > "$GH_DOWNLOAD_COUNT"
    if [[ "$GH_MODE" == delayed-assets && "$count" -lt 3 ]]; then
      echo 'release assets are still uploading' >&2
      exit 1
    fi
    destination=''
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == --dir ]]; then destination="$2"; shift 2; else shift; fi
    done
    cp -R "$GH_FIXTURE"/. "$destination"/
    ;;
  "release create"*) echo 'existing immutable tag must not be recreated' >&2; exit 1 ;;
  "attestation verify"*)
    if [[ " $* " != *" --source-ref refs/heads/main --source-digest $GH_EXPECTED_REVISION "* ]]; then
      echo "source digest mismatch: $*" >&2
      exit 1
    fi
    [[ "$GH_MODE" != wrong-source-digest ]] || { echo 'wrong source digest in provenance' >&2; exit 1; }
    ;;
  *) exit 1 ;;
esac
"#,
            );
            executable_script(
                &root.join("bin/sleep"),
                "#!/bin/sh\necho \"sleep $*\" >> \"$GH_LOG\"\nexit 0\n",
            );
            let result = run_rendered_shell(
                &create_body,
                &root,
                &[
                    ("GH_LOG".to_owned(), log.display().to_string()),
                    ("GH_REMOTE_SHA".to_owned(), revision.to_owned()),
                    ("GH_TREE_FILE".to_owned(), tree_file.display().to_string()),
                    ("GH_FIXTURE".to_owned(), fixture.display().to_string()),
                    ("GH_DOWNLOAD_COUNT".to_owned(), download_count.display().to_string()),
                    ("GH_EXPECTED_REVISION".to_owned(), revision.to_owned()),
                    ("GH_MODE".to_owned(), mode.to_owned()),
                    ("REPOSITORY".to_owned(), workflow_setup_action_repository().to_owned()),
                    ("DEFAULT_BRANCH".to_owned(), "main".to_owned()),
                    ("HEAD_SHA".to_owned(), revision.to_owned()),
                    ("CLOSURE".to_owned(), closure.clone()),
                    ("TAG".to_owned(), product_tag(&closure)),
                ],
            );
            let logged = must(fs::read_to_string(&log), "read release-convergence log");
            if mode == "delayed-assets" {
                assert!(
                    result.status.success(),
                    "an existing product converges after delayed assets: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
                assert_eq!(
                    must(fs::read_to_string(&download_count), "read asset download count").trim(),
                    "3"
                );
                assert_eq!(
                    logged.lines().filter(|line| line.starts_with("release create ")).count(),
                    0,
                    "an existing incomplete tag is polled instead of recreated"
                );
                assert_eq!(
                    logged.lines().filter(|line| line.starts_with("attestation verify ")).count(),
                    4,
                    "the completed manifest and every platform asset bind to manifest revision"
                );
            } else {
                assert!(
                    !result.status.success(),
                    "an invalid source attestation never converges"
                );
                assert_eq!(
                    logged.lines().filter(|line| line.starts_with("release create ")).count(),
                    0,
                    "an untrusted tag is not replaced or recreated"
                );
                assert!(
                    String::from_utf8_lossy(&result.stderr)
                        .contains("did not converge after four bounded create attempts"),
                    "verification stays bounded and fails closed: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn nonconverging_release_create_is_bounded_to_four_attempts() {
        let content = owner_content(&[]);
        let create_body = step_body(&content, "Create the release");
        let blob = "d".repeat(40);
        let closure = closure_for_api_tree("Cargo.toml", &blob);
        let root = std::env::temp_dir().join(format!(
            "velnor-runtime-release-bounded-{}",
            crate::s2::unique_suffix()
        ));
        must(
            fs::create_dir_all(&root),
            "create bounded retry test directory",
        );
        let log = root.join("gh.log");
        let tree_file = root.join("default-tree.json");
        must(
            fs::write(&tree_file, api_tree("Cargo.toml", &blob)),
            "write matching default-branch tree",
        );
        let head = FIXTURE_REVISION;
        publisher_gh_stub(
            &root,
            r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
case "$1 $2" in
  "api "*)
    case "$2" in
      repos/*/commits/*) echo "$GH_REMOTE_SHA" ;;
      repos/*/git/trees/*) cat "$GH_TREE_FILE" ;;
      "repos/$REPOSITORY/releases/tags/$TAG") echo 'Not Found (HTTP 404)' >&2; exit 1 ;;
      *) exit 1 ;;
    esac
    ;;
  "release create"*) exit 1 ;;
  *) exit 1 ;;
esac
"#,
        );
        executable_script(
            &root.join("bin/sleep"),
            "#!/bin/sh\necho \"sleep $*\" >> \"$GH_LOG\"\nexit 0\n",
        );
        let result = run_rendered_shell(
            &create_body,
            &root,
            &[
                ("GH_LOG".to_owned(), log.display().to_string()),
                ("GH_REMOTE_SHA".to_owned(), head.to_owned()),
                ("GH_TREE_FILE".to_owned(), tree_file.display().to_string()),
                (
                    "REPOSITORY".to_owned(),
                    workflow_setup_action_repository().to_owned(),
                ),
                ("DEFAULT_BRANCH".to_owned(), "main".to_owned()),
                ("HEAD_SHA".to_owned(), head.to_owned()),
                ("CLOSURE".to_owned(), closure.clone()),
                ("TAG".to_owned(), product_tag(&closure)),
            ],
        );
        assert!(
            !result.status.success(),
            "missing conflict release eventually fails closed"
        );
        let logged = must(fs::read_to_string(&log), "read bounded retry log");
        assert_eq!(
            logged
                .lines()
                .filter(|line| line.starts_with("release create "))
                .count(),
            4,
            "create retries have a hard bound"
        );
        assert_eq!(
            logged
                .lines()
                .filter(|line| line.starts_with("sleep "))
                .count(),
            3,
            "only the three gaps between four attempts are delayed"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("after four bounded create attempts"),
            "the final failure explains the bound: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn release_lookup_errors_fail_closed_without_create() {
        let content = owner_content(&[]);
        let create_body = step_body(&content, "Create the release");
        let blob = "f".repeat(40);
        let closure = closure_for_api_tree("Cargo.toml", &blob);
        let root = std::env::temp_dir().join(format!(
            "velnor-runtime-release-query-error-{}",
            crate::s2::unique_suffix()
        ));
        must(fs::create_dir_all(&root), "create release-query test directory");
        let tree_file = root.join("default-tree.json");
        let log = root.join("gh.log");
        must(
            fs::write(&tree_file, api_tree("Cargo.toml", &blob)),
            "write matching source tree",
        );
        publisher_gh_stub(
            &root,
            r#"#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_LOG"
if [[ "$1" == api ]]; then
  case "$2" in
    repos/*/commits/*) echo "$GH_REMOTE_SHA" ;;
    repos/*/git/trees/*) cat "$GH_TREE_FILE" ;;
    "repos/$REPOSITORY/releases/tags/$TAG") echo 'HTTP 403: resource forbidden' >&2; exit 1 ;;
    *) exit 1 ;;
  esac
  exit
fi
if [[ "$1 $2" == "release create"* ]]; then
  echo 'release creation must not run after an indeterminate lookup' >&2
  exit 1
fi
exit 1
"#,
        );
        let result = run_rendered_shell(
            &create_body,
            &root,
            &[
                ("GH_LOG".to_owned(), log.display().to_string()),
                ("GH_REMOTE_SHA".to_owned(), FIXTURE_REVISION.to_owned()),
                ("GH_TREE_FILE".to_owned(), tree_file.display().to_string()),
                (
                    "REPOSITORY".to_owned(),
                    workflow_setup_action_repository().to_owned(),
                ),
                ("DEFAULT_BRANCH".to_owned(), "main".to_owned()),
                ("HEAD_SHA".to_owned(), FIXTURE_REVISION.to_owned()),
                ("CLOSURE".to_owned(), closure.clone()),
                ("TAG".to_owned(), product_tag(&closure)),
            ],
        );
        assert!(!result.status.success(), "403 is not a missing release");
        let logged = must(fs::read_to_string(&log), "read query-error log");
        assert!(
            !logged.lines().any(|line| line.starts_with("release create ")),
            "indeterminate release lookup cannot reach create: {logged}"
        );
    }

    #[test]
    fn smoke_test_pins_the_producer_ref() {
        let content = owner_content(&[]);
        let smoke = must_some(
            step_bodies(&content)
                .into_iter()
                .find_map(|(name, body)| (name == "Smoke-test the release").then_some(body)),
            "producer smoke-test body",
        );
        assert_eq!(
            smoke.matches("--source-ref refs/heads/main").count(),
            2,
            "the smoke test pins the default-branch ref on the asset and the manifest"
        );
        let mut config = owner_config(&[]);
        config.default_branch = "trunk".to_owned();
        let error = runtime_products_content(&config).expect_err("source ref is fixed to main");
        assert!(error.to_string().contains("refs/heads/main"), "{error}");
    }

    /// Every `run: |` shell body in the rendered producer, keyed by step
    /// name and de-indented. The template is string-built, so these bodies
    /// are what actually executes in CI: parsing and running them here
    /// catches template escaping bugs that string assertions cannot see.
    fn step_bodies(content: &str) -> Vec<(String, String)> {
        let mut bodies = Vec::new();
        let mut name = String::new();
        let mut current: Option<Vec<String>> = None;
        for line in content.lines() {
            if let Some(body) = current.as_mut() {
                if line.trim().is_empty() || line.starts_with("          ") {
                    body.push(line.strip_prefix("          ").unwrap_or("").to_owned());
                    continue;
                }
                bodies.push((std::mem::take(&mut name), body.join("\n")));
                current = None;
            }
            if let Some(step) = line.strip_prefix("      - name: ") {
                name = step.to_owned();
            } else if line.trim() == "run: |" {
                current = Some(Vec::new());
            }
        }
        if let Some(body) = current {
            bodies.push((name, body.join("\n")));
        }
        bodies
    }

    fn step_body(content: &str, step_name: &str) -> String {
        must_some(
            step_bodies(content)
                .into_iter()
                .find_map(|(name, body)| (name == step_name).then_some(body)),
            &format!("the `{step_name}` rendered shell body"),
        )
    }

    /// Execute the generated publish-job gate against synthetic upstream
    /// outputs. Keep the evaluator deliberately narrow: any generator change
    /// to the emitted expression must update this test before it can silently
    /// stop gating stale or already-published products.
    fn rendered_publish_job_runs(publish_job: &str, exists: &str, fresh: &str) -> bool {
        let condition = publish_job
            .lines()
            .find_map(|line| line.trim().strip_prefix("if: "))
            .expect("rendered publish job condition");
        assert_eq!(
            condition,
            "needs.closure.outputs.exists != 'true' && needs.freshness.outputs.fresh == 'true'",
            "test the exact workflow-level publish condition"
        );
        exists != "true" && fresh == "fresh=true"
    }

    #[cfg(unix)]
    fn executable_script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        must(fs::write(path, body), "write executable test script");
        must(
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)),
            "make test script executable",
        );
    }

    #[cfg(unix)]
    fn run_rendered_shell(
        body: &str,
        root: &Path,
        variables: &[(String, String)],
    ) -> std::process::Output {
        let script = root.join("rendered-step.sh");
        must(fs::write(&script, body), "write rendered shell body");
        let path = format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = std::process::Command::new("bash");
        command
            .args(["-euo", "pipefail"])
            .arg(&script)
            .current_dir(root)
            .env("PATH", path)
            .env("TMPDIR", root);
        for (key, value) in variables {
            command.env(key, value);
        }
        must(command.output(), "execute rendered shell body")
    }

    #[cfg(unix)]
    fn publisher_gh_stub(root: &Path, body: &str) {
        let bin = root.join("bin");
        must(fs::create_dir_all(&bin), "create command stubs directory");
        executable_script(&bin.join("gh"), body);
    }

    #[cfg(unix)]
    fn release_fixture(root: &Path, closure: &str, revision: &str, tamper: bool) -> PathBuf {
        let fixture = root.join("release-fixture");
        must(
            fs::create_dir_all(&fixture),
            "create existing release fixture",
        );
        let mut products = serde_json::Map::new();
        for platform in platforms() {
            let asset = platform.asset_name();
            let content = if platform.key() == "Linux-X64" {
                format!(
                    "#!/bin/sh\ncase \"$1\" in --closure) echo {closure};; --revision) echo {revision};; *) exit 3;; esac\n"
                )
            } else {
                format!("test-binary-{}", platform.key())
            };
            must(
                fs::write(fixture.join(&asset), &content),
                "write release asset",
            );
            products.insert(
                platform.key(),
                serde_json::json!({"binary": digest_of(&content), "asset": asset}),
            );
        }
        let manifest = serde_json::json!({
            "closure": closure,
            "revision": revision,
            "profile": "release",
            "features": "",
            "products": products,
        });
        must(
            fs::write(
                fixture.join("manifest.json"),
                serde_json::to_vec(&manifest).expect("serialize manifest"),
            ),
            "write existing release manifest",
        );
        if tamper {
            let linux = platforms()
                .into_iter()
                .find(|platform| platform.key() == "Linux-X64")
                .expect("Linux x64 product");
            must(
                fs::write(fixture.join(linux.asset()), "tampered binary"),
                "tamper existing asset after manifest digest",
            );
        }
        fixture
    }

    fn api_tree(path: &str, sha: &str) -> String {
        serde_json::json!({
            "truncated": false,
            "tree": [{"mode": "100644", "type": "blob", "sha": sha, "path": path}],
        })
        .to_string()
    }

    fn closure_for_api_tree(path: &str, sha: &str) -> String {
        crate::s2::closure::canonical_digest(
            &[format!("100644 blob {sha}\t{path}")],
            CI_FEATURES,
            PROFILE_RELEASE,
        )
    }

    #[cfg(unix)]
    #[test]
    fn rendered_shell_parses() {
        let content = owner_content(&[]);
        let bodies = step_bodies(&content);
        for owned in [
            "Prove the default-branch ref",
            "Resolve source closure",
            "Check for an existing product",
            "Prove a hermetic build environment",
            "Prove the product closure",
            "Assemble and verify the release",
            "Smoke-test the release",
            "Create the release",
        ] {
            assert!(
                bodies
                    .iter()
                    .any(|(name, body)| name == owned && !body.is_empty()),
                "the `{owned}` shell body is extracted for parsing"
            );
        }
        for (index, (name, body)) in bodies.iter().enumerate() {
            let path = std::env::temp_dir().join(format!(
                "velnor-workflow-producer-shell-{index}-{}",
                crate::s2::unique_suffix()
            ));
            must(fs::write(&path, body), "write the shell body");
            let output = must(
                std::process::Command::new("bash")
                    .arg("-n")
                    .arg(&path)
                    .output(),
                "parse the shell body",
            );
            let _ = fs::remove_file(&path);
            assert!(
                output.status.success(),
                "the `{name}` shell body parses: {body}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn default_branch_guard_accepts_only_the_default_ref() {
        let content = owner_content(&[]);
        let bodies = step_bodies(&content);
        let guard = must_some(
            bodies
                .iter()
                .find_map(|(name, body)| (name == "Prove the default-branch ref").then_some(body)),
            "the rendered guard body",
        );
        for (reference, accepted) in [
            ("refs/heads/main", true),
            ("refs/heads/trunk", false),
            ("refs/tags/v1.2.3", false),
            ("", false),
        ] {
            let path = std::env::temp_dir().join(format!(
                "velnor-workflow-producer-guard-{}",
                crate::s2::unique_suffix()
            ));
            must(fs::write(&path, guard), "write the guard body");
            let output = must(
                std::process::Command::new("bash")
                    .arg(&path)
                    .env("REF", reference)
                    .output(),
                "run the guard body",
            );
            let _ = fs::remove_file(&path);
            assert_eq!(
                output.status.success(),
                accepted,
                "ref `{reference}` is {}: {}",
                if accepted { "accepted" } else { "rejected" },
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    /// The rendered bytes, pinned to the digest of the reviewed render. The
    /// digest is the expectation, not a second call into the same code, so a
    /// renderer change shows up here and has to be carried into the pin
    /// deliberately; the structural assertions above say what the pinned
    /// bytes are for.
    #[test]
    fn rendered_bytes_are_pinned() {
        const PINNED: &str = "68cd7bc08ec619c318e9325ecd6163228592eacf29dac7f29774536cd8708bc4";
        let content = owner_content(&["maintenance.yml"]);
        let digest = digest_of(&content);
        assert_eq!(digest, PINNED, "rendered producer bytes changed");
    }
}
