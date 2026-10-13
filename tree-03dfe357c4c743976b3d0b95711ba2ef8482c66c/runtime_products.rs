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

use std::fmt::Write as _;

use super::{Args, Primitive, RenderCtx, Rendered};
use crate::closure::{
    product_tag, CI_FEATURES, CLOSURE_PATHS, CLOSURE_VERSION, PRODUCT_TAG_PREFIX, PROFILE_RELEASE,
};
use crate::{
    config_rust_toolchain, workflow_setup_action_repository, yaml_scalar, ActionPin,
    GeneratorError, ProjectConfig, RustToolchain, GENERATED_HEADER, HOSTED_WORKFLOW_RUNTIME_HOME,
};

/// The workflow file the producer renders into. Consumers pin this path in
/// their attestation check, so the name is load-bearing: a rename breaks
/// every verifier until the setup action ships the new pin with it.
pub(crate) const RUNTIME_PRODUCTS_FILE: &str = "ci-runtime-products.yml";

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

/// The Linux ARM64 builder. The generation config names no ARM lane (its Linux
/// lane is `github_runner`, its Apple lane is `macos_runner`), so the label is
/// fixed to the hosted ARM runner the release family's guest matrix already
/// builds on. The setup action's cache key and the Velnor provisioner both
/// address `Linux-ARM64`, so the product must exist whatever the config says.
const LINUX_ARM64_RUNNER: &str = "ubuntu-24.04-arm";

/// The manifest acceptance filter, exactly as the setup action evaluates it:
/// full closure, release profile, empty features, a 64-hex digest for the
/// platform, and the asset name the platform expects. The publish job
/// evaluates this same filter over the assembled manifest, so a manifest no
/// consumer would accept never reaches a release.
const MANIFEST_ACCEPT_FILTER: &str = ".closure == $closure and .profile == \"release\" and .features == \"\" and (.products[$platform].binary | test(\"^[0-9a-f]{64}$\")) and .products[$platform].asset == $asset";

/// The isolated Cargo home the producer steps build under, as a rendered
/// step-level `env:` value. The `runner` context is unavailable in job-level
/// `env:` (GitHub rejects the workflow at compile time), so each step that
/// needs isolation carries this as step-level `env:`, which does provide
/// `runner`.
const PRODUCER_CARGO_HOME_VALUE: &str = "${{ runner.temp }}/velnor-producer-cargo-home";

/// One natively built consumer platform: the `RUNNER_OS`-`RUNNER_ARCH` pair
/// the setup action resolves its asset name from, and the runner that builds
/// it. Linux X64 serves the `github_runner` lane and the Velnor hosts, Linux
/// ARM64 serves ARM consumers of both, and macOS ARM64 serves the Apple lane
/// (`macos_runner` defaults to an ARM label, and no lane in the repository
/// selects an Intel Mac, so there is no macOS X64 consumer to build for).
struct Platform {
    os: &'static str,
    arch: &'static str,
    runner: String,
}

impl Platform {
    /// The `RUNNER_OS`-`RUNNER_ARCH` platform key: the manifest key and the
    /// asset suffix in one.
    fn key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }

    /// The release asset name the setup action downloads for this platform.
    fn asset(&self) -> String {
        format!("velnor-workflow-{}", self.key())
    }
}

/// The consumer platforms, in manifest order. The Linux X64 and macOS ARM64
/// builders follow the owner's configured lanes; Linux ARM64 has no
/// configured lane and builds on the fixed hosted ARM runner.
fn platforms(config: &ProjectConfig) -> [Platform; 3] {
    [
        Platform {
            os: "Linux",
            arch: "X64",
            runner: config.github_runner.clone(),
        },
        Platform {
            os: "Linux",
            arch: "ARM64",
            runner: LINUX_ARM64_RUNNER.to_owned(),
        },
        Platform {
            os: "macOS",
            arch: "ARM64",
            runner: config.macos_runner.clone(),
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
pub(crate) fn runtime_products_content(config: &ProjectConfig) -> Option<String> {
    if config.repository != workflow_setup_action_repository() {
        return None;
    }
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
    let platforms = platforms(config);
    let mut matrix = String::new();
    for platform in &platforms {
        let _ = writeln!(
            matrix,
            "          - os: {}\n            arch: {}\n            runner: {}",
            platform.os,
            platform.arch,
            yaml_scalar(&platform.runner),
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
            asset = platform.asset(),
            var = platform_variable(platform),
        );
    }
    let manifest_program = format!(
        "{{closure: $closure, profile: \"release\", features: \"\", products: {{{manifest_products}}}}}"
    );
    let mut manifest_digests = String::new();
    for platform in &platforms {
        let _ = writeln!(
            manifest_digests,
            "            --arg {var} \"$(cat dist/{asset}.sha256)\" \\",
            var = platform_variable(platform),
            asset = platform.asset(),
        );
    }
    let platform_list = platforms
        .iter()
        .map(Platform::key)
        .collect::<Vec<_>>()
        .join(" ");
    let release_assets = platforms
        .iter()
        .map(|platform| format!("dist/{}", platform.asset()))
        .collect::<Vec<_>>()
        .join(" ");
    let closure_footer = format!(
        "closure-version:{CLOSURE_VERSION}\\nfeatures:{CI_FEATURES}\\nprofile:{PROFILE_RELEASE}\\n"
    );
    Some(format!(
        r#"{header}# Stage-0 runtime products: the immutable `velnor-workflow` binaries every
# consumer lane installs through the setup action instead of compiling.
#
# One release per source closure: tags name the closure's product tag (worked
# example for the zero closure: `{example_tag}`), so an unrelated monorepo
# change never rebuilds the runtime. Each platform job builds natively with
# `cargo build --locked --no-default-features --release`, proves the binary's
# own `--closure` report equals the tag closure, attests the asset, and
# uploads it; the publish job proves transport integrity, assembles
# `manifest.json`, attests it, smoke-tests the exact consumer flow
# (attestation, manifest, digest, self-report) against those same bytes, and
# only then creates the release. Verification precedes exposure: a product no
# consumer would accept never reaches a release.
#
# The workflow never overwrites: when the tag already exists the run skips,
# and the publish job re-checks immediately before creating the release.
name: Velnor workflow runtime products
run-name: Runtime products · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}

on:
  push:
    branches: [{default_branch}]
  workflow_dispatch:

# Same-ref runs serialize so two pushes can never race on one release; a
# publish in flight is never cancelled.
concurrency:
  group: runtime-products-${{{{ github.ref }}}}
  cancel-in-progress: false

permissions:
  contents: read

jobs:
  closure:
    name: Resolve runtime closure
    runs-on: {closure_runner}
    timeout-minutes: 10
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
        run: |
          set -euo pipefail
          if gh release view "$TAG" --repo {repository} >/dev/null 2>&1; then
            echo "exists=true" >> "$GITHUB_OUTPUT"
          else
            echo "exists=false" >> "$GITHUB_OUTPUT"
          fi

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

  publish:
    name: Publish runtime products
    needs: [closure, build]
    if: needs.closure.outputs.exists != 'true'
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
          jq -n \
            --arg closure "$CLOSURE" \
{manifest_digests}            '{manifest_products}' \
            > dist/manifest.json
          for platform in {platform_list}; do
            jq -e --arg closure "$CLOSURE" --arg platform "$platform" --arg asset "velnor-workflow-$platform" \
              '{accept_filter}' dist/manifest.json >/dev/null
          done
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
          asset="velnor-workflow-${{RUNNER_OS}}-${{RUNNER_ARCH}}"
          gh attestation verify "dist/$asset" --owner {owner} --signer-workflow {repository}/.github/workflows/{workflow_file} --source-ref {branch_ref}
          gh attestation verify "dist/manifest.json" --owner {owner} --signer-workflow {repository}/.github/workflows/{workflow_file} --source-ref {branch_ref}
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
      - name: Create the release
        shell: bash
        env:
          GH_TOKEN: ${{{{ github.token }}}}
          CLOSURE: ${{{{ needs.closure.outputs.closure }}}}
          TAG: ${{{{ needs.closure.outputs.tag }}}}
          HEAD_SHA: ${{{{ needs.closure.outputs.head-sha }}}}
        run: |
          set -euo pipefail
          # Every verification above passed on exactly these bytes; the
          # re-check immediately before creating keeps the never-overwrite
          # promise against a concurrent run that published first.
          if gh release view "$TAG" --repo {repository} >/dev/null 2>&1; then
            echo "::notice::release $TAG already exists; leaving it untouched"
            exit 0
          fi
          gh release create "$TAG" --repo {repository} --target "$HEAD_SHA" --title "$TAG" \
            --notes "Immutable velnor-workflow runtime product for source closure $CLOSURE (built from $HEAD_SHA). Consumers verify the manifest digest, the binary self-report, and the build provenance attestation." \
            {release_assets} dist/manifest.json
"#,
        header = GENERATED_HEADER,
        example_tag = example_tag(),
        default_branch = yaml_scalar(&config.default_branch),
        branch_ref = format!("refs/heads/{}", config.default_branch),
        closure_runner = yaml_scalar(&config.github_runner),
        publish_runner = yaml_scalar(&config.github_runner),
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
    ))
}

/// The `jq` variable holding a platform's digest while the manifest assembles:
/// `linux_x64` for `Linux-X64`, and so on.
fn platform_variable(platform: &Platform) -> String {
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
        let Some(content) = runtime_products_content(ctx.config) else {
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
    use std::path::PathBuf;

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{RunnerMode, UnitKind};

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

    fn must_some<T>(option: Option<T>, context: &str) -> T {
        option.unwrap_or_else(|| panic!("{context}"))
    }

    fn unit() -> crate::Unit {
        crate::Unit {
            id: "rust-example".to_owned(),
            label: "rust-example".to_owned(),
            kind: UnitKind::Rust,
            root: ".".to_owned(),
            pinned_lockfile: true,
            watch: vec!["Cargo.toml".to_owned()],
            pr_commands: vec!["cargo check".to_owned()],
            full_commands: vec!["cargo check".to_owned()],
            github_pr_commands: None,
            github_full_commands: None,
            velnor_pr_commands: None,
            velnor_full_commands: None,
            depends_on: Vec::new(),
            cache: None,
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: Some(crate::RustToolchain {
                channel: "1.91.1".to_owned(),
                components: Vec::new(),
                targets: Vec::new(),
                profile: None,
            }),
            services: Vec::new(),
            requires_trusted: false,
            workspace_check: false,
        }
    }

    fn config(workflow_files: &[&str]) -> ProjectConfig {
        crate::ProjectConfig {
            repository: String::new(),
            workflow_revision: FIXTURE_REVISION.to_owned(),
            profile: "generic".to_owned(),
            analysis: crate::AnalysisSummary {
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
            runners: RunnerMode::Both,
            automatic: RunnerMode::Both,
            github_runner: "ubuntu-24.04".to_owned(),
            macos_runner: "macos-15".to_owned(),
            velnor_labels: vec!["self-hosted".to_owned(), "example-runner".to_owned()],
            release_enabled: false,
            release_reason: String::new(),
            release: None,
            renovate_enabled: false,
            renovate_reason: String::new(),
            renovate: None,
            units: vec![unit()],
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            velnor_runner_group: None,
            velnor_trusted_label: None,
            velnor_trusted_runner_available: None,
            pull_request_on_velnor: false,
            default_dispatch_runner: crate::DEFAULT_DISPATCH_RUNNER.to_owned(),
            automatic_lanes: crate::DEFAULT_AUTOMATIC_LANES.to_owned(),
            velnor_rust_needs: crate::VelnorRustNeeds::Parallel,
            velnor_concurrency_group: None,
            velnor_serial_stack_groups: false,
            static_files: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
            github_cache: crate::config::CacheGithubSection::default(),
            velnor_host_cache: crate::config::CacheVelnorSection::default(),
        }
    }

    fn owner_config(workflow_files: &[&str]) -> ProjectConfig {
        let mut config = config(workflow_files);
        config.repository = workflow_setup_action_repository().to_owned();
        config
    }

    fn owner_content(workflow_files: &[&str]) -> String {
        must_some(
            runtime_products_content(&owner_config(workflow_files)),
            "the owner renders the producer",
        )
    }

    fn scanned_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-runtime-products-{name}-{}",
            crate::unique_suffix()
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
        let shape = must(
            crate::scan::scan_shape(root, crate::RunnerMode::Both, "main", &[]),
            "scan fixture",
        );
        let directory = root.join(crate::config::GENERATION_CONFIG_PATH);
        must(
            fs::create_dir_all(directory.parent().unwrap_or(root)),
            "create generation config directory",
        );
        must(
            fs::write(
                &directory,
                format!("schema = 1\n\n[generator]\nrepository = \"example/declared\"\n\n{rows}"),
            ),
            "write declared config",
        );
        let generation = must(crate::config::discover(root), "discover declared config");
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
            runtime_products_content(&config(&[])).is_none(),
            "a repository without identity renders no producer"
        );
        let mut consumer = config(&[]);
        consumer.repository = "example/consumer".to_owned();
        assert!(
            runtime_products_content(&consumer).is_none(),
            "a consumer repository renders no producer"
        );
    }

    #[test]
    fn owner_renders_the_producer_on_default_branch_push_and_dispatch() {
        let mut config = owner_config(&[]);
        config.default_branch = "trunk".to_owned();
        let content = must_some(
            runtime_products_content(&config),
            "the owner renders the producer",
        );
        assert!(content.starts_with(GENERATED_HEADER), "{content}");
        assert!(
            content.contains("name: Velnor workflow runtime products"),
            "{content}"
        );
        assert!(
            content.contains("branches: [trunk]"),
            "push gates on the configured default branch: {content}"
        );
        assert!(content.contains("workflow_dispatch:"), "{content}");
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
        let build = must_some(build.split("\n  publish:\n").next(), "the build job body");
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
    }

    #[test]
    fn manifest_shape_matches_the_consumer_contract() {
        let content = owner_content(&[]);
        let action = setup_action_source();
        assert!(
            action.contains(MANIFEST_ACCEPT_FILTER),
            "the acceptance filter is the setup action's own"
        );
        assert_eq!(
            content.matches(MANIFEST_ACCEPT_FILTER).count(),
            2,
            "assemble and smoke-test evaluate the consumer filter: {content}"
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
        let velnor = crate::workflow_pinned_policy_runtime_velnor(FIXTURE_REVISION, "checkout");
        assert!(
            velnor.contains(MANIFEST_ACCEPT_FILTER),
            "the Velnor consumer evaluates the same filter"
        );
        let repository = workflow_setup_action_repository();
        assert!(
            velnor.contains(&format!(
                "--signer-workflow {repository}/.github/workflows/{RUNTIME_PRODUCTS_FILE}"
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
        // `--signer-ref` does not exist in gh: `--source-ref` is the flag
        // that pins the producer run to the default branch.
        let source_ref = "--source-ref refs/heads/main";
        for flag in [signer.as_str(), owner_flag.as_str(), source_ref] {
            assert!(
                content.contains(flag),
                "the smoke test verifies {flag}: {content}"
            );
        }
        let action = setup_action_source();
        for flag in [signer.as_str(), owner_flag.as_str(), source_ref] {
            assert!(
                action.contains(flag),
                "the setup action verifies the same {flag}"
            );
        }
        // Subject-level: both consumers verify the manifest as well as the
        // asset, against the same pinned producer workflow.
        let velnor = crate::workflow_pinned_policy_runtime_velnor(FIXTURE_REVISION, "checkout");
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
        // test verify the same two subjects under the same ref pin: an
        // attestation minted anywhere but the default branch verifies
        // nowhere.
        let action = setup_action_source();
        assert_eq!(
            action.matches("--source-ref refs/heads/main").count(),
            2,
            "the setup action pins the ref on the asset and the manifest"
        );
        let velnor = crate::workflow_pinned_policy_runtime_velnor(FIXTURE_REVISION, "checkout");
        assert_eq!(
            velnor.matches("--source-ref refs/heads/main").count(),
            2,
            "the Velnor provisioner pins the ref on the asset and the manifest"
        );
        let content = owner_content(&[]);
        assert_eq!(
            content.matches("--source-ref refs/heads/main").count(),
            2,
            "the producer smoke test pins the ref on the asset and the manifest: {content}"
        );
    }

    #[test]
    fn platforms_cover_the_consumer_lanes() {
        let mut config = owner_config(&[]);
        config.github_runner = "ubuntu-22.04".to_owned();
        config.macos_runner = "macos-26".to_owned();
        let content = must_some(
            runtime_products_content(&config),
            "the owner renders the producer",
        );
        for (os, arch, runner) in [
            ("Linux", "X64", "ubuntu-22.04"),
            ("Linux", "ARM64", LINUX_ARM64_RUNNER),
            ("macOS", "ARM64", "macos-26"),
        ] {
            assert!(
                content.contains(&format!(
                    "- os: {os}\n            arch: {arch}\n            runner: {runner}"
                )),
                "the matrix builds {os}-{arch} on {runner}: {content}"
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
            2,
            "build and publish skip when the tag exists: {content}"
        );
        assert!(
            content.contains("release $TAG already exists; leaving it untouched"),
            "the publish job re-checks before creating: {content}"
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
            runtime_products_content(&owner_config(&["maintenance.yml"])).as_deref(),
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
            crate::generated_files(&owner_config(&[])),
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
            crate::generated_files(&config(&[])),
            "generate the consumer surface",
        );
        assert!(
            !files.contains_key(&path),
            "a consumer surface carries no producer"
        );
        let mut adopted = owner_config(&[]);
        adopted.adopted_workflow_surface = true;
        let files = must(
            crate::generated_files(&adopted),
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
            runtime_products_content(&config),
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
    }

    #[test]
    fn producer_runs_only_on_the_default_branch() {
        let content = owner_content(&[]);
        let closure = must_some(content.split("  closure:\n").nth(1), "the closure job");
        let closure = must_some(closure.split("\n  build:\n").next(), "the closure job body");
        // The guard is the first step: a dispatch from anywhere else fails
        // before the checkout, the closure resolution, or any build.
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
        let content = must_some(
            runtime_products_content(&config),
            "the owner renders the producer",
        );
        assert!(
            content.contains("[[ \"$REF\" == \"refs/heads/trunk\" ]]"),
            "the guard follows the configured default branch: {content}"
        );
        assert!(
            content.contains("the producer publishes only from refs/heads/trunk"),
            "the failure names the configured ref: {content}"
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
            !publish.contains("gh release download"),
            "the smoke test proves the local bytes; nothing is downloaded from an exposed release: {publish}"
        );
        assert!(
            !content.contains("skipped"),
            "no skipped output remains: verification gates the create directly: {content}"
        );
        let recheck = must_some(
            publish.find("already exists; leaving it untouched"),
            "the pre-create re-check",
        );
        assert!(
            recheck < create,
            "the re-check fires immediately before creating: {publish}"
        );
    }

    #[test]
    fn smoke_test_pins_the_producer_ref() {
        let content = owner_content(&[]);
        assert_eq!(
            content.matches("--source-ref refs/heads/main").count(),
            2,
            "the smoke test pins the default-branch ref on the asset and the manifest: {content}"
        );
        let mut config = owner_config(&[]);
        config.default_branch = "trunk".to_owned();
        let content = must_some(
            runtime_products_content(&config),
            "the owner renders the producer",
        );
        assert_eq!(
            content.matches("--source-ref refs/heads/trunk").count(),
            2,
            "the smoke-test pin follows the configured default branch: {content}"
        );
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
                crate::unique_suffix()
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
                crate::unique_suffix()
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
        const PINNED: &str = "379826fe993b2ff59cebba1b0f1dbeca283f25f4c89681d1c55c3f053ced453b";
        let content = owner_content(&["maintenance.yml"]);
        let digest = digest_of(&content);
        assert_eq!(digest, PINNED, "rendered producer bytes changed");
    }
}
