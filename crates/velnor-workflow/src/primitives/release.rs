//! The release-side families: publishing, rolling preview, maintenance, and
//! provenance signing.
//!
//! Every family here renders one workflow file from the scanned shape, the
//! resolved config, and — for the families a repository may drive itself — its
//! declared arguments. A release contract is explicit input: the generic
//! publishers render only what a config or catalog declares, never a guess
//! from a manifest.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::{
    checks_env, render_cargo_source_preparation, render_pinned_toolchain_steps,
    render_retained_output_cache_note, Args, CacheBackend, Primitive, RenderCtx, Rendered,
    WorkflowIr, MAINTENANCE, PREVIEW, RELEASE, RELEASE_SIGNER, STATIC_WORKFLOW,
};
use crate::{
    github_expression, lane_supports_unit, rendered_cache_values, shell_quote, unit_display_label,
    velnor_runner, velnor_runner_group, workflow_runtime_setup,
    workflow_runtime_setup_with_install_rev, workflow_setup_install_rev, yaml_scalar, ActionPin,
    GeneratorError, ProjectConfig, ReleaseSpec, RunnerMode, Unit, UnitKind, GENERATED_HEADER,
    VELNOR_RELEASE_PACKAGE_SIGNER_TEMPLATE,
};

/// The release-side file families and the canonical file each one renders.
/// Every entry the generated surface owns gets a default row, unless the
/// config declares the family itself.
pub(crate) const RELEASE_SIDE_FILES: &[(&str, &str)] = &[
    ("release.yml", RELEASE),
    ("preview.yml", PREVIEW),
    ("maintenance.yml", MAINTENANCE),
    ("ci-release-package-signer.yml", RELEASE_SIGNER),
];

/// The canonical file a declared release-side family renders, when the family
/// is pinned to one name.
pub(crate) fn canonical_release_side_file(primitive: &str) -> Option<&'static str> {
    RELEASE_SIDE_FILES
        .iter()
        .find(|(_, family)| *family == primitive)
        .map(|(file, _)| *file)
}

/// Whether the primitive renders one of the release-side workflow files.
pub(crate) fn is_release_side(primitive: &str) -> bool {
    canonical_release_side_file(primitive).is_some() || primitive == STATIC_WORKFLOW
}

/// The workspace package compiled into the job image: the Dockerfile copies
/// `release-binaries/<arch>/velnor-workflow`, so the platform lane builds
/// exactly this package natively on each builder.
const IMAGE_WORKFLOW_PACKAGE: &str = "velnor-workflow";
/// The job-image Dockerfile the platform lane builds, relative to the
/// repository root.
const IMAGE_DOCKERFILE: &str = "docker/job-ubuntu.Dockerfile";
/// The release-record schema the stable publisher assembles. The record tool
/// and the package consumer refuse any other tag.
const RELEASE_RECORD_SCHEMA: &str = "velnor.release-record/v1";

/// The source URL stamped into OCI labels and the release record: the
/// release contract's own source repository on github.com.
fn release_source_url(release: &ReleaseSpec) -> String {
    format!("https://github.com/{}", release.source_repository)
}

// The default-surface content helpers. Both the estate catalog's name-keyed
// dispatch and the primitive rows render through these, so the two paths
// cannot drift.

/// The `release.yml` content for a config, or `None` when the config declares
/// no release contract and the publisher is omitted.
pub(crate) fn release_content(config: &ProjectConfig) -> Option<String> {
    config
        .release
        .as_ref()
        .map(|release| render_release(config, release))
}

/// The `preview.yml` content for a config.
pub(crate) fn preview_content(config: &ProjectConfig) -> String {
    render_preview(config, config.release.as_ref())
}

/// The headerless `maintenance.yml` body for a config.
pub(crate) fn maintenance_content(config: &ProjectConfig) -> String {
    render_maintenance(config)
}

/// The `ci-release-package-signer.yml` content.
pub(crate) fn release_signer_content() -> String {
    crate::render_static_template(VELNOR_RELEASE_PACKAGE_SIGNER_TEMPLATE)
}

/// A `static-workflow` row names an imported workflow body, which is not a
/// generation input: the primitive stays registered so the declaration fails
/// with explicit guidance instead of an unknown-primitive error.
pub(crate) struct StaticWorkflow;

impl Primitive for StaticWorkflow {
    fn id(&self) -> &'static str {
        STATIC_WORKFLOW
    }

    fn schema(&self) -> &'static [&'static str] {
        &["template"]
    }

    fn render(&self, _ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        Err(GeneratorError::usage(
            "`static-workflow` is not supported; imported workflow bodies are not a generation input. Declare a typed `release`, `preview`, `release-signer`, or `package-feed` capability",
        ))
    }
}

/// The declared `release.yml` publisher.
pub(crate) struct Release;

impl Primitive for Release {
    fn id(&self) -> &'static str {
        RELEASE
    }

    fn schema(&self) -> &'static [&'static str] {
        &[
            "artifact_path",
            "binary",
            "consumer_repository",
            "image",
            "kind",
            "manifest_schema",
            "package",
            "packages",
            "source_repository",
            "targets",
        ]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let spec = declared_or_configured_spec(ctx, args)?;
        let content = render_release(ctx.config, &spec);
        render_file(ctx, "release.yml", content)
    }
}

/// The declared `preview.yml` rolling lane.
pub(crate) struct Preview;

impl Primitive for Preview {
    fn id(&self) -> &'static str {
        PREVIEW
    }

    fn schema(&self) -> &'static [&'static str] {
        &["binary", "package", "targets"]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let content = if args.keys().is_empty() {
            // The default row renders the configured contract exactly as the
            // legacy dispatch does, omission contract included.
            preview_content(ctx.config)
        } else {
            let spec = declared_preview_spec(args)?;
            if !release_contract_complete(&spec) {
                return Err(incomplete_contract(ctx.family, &spec));
            }
            render_preview(ctx.config, Some(&spec))
        };
        render_file(ctx, "preview.yml", content)
    }
}

/// The declared `maintenance.yml` cache-hygiene workflow.
pub(crate) struct Maintenance;

impl Primitive for Maintenance {
    fn id(&self) -> &'static str {
        MAINTENANCE
    }

    fn schema(&self) -> &'static [&'static str] {
        &[]
    }

    fn render(&self, ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let content = format!("{GENERATED_HEADER}{}", maintenance_content(ctx.config));
        render_file(ctx, "maintenance.yml", content)
    }
}

/// The declared release artifact provenance signer.
pub(crate) struct ReleaseSigner;

impl Primitive for ReleaseSigner {
    fn id(&self) -> &'static str {
        RELEASE_SIGNER
    }

    fn schema(&self) -> &'static [&'static str] {
        &[]
    }

    fn render(&self, ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        render_file(
            ctx,
            "ci-release-package-signer.yml",
            release_signer_content(),
        )
    }
}

/// The release contract a row renders: the declared arguments when the row
/// declares any, otherwise the config's contract.
fn declared_or_configured_spec(
    ctx: &RenderCtx<'_>,
    args: &Args<'_>,
) -> Result<ReleaseSpec, GeneratorError> {
    if args.keys().is_empty() {
        return ctx.config.release.clone().ok_or_else(|| {
            GeneratorError::usage(format!(
                "`{}` renders `release.yml` only for a repository with a release contract; declare `kind` and the contract arguments",
                ctx.family
            ))
        });
    }
    let spec = declared_spec(ctx.family, args)?;
    if !release_contract_complete(&spec) {
        return Err(incomplete_contract(ctx.family, &spec));
    }
    Ok(spec)
}

/// Parse a declared release contract. Every field is explicit input; a
/// contract the generator cannot verify completely is a usage error, never a
/// partially rendered publisher.
fn declared_spec(family: &str, args: &Args<'_>) -> Result<ReleaseSpec, GeneratorError> {
    let kind = match args.string("kind")?.as_deref() {
        Some("crates" | "rust-binary" | "native" | "pages" | "homebrew" | "apt") => {
            args.string("kind")?.unwrap_or_default()
        }
        Some(other) => {
            return Err(GeneratorError::usage(format!(
                "`{family}` `kind` must be `crates`, `rust-binary`, `native`, `pages`, `homebrew`, or `apt`, found `{other}`"
            )))
        }
        None => {
            return Err(GeneratorError::usage(format!(
                "`{family}` needs `kind`; one of `crates`, `rust-binary`, `native`, `pages`, `homebrew`, or `apt`"
            )))
        }
    };
    Ok(ReleaseSpec {
        kind,
        package: args.string("package")?.unwrap_or_default(),
        packages: args.strings("packages")?.unwrap_or_default(),
        binary: args.string("binary")?.unwrap_or_default(),
        targets: args.strings("targets")?.unwrap_or_default(),
        image: args.string("image")?.unwrap_or_default(),
        source_repository: args.string("source_repository")?.unwrap_or_default(),
        consumer_repository: args.string("consumer_repository")?.unwrap_or_default(),
        artifact_path: args.string("artifact_path")?.unwrap_or_default(),
        description: String::new(),
        manifest_schema: args.string("manifest_schema")?.unwrap_or_default(),
    })
}

/// Parse a declared rolling-preview contract: the preview lane publishes a
/// Rust binary, so the contract is the binary publisher's without a `kind`.
fn declared_preview_spec(args: &Args<'_>) -> Result<ReleaseSpec, GeneratorError> {
    Ok(ReleaseSpec {
        kind: "rust-binary".to_owned(),
        package: args.string("package")?.unwrap_or_default(),
        packages: Vec::new(),
        binary: args.string("binary")?.unwrap_or_default(),
        targets: args.strings("targets")?.unwrap_or_default(),
        image: String::new(),
        source_repository: String::new(),
        consumer_repository: String::new(),
        artifact_path: String::new(),
        description: String::new(),
        manifest_schema: String::new(),
    })
}

fn incomplete_contract(family: &str, spec: &ReleaseSpec) -> GeneratorError {
    let missing = match spec.kind.as_str() {
        "crates" => "`packages`",
        "rust-binary" => "`package`, `binary`, and `targets`",
        "pages" => "`artifact_path`",
        _ => "the contract",
    };
    GeneratorError::usage(format!(
        "`{family}` declares an incomplete release contract; a `{}` publisher needs {missing}",
        spec.kind.as_str()
    ))
}

fn render_file(
    ctx: &RenderCtx<'_>,
    canonical: &str,
    content: String,
) -> Result<Rendered, GeneratorError> {
    let file = ctx
        .file
        .filter(|file| !file.is_empty())
        .ok_or_else(|| {
            GeneratorError::usage(format!(
                "`{}` renders `{canonical}` and needs `file`",
                ctx.family
            ))
        })?
        .to_owned();
    Ok(Rendered {
        files: std::iter::once((
            std::path::PathBuf::from(".github/workflows").join(file),
            content,
        ))
        .collect(),
        ..Rendered::default()
    })
}

pub(crate) fn release_contract_complete(release: &ReleaseSpec) -> bool {
    let targets_are_real = |targets: &[String]| {
        !targets.is_empty()
            && targets.iter().all(|target| {
                target.ends_with("-unknown-linux-gnu") || target.ends_with("-apple-darwin")
            })
    };
    match release.kind.as_str() {
        "crates" => !release.packages.is_empty(),
        "rust-binary" | "native" => {
            !release.package.is_empty()
                && !release.binary.is_empty()
                && targets_are_real(&release.targets)
        }
        "pages" => !release.artifact_path.is_empty(),
        "homebrew" => !release.package.is_empty() && !release.source_repository.is_empty(),
        "apt" => !release.package.is_empty() && !release.consumer_repository.is_empty(),
        _ => false,
    }
}

fn release_watch_paths(config: &ProjectConfig) -> String {
    let mut paths = BTreeSet::new();
    for unit in &config.units {
        paths.extend(unit.watch.iter().cloned());
    }
    paths.insert(".github/ci/**".to_owned());
    paths.insert(".github/workflows/preview.yml".to_owned());
    let mut output = String::new();
    for path in paths {
        let _ = writeln!(output, "      - {}", yaml_scalar(&path));
    }
    output
}

fn guest_seed_recipe_globs(config: &ProjectConfig) -> Vec<String> {
    if !config
        .units
        .iter()
        .any(|unit| unit.watch.iter().any(|path| path.contains("microvm")))
    {
        return Vec::new();
    }
    let mut globs = vec![
        "microvm/**".to_owned(),
        "Cargo.lock".to_owned(),
        "rust-toolchain.toml".to_owned(),
    ];
    for unit in &config.units {
        let root = unit.root.trim_end_matches('/');
        if root.starts_with("crates/") {
            globs.push(format!("{root}/**"));
        }
    }
    globs.sort();
    globs.dedup();
    globs
}

fn rust_package_unit<'a>(config: &'a ProjectConfig, package: &str) -> Option<&'a Unit> {
    config
        .units
        .iter()
        .find(|unit| unit.kind == UnitKind::Rust && unit_display_label(unit) == package)
}

fn detected_package_bin(config: &ProjectConfig, kind: &str, package: &str) -> Option<String> {
    let prefix = format!("{kind}:{package}:");
    config
        .analysis
        .detected
        .iter()
        .find_map(|item| item.strip_prefix(&prefix).map(str::to_owned))
}

fn guest_payload_bins(config: &ProjectConfig, package: &str) -> Option<(String, String)> {
    let agent = detected_package_bin(config, "guest-agent", package)?;
    let image = detected_package_bin(config, "guest-image", package)?;
    Some((agent, image))
}

fn release_package_dir(config: &ProjectConfig, package: &str) -> String {
    match rust_package_unit(config, package) {
        Some(unit) if unit.root == "." || unit.root.is_empty() => "release".to_owned(),
        Some(unit) => format!("{}/release", unit.root.trim_end_matches('/')),
        None => "release".to_owned(),
    }
}

fn release_package_guest_dir(config: &ProjectConfig, package: &str) -> String {
    format!("{}/microvm", release_package_dir(config, package))
}

/// Whether the release package declares the `release-build` cargo feature:
/// the scan records one `release-build:{package}` marker per declaring
/// package, and only those lanes build with release identity.
fn release_build_detected(config: &ProjectConfig, package: &str) -> bool {
    let marker = format!("release-build:{package}");
    config.analysis.detected.iter().any(|item| item == &marker)
}

/// Whether the contract renders the identity release lane: a native
/// publisher whose package carries release identity. Every other publisher
/// keeps its plain build exactly.
fn native_identity_release(config: &ProjectConfig, release: &ReleaseSpec) -> bool {
    release.kind == "native" && release_build_detected(config, &release.package)
}

/// Whether the contract renders Debian packaging with release identity:
/// the identity lane plus a declared package consumer.
fn native_debian_release(config: &ProjectConfig, release: &ReleaseSpec) -> bool {
    native_identity_release(config, release) && !release.consumer_repository.is_empty()
}

/// A Debian matrix row the contract targets resolve to, or `None` when a
/// target has no Debian architecture. Debian packaging stays Linux-only;
/// anything else fails closed at the packaging step instead of silently
/// dropping an architecture.
fn deb_architectures(targets: &[String]) -> Option<Vec<(&'static str, &str)>> {
    targets
        .iter()
        .map(|target| match target.as_str() {
            "x86_64-unknown-linux-gnu" => Some(("amd64", target.as_str())),
            "aarch64-unknown-linux-gnu" => Some(("arm64", target.as_str())),
            _ => None,
        })
        .collect()
}

fn deb_arch_matrix(config: &ProjectConfig, targets: &[String], guest: bool) -> Option<String> {
    let mut matrix = String::new();
    for (arch, target) in deb_architectures(targets)? {
        // The guest payload artifacts keep the toolchain arch spelling, so a
        // guest lane carries both spellings and downloads by the toolchain one.
        let guest_arch = guest.then_some(match arch {
            "amd64" => "\n            guest_arch: x86_64",
            _ => "\n            guest_arch: aarch64",
        });
        let _ = writeln!(
            matrix,
            "          - arch: {arch}\n            target: {target}\n            runner: {}{}",
            release_runner(config, target),
            guest_arch.unwrap_or_default(),
        );
    }
    Some(matrix)
}

fn hashfiles_expr(globs: &[String]) -> String {
    globs
        .iter()
        .map(|glob| format!("'{glob}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Restore/reuse/collect/save a recipe-identity guest seed. Exact-only restore;
/// save only on a trusted default-branch miss.
fn render_guest_seed_restore(recipe_globs: &[String]) -> String {
    let hashfiles = hashfiles_expr(recipe_globs);
    let restore = ActionPin::CacheRestore.reference();
    format!(
        r"      - name: Restore guest seed
        id: guest-seed
        uses: {restore}
        with:
          path: .guest-seed/${{{{ matrix.arch }}}}
          key: guest-seed-${{{{ matrix.arch }}}}-${{{{ hashFiles({hashfiles}) }}}}
",
    )
}

/// The guest agent ships inside the deb, so identity lanes build it with
/// release identity; the preview lane binds the resolved source commit.
fn render_guest_agent_build_step(
    package: &str,
    agent_bin: &str,
    cargo_cmd: &str,
    identity: bool,
    preview_commit: Option<&str>,
) -> String {
    let package = yaml_scalar(package);
    let agent_bin = yaml_scalar(agent_bin);
    let mut agent_env = String::new();
    let agent_features = if identity {
        agent_env.push_str("        env:\n          VELNOR_RELEASE_BUILD: \"1\"\n");
        if let Some(commit) = preview_commit {
            let _ = writeln!(agent_env, "          VELNOR_PREVIEW_SOURCE_SHA: {commit}");
        }
        " --features release-build"
    } else {
        ""
    };
    format!(
        r#"      - name: Build guest agent for rootfs
{agent_env}        run: |
          set -euo pipefail
          agent="target/$TARGET/release/{agent_bin}"
          {cargo_cmd} build --locked --release --package {package} --bin {agent_bin}{agent_features} --target "$TARGET"
          test -x "$agent"
"#,
    )
}

fn render_guest_seed_assemble(
    default_branch: &str,
    recipe_globs: &[String],
    package: &str,
    agent_bin: &str,
    image_bin: &str,
    cargo_cmd: &str,
) -> String {
    let hashfiles = hashfiles_expr(recipe_globs);
    let save = ActionPin::CacheSave.reference();
    let package = yaml_scalar(package);
    let agent_bin = yaml_scalar(agent_bin);
    let image_bin = yaml_scalar(image_bin);
    format!(
        r#"      - name: Reuse verified guest seed
        id: guest-seed-reuse
        run: |
          set -euo pipefail
          seed=".guest-seed/${{{{ matrix.arch }}}}"
          if [ ! -s "$seed/vmlinux" ] || [ ! -s "$seed/vmlinux.sha256" ] || [ ! -s "$seed/rootfs.ext4" ] || [ ! -s "$seed/rootfs.sha256" ]; then
            echo "restored=false" >> "$GITHUB_OUTPUT"
            exit 0
          fi
          if ! (cd "$seed" && sha256sum --check --strict vmlinux.sha256) \
            || ! (cd "$seed" && sha256sum --check --strict rootfs.sha256); then
            echo "restored=false" >> "$GITHUB_OUTPUT"
            exit 0
          fi
          agent="target/$TARGET/release/{agent_bin}"
          agent_dump="$(mktemp)"
          trap 'rm -f -- "$agent_dump"' EXIT
          # shellcheck disable=SC2209
          if ! DEBUGFS_PAGER=cat debugfs -R "dump /usr/bin/{agent_bin} $agent_dump" "$seed/rootfs.ext4" >/dev/null \
            || ! cmp -- "$agent_dump" "$agent"; then
            echo "restored=false" >> "$GITHUB_OUTPUT"
            exit 0
          fi
          mkdir -p dist/microvm
          cp "$seed/vmlinux" "$seed/vmlinux.sha256" "$seed/rootfs.ext4" dist/microvm/
          cp "$seed/rootfs.sha256" dist/microvm/rootfs.sha256
          cp "$agent" dist/microvm/{agent_bin}
          sha256sum "$agent" | awk '{{print $1}}' > dist/microvm/guest-agent.sha256
          echo "restored=true" >> "$GITHUB_OUTPUT"
      - name: Download pinned kernel tarball
        if: steps.guest-seed-reuse.outputs.restored != 'true'
        run: |
          set -euo pipefail
          url="$(jq -er '.kernel_tarball' microvm/pins.json)"
          sha="$(jq -er '.kernel_tarball_sha256' microvm/pins.json)"
          curl --fail --show-error --silent --location --http1.1 \
            --continue-at - --retry 20 --retry-all-errors --retry-delay 5 \
            --retry-max-time 1800 \
            --connect-timeout 30 --max-time 900 \
            -o linux.tar.xz "$url"
          echo "$sha  linux.tar.xz" | sha256sum -c -
      - name: Build guest vmlinux and rootfs.ext4
        if: steps.guest-seed-reuse.outputs.restored != 'true'
        run: |
          set -euo pipefail
          # Hosted mold setup replaces /usr/bin/ld. Linux kconfig then fails
          # closed with "ld: unknown linker" / "this linker is not supported".
          if [ -x /usr/bin/ld.bfd ]; then
            if [ "$(id -u)" -eq 0 ]; then
              ln -sf /usr/bin/ld.bfd /usr/bin/ld
            else
              sudo ln -sf /usr/bin/ld.bfd /usr/bin/ld
            fi
          fi
          ld --version | grep -Eiq 'GNU (ld|gold)|bfd' \
            || {{ echo "::error::kernel build needs GNU ld; mold must not own /usr/bin/ld" >&2; exit 1; }}
          mkdir -p dist/microvm
          agent="target/$TARGET/release/{agent_bin}"
          {cargo_cmd} run --locked --release --package {package} --bin {image_bin} -- \
            build --arch "${{{{ matrix.arch }}}}" --out dist/microvm --tarball linux.tar.xz \
            --guest-agent "$agent"
          test -s dist/microvm/vmlinux
          test -s dist/microvm/rootfs.ext4
          sha256sum dist/microvm/rootfs.ext4 | awk '{{print $1}}' > dist/microvm/rootfs.sha256
          cp "$agent" dist/microvm/{agent_bin}
          sha256sum "$agent" | awk '{{print $1}}' > dist/microvm/guest-agent.sha256
      - name: Collect verified guest seed
        if: github.event_name == 'push' && github.ref == 'refs/heads/{default_branch}' && steps.guest-seed.outputs.cache-hit != 'true'
        run: |
          set -euo pipefail
          seed=".guest-seed/${{{{ matrix.arch }}}}"
          mkdir -p "$seed"
          cp dist/microvm/vmlinux dist/microvm/rootfs.ext4 dist/microvm/rootfs.sha256 "$seed/"
          (cd "$seed" && sha256sum vmlinux > vmlinux.sha256)
          (cd "$seed" && sha256sum rootfs.ext4 > rootfs.sha256)
          (cd "$seed" && sha256sum --check --strict vmlinux.sha256)
          (cd "$seed" && sha256sum --check --strict rootfs.sha256)
      - name: Save guest seed
        if: github.event_name == 'push' && github.ref == 'refs/heads/{default_branch}' && steps.guest-seed.outputs.cache-hit != 'true'
        uses: {save}
        with:
          path: .guest-seed/${{{{ matrix.arch }}}}
          key: guest-seed-${{{{ matrix.arch }}}}-${{{{ hashFiles({hashfiles}) }}}}
"#
    )
}

fn guest_arch_matrix(config: &ProjectConfig) -> String {
    let mut matrix = String::new();
    for (arch, target) in [
        ("x86_64", "x86_64-unknown-linux-gnu"),
        ("aarch64", "aarch64-unknown-linux-gnu"),
    ] {
        // aarch64 guest-agent + aws-lc/openssl need native headers. Crossing
        // on ubuntu-24.04 with only gcc-aarch64-linux-gnu fails closed
        // (bits/libc-header-start.h / sys/types.h). The image lane already
        // names GitHub's hosted arm64 label for the same reason.
        let runner = match arch {
            "aarch64" => "ubuntu-24.04-arm".to_owned(),
            _ => yaml_scalar(&config.github_runner),
        };
        let _ = writeln!(
            matrix,
            "          - arch: {arch}\n            target: {target}\n            runner: {runner}",
        );
    }
    matrix
}

fn render_guest_payload_job(
    config: &ProjectConfig,
    release: &ReleaseSpec,
    preview: bool,
) -> Option<String> {
    let globs = guest_seed_recipe_globs(config);
    if globs.is_empty() {
        return None;
    }
    let (agent_bin, image_bin) = guest_payload_bins(config, &release.package)?;
    let checkout = ActionPin::Checkout.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let mut setup = String::new();
    let workflow = WorkflowIr::from_config(config);
    let cargo_cmd = if let Some(unit) = rust_package_unit(config, &release.package) {
        workflow.render_tool_provisioning(&mut setup, RunnerMode::Github, unit, true);
        "mbx"
    } else if let Some(toolchain) = crate::config_rust_toolchain(config) {
        render_pinned_toolchain_steps(
            &mut setup,
            ActionPin::CacheRestore.reference(),
            ActionPin::CacheSave.reference(),
            &toolchain,
            Some(&format!(
                "github.event_name == 'push' && github.ref == 'refs/heads/{}' && steps.rustup-toolchain.outputs.cache-hit != 'true'",
                config.default_branch
            )),
        );
        "cargo"
    } else {
        "cargo"
    };
    setup.push_str(
        r#"      - name: Add Rust target
        run: rustup target add "$TARGET"
      - name: Install guest seed tools
        run: |
          set -euo pipefail
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends \
            build-essential flex bison bc libssl-dev libelf-dev xz-utils e2fsprogs \
            mmdebstrap debootstrap arch-test
          if [ "${{ matrix.arch }}" = "aarch64" ]; then
            # Cross GCC without the aarch64 sysroot cannot compile aws-lc or
            # vendored openssl (sys/types.h / bits/libc-header-start.h).
            sudo apt-get install -y --no-install-recommends \
              gcc-aarch64-linux-gnu libc6-dev-arm64-cross linux-libc-dev-arm64-cross
          else
            sudo apt-get install -y --no-install-recommends qemu-user-static binfmt-support
            sudo update-binfmts --enable qemu-aarch64 || true
          fi
"#,
    );
    // The agent ships inside the deb, so it carries release identity exactly
    // when the lane packages identity debs; otherwise the plain build stays.
    let identity = native_debian_release(config, release);
    let preview_commit = (preview && identity).then_some("${{ needs.identity.outputs.commit }}");
    let mut steps = render_guest_seed_restore(&globs);
    steps.push_str(&render_guest_agent_build_step(
        &release.package,
        &agent_bin,
        cargo_cmd,
        identity,
        preview_commit,
    ));
    steps.push_str(&render_guest_seed_assemble(
        &config.default_branch,
        &globs,
        &release.package,
        &agent_bin,
        &image_bin,
        cargo_cmd,
    ));
    let matrix = guest_arch_matrix(config);
    // The preview lane binds the resolved identity commit instead of the
    // moving branch tip; the stable lane keeps its tag checkout.
    let (needs, checkout_ref) = if preview {
        (
            "    needs: [identity]\n",
            "          ref: ${{ needs.identity.outputs.commit }}\n",
        )
    } else {
        ("", "")
    };
    Some(format!(
        "  guest-payload:\n    name: Guest payload ${{{{ matrix.arch }}}}\n{needs}    runs-on: ${{{{ matrix.runner }}}}\n    timeout-minutes: 180\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    env:\n      TARGET: ${{{{ matrix.target }}}}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n{checkout_ref}          persist-credentials: false\n{setup}{steps}      - name: Upload guest payload\n        uses: {upload}\n        with:\n          name: guest-payload-${{{{ matrix.arch }}}}\n          path: dist/microvm/*\n          if-no-files-found: error\n",
        matrix = matrix,
    ))
}

fn render_debian_job(config: &ProjectConfig, release: &ReleaseSpec, guest: bool) -> String {
    let package = yaml_scalar(&release.package);
    let checkout = ActionPin::Checkout.reference();
    let download = ActionPin::DownloadArtifact.reference();
    let attest = ActionPin::Attest.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let mut setup = workflow_runtime_setup_for_config(config);
    let workflow = WorkflowIr::from_config(config);
    if let Some(unit) = rust_package_unit(config, &release.package) {
        workflow.render_tool_provisioning(&mut setup, RunnerMode::Github, unit, false);
    }
    let mut needs = vec![
        "admit-runner".to_owned(),
        "verify".to_owned(),
        "build".to_owned(),
    ];
    if guest {
        needs.push("guest-payload".to_owned());
    }
    let needs = needs.join(", ");
    let mut steps = format!(
        "      - name: Checkout\n        uses: {checkout}\n        with:\n          persist-credentials: false\n{setup}      - name: Download release artifacts\n        uses: {download}\n        with:\n          path: dist\n          pattern: {}-*\n          merge-multiple: true\n",
        canonical_lane(config),
    );
    let mut package_cmd = format!(
        "          velnor-workflow release package-deb --package {package} --version \"${{VERSION#v}}\""
    );
    if guest {
        let (agent_bin, image_bin) = guest_payload_bins(config, &release.package)
            .map(|(agent, image)| (yaml_scalar(&agent), yaml_scalar(&image)))
            .unwrap_or_default();
        let stage_root = shell_quote(&release_package_guest_dir(config, &release.package));
        let cargo_cmd = if rust_package_unit(config, &release.package).is_some() {
            "mbx"
        } else {
            "cargo"
        };
        let _ = write!(
            steps,
            "      - name: Download guest payload\n        uses: {download}\n        with:\n          name: guest-payload-${{{{ matrix.arch }}}}\n          path: dist/microvm\n      - name: Stage guest payload\n        run: |\n          set -euo pipefail\n          arch=\"${{{{ matrix.arch }}}}\"\n          root={stage_root}\n          mkdir -p \"$root\" extracted\n          url=\"$(jq -er --arg a \"$arch\" '.tarballs[$a].url' microvm/pins.json)\"\n          sha=\"$(jq -er --arg a \"$arch\" '.tarballs[$a].sha256' microvm/pins.json)\"\n          fc_member=\"$(jq -er --arg a \"$arch\" '.tarballs[$a].firecracker_member' microvm/pins.json)\"\n          jailer_member=\"$(jq -er --arg a \"$arch\" '.tarballs[$a].jailer_member' microvm/pins.json)\"\n          curl --fail --show-error --silent --location --http1.1 \\\n            --continue-at - --retry 20 --retry-all-errors --retry-delay 5 \\\n            --retry-max-time 1800 --connect-timeout 30 --max-time 900 \\\n            -o firecracker.tgz \"$url\"\n          echo \"$sha  firecracker.tgz\" | sha256sum -c -\n          tar -xzf firecracker.tgz -C extracted\n          cp \"extracted/${{fc_member}}\" \"$root/firecracker\"\n          cp \"extracted/${{jailer_member}}\" \"$root/jailer\"\n          test -s dist/microvm/{agent_bin}\n          test -s dist/microvm/guest-agent.sha256\n          test -s dist/microvm/vmlinux\n          test -s dist/microvm/rootfs.ext4\n          test -s dist/microvm/rootfs.sha256\n          guest_agent_sha256=\"$(awk '{{print $1}}' dist/microvm/guest-agent.sha256)\"\n          rootfs_sha256=\"$(awk '{{print $1}}' dist/microvm/rootfs.sha256)\"\n          cp dist/microvm/{agent_bin} \"$root/{agent_bin}\"\n          cp dist/microvm/vmlinux \"$root/vmlinux\"\n          cp dist/microvm/rootfs.ext4 \"$root/rootfs.ext4\"\n          chmod 0755 \"$root/firecracker\" \"$root/jailer\" \"$root/{agent_bin}\"\n          {cargo_cmd} run --locked --release --package {package} --bin {image_bin} -- \\\n            stage --root \"$root\" --arch \"$arch\" \\\n            --rootfs-sha256 \"$rootfs_sha256\" \\\n            --guest-agent-sha256 \"$guest_agent_sha256\"\n"
        );
        package_cmd.push_str(" --guest dist/microvm --target \"${{ matrix.target }}\"");
    }
    let header = if guest {
        format!(
            "  debian:\n    name: Package Debian artifacts\n    needs: [{needs}]\n    runs-on: {runner}\n    timeout-minutes: 45\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{}    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    steps:\n",
            guest_arch_matrix(config),
            needs = needs,
            runner = yaml_scalar(&config.github_runner),
        )
    } else {
        format!(
            "  debian:\n    name: Package Debian artifacts\n    needs: [{needs}]\n    runs-on: {}\n    timeout-minutes: 45\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    steps:\n",
            yaml_scalar(&config.github_runner),
        )
    };
    format!(
        "{header}{steps}      - name: Build Debian packages\n        env:\n          VERSION: ${{{{ github.ref_name }}}}\n        run: |\n          set -euo pipefail\n{package_cmd}\n      - name: Attest Debian packages\n        uses: {attest}\n        with:\n          subject-path: dist/*.deb\n      - name: Upload Debian packages\n        uses: {upload}\n        with:\n          name: debian-packages\n          path: dist/*.deb\n          if-no-files-found: error\n          retention-days: 2\n"
    )
}

/// The identity metadata job: one host-native release-build binary exports
/// the acyclic identity files (`build-identity.json`, `manifest.json`) the
/// deb lanes ship, plus itself as the record-emitting release tool. The
/// cross-compiling deb jobs can never execute their target binary, so they
/// copy this artifact instead of exporting it.
fn render_release_metadata_job(
    config: &ProjectConfig,
    release: &ReleaseSpec,
    preview: bool,
) -> String {
    let package = yaml_scalar(&release.package);
    let binary = yaml_scalar(&release.binary);
    let checkout = ActionPin::Checkout.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let mut setup = workflow_runtime_setup_for_config(config);
    let workflow = WorkflowIr::from_config(config);
    let cargo_cmd = if let Some(unit) = rust_package_unit(config, &release.package) {
        workflow.render_tool_provisioning(&mut setup, RunnerMode::Github, unit, false);
        "mbx"
    } else {
        "cargo"
    };
    let (job, name, needs, gate, artifact, checkout_ref, sha_env, sha_check, retention) = if preview
    {
        (
            "metadata",
            "Compile preview metadata once",
            "    needs: [identity]\n",
            format!(
                "    if: ${{{{ github.ref == 'refs/heads/{}' }}}}\n",
                config.default_branch
            ),
            "preview-metadata",
            "          ref: ${{ needs.identity.outputs.commit }}\n",
            "          VELNOR_PREVIEW_SOURCE_SHA: ${{ needs.identity.outputs.commit }}\n",
            "          jq -e '.source_sha and .crate_version' build-identity.json >/dev/null\n          jq -e 'type == \"object\"' manifest.json >/dev/null\n          jq -e --arg sha \"$SOURCE_COMMIT\" '.source_sha == $sha' build-identity.json >/dev/null \\\n            || { echo \"::error::build identity source_sha != preview source commit $SOURCE_COMMIT\" >&2; exit 1; }\n",
            1,
        )
    } else {
        (
            "metadata",
            "Compile release metadata once",
            "    needs: [verify]\n",
            String::new(),
            "release-metadata",
            "",
            "",
            "",
            2,
        )
    };
    let source_env = if preview {
        "          SOURCE_COMMIT: ${{ needs.identity.outputs.commit }}\n"
    } else {
        ""
    };
    // The stable lane binds the compiled manifest into the OCI labels and
    // the release record through this output; the preview lane has no
    // record, so its bytes stay untouched.
    let (outputs, export_id, sha_lines) = if preview {
        ("", "", "")
    } else {
        (
            "    outputs:\n      manifest_sha256: ${{ steps.export.outputs.manifest_sha256 }}\n",
            "        id: export\n",
            "          sha256=\"$(sha256sum manifest.json | awk '{print $1}')\"\n          echo \"manifest_sha256=$sha256\" >> \"$GITHUB_OUTPUT\"\n",
        )
    };
    format!(
        "  {job}:\n    name: {name}\n{needs}{gate}{outputs}    runs-on: {runner}\n    timeout-minutes: 30\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n{checkout_ref}          persist-credentials: false\n{setup}      - name: Export metadata from one release-build binary\n{export_id}        env:\n          CARGO_INCREMENTAL: \"0\"\n          VELNOR_RELEASE_BUILD: \"1\"\n{sha_env}{source_env}        run: |\n          set -euo pipefail\n          {cargo_cmd} build -q --package {package} --release --locked --features release-build\n          runner=\"target/release/{binary}\"\n          test -x \"$runner\"\n          \"$runner\" release export > build-identity.json\n          \"$runner\" capabilities export > manifest.json\n{sha_lines}{sha_check}          cp \"$runner\" {binary}-release-tool\n      - name: Upload release metadata\n        uses: {upload}\n        with:\n          name: {artifact}\n          path: |\n            build-identity.json\n            manifest.json\n            {binary}-release-tool\n          if-no-files-found: error\n          retention-days: {retention}\n",
        runner = yaml_scalar(&config.github_runner),
    )
}

/// Build every workspace binary package for the deb target, with
/// `release-build` exactly where the declaring manifest carries it. The
/// release package itself is excluded: it builds separately below, runner
/// binary only, so the guest agent is never rebuilt with a second identity
/// (it arrives from the guest payload artifact instead).
fn debian_sibling_build_steps(cargo_cmd: &str, package: &str) -> String {
    let package = shell_quote(package);
    format!(
        "      - name: Build sibling binaries for the deb\n        env:\n          CARGO_INCREMENTAL: \"0\"\n          RUSTC_WRAPPER: sccache\n        run: |\n          set -euo pipefail\n          metadata=\"$(cargo metadata --locked --no-deps --format-version 1)\"\n          while IFS= read -r sibling; do\n            [ -n \"$sibling\" ] || continue\n            if jq -e --arg p \"$sibling\" '.packages[] | select(.name == $p) | .features | has(\"release-build\")' <<<\"$metadata\" >/dev/null; then\n              features=\"--features release-build\"\n            else\n              features=\"\"\n            fi\n            # shellcheck disable=SC2086\n            {cargo_cmd} build --locked --release --package \"$sibling\" $features --target \"$TARGET\"\n          done < <(jq -r --arg release {package} '.packages[] | select(.name != $release) | select(.targets | map(.kind[]) | flatten | any(. == \"bin\")) | .name' <<<\"$metadata\" | sort -u)\n"
    )
}

/// Stage the acyclic identity files, then emit the deb's own package record
/// with the host-native release tool: the tool refuses any record whose
/// kind, commit, crate version, or runner binary digest disagrees with its
/// own embedded identity, and writes only the canonical bytes.
fn debian_identity_steps(
    release_dir: &str,
    binary: &str,
    repository: &str,
    kind: &str,
    crate_expr: &str,
    preview: bool,
) -> String {
    let release_dir = shell_quote(release_dir);
    let repository = shell_quote(repository);
    let tool = format!("metadata/{binary}-release-tool");
    let source_check = if preview {
        "          jq -e --arg sha \"$SOURCE_COMMIT\" '.source_sha == $sha' metadata/build-identity.json >/dev/null \\\n            || { echo \"::error::build identity source_sha != preview source commit $SOURCE_COMMIT\" >&2; exit 1; }\n"
    } else {
        ""
    };
    let commit_value = if preview {
        "\"$SOURCE_COMMIT\"".to_owned()
    } else {
        "$(jq -er '.source_sha' metadata/build-identity.json)".to_owned()
    };
    let record_check = if preview {
        format!(
            "          jq -e --arg version \"$VERSION\" --arg commit \"$SOURCE_COMMIT\" \\\n            '.build.kind == \"preview\" and .build.debian_version == $version and .build.commit == $commit' \\\n            {release_dir}/package-record.json >/dev/null \\\n            || {{ echo \"::error::emitted package record does not name this preview identity\" >&2; exit 1; }}\n"
        )
    } else {
        String::new()
    };
    format!(
        "      - name: Stage acyclic identity files packaged into the deb\n        run: |\n          set -euo pipefail\n          test -s metadata/build-identity.json || {{ echo \"::error::release metadata missing build-identity.json\" >&2; exit 1; }}\n          test -s metadata/manifest.json || {{ echo \"::error::release metadata missing manifest.json\" >&2; exit 1; }}\n          jq -e '.source_sha and .crate_version' metadata/build-identity.json >/dev/null\n          jq -e 'type == \"object\"' metadata/manifest.json >/dev/null\n{source_check}          mkdir -p {release_dir}\n          cp metadata/build-identity.json metadata/manifest.json {release_dir}/\n      - name: Stage the deb's own package record\n        run: |\n          set -euo pipefail\n          chmod +x {tool}\n          binary=\"target/$TARGET/release/{binary}\"\n          test -s \"$binary\" || {{ echo \"::error::missing cross-built runner binary\" >&2; exit 1; }}\n          binary_sha256=\"$(sha256sum \"$binary\" | awk '{{print $1}}')\"\n          manifest_sha256=\"$(sha256sum metadata/manifest.json | awk '{{print $1}}')\"\n          manifest_version=\"$(jq -er '.version | numbers' metadata/manifest.json)\"\n          source_sha={commit_value}\n          jq -n \\\n            --arg schema \"velnor.package-record/v1\" \\\n            --arg repo {repository} \\\n            --arg kind \"{kind}\" \\\n            --arg commit \"$source_sha\" \\\n            --arg crate {crate_expr} \\\n            --arg debian \"$VERSION\" \\\n            --argjson mv \"$manifest_version\" \\\n            --arg mhash \"$manifest_sha256\" \\\n            --arg arch \"${{{{ matrix.arch }}}}\" \\\n            --arg target \"$TARGET\" \\\n            --arg binary \"$binary_sha256\" \\\n            '{{\n              schema: $schema,\n              build: {{ repository: $repo, kind: $kind, commit: $commit,\n                       crate_version: $crate, debian_version: $debian,\n                       manifest_version: $mv, manifest_sha256: $mhash }},\n              architecture: {{ arch: $arch, target: $target, binary_sha256: $binary }}\n            }}' > package-record.candidate.json\n          {tool} release emit \\\n            --record package-record.candidate.json \\\n            --binary \"$binary\" \\\n            --out {release_dir}/package-record.json\n{record_check}"
    )
}

/// Stage the pinned Firecracker, jailer, and guest agent into the deb
/// staging root, then bind them with the guest manifest stage command. The
/// downloaded agent is also installed over the target directory copy, so
/// the packaged `/usr/bin` agent and the microvm payload are one identical
/// byte stream, never two builds that merely should agree.
fn debian_guest_steps(config: &ProjectConfig, release: &ReleaseSpec, cargo_cmd: &str) -> String {
    let package = yaml_scalar(&release.package);
    let (agent_bin, image_bin) = guest_payload_bins(config, &release.package)
        .map(|(agent, image)| (yaml_scalar(&agent), yaml_scalar(&image)))
        .unwrap_or_default();
    let stage_root = shell_quote(&release_package_guest_dir(config, &release.package));
    let download = ActionPin::DownloadArtifact.reference();
    format!(
        "      - name: Download guest payload\n        uses: {download}\n        with:\n          name: guest-payload-${{{{ matrix.guest_arch }}}}\n          path: guest-payload\n      - name: Stage pinned Firecracker, jailer, and guest agent\n        run: |\n          set -euo pipefail\n          arch=\"${{{{ matrix.guest_arch }}}}\"\n          root={stage_root}\n          mkdir -p \"$root\" extracted\n          url=\"$(jq -er --arg a \"$arch\" '.tarballs[$a].url' microvm/pins.json)\"\n          sha=\"$(jq -er --arg a \"$arch\" '.tarballs[$a].sha256' microvm/pins.json)\"\n          fc_member=\"$(jq -er --arg a \"$arch\" '.tarballs[$a].firecracker_member' microvm/pins.json)\"\n          jailer_member=\"$(jq -er --arg a \"$arch\" '.tarballs[$a].jailer_member' microvm/pins.json)\"\n          curl --fail --show-error --silent --location --http1.1 \\\n            --continue-at - --retry 20 --retry-all-errors --retry-delay 5 \\\n            --retry-max-time 1800 --connect-timeout 30 --max-time 900 \\\n            -o firecracker.tgz \"$url\"\n          echo \"$sha  firecracker.tgz\" | sha256sum -c -\n          tar -xzf firecracker.tgz -C extracted\n          cp \"extracted/${{fc_member}}\" \"$root/firecracker\"\n          cp \"extracted/${{jailer_member}}\" \"$root/jailer\"\n          test -s guest-payload/{agent_bin} || {{ echo \"::error::guest payload missing guest-agent\" >&2; exit 1; }}\n          test -s guest-payload/guest-agent.sha256 || {{ echo \"::error::guest payload missing guest-agent checksum\" >&2; exit 1; }}\n          guest_agent_sha256=\"$(awk '{{print $1}}' guest-payload/guest-agent.sha256)\"\n          [ \"${{#guest_agent_sha256}}\" -eq 64 ] || {{ echo \"::error::guest-agent digest has invalid length\" >&2; exit 1; }}\n          actual_guest_agent_sha256=\"$(sha256sum guest-payload/{agent_bin} | awk '{{print $1}}')\"\n          [ \"$actual_guest_agent_sha256\" = \"$guest_agent_sha256\" ] || {{ echo \"::error::downloaded guest-agent digest mismatch\" >&2; exit 1; }}\n          install -Dm0755 guest-payload/{agent_bin} \"target/$TARGET/release/{agent_bin}\"\n          cmp -- \"target/$TARGET/release/{agent_bin}\" guest-payload/{agent_bin}\n          test -s guest-payload/vmlinux || {{ echo \"::error::guest payload missing vmlinux\" >&2; exit 1; }}\n          test -s guest-payload/rootfs.ext4 || {{ echo \"::error::guest payload missing rootfs.ext4\" >&2; exit 1; }}\n          test -s guest-payload/rootfs.sha256 || {{ echo \"::error::guest payload missing rootfs.sha256\" >&2; exit 1; }}\n          rootfs_sha256=\"$(awk '{{print $1}}' guest-payload/rootfs.sha256)\"\n          [ \"${{#rootfs_sha256}}\" -eq 64 ] || {{ echo \"::error::guest rootfs digest has invalid length\" >&2; exit 1; }}\n          actual_rootfs_sha256=\"$(sha256sum guest-payload/rootfs.ext4 | awk '{{print $1}}')\"\n          [ \"$actual_rootfs_sha256\" = \"$rootfs_sha256\" ] || {{ echo \"::error::downloaded guest rootfs digest mismatch\" >&2; exit 1; }}\n          cp guest-payload/{agent_bin} \"$root/{agent_bin}\"\n          cp guest-payload/vmlinux \"$root/vmlinux\"\n          cp guest-payload/rootfs.ext4 \"$root/rootfs.ext4\"\n          chmod 0755 \"$root/firecracker\" \"$root/jailer\" \"$root/{agent_bin}\"\n          fc_sha=\"$(sha256sum \"$root/firecracker\" | awk '{{print $1}}')\"\n          jailer_sha=\"$(sha256sum \"$root/jailer\" | awk '{{print $1}}')\"\n          expected_fc=\"$(jq -er --arg a \"$arch\" '.tarballs[$a].firecracker_sha256' microvm/pins.json)\"\n          expected_jailer=\"$(jq -er --arg a \"$arch\" '.tarballs[$a].jailer_sha256' microvm/pins.json)\"\n          [ \"$fc_sha\" = \"$expected_fc\" ] || {{ echo \"::error::firecracker sha $fc_sha != pin $expected_fc\" >&2; exit 1; }}\n          [ \"$jailer_sha\" = \"$expected_jailer\" ] || {{ echo \"::error::jailer sha $jailer_sha != pin $expected_jailer\" >&2; exit 1; }}\n          {cargo_cmd} run --locked --release --package {package} --bin {image_bin} -- \\\n            stage --root \"$root\" --arch \"$arch\" \\\n            --rootfs-sha256 \"$rootfs_sha256\" \\\n            --guest-agent-sha256 \"$guest_agent_sha256\"\n          kernel=\"$(jq -er '.kernel' \"$root/manifest.json\")\"\n          rootfs=\"$(jq -er '.rootfs' \"$root/manifest.json\")\"\n          case \"$kernel$rootfs\" in\n            *UNSET*) echo \"::error::staged kernel/rootfs checksum is still UNSET\" >&2; exit 1 ;;\n          esac\n          [ \"${{#kernel}}\" -eq 64 ] || {{ echo \"::error::kernel sha256 length ${{#kernel}}\" >&2; exit 1; }}\n          [ \"${{#rootfs}}\" -eq 64 ] || {{ echo \"::error::rootfs sha256 length ${{#rootfs}}\" >&2; exit 1; }}\n"
    )
}

/// The empty-deb-incident guards: version and architecture match the lane
/// identity, the deb carries exactly one runner binary, it is not
/// suspiciously small, and the packaged package record is byte-identical to
/// the emitted one and names the packaged runner binary. The record is
/// located by basename, so any install layout the manifest declares passes.
fn debian_guard_steps(
    release_dir: &str,
    binary: &str,
    stem: &str,
    preview_crate_check: bool,
) -> String {
    let release_dir = shell_quote(release_dir);
    let crate_check = if preview_crate_check {
        "          dpkg --compare-versions \"$VERSION\" lt \"$CRATE_VERSION\" \\\n            || { echo \"::error::preview version $VERSION must compare older than its crate version\" >&2; exit 1; }\n"
    } else {
        ""
    };
    format!(
        "      - name: Guard the deb with the empty-deb-incident checks\n        run: |\n          set -euo pipefail\n          deb=\"dist/{stem}-${{{{ matrix.arch }}}}.deb\"\n          test -f \"$deb\" || {{ echo \"::error::missing expected deb $deb\" >&2; exit 1; }}\n          [ \"$(dpkg-deb -f \"$deb\" Version)\" = \"$VERSION\" ] || {{ echo \"::error::deb version != lane version\" >&2; exit 1; }}\n          [ \"$(dpkg-deb -f \"$deb\" Architecture)\" = \"${{{{ matrix.arch }}}}\" ] || {{ echo \"::error::deb arch != ${{{{ matrix.arch }}}}\" >&2; exit 1; }}\n          manifest=\"$(mktemp)\"\n          dpkg-deb -c \"$deb\" > \"$manifest\"\n          awk '$NF == \"./usr/bin/{binary}\" {{ count++; if (substr($1, 1, 1) != \"-\") type_ok=0; else if (count == 1) type_ok=1 }} END {{ exit !(count == 1 && type_ok == 1) }}' \"$manifest\" \\\n            || {{ echo \"::error::deb must contain exactly one regular usr/bin/{binary}\" >&2; exit 1; }}\n          [ \"$(stat -c%s \"$deb\")\" -gt 1000000 ] || {{ echo \"::error::deb suspiciously small: $(stat -c%s \"$deb\") bytes\" >&2; exit 1; }}\n          record_path=\"$(awk '$NF ~ /(^|\\/)package-record\\.json$/ {{ print $NF }}' \"$manifest\")\"\n          [ -n \"$record_path\" ] || {{ echo \"::error::deb missing its package record\" >&2; exit 1; }}\n          [ \"$(printf '%s\\n' \"$record_path\" | wc -l | tr -d ' ')\" -eq 1 ] || {{ echo \"::error::deb carries more than one package record\" >&2; exit 1; }}\n          dpkg-deb --fsys-tarfile \"$deb\" | tar -xOf - \"$record_path\" \\\n            | cmp - {release_dir}/package-record.json \\\n            || {{ echo \"::error::packaged package-record.json differs from the emitted record\" >&2; exit 1; }}\n          record_sha256=\"$(dpkg-deb --fsys-tarfile \"$deb\" | tar -xOf - \"$record_path\" | sha256sum | awk '{{print $1}}')\"\n          [ \"$record_sha256\" = \"$(awk 'NF {{print $1; exit}}' {release_dir}/package-record.json.sha256)\" ] \\\n            || {{ echo \"::error::packaged package-record.json digest != emitted sidecar\" >&2; exit 1; }}\n          packaged_binary_sha256=\"$(dpkg-deb --fsys-tarfile \"$deb\" | tar -xOf - ./usr/bin/{binary} | sha256sum | awk '{{print $1}}')\"\n          [ \"$packaged_binary_sha256\" = \"$(dpkg-deb --fsys-tarfile \"$deb\" | tar -xOf - \"$record_path\" | jq -r '.architecture.binary_sha256')\" ] \\\n            || {{ echo \"::error::deb runner binary does not match the package record\" >&2; exit 1; }}\n{crate_check}"
    )
}

/// The identity Debian job: per-arch debs built from release-identity
/// binaries, with the acyclic identity files and the emitted package record
/// staged before `cargo deb`, and the empty-deb-incident guards after. A
/// contract target without a Debian architecture fails the job closed
/// instead of silently dropping an architecture.
/// The fail-closed Debian job: a contract target without a Debian
/// architecture fails the lane loudly instead of silently dropping it.
fn render_undebianable_job(config: &ProjectConfig, release: &ReleaseSpec, needs: &str) -> String {
    let targets = release.targets.join(", ");
    format!(
        "  debian:\n    name: Package Debian artifacts\n    needs: [{needs}]\n    runs-on: {runner}\n    timeout-minutes: 5\n    steps:\n      - name: Reject undebianable target\n        run: |\n          echo '::error::native Debian packaging supports x86_64/aarch64 linux only, found {targets}' >&2\n          exit 1\n",
        runner = yaml_scalar(&config.github_runner),
    )
}

fn identity_debian_lane_env(preview: bool, version: &str) -> String {
    if preview {
        format!(
            "      VERSION: {version}\n      CRATE_VERSION: ${{{{ needs.identity.outputs.crate_version }}}}\n      SOURCE_COMMIT: ${{{{ needs.identity.outputs.commit }}}}\n      VELNOR_RELEASE_BUILD: \"1\"\n      VELNOR_PREVIEW_SOURCE_SHA: ${{{{ needs.identity.outputs.commit }}}}\n"
        )
    } else {
        format!("      VERSION: {version}\n      VELNOR_RELEASE_BUILD: \"1\"\n")
    }
}

/// The stable deb reuses the build job's release binary instead of building
/// a second one: one binary feeds the tarball, the record, and the deb,
/// never two builds that merely should agree. The recorded digest binds the
/// reuse to the same sidecar the release record is assembled from.
fn debian_reuse_release_steps(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    format!(
        "      - name: Download release binary\n        uses: {download}\n        with:\n          name: {lane}-${{{{ matrix.target }}}}\n          path: build-artifacts\n      - name: Reuse the build job's release binary\n        run: |\n          set -euo pipefail\n          tarball=\"build-artifacts/{binary}-$VERSION-${{{{ matrix.target }}}}.tar.gz\"\n          bin_sidecar=\"build-artifacts/{binary}-${{{{ matrix.arch }}}}.bin.sha256\"\n          test -f \"$tarball\" || {{ echo \"::error::missing build tarball $tarball\" >&2; exit 1; }}\n          test -f \"$tarball.sha256\" || {{ echo \"::error::missing tarball checksum $tarball.sha256\" >&2; exit 1; }}\n          test -f \"$bin_sidecar\" || {{ echo \"::error::missing binary checksum $bin_sidecar\" >&2; exit 1; }}\n          digest=\"$(sha256sum \"$tarball\" | awk '{{print $1}}')\"\n          sidecar=\"$(awk 'NF {{print $1; exit}}' \"$tarball.sha256\")\"\n          [ \"$digest\" = \"$sidecar\" ] || {{ echo \"::error::build tarball fails its sidecar checksum\" >&2; exit 1; }}\n          recorded=\"$(awk 'NF {{print $1; exit}}' \"$bin_sidecar\")\"\n          case \"$recorded\" in\n            ''|*[!0-9a-f]*) echo \"::error::recorded binary digest is not lowercase hex\" >&2; exit 1 ;;\n          esac\n          [ \"${{#recorded}}\" -eq 64 ] || {{ echo \"::error::recorded binary digest has invalid length\" >&2; exit 1; }}\n          mkdir -p \"target/$TARGET/release\"\n          tar -xzf \"$tarball\" -C \"target/$TARGET/release\" \"{binary}\"\n          reused=\"target/$TARGET/release/{binary}\"\n          chmod 0755 \"$reused\"\n          test -x \"$reused\"\n          actual=\"$(sha256sum \"$reused\" | awk '{{print $1}}')\"\n          [ \"$actual\" = \"$recorded\" ] || {{ echo \"::error::reused runner binary does not match the recorded digest\" >&2; exit 1; }}\n",
        download = ActionPin::DownloadArtifact.reference(),
        lane = canonical_lane(config),
        binary = yaml_scalar(&release.binary),
    )
}

fn render_identity_debian_job(
    config: &ProjectConfig,
    release: &ReleaseSpec,
    guest: bool,
    preview: bool,
) -> String {
    let package = yaml_scalar(&release.package);
    let binary = yaml_scalar(&release.binary);
    let checkout = ActionPin::Checkout.reference();
    let download = ActionPin::DownloadArtifact.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let sccache = ActionPin::Sccache.reference();
    let mut setup = workflow_runtime_setup_for_config(config);
    let workflow = WorkflowIr::from_config(config);
    let cargo_cmd = if let Some(unit) = rust_package_unit(config, &release.package) {
        workflow.render_tool_provisioning(&mut setup, RunnerMode::Github, unit, false);
        "mbx"
    } else {
        "cargo"
    };
    let mut needs = if preview {
        vec!["identity".to_owned(), "metadata".to_owned()]
    } else {
        vec![
            "admit-runner".to_owned(),
            "verify".to_owned(),
            "build".to_owned(),
            "metadata".to_owned(),
        ]
    };
    if guest {
        needs.push("guest-payload".to_owned());
    }
    let needs = needs.join(", ");
    let Some(matrix) = deb_arch_matrix(config, &release.targets, guest) else {
        return render_undebianable_job(config, release, &needs);
    };
    let (name, gate, checkout_ref, version, stem, kind, crate_expr, retention) = if preview {
        (
            "Build ${{ matrix.arch }} preview deb",
            format!(
                "    if: ${{{{ github.ref == 'refs/heads/{}' }}}}\n",
                config.default_branch
            ),
            "          ref: ${{ needs.identity.outputs.commit }}\n",
            "${{ needs.identity.outputs.version }}",
            format!("{}-preview", release.package),
            "preview",
            "\"$CRATE_VERSION\"",
            1,
        )
    } else {
        (
            "Build ${{ matrix.arch }} deb (release-build)",
            String::new(),
            "",
            "${{ needs.verify.outputs.version }}",
            release.package.clone(),
            "stable",
            "\"$VERSION\"",
            2,
        )
    };
    let lane_env = identity_debian_lane_env(preview, version);
    let metadata_artifact = if preview {
        "preview-metadata"
    } else {
        "release-metadata"
    };
    let mut steps = format!(
        "      - name: Checkout\n        uses: {checkout}\n        with:\n{checkout_ref}          persist-credentials: false\n{setup}      - name: Add Rust target\n        run: rustup target add \"$TARGET\"\n      - name: Set up sccache\n        uses: {sccache}\n        with:\n          version: v0.16.0\n      - name: Install cargo-deb\n        env:\n          CARGO_INCREMENTAL: \"0\"\n          RUSTC_WRAPPER: sccache\n        run: |\n          set -euo pipefail\n          cargo install cargo-deb --version 3.7.0 --locked\n          cargo-deb --version\n      - name: Download release metadata\n        uses: {download}\n        with:\n          name: {metadata_artifact}\n          path: metadata\n",
    );
    if preview {
        let _ = writeln!(
            steps,
            "      - name: Build release runner binary\n        env:\n          CARGO_INCREMENTAL: \"0\"\n          RUSTC_WRAPPER: sccache\n        run: |\n          set -euo pipefail\n          {cargo_cmd} build --locked --release --package {package} --bin {binary} --features release-build --target \"$TARGET\""
        );
    } else {
        steps.push_str(&debian_reuse_release_steps(config, release));
    }
    steps.push_str(&debian_sibling_build_steps(cargo_cmd, &release.package));
    steps.push_str(&debian_identity_steps(
        &release_package_dir(config, &release.package),
        &release.binary,
        &config.repository,
        kind,
        crate_expr,
        preview,
    ));
    if guest {
        steps.push_str(&debian_guest_steps(config, release, cargo_cmd));
    }
    let versioned_stem = format!("{stem}-{version}");
    let asset_name = format!("{versioned_stem}-${{{{ matrix.arch }}}}.deb");
    let _ = writeln!(
        steps,
        "      - name: Build Debian packages\n        run: |\n          set -euo pipefail\n          velnor-workflow release package-deb --package {package} --version \"$VERSION\" --no-build true --asset-name \"{asset_name}\" --target \"$TARGET\""
    );
    steps.push_str(&debian_guard_steps(
        &release_package_dir(config, &release.package),
        &release.binary,
        &versioned_stem,
        preview,
    ));
    format!(
        "  debian:\n    name: {name}\n    needs: [{needs}]\n{gate}    runs-on: {runner}\n    timeout-minutes: 90\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    env:\n      TARGET: ${{{{ matrix.target }}}}\n{lane_env}    permissions:\n      contents: read\n    steps:\n{steps}      - name: Upload Debian packages\n        uses: {upload}\n        with:\n          name: debian-packages\n          path: |\n            dist/*.deb\n            dist/*.deb.sha256\n          if-no-files-found: error\n          retention-days: {retention}\n",
        runner = release_matrix_runner(config, &release.targets),
    )
}

/// One signer call per Debian architecture: the shared package signer
/// attests the exact bytes the debian job uploaded, binding the lane's
/// source ref. The consumer lane verifies these attestations before it
/// binds any deb, so attestation never stays an inline afterthought.
fn render_sign_deb_job(
    config: &ProjectConfig,
    release: &ReleaseSpec,
    preview: bool,
) -> Option<String> {
    let arches = deb_architectures(&release.targets)?;
    let mut matrix = String::new();
    for (arch, _) in &arches {
        let _ = writeln!(matrix, "          - arch: {arch}");
    }
    let (name, needs, gate, stem, version, source_ref) = if preview {
        (
            "Sign ${{ matrix.arch }} preview deb",
            "    needs: [identity, debian]\n",
            format!(
                "    if: ${{{{ github.ref == 'refs/heads/{}' }}}}\n",
                config.default_branch
            ),
            format!("{}-preview", release.package),
            "${{ needs.identity.outputs.version }}",
            format!("refs/heads/{}", config.default_branch),
        )
    } else {
        (
            "Sign ${{ matrix.arch }} Debian package",
            "    needs: [verify, debian]\n",
            String::new(),
            release.package.clone(),
            "${{ needs.verify.outputs.version }}",
            "refs/tags/${{ github.ref_name }}".to_owned(),
        )
    };
    Some(format!(
        "  sign-deb:\n    name: {name}\n{needs}{gate}    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    permissions:\n      attestations: write\n      contents: read\n      id-token: write\n    uses: ./.github/workflows/ci-release-package-signer.yml\n    with:\n      artifact-name: debian-packages\n      subject-path: {stem}-{version}-${{{{ matrix.arch }}}}.deb\n      source-ref: {source_ref}\n"
    ))
}

/// The multi-arch platform matrix: one native builder per consumer
/// architecture. The release record and the package consumer demand exactly
/// amd64+arm64; any other target set fails the lane closed instead of
/// shipping a partial index.
fn image_platform_matrix(config: &ProjectConfig, targets: &[String]) -> Option<String> {
    let arches = deb_architectures(targets)?;
    let mut have: Vec<&str> = arches.iter().map(|(arch, _)| *arch).collect();
    have.sort_unstable();
    if have.as_slice() != ["amd64", "arm64"] {
        return None;
    }
    let mut matrix = String::new();
    for (arch, _) in &arches {
        let runner = match *arch {
            "amd64" => yaml_scalar(&config.github_runner),
            // GitHub's hosted arm64 label; the release contract names it and
            // no second hosted label exists to configure.
            _ => "ubuntu-24.04-arm".to_owned(),
        };
        let _ = writeln!(
            matrix,
            "          - arch: {arch}\n            platform: linux/{arch}\n            runner: {runner}"
        );
    }
    Some(matrix)
}

/// The image admission gate: inspect the version tag without mutating it. An
/// absent tag opens the platform lane; a present tag is adopted only with a
/// matching GitHub release (or an explicitly supplied recovery digest), so a
/// version tag is never clobbered and unknown bytes are never adopted.
fn render_image_admission_job(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let buildx = ActionPin::DockerBuildx.reference();
    let login = ActionPin::DockerLogin.reference();
    format!(
        "  image-admission:\n    needs: [admit-runner, verify]\n    name: Admit immutable image tag\n    timeout-minutes: 10\n    runs-on: {runner}\n    permissions:\n      contents: read\n      packages: read\n    outputs:\n      existing: ${{{{ steps.inspect.outputs.existing }}}}\n      index_digest: ${{{{ steps.inspect.outputs.index_digest }}}}\n    steps:\n      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n      - name: Log in to GHCR\n        uses: {login}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ secrets.GITHUB_TOKEN }}}}\n      - name: Inspect version tag without mutation\n        id: inspect\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n          GHCR_IMAGE: {image}\n          VERSION: ${{{{ needs.verify.outputs.version }}}}\n          REPOSITORY: ${{{{ github.repository }}}}\n          RECOVERY_INDEX_DIGEST: ${{{{ github.event_name == 'workflow_dispatch' && inputs.existing-image-digest || '' }}}}\n        run: |\n          set -euo pipefail\n          ref=\"${{GHCR_IMAGE}}:${{VERSION}}\"\n          error_file=\"$(mktemp)\"\n          if docker buildx imagetools inspect \"$ref\" --format '{{{{json .}}}}' > image.json 2>\"$error_file\"; then\n            index_digest=\"$(jq -er '.manifest.digest' image.json)\"\n            case \"$index_digest\" in\n              sha256:[0-9a-fA-F]*) ;;\n              *) echo \"::error::existing image tag $ref returned an invalid index digest\" >&2; exit 1 ;;\n            esac\n            if ! gh release view \"v${{VERSION}}\" --repo \"$REPOSITORY\" >/dev/null 2>&1; then\n              if [ -z \"$RECOVERY_INDEX_DIGEST\" ] || [ \"$index_digest\" != \"$RECOVERY_INDEX_DIGEST\" ]; then\n                echo \"::error::OCI tag $ref already exists without a matching GitHub release; refusing to adopt unknown bytes\" >&2\n                exit 1\n              fi\n              echo \"adopting explicitly supplied recovery index $index_digest\"\n            fi\n            {{\n              echo \"existing=true\"\n              echo \"index_digest=$index_digest\"\n            }} >> \"$GITHUB_OUTPUT\"\n          elif grep -Eiq 'manifest unknown|no such manifest|not found|name unknown' \"$error_file\"; then\n            {{\n              echo \"existing=false\"\n              echo \"index_digest=\"\n            }} >> \"$GITHUB_OUTPUT\"\n          else\n            cat \"$error_file\" >&2\n            echo \"::error::could not determine whether OCI tag $ref exists; refusing a fail-open publish\" >&2\n            exit 1\n          fi\n",
        runner = yaml_scalar(&config.github_runner),
        image = release.image,
    )
}

/// The per-arch OCI platform lane: each native builder compiles the workflow
/// binary the job image embeds, then builds and pushes its platform image
/// under a disposable commit-scoped staging tag. Version tags are admitted
/// and promoted only by the index job; staging tags can never overwrite a
/// consumer-facing tag.
fn render_image_platform_job(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let needs = "admit-runner, verify, metadata, image-admission";
    let gate = "    if: ${{ needs.image-admission.outputs.existing != 'true' }}\n";
    let Some(matrix) = image_platform_matrix(config, &release.targets) else {
        return format!(
            "  image-platform:\n    name: Build ${{{{ matrix.arch }}}} GHCR image\n    needs: [{needs}]\n{gate}    runs-on: {runner}\n    timeout-minutes: 5\n    steps:\n      - name: Reject non-multi-arch image contract\n        run: |\n          echo '::error::native OCI lane needs exactly x86_64+aarch64 linux targets' >&2\n          exit 1\n",
            runner = yaml_scalar(&config.github_runner),
        );
    };
    let checkout = ActionPin::Checkout.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let buildx = ActionPin::DockerBuildx.reference();
    let login = ActionPin::DockerLogin.reference();
    let build = ActionPin::DockerBuild.reference();
    let mut setup = String::new();
    let workflow = WorkflowIr::from_config(config);
    let cargo_cmd = if let Some(unit) = rust_package_unit(config, IMAGE_WORKFLOW_PACKAGE) {
        workflow.render_tool_provisioning(&mut setup, RunnerMode::Github, unit, false);
        "mbx"
    } else {
        "cargo"
    };
    format!(
        "  image-platform:\n    name: Build ${{{{ matrix.arch }}}} GHCR image\n    needs: [{needs}]\n{gate}    timeout-minutes: 60\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    runs-on: ${{{{ matrix.runner }}}}\n    permissions:\n      contents: read\n      packages: write\n    env:\n      GHCR_IMAGE: {image}\n      VERSION: ${{{{ needs.verify.outputs.version }}}}\n      COMMIT: ${{{{ github.sha }}}}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n          ref: ${{{{ github.sha }}}}\n          fetch-depth: 1\n          persist-credentials: false\n{setup}      - name: Build the workflow binary for the image\n        env:\n          CARGO_INCREMENTAL: \"0\"\n        run: |\n          set -euo pipefail\n          {cargo_cmd} build --locked --release --package {workflow_package}\n          binary=\"release-binaries/${{{{ matrix.arch }}}}/{workflow_package}\"\n          mkdir -p \"release-binaries/${{{{ matrix.arch }}}}\"\n          cp \"target/release/{workflow_package}\" \"$binary\"\n          chmod 0755 \"$binary\"\n          test -x \"$binary\"\n      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n          keep-state: true\n      - name: Log in to GHCR\n        uses: {login}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ secrets.GITHUB_TOKEN }}}}\n      - name: Build + push platform image\n        id: push\n        uses: {build}\n        with:\n          context: .\n          file: {dockerfile}\n          platforms: ${{{{ matrix.platform }}}}\n          push: true\n          cache-from: |\n            type=registry,ref=${{{{ env.GHCR_IMAGE }}}}:buildcache-${{{{ matrix.arch }}}}\n            type=gha,scope=velnor-job-ubuntu-${{{{ matrix.arch }}}}\n          cache-to: |\n            type=registry,ref=${{{{ env.GHCR_IMAGE }}}}:buildcache-${{{{ matrix.arch }}}},mode=max\n            type=gha,scope=velnor-job-ubuntu-${{{{ matrix.arch }}}},mode=max\n          secrets: |\n            mise_github_token=${{{{ github.token }}}}\n          build-args: |\n            VELNOR_IMAGE_VERSION=${{{{ needs.verify.outputs.version }}}}\n          tags: ${{{{ env.GHCR_IMAGE }}}}:release-${{{{ env.COMMIT }}}}-${{{{ matrix.arch }}}}\n          labels: |\n            org.opencontainers.image.version=${{{{ needs.verify.outputs.version }}}}\n            org.opencontainers.image.revision=${{{{ github.sha }}}}\n            org.opencontainers.image.source={source_url}\n            org.velnor.manifest-sha256=${{{{ needs.metadata.outputs.manifest_sha256 }}}}\n      - name: Record platform digest\n        env:\n          ARCH: ${{{{ matrix.arch }}}}\n        run: |\n          set -euo pipefail\n          docker buildx imagetools inspect \\\n            \"${{GHCR_IMAGE}}:release-${{COMMIT}}-${{ARCH}}\" \\\n            --format '{{{{json .}}}}' > image-inspect.json\n          PLATFORM_DIGEST=\"$(jq -er --arg arch \"$ARCH\" '\n            [.manifest.manifests[]\n             | select(.platform.architecture == $arch and .platform.os == \"linux\")\n             | select((.annotations[\"vnd.docker.reference.type\"] // \"\") != \"attestation-manifest\")\n             | .digest]\n            | if length == 1 then .[0] else error(\"expected one image manifest for architecture\") end\n          ' image-inspect.json)\"\n          case \"$PLATFORM_DIGEST\" in\n            sha256:[0-9a-fA-F]*) ;;\n            *) echo \"::error::staging image inspection did not return a platform digest\" >&2; exit 1 ;;\n          esac\n          printf '%s\\n' \"$PLATFORM_DIGEST\" > \"image-${{{{ matrix.arch }}}}.digest\"\n      - name: Upload platform digest\n        uses: {upload}\n        with:\n          name: image-platform-${{{{ matrix.arch }}}}\n          path: image-${{{{ matrix.arch }}}}.digest\n          if-no-files-found: error\n          retention-days: 2\n",
        image = release.image,
        source_url = release_source_url(release),
        workflow_package = IMAGE_WORKFLOW_PACKAGE,
        dockerfile = IMAGE_DOCKERFILE,
    )
}

/// The index job: assemble the two staging platforms into one immutable
/// multi-arch version tag (or re-verify an admitted one), and export the
/// index digest the release record binds. The job runs whenever its inputs
/// are trustworthy — including when the platform lane correctly skipped —
/// but never when admission itself failed.
fn render_image_index_job(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let download = ActionPin::DownloadArtifact.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let buildx = ActionPin::DockerBuildx.reference();
    let login = ActionPin::DockerLogin.reference();
    format!(
        "  image:\n    if: ${{{{ always() && needs.admit-runner.result == 'success' && needs.verify.result == 'success' && needs.metadata.result == 'success' && needs.image-admission.result == 'success' && (needs.image-platform.result == 'success' || needs.image-platform.result == 'skipped') }}}}\n    needs: [admit-runner, verify, metadata, image-platform, image-admission]\n    name: Assemble one multi-platform GHCR image\n    timeout-minutes: 15\n    runs-on: {runner}\n    permissions:\n      contents: read\n      packages: write\n    outputs:\n      index_digest: ${{{{ steps.push.outputs.index_digest }}}}\n      manifest_sha256: ${{{{ needs.metadata.outputs.manifest_sha256 }}}}\n    env:\n      GHCR_IMAGE: {image}\n      SOURCE_URL: {source_url}\n      VERSION: ${{{{ needs.verify.outputs.version }}}}\n      COMMIT: ${{{{ github.sha }}}}\n    steps:\n      - name: Download platform digests\n        if: ${{{{ needs.image-admission.outputs.existing != 'true' }}}}\n        uses: {download}\n        with:\n          pattern: image-platform-*\n          path: image-artifacts\n          merge-multiple: true\n      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n      - name: Log in to GHCR\n        uses: {login}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ secrets.GITHUB_TOKEN }}}}\n      - name: Assemble and inspect immutable image index\n        id: push\n        env:\n          AMD64_DIGEST_FILE: image-artifacts/image-amd64.digest\n          ARM64_DIGEST_FILE: image-artifacts/image-arm64.digest\n          IMAGE_ALREADY_EXISTS: ${{{{ needs.image-admission.outputs.existing }}}}\n          EXPECTED_EXISTING_INDEX_DIGEST: ${{{{ needs.image-admission.outputs.index_digest }}}}\n        run: |\n          set -euo pipefail\n          if [ \"$IMAGE_ALREADY_EXISTS\" = true ]; then\n            docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > image-digests.json\n            index_digest=\"$(jq -er '.manifest.digest' image-digests.json)\"\n            [ \"$index_digest\" = \"$EXPECTED_EXISTING_INDEX_DIGEST\" ] || {{\n              echo \"::error::version tag moved from $EXPECTED_EXISTING_INDEX_DIGEST to $index_digest during admission\" >&2\n              exit 1\n            }}\n          else\n            for path in \"$AMD64_DIGEST_FILE\" \"$ARM64_DIGEST_FILE\"; do\n              test -s \"$path\"\n              digest=\"$(tr -d '[:space:]' < \"$path\")\"\n              case \"$digest\" in\n                sha256:[0-9a-fA-F]*) ;;\n                *) echo \"::error::invalid platform digest in $path\" >&2; exit 1 ;;\n              esac\n            done\n            amd64_digest=\"$(tr -d '[:space:]' < \"$AMD64_DIGEST_FILE\")\"\n            arm64_digest=\"$(tr -d '[:space:]' < \"$ARM64_DIGEST_FILE\")\"\n            docker buildx imagetools create \\\n              --tag \"${{GHCR_IMAGE}}:${{VERSION}}\" \\\n              \"${{GHCR_IMAGE}}:release-${{COMMIT}}-amd64\" \\\n              \"${{GHCR_IMAGE}}:release-${{COMMIT}}-arm64\"\n            docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > image-digests.json\n            jq -e --arg amd \"$amd64_digest\" --arg arm \"$arm64_digest\" '\n              any(.manifest.manifests[]; .digest == $amd and .platform.architecture == \"amd64\") and\n              any(.manifest.manifests[]; .digest == $arm and .platform.architecture == \"arm64\")\n            ' image-digests.json >/dev/null || {{\n              echo \"::error::version tag does not reference both newly built platform digests\" >&2\n              exit 1\n            }}\n          fi\n          docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > image-digests.json\n          index_digest=\"$(jq -er '.manifest.digest' image-digests.json)\"\n          case \"$index_digest\" in\n            sha256:[0-9a-fA-F]*) ;;\n            *) echo \"::error::manifest inspection did not return an index digest\" >&2; exit 1 ;;\n          esac\n          printf 'index_digest=%s\\n' \"$index_digest\" >> \"$GITHUB_OUTPUT\"\n          printf '%s\\n' \"$index_digest\" > image-index.digest\n      - name: Download release metadata\n        uses: {download}\n        with:\n          name: release-metadata\n      - name: Upload image digests\n        uses: {upload}\n        with:\n          name: image-digests\n          path: |\n            image-digests.json\n            image-index.digest\n          if-no-files-found: error\n          retention-days: 2\n",
        runner = yaml_scalar(&config.github_runner),
        image = release.image,
        source_url = release_source_url(release),
    )
}

/// The strict single-token checksum-sidecar reader both record steps share:
/// the sidecar must exist, fit in 4 KiB, and hold exactly one 64-hex token.
fn read_sha_fn(indent: &str, path_expr: &str) -> String {
    format!(
        "{indent}read_sha() {{\n{indent}  local path=\"{path_expr}\" size token\n{indent}  [ -f \"$path\" ] || {{ echo \"::error::missing checksum sidecar: $path\" >&2; return 1; }}\n{indent}  size=\"$(stat -c%s \"$path\")\"\n{indent}  [ \"$size\" -le 4096 ] || {{ echo \"::error::checksum sidecar exceeds 4096 bytes: $path\" >&2; return 1; }}\n{indent}  token=\"$(awk '\n{indent}    {{\n{indent}      for (field = 1; field <= NF; field++) {{\n{indent}        token_count++\n{indent}        if (token_count == 1) token = $field\n{indent}      }}\n{indent}    }}\n{indent}    END {{\n{indent}      if (token_count != 1) exit 1\n{indent}      print token\n{indent}    }}\n{indent}  ' \"$path\")\" || {{ echo \"::error::checksum sidecar must contain one token: $path\" >&2; return 1; }}\n{indent}  [ \"${{#token}}\" -eq 64 ] || {{ echo \"::error::invalid checksum sidecar: $path\" >&2; return 1; }}\n{indent}  case \"$token\" in\n{indent}    *[!0-9a-f]*) echo \"::error::invalid checksum sidecar: $path\" >&2; return 1 ;;\n{indent}  esac\n{indent}  printf '%s\\n' \"$token\"\n{indent}}}\n",
    )
}

/// The native publish job: both lanes' artifacts (tarballs plus the Debian
/// packages the plain publisher drops) are downloaded, their provenance is
/// re-verified — debs against the shared signer — and the release ships the
/// debs, the consumer manifest, and the independent checksums the package
/// consumer binds. The manifest schema is the repository's declared
/// contract; without it the step fails closed instead of stamping a guess.
///
/// Before anything is published, the job assembles the release record from
/// the downloaded bytes, re-verifies it through the release tool, proves the
/// packaged binaries match the record, and proves the OCI index and the
/// release tag stayed immutable. The release itself is created once and
/// never clobbered: a re-run against an existing tag re-verifies the
/// published bytes instead of uploading a non-reproducible rebuild.
/// The publish asset lists: the explicit `gh release create` subjects, the
/// shell-expanded idempotency re-download set, and the arch/target tuples
/// the per-arch re-verification loop walks. Tarballs keep their
/// target-triple names, debs their arch names.
fn native_publish_asset_lists(release: &ReleaseSpec, version: &str) -> (String, String, String) {
    let arches = deb_architectures(&release.targets).unwrap_or_default();
    let mut assets = vec![
        "release-record.json".to_owned(),
        "release-record.json.sha256".to_owned(),
        "manifest.json".to_owned(),
        "manifest.json.sha256".to_owned(),
        "release-manifest.json".to_owned(),
        "SHA256SUMS".to_owned(),
    ];
    for target in &release.targets {
        assets.push(format!(
            "artifacts/{}-{version}-{target}.tar.gz",
            release.binary
        ));
        assets.push(format!(
            "artifacts/{}-{version}-{target}.tar.gz.sha256",
            release.binary
        ));
    }
    for (arch, _) in &arches {
        assets.push(format!(
            "artifacts/{}-{version}-{arch}.deb",
            release.package
        ));
        assets.push(format!(
            "artifacts/{}-{version}-{arch}.deb.sha256",
            release.package
        ));
    }
    let assets = assets.join(" \\\n              ");
    let mut published = vec![
        "release-record.json".to_owned(),
        "release-record.json.sha256".to_owned(),
        "release-manifest.json".to_owned(),
        "manifest.json".to_owned(),
        "manifest.json.sha256".to_owned(),
        "SHA256SUMS".to_owned(),
    ];
    for target in &release.targets {
        published.push(format!(
            "\"{}-${{VERSION}}-{target}.tar.gz\"",
            release.binary
        ));
        published.push(format!(
            "\"{}-${{VERSION}}-{target}.tar.gz.sha256\"",
            release.binary
        ));
    }
    for (arch, _) in &arches {
        published.push(format!("\"{}-${{VERSION}}-{arch}.deb\"", release.package));
        published.push(format!(
            "\"{}-${{VERSION}}-{arch}.deb.sha256\"",
            release.package
        ));
    }
    let published = published.join(" \\\n                        ");
    let arch_targets = arches
        .iter()
        .map(|(arch, target)| format!("'{arch} {target}'"))
        .collect::<Vec<_>>()
        .join(" ");
    (assets, published, arch_targets)
}

fn render_native_publish_job(
    config: &ProjectConfig,
    release: &ReleaseSpec,
    needs: &[String],
) -> String {
    let checkout = ActionPin::Checkout.reference();
    let download = ActionPin::DownloadArtifact.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let buildx = ActionPin::DockerBuildx.reference();
    let login = ActionPin::DockerLogin.reference();
    let needs = needs.join(", ");
    let version = "${{ needs.verify.outputs.version }}";
    let (assets, published, arch_targets) = native_publish_asset_lists(release, version);
    let subject_count = 2 * release.targets.len();
    let manifest_step = if release.manifest_schema.is_empty() {
        "      - name: Assemble consumer release manifest\n        run: |\n          echo '::error::native release with a package consumer needs manifest_schema; declare the consumer manifest schema URN' >&2\n          exit 1\n"
            .to_owned()
    } else {
        let schema = shell_quote(&release.manifest_schema);
        format!(
            "      - name: Assemble consumer release manifest\n        run: |\n          set -euo pipefail\n          jq -S -n \\\n            --arg schema {schema} \\\n            --arg source_repository \"$GITHUB_REPOSITORY\" \\\n            --arg source_ref \"$SOURCE_REF\" \\\n            --arg source_commit \"$SOURCE_COMMIT\" \\\n            --arg version \"$VERSION\" \\\n            --slurpfile assets assets.jsonl \\\n            '{{schema:$schema,source_repository:$source_repository,source_ref:$source_ref,source_commit:$source_commit,version:$version,assets:$assets}}' \\\n            > release-manifest.json\n"
        )
    };
    let record_assembly = format!(
        "      - name: Assemble the release record from downloaded artifacts\n        run: |\n          set -euo pipefail\n{read_sha}          bin_amd64=\"$(read_sha \"{binary}-amd64.bin.sha256\")\"\n          bin_arm64=\"$(read_sha \"{binary}-arm64.bin.sha256\")\"\n          deb_amd64=\"$(read_sha \"{package}-${{VERSION}}-amd64.deb.sha256\")\"\n          deb_arm64=\"$(read_sha \"{package}-${{VERSION}}-arm64.deb.sha256\")\"\n          # Per-platform OCI digests from the image job.\n          plat_amd64=\"$(jq -r '.manifest.manifests[] | select(.platform.architecture==\"amd64\") | .digest' artifacts/image-digests.json)\"\n          plat_arm64=\"$(jq -r '.manifest.manifests[] | select(.platform.architecture==\"arm64\") | .digest' artifacts/image-digests.json)\"\n          manifest_version=\"$(jq -er '.version | select(type == \"number\" and floor == . and . > 0)' artifacts/manifest.json)\"\n\n          jq -n \\\n            --arg schema \"{schema}\" \\\n            --arg repo {repo} \\\n            --arg tag \"v${{VERSION}}\" \\\n            --arg commit \"$COMMIT\" \\\n            --arg version \"$VERSION\" \\\n            --argjson mv \"$manifest_version\" \\\n            --arg mhash \"$MANIFEST_SHA256\" \\\n            --arg bin_amd64 \"$bin_amd64\" --arg deb_amd64 \"$deb_amd64\" --arg plat_amd64 \"$plat_amd64\" \\\n            --arg bin_arm64 \"$bin_arm64\" --arg deb_arm64 \"$deb_arm64\" --arg plat_arm64 \"$plat_arm64\" \\\n            --arg index \"$INDEX_DIGEST\" \\\n            --arg ref \"${{GHCR_IMAGE}}@${{INDEX_DIGEST}}\" \\\n            --arg source \"$SOURCE_URL\" \\\n            '{{\n              schema: $schema,\n              build: {{ repository: $repo, tag: $tag, commit: $commit, crate_version: $version,\n                       debian_version: $version, manifest_version: $mv, manifest_sha256: $mhash }},\n              architectures: [\n                {{ arch: \"amd64\", target: \"x86_64-unknown-linux-gnu\", binary_sha256: $bin_amd64, deb_sha256: $deb_amd64, oci_platform_digest: $plat_amd64 }},\n                {{ arch: \"arm64\", target: \"aarch64-unknown-linux-gnu\", binary_sha256: $bin_arm64, deb_sha256: $deb_arm64, oci_platform_digest: $plat_arm64 }}\n              ],\n              oci_index_digest: $index,\n              oci_image_ref: $ref,\n              oci_labels: {{ version: $version, revision: $commit, source: $source, manifest_sha256: $mhash }},\n              apt: {{ origin: \"Velnor\", suite: \"stable\", component: \"main\" }}\n            }}' > record.candidate.json\n",
        read_sha = read_sha_fn("          ", "artifacts/$1"),
        binary = release.binary,
        package = release.package,
        schema = RELEASE_RECORD_SCHEMA,
        repo = shell_quote(&release.source_repository),
    );
    let record_reverify = format!(
        "      - name: Re-verify the record + emit canonical bytes + checksum\n        run: |\n          set -euo pipefail\n          # Independent re-assembly + coherence check; writes canonical\n          # release-record.json + release-record.json.sha256 (the digest lives in\n          # the sidecar, never inside the record — acyclic).\n          chmod +x artifacts/{binary}-release-tool\n          artifacts/{binary}-release-tool release assemble \\\n            --record record.candidate.json \\\n            --artifacts artifacts \\\n            --out release-record.json\n          cp release-record.json.sha256 SHA256SUMS.record\n          # Publish the compiled manifest + its checksum too, so {consumer} can\n          # verify sha256(manifest.json) == record.build.manifest_sha256 and that\n          # the manifest's embedded source_sha matches the record commit.\n          cp artifacts/manifest.json manifest.json\n          sha256sum manifest.json | awk '{{print $1}}' > manifest.json.sha256\n",
        binary = release.binary,
        consumer = release.consumer_repository,
    );
    let packaged_identity = format!(
        "      - name: Verify packaged runner identity before release creation\n        run: |\n          set -euo pipefail\n          for arch in amd64 arm64; do\n            deb=\"artifacts/{package}-${{VERSION}}-${{arch}}.deb\"\n            record_bin=\"$(jq -er --arg arch \"$arch\" '\n              [.architectures[] | select(.arch == $arch)]\n              | if length == 1 then .[0].binary_sha256 else error(\"record must contain exactly one architecture\") end\n            ' release-record.json)\"\n            case \"$record_bin\" in\n              *[!0-9a-f]*|'') echo \"::error::record has invalid runner binary sha256 ($arch)\" >&2; exit 1 ;;\n            esac\n            [ \"${{#record_bin}}\" -eq 64 ] || {{ echo \"::error::record has invalid runner binary sha256 ($arch)\" >&2; exit 1; }}\n            packaged_binary_sha=\"$(dpkg-deb --fsys-tarfile \"$deb\" | tar -xOf - ./usr/bin/{binary} | sha256sum | awk '{{print $1}}')\"\n            [ \"$packaged_binary_sha\" = \"$record_bin\" ] || {{\n              echo \"::error::packaged /usr/bin/{binary} digest != release record ($arch)\" >&2\n              exit 1\n            }}\n          done\n",
        binary = release.binary,
        package = release.package,
    );
    let create_verify = format!(
        "      - name: Create release once — no clobber, record-verified idempotency\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          tag=\"v${{VERSION}}\"\n          # Fail (never mint at main HEAD) if there is nothing to publish.\n          [ -f release-record.json ] || {{ echo \"::error::no assembled record\" >&2; exit 1; }}\n          if gh release view \"$tag\" >/dev/null 2>&1; then\n            # Existing release: rebuilds are NOT bit-reproducible (tar/deb/OCI\n            # payloads embed build metadata), so byte-equality against a fresh\n            # build can never hold and re-runs would fail forever. Instead\n            # verify the PUBLISHED release is coherent: its record names this\n            # exact tag/commit/version/manifest and every published asset\n            # matches the digests inside that record. Never clobber (N-immutable).\n            tmp=\"$(mktemp -d)\"\n            for name in {published}; do\n              gh release download \"$tag\" --pattern \"$name\" --dir \"$tmp\" --clobber >/dev/null 2>&1 \\\n                || {{ echo \"::error::release $tag exists but is missing $name — refusing partial overwrite\" >&2; exit 1; }}\n            done\n            # Sidecar checksums must match their payloads.\n{read_sha}            [ \"$(read_sha \"$tmp/release-record.json.sha256\")\" = \"$(sha256sum \"$tmp/release-record.json\" | awk '{{print $1}}')\" ] \\\n              || {{ echo \"::error::published release-record.json fails its sidecar checksum\" >&2; exit 1; }}\n            [ \"$(read_sha \"$tmp/manifest.json.sha256\")\" = \"$(sha256sum \"$tmp/manifest.json\" | awk '{{print $1}}')\" ] \\\n              || {{ echo \"::error::published manifest.json fails its sidecar checksum\" >&2; exit 1; }}\n            # The record must name this exact tag, source commit, crate version\n            # and the manifest this source tree compiles to (the compiled\n            # manifest is byte-reproducible; binaries are not).\n            [ \"$(jq -r '.build.tag' \"$tmp/release-record.json\")\" = \"$tag\" ] \\\n              || {{ echo \"::error::published record tag mismatch — refusing to touch $tag\" >&2; exit 1; }}\n            [ \"$(jq -r '.build.commit' \"$tmp/release-record.json\")\" = \"$COMMIT\" ] \\\n              || {{ echo \"::error::published record commit mismatch — tag may have moved\" >&2; exit 1; }}\n            [ \"$(jq -r '.build.crate_version' \"$tmp/release-record.json\")\" = \"$VERSION\" ] \\\n              || {{ echo \"::error::published record version mismatch\" >&2; exit 1; }}\n            [ \"$(jq -r '.build.manifest_sha256' \"$tmp/release-record.json\")\" = \"$(sha256sum \"$tmp/manifest.json\" | awk '{{print $1}}')\" ] \\\n              || {{ echo \"::error::published record/manifest incoherent\" >&2; exit 1; }}\n            [ \"$(jq -r '.build.manifest_sha256' \"$tmp/release-record.json\")\" = \"$MANIFEST_SHA256\" ] \\\n              || {{ echo \"::error::published record manifest != manifest compiled from $COMMIT\" >&2; exit 1; }}\n            expected_oci_index=\"$(jq -er '.oci_index_digest' \"$tmp/release-record.json\")\"\n            [ \"$expected_oci_index\" = \"$INDEX_DIGEST\" ] \\\n              || {{ echo \"::error::published record OCI index != current release image\" >&2; exit 1; }}\n            [ \"$(jq -r '.oci_image_ref' \"$tmp/release-record.json\")\" = \"${{GHCR_IMAGE}}@${{expected_oci_index}}\" ] \\\n              || {{ echo \"::error::published record OCI reference is not digest-pinned\" >&2; exit 1; }}\n            # Every published package asset must match the record's digests.\n            (cd \"$tmp\" && sha256sum --check --strict SHA256SUMS)\n            for tuple in {arch_targets}; do\n              read -r arch target <<<\"$tuple\"\n              tarball=\"{binary}-${{VERSION}}-${{target}}.tar.gz\"\n              deb=\"{package}-${{VERSION}}-${{arch}}.deb\"\n              record_deb=\"$(jq -r --arg arch \"$arch\" '.architectures[] | select(.arch == $arch) | .deb_sha256' \"$tmp/release-record.json\")\"\n              record_bin=\"$(jq -r --arg arch \"$arch\" '.architectures[] | select(.arch == $arch) | .binary_sha256' \"$tmp/release-record.json\")\"\n              [ \"$(read_sha \"$tmp/${{tarball}}.sha256\")\" = \"$(sha256sum \"$tmp/$tarball\" | awk '{{print $1}}')\" ] \\\n                || {{ echo \"::error::published tarball fails its sidecar checksum ($arch)\" >&2; exit 1; }}\n              [ \"$(read_sha \"$tmp/${{deb}}.sha256\")\" = \"$(sha256sum \"$tmp/$deb\" | awk '{{print $1}}')\" ] \\\n                || {{ echo \"::error::published deb fails its sidecar checksum ($arch)\" >&2; exit 1; }}\n              [ \"$(sha256sum \"$tmp/$deb\" | awk '{{print $1}}')\" = \"$record_deb\" ] \\\n                || {{ echo \"::error::published deb digest != record ($arch)\" >&2; exit 1; }}\n              mkdir -p \"$tmp/extract-$arch\"\n              tar -xzf \"$tmp/$tarball\" -C \"$tmp/extract-$arch\"\n              [ \"$(sha256sum \"$tmp/extract-$arch/{binary}\" | awk '{{print $1}}')\" = \"$record_bin\" ] \\\n                || {{ echo \"::error::published binary digest != record ($arch)\" >&2; exit 1; }}\n            done\n            # Already-published path verifies only and uploads nothing:\n            # attestation stays in the build/sign jobs over the fresh-build\n            # subjects staged above, never post-publish here.\n            echo \"Release $tag already published; record + assets verified coherent. Nothing to upload.\"\n          else\n            gh release create \"$tag\" --verify-tag --target \"$COMMIT\" --title \"$tag\" --generate-notes \\\n              {assets}\n          fi\n          # NOTE (N6): this workflow deliberately does NOT push to {consumer} or\n          # dispatch its publish. {consumer} pulls this record itself\n          # and verifies it before reprepro. No APT credential exists here.\n",
        read_sha = read_sha_fn("            ", "$1"),
        published = published,
        arch_targets = arch_targets,
        binary = release.binary,
        package = release.package,
        assets = assets,
        consumer = release.consumer_repository,
    );
    format!(
        "  publish:\n    name: Publish GitHub release\n    needs: [{needs}]\n    runs-on: {runner}\n    timeout-minutes: 20\n    environment: github-release\n    permissions:\n      contents: write\n      packages: read\n    env:\n      VERSION: {version}\n      SOURCE_REF: ${{{{ github.ref }}}}\n      SOURCE_COMMIT: ${{{{ github.sha }}}}\n      COMMIT: ${{{{ github.sha }}}}\n      INDEX_DIGEST: ${{{{ needs.image.outputs.index_digest }}}}\n      MANIFEST_SHA256: ${{{{ needs.image.outputs.manifest_sha256 }}}}\n      GHCR_IMAGE: {image}\n      SOURCE_URL: {source_url}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n          persist-credentials: false\n      - name: Download release artifacts\n        uses: {download}\n        with:\n          path: artifacts\n          pattern: {lane}-*\n          merge-multiple: true\n      - name: Download Debian packages\n        uses: {download}\n        with:\n          name: debian-packages\n          path: artifacts\n      - name: Download release metadata\n        uses: {download}\n        with:\n          name: release-metadata\n          path: artifacts\n      - name: Download image digests\n        uses: {download}\n        with:\n          name: image-digests\n          path: artifacts\n      - name: Verify tarball provenance\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          for artifact in artifacts/*.tar.gz; do gh attestation verify \"$artifact\" --repo \"$GITHUB_REPOSITORY\"; done\n      - name: Verify deb provenance\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          for artifact in artifacts/*.deb; do gh attestation verify \"$artifact\" --repo \"$GITHUB_REPOSITORY\" --signer-workflow \"$GITHUB_REPOSITORY/.github/workflows/ci-release-package-signer.yml\"; done\n{record_assembly}{record_reverify}      - name: Assemble independent checksums\n        run: |\n          set -euo pipefail\n          shopt -s nullglob\n          subjects=(artifacts/*.tar.gz artifacts/*.deb)\n          test \"${{#subjects[@]}}\" -eq {subject_count}\n          : > SHA256SUMS\n          : > assets.jsonl\n          for subject in \"${{subjects[@]}}\"; do\n            name=$(basename \"$subject\")\n            digest=$(sha256sum \"$subject\" | awk '{{print $1}}')\n            sidecar=\"$(awk 'NF {{print $1; exit}}' \"${{subject}}.sha256\")\"\n            [[ \"$digest\" =~ ^[0-9a-f]{{64}}$ && \"$sidecar\" = \"$digest\" ]] \\\n              || {{ echo \"::error::$name sidecar does not match its payload\" >&2; exit 1; }}\n            printf '%s  %s\\n' \"$digest\" \"$name\" >> SHA256SUMS\n            jq -cn --arg name \"$name\" --arg sha256 \"$digest\" '{{name:$name,sha256:$sha256}}' >> assets.jsonl\n          done\n          test \"$(wc -l < SHA256SUMS | tr -d ' ')\" -eq {subject_count}\n          (cd artifacts && sha256sum --check --strict ../SHA256SUMS)\n{manifest_step}      - name: Stage package subjects for hosted signer\n        run: |\n          set -euo pipefail\n          mkdir signer-input\n          cp artifacts/{binary}-*.tar.gz artifacts/{package}-*.deb signer-input/\n{packaged_identity}      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n      - name: Log in to GHCR for immutable image verification\n        uses: {login}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ secrets.GITHUB_TOKEN }}}}\n      - name: Verify OCI index stayed immutable before publication\n        env:\n          EXPECTED_INDEX_DIGEST: ${{{{ needs.image.outputs.index_digest }}}}\n        run: |\n          set -euo pipefail\n          docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > published-image.json\n          published_index=\"$(jq -er '.manifest.digest' published-image.json)\"\n          [ \"$published_index\" = \"$EXPECTED_INDEX_DIGEST\" ] || {{\n            echo \"::error::OCI version tag moved from $EXPECTED_INDEX_DIGEST to $published_index before release publication\" >&2\n            exit 1\n          }}\n      - name: Verify release tag stayed immutable before publication\n        env:\n          EXPECTED_TAG_REF: ${{{{ github.ref }}}}\n          EXPECTED_TAG_COMMIT: ${{{{ github.sha }}}}\n        run: |\n          set -euo pipefail\n          remote_tag_refs=\"$(git ls-remote --exit-code origin \"$EXPECTED_TAG_REF\" \"$EXPECTED_TAG_REF^{{}}\")\"\n          remote_tag_commit=\"$(printf '%s\\n' \"$remote_tag_refs\" | awk -v expected=\"$EXPECTED_TAG_REF\" '\n            $2 == expected \"^{{}}\" {{ peeled=$1; found_peeled=1; next }}\n            $2 == expected && !found_peeled {{ raw=$1 }}\n            END {{\n              if (found_peeled) print peeled\n              else if (raw != \"\") print raw\n            }}\n          ')\"\n          case \"$remote_tag_commit\" in\n            *[!0-9a-f]*|'') echo \"::error::release tag $EXPECTED_TAG_REF did not resolve to lowercase hex\" >&2; exit 1 ;;\n          esac\n          [ \"${{#remote_tag_commit}}\" -eq 40 ] || {{ echo \"::error::release tag $EXPECTED_TAG_REF did not resolve to one commit\" >&2; exit 1; }}\n          [ \"$remote_tag_commit\" = \"$EXPECTED_TAG_COMMIT\" ] || {{\n            echo \"::error::release tag $EXPECTED_TAG_REF moved from $EXPECTED_TAG_COMMIT to $remote_tag_commit\" >&2\n            exit 1\n          }}\n{create_verify}      - name: Upload package subjects\n        uses: {upload}\n        with:\n          name: package-subjects\n          path: signer-input\n          if-no-files-found: error\n          retention-days: 2\n",
        runner = selected_runner(config),
        lane = canonical_lane(config),
        image = release.image,
        source_url = release_source_url(release),
        binary = release.binary,
        package = release.package,
    )
}

/// The preview identity job: one resolved `~preview.N+sha7` version, bound
/// to exactly the commit that triggered the run and never re-resolved from
/// the moving branch tip. Every downstream job consumes these outputs.
fn render_preview_identity_job(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let checkout = ActionPin::Checkout.reference();
    let setup = workflow_runtime_setup_for_config(config);
    let manifest = match rust_package_unit(config, &release.package) {
        Some(unit) if unit.root == "." || unit.root.is_empty() => "Cargo.toml".to_owned(),
        Some(unit) => format!("{}/Cargo.toml", unit.root.trim_end_matches('/')),
        None => "Cargo.toml".to_owned(),
    };
    let manifest = shell_quote(&manifest);
    format!(
        "  identity:\n    name: Resolve preview identity\n    if: ${{{{ github.ref == 'refs/heads/{}' }}}}\n    timeout-minutes: 10\n    runs-on: {runner}\n    outputs:\n      version: ${{{{ steps.identity.outputs.version }}}}\n      crate_version: ${{{{ steps.identity.outputs.crate_version }}}}\n      name: ${{{{ steps.identity.outputs.name }}}}\n      commit: ${{{{ steps.identity.outputs.commit }}}}\n      short_commit: ${{{{ steps.identity.outputs.short_commit }}}}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n          ref: ${{{{ github.sha }}}}\n          fetch-depth: 1\n          persist-credentials: false\n{setup}      - name: Enforce workflow policy\n        env:\n          EVENT_NAME: ${{{{ github.event_name }}}}\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n      - name: Resolve the preview version from the crate manifest\n        id: identity\n        env:\n          EVENT_SHA: ${{{{ github.sha }}}}\n          RUN_NUMBER: ${{{{ github.run_number }}}}\n        run: |\n          set -euo pipefail\n          commit=\"$(git rev-parse HEAD)\"\n          case \"$commit\" in\n            *[!0-9a-f]*|'') echo \"::error::HEAD did not resolve to lowercase hex\" >&2; exit 1 ;;\n          esac\n          [ \"${{#commit}}\" -eq 40 ] || {{ echo \"::error::HEAD is not a 40-hex commit\" >&2; exit 1; }}\n          [ \"$commit\" = \"$EVENT_SHA\" ] || {{ echo \"::error::checkout $commit != event commit $EVENT_SHA\" >&2; exit 1; }}\n          case \"$RUN_NUMBER\" in\n            ''|*[!0-9]*) echo \"::error::invalid workflow run number $RUN_NUMBER\" >&2; exit 1 ;;\n          esac\n          [ \"$RUN_NUMBER\" -gt 0 ] || {{ echo \"::error::run number must be positive\" >&2; exit 1; }}\n          crate=\"$(sed -n 's/^version = \"\\(.*\\)\"/\\1/p' {manifest} | head -n1)\"\n          case \"$crate\" in\n            ''|*[!0-9.]*) echo \"::error::crate version $crate is not an X.Y.Z version\" >&2; exit 1 ;;\n          esac\n          [[ \"$crate\" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+$ ]] \\\n            || {{ echo \"::error::crate version $crate is not an X.Y.Z version\" >&2; exit 1; }}\n          short_commit=\"${{commit:0:7}}\"\n          version=\"${{crate}}~preview.${{RUN_NUMBER}}+${{short_commit}}\"\n          [[ \"$version\" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+~preview\\.[0-9]+\\+[0-9a-f]{{7}}$ ]] \\\n            || {{ echo \"::error::preview version $version violates the preview contract\" >&2; exit 1; }}\n          {{\n            echo \"version=$version\"\n            echo \"crate_version=$crate\"\n            echo \"name=Preview $version\"\n            echo \"commit=$commit\"\n            echo \"short_commit=$short_commit\"\n          }} >> \"$GITHUB_OUTPUT\"\n",
        config.default_branch,
        runner = yaml_scalar(&config.github_runner),
    )
}

/// The rolling preview publish job: the per-arch preview debs are
/// re-verified from their own bytes (each deb's packaged record must name
/// exactly this identity), then the rolling `preview` release is replaced
/// atomically under its `Preview <version>` title. A monotonicity guard
/// refuses to move the lane backward when a superseded run re-publishes.
fn render_preview_publish_job(config: &ProjectConfig, release: &ReleaseSpec, sign: bool) -> String {
    let download = ActionPin::DownloadArtifact.reference();
    let needs = if sign {
        "identity, debian, sign-deb"
    } else {
        "identity, debian"
    };
    let binary = yaml_scalar(&release.binary);
    let arches = deb_architectures(&release.targets).unwrap_or_default();
    let arch_list = arches
        .iter()
        .map(|(arch, _)| (*arch).to_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let mut assets = vec!["SHA256SUMS".to_owned(), "release-manifest.json".to_owned()];
    for (arch, _) in &arches {
        assets.push(format!(
            "\"artifacts/{}-preview-$VERSION-{arch}.deb\"",
            release.package
        ));
        assets.push(format!(
            "\"artifacts/{}-preview-$VERSION-{arch}.deb.sha256\"",
            release.package
        ));
    }
    let create_assets = assets.join(" \\\n            ");
    let mut expected_assets = vec![
        "\"SHA256SUMS\"".to_owned(),
        "\"release-manifest.json\"".to_owned(),
    ];
    for (arch, _) in &arches {
        expected_assets.push(format!(
            "(\"{}-preview-\" + $asset_version + \"-{arch}.deb\")",
            release.package
        ));
        expected_assets.push(format!(
            "(\"{}-preview-\" + $asset_version + \"-{arch}.deb.sha256\")",
            release.package
        ));
    }
    let expected_assets = expected_assets.join(",\n                ");
    let manifest_step = if release.manifest_schema.is_empty() {
        "      - name: Assemble consumer release manifest\n        run: |\n          echo '::error::native preview with a package consumer needs manifest_schema; declare the consumer manifest schema URN' >&2\n          exit 1\n"
            .to_owned()
    } else {
        let schema = shell_quote(&release.manifest_schema);
        format!(
            "      - name: Assemble consumer release manifest\n        run: |\n          set -euo pipefail\n          jq -S -n \\\n            --arg schema {schema} \\\n            --arg source_repository \"$GITHUB_REPOSITORY\" \\\n            --arg source_ref \"refs/heads/{branch}\" \\\n            --arg source_commit \"$COMMIT\" \\\n            --arg version \"$VERSION\" \\\n            --slurpfile assets assets.jsonl \\\n            '{{schema:$schema,source_repository:$source_repository,source_ref:$source_ref,source_commit:$source_commit,version:$version,assets:$assets}}' \\\n            > release-manifest.json\n          [ -f release-manifest.json ] || {{ echo \"::error::no assembled consumer manifest\" >&2; exit 1; }}\n          jq -e --arg version \"$VERSION\" \\\n            '.version == $version and .source_ref == \"refs/heads/{branch}\" and\n             (.assets | length) == {count}' release-manifest.json >/dev/null\n",
            branch = config.default_branch,
            count = arches.len(),
        )
    };
    format!(
        "  publish:\n    needs: [{needs}]\n    name: Replace the rolling preview release\n    if: ${{{{ github.ref == 'refs/heads/{branch}' }}}}\n    timeout-minutes: 20\n    runs-on: {runner}\n    permissions:\n      contents: write\n    env:\n      VERSION: ${{{{ needs.identity.outputs.version }}}}\n      NAME: ${{{{ needs.identity.outputs.name }}}}\n      COMMIT: ${{{{ needs.identity.outputs.commit }}}}\n      GH_REPO: ${{{{ github.repository }}}}\n    steps:\n      - name: Download preview debs\n        uses: {download}\n        with:\n          name: debian-packages\n          path: artifacts\n      - name: Download preview metadata\n        uses: {download}\n        with:\n          name: preview-metadata\n          path: preview-metadata\n      - name: Assemble independent checksums\n        run: |\n          set -euo pipefail\n          shopt -s nullglob\n          subjects=(artifacts/*-preview-*.deb)\n          test \"${{#subjects[@]}}\" -eq {count}\n          : > SHA256SUMS\n          : > assets.jsonl\n          for subject in \"${{subjects[@]}}\"; do\n            name=$(basename \"$subject\")\n            digest=$(sha256sum \"$subject\" | awk '{{print $1}}')\n            sidecar=\"$(awk 'NF {{print $1; exit}}' \"${{subject}}.sha256\")\"\n            [[ \"$digest\" =~ ^[0-9a-f]{{64}}$ && \"$sidecar\" = \"$digest\" ]] \\\n              || {{ echo \"::error::$name sidecar does not match its payload\" >&2; exit 1; }}\n            printf '%s  %s\\n' \"$digest\" \"$name\" >> SHA256SUMS\n            jq -cn --arg name \"$name\" --arg sha256 \"$digest\" '{{name:$name,sha256:$sha256}}' >> assets.jsonl\n          done\n          test \"$(wc -l < SHA256SUMS | tr -d ' ')\" -eq {count}\n          (cd artifacts && sha256sum --check --strict ../SHA256SUMS)\n          chmod +x preview-metadata/{binary}-release-tool\n          tmpdir=\"$(mktemp -d)\"\n          trap 'rm -rf -- \"$tmpdir\"' EXIT\n          for arch in {arch_list}; do\n            deb=\"artifacts/{package}-preview-${{VERSION}}-${{arch}}.deb\"\n            [ -f \"$deb\" ] || {{ echo \"::error::missing preview deb for $arch\" >&2; exit 1; }}\n            [ \"$(dpkg-deb -f \"$deb\" Version)\" = \"$VERSION\" ] \\\n              || {{ echo \"::error::published deb version != preview identity ($arch)\" >&2; exit 1; }}\n            [ \"$(dpkg-deb -f \"$deb\" Architecture)\" = \"$arch\" ] \\\n              || {{ echo \"::error::published deb arch mismatch ($arch)\" >&2; exit 1; }}\n            record=\"$tmpdir/package-record-$arch.json\"\n            record_path=\"$(dpkg-deb -c \"$deb\" | awk '$NF ~ /(^|\\/)package-record\\.json$/ {{ print $NF }}')\"\n            [ -n \"$record_path\" ] || {{ echo \"::error::deb missing its package record ($arch)\" >&2; exit 1; }}\n            dpkg-deb --fsys-tarfile \"$deb\" | tar -xOf - \"$record_path\" > \"$record\"\n            record_digest=\"$(sha256sum \"$record\" | awk '{{print $1}}')\"\n            preview-metadata/{binary}-release-tool release verify-record \\\n              --record \"$record\" --sha256 \"$record_digest\" >/dev/null \\\n              || {{ echo \"::error::packaged package record is incoherent ($arch)\" >&2; exit 1; }}\n            packaged_binary=\"$(dpkg-deb --fsys-tarfile \"$deb\" | tar -xOf - ./usr/bin/{binary} | sha256sum | awk '{{print $1}}')\"\n            jq -e --arg commit \"$COMMIT\" --arg version \"$VERSION\" --arg arch \"$arch\" --arg binary \"$packaged_binary\" \\\n              '.build.kind == \"preview\" and .build.commit == $commit and\n               .build.debian_version == $version and\n               .architecture.arch == $arch and .architecture.binary_sha256 == $binary' \\\n              \"$record\" >/dev/null \\\n              || {{ echo \"::error::packaged package record does not name this preview identity ($arch)\" >&2; exit 1; }}\n          done\n{manifest_step}      - name: Replace the rolling preview release atomically\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          live_response=\"$(mktemp)\"\n          trap 'rm -f -- \"$live_response\"' EXIT\n          live_status=0\n          gh api -i \"repos/$GITHUB_REPOSITORY/releases/tags/preview\" > \"$live_response\" 2>/dev/null || live_status=$?\n          live_http=\"$(awk 'NR == 1 {{print $2; exit}}' \"$live_response\")\"\n          if [ \"$live_status\" -ne 0 ]; then\n            [ \"$live_http\" = \"404\" ] \\\n              || {{ echo \"::error::could not read the live preview release (gh api exit $live_status, HTTP ${{live_http:-unknown}}); refusing to replace it\" >&2; exit 1; }}\n          else\n            live_body=\"$(awk 'body {{print; next}} /^\\r?$/ {{body = 1}}' \"$live_response\")\"\n            if ! live_name=\"$(jq -er '.name | strings' <<<\"$live_body\" 2>/dev/null)\"; then\n              echo \"::error::live preview release response carries no name; refusing to replace it\" >&2\n              exit 1\n            fi\n            [ -n \"$live_name\" ] || {{ echo \"::error::live preview release has an empty name; refusing to replace it\" >&2; exit 1; }}\n            live_version=\"${{live_name#Preview }}\"\n            [[ \"$live_name\" = \"Preview $live_version\" && \"$live_version\" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+~preview\\.[0-9]+\\+[0-9a-f]{{7}}$ ]] \\\n              || {{ echo \"::error::live preview release names '$live_name', which is not 'Preview <version>' under the preview contract; refusing to delete it\" >&2; exit 1; }}\n            if dpkg --compare-versions \"$live_version\" eq \"$VERSION\"; then\n              :\n            elif dpkg --compare-versions \"$live_version\" gt \"$VERSION\"; then\n              echo \"::error::live preview $live_version is newer than candidate $VERSION; refusing to move the rolling preview backward — re-run the LATEST preview run instead\" >&2\n              exit 1\n            fi\n          fi\n          gh release delete preview --cleanup-tag --yes || true\n          gh release create preview --target \"$COMMIT\" --prerelease --title \"$NAME\" \\\n            {create_assets}\n      - name: Verify the published rolling preview\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          gh api \"repos/$GITHUB_REPOSITORY/releases/tags/preview\" > published.json\n          asset_version=\"${{VERSION//'~'/'.'}}\"\n          jq -e \\\n            --arg name \"$NAME\" --arg commit \"$COMMIT\" --arg asset_version \"$asset_version\" '\n              (.draft | not) and .prerelease == true and\n              .tag_name == \"preview\" and .name == $name and\n              .target_commitish == $commit and\n              ([.assets[].name] | sort) ==\n              ([\n                {expected_assets}\n              ] | sort)\n            ' published.json >/dev/null\n          jq -e --arg version \"$VERSION\" \\\n            '.version == $version and .source_ref == \"refs/heads/{branch}\" and\n             (.assets | length) == {count}' release-manifest.json >/dev/null\n",
        branch = config.default_branch,
        runner = selected_runner(config),
        count = arches.len(),
        package = release.package,
    )
}

/// The native rolling preview: identity, metadata, per-arch preview debs,
/// signer attestations, and the atomic rolling release replace. The lane
/// never cancels in progress: a cancelled delete-and-recreate strands the
/// rolling release halfway replaced.
fn render_native_preview(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut jobs = render_preview_identity_job(config, release);
    jobs.push('\n');
    let guest_job = render_guest_payload_job(config, release, true);
    if let Some(guest_job) = &guest_job {
        jobs.push_str(guest_job);
        jobs.push('\n');
    }
    jobs.push_str(&render_release_metadata_job(config, release, true));
    jobs.push('\n');
    jobs.push_str(&render_identity_debian_job(
        config,
        release,
        guest_job.is_some(),
        true,
    ));
    jobs.push('\n');
    let sign_job = render_sign_deb_job(config, release, true);
    if let Some(sign_job) = &sign_job {
        jobs.push_str(sign_job);
        jobs.push('\n');
    }
    jobs.push_str(&render_preview_publish_job(
        config,
        release,
        sign_job.is_some(),
    ));
    format!(
        "{GENERATED_HEADER}name: Preview\nrun-name: Preview · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\non:\n  push:\n    branches: [{}]\n    paths:\n{paths}  workflow_dispatch:\n\nconcurrency:\n  group: preview-${{{{ github.repository }}}}\n  cancel-in-progress: true\n\npermissions:\n  contents: read\n\njobs:\n{jobs}",
        yaml_scalar(&config.default_branch),
        paths = release_watch_paths(config),
    )
}

fn release_runner(config: &ProjectConfig, target: &str) -> String {
    if target.ends_with("-apple-darwin") {
        yaml_scalar(&config.macos_runner)
    } else {
        yaml_scalar(&config.github_runner)
    }
}

fn configured_runner(config: &ProjectConfig, lane: RunnerMode) -> String {
    match lane {
        RunnerMode::Github | RunnerMode::Both => yaml_scalar(&config.github_runner),
        RunnerMode::Velnor => velnor_runner(&config.velnor_labels, velnor_runner_group(config)),
    }
}

fn selected_runner(config: &ProjectConfig) -> String {
    configured_runner(config, config.runners)
}

fn workflow_runtime_setup_for_config(config: &ProjectConfig) -> String {
    if config.runners == RunnerMode::Velnor {
        String::new()
    } else {
        workflow_runtime_setup(RunnerMode::Github)
    }
}

fn release_lanes(config: &ProjectConfig, target: &str) -> Vec<(&'static str, String)> {
    let github = || release_runner(config, target);
    let velnor = || configured_runner(config, RunnerMode::Velnor);
    // Release and preview artifact builders stay GitHub-hosted for a
    // dual-lane repository. The dual-lane contract still applies to core CI
    // and the literal Velnor release verification jobs below, while the
    // release-side matrix must not carry a self-hosted value behind
    // `matrix.runner`: the pinned policy runtime validates that raw value and
    // cannot prove a matrix entry is the approved runner mapping. Explicit
    // Velnor-only repositories retain their trusted Velnor release lane.
    match config.runners {
        RunnerMode::Github | RunnerMode::Both => vec![("github", github())],
        RunnerMode::Velnor => vec![("velnor", velnor())],
    }
}

/// A release-side matrix may use a literal runner when every row resolves to
/// the same runner. Keep the `runner` field in each row for the reviewed
/// matrix contract; only replace the job-level expression when doing so does
/// not change any row's scheduling semantics. Heterogeneous target runners
/// retain the expression because one job cannot encode multiple literal
/// `runs-on` values.
fn release_matrix_runner(config: &ProjectConfig, targets: &[String]) -> String {
    let runners = targets
        .iter()
        .flat_map(|target| release_lanes(config, target).into_iter())
        .map(|(_, runner)| runner)
        .collect::<BTreeSet<_>>();
    if runners.len() == 1 {
        runners
            .into_iter()
            .next()
            .unwrap_or_else(|| github_expression("matrix.runner"))
    } else {
        github_expression("matrix.runner")
    }
}

fn canonical_lane(config: &ProjectConfig) -> &'static str {
    if config.runners == RunnerMode::Velnor {
        "velnor"
    } else {
        "github"
    }
}

fn render_preview(config: &ProjectConfig, release: Option<&ReleaseSpec>) -> String {
    let Some(release) =
        release.filter(|release| matches!(release.kind.as_str(), "rust-binary" | "native"))
    else {
        return format!(
            "{GENERATED_HEADER}# Preview is omitted: no complete Rust binary release contract.\n"
        );
    };
    // The scan refuses a Rust repository without a pin, so this is only
    // unreachable for hand-built configs; without the pin there is no
    // toolchain contract to build the binary under, so fail closed.
    let Some(toolchain) = crate::config_rust_toolchain(config) else {
        return format!(
            "{GENERATED_HEADER}# Preview is omitted: the repository pins no Rust toolchain.\n"
        );
    };
    if native_debian_release(config, release) {
        return render_native_preview(config, release);
    }
    // The build jobs run only from trusted pushes to the default branch, so
    // that — and nothing broader — is what may save the toolchain cache.
    let mut toolchain_steps = String::new();
    render_pinned_toolchain_steps(
        &mut toolchain_steps,
        ActionPin::CacheRestore.reference(),
        ActionPin::CacheSave.reference(),
        &toolchain,
        Some(&format!(
            "github.event_name == 'push' && github.ref == 'refs/heads/{}' && steps.rustup-toolchain.outputs.cache-hit != 'true'",
            config.default_branch
        )),
    );
    let mut matrix = String::new();
    for target in &release.targets {
        for (lane, runner) in release_lanes(config, target) {
            let _ = writeln!(
                matrix,
                "          - target: {}\n            lane: {lane}\n            runner: {runner}",
                yaml_scalar(target),
            );
        }
    }
    let matrix_runner = release_matrix_runner(config, &release.targets);
    let mut output = format!(
        r#"{GENERATED_HEADER}name: Preview\nrun-name: Preview · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\non:\n  push:\n    branches: [{}]\n    paths:\n{paths}  workflow_dispatch:\n\nconcurrency:\n  group: preview-${{{{ github.repository }}}}\n  cancel-in-progress: true\n\npermissions:\n  contents: read\n\njobs:\n  build:\n    name: Preview / ${{{{ matrix.target }}}}\n    runs-on: {matrix_runner}\n    timeout-minutes: 75\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0\n      - name: Build preview binary\n        env:\n          CARGO_INCREMENTAL: "0"\n          RUSTC_WRAPPER: sccache\n        run: cargo build --locked --release --package {} --bin {} --target "${{{{ matrix.target }}}}"\n      - name: Package preview binary\n        run: velnor-workflow release package-binary --target "${{{{ matrix.target }}}}" --version preview --package {} --binary {}\n      - name: Attest preview artifact\n        uses: {}\n        with:\n          subject-path: dist/*.tar.gz\n      - name: Upload preview artifact\n        uses: {}\n        with:\n          name: ${{{{ matrix.target }}}}\n          path: dist/*\n          if-no-files-found: error\n          retention-days: 1\n\n  publish:\n    name: Publish rolling preview\n    needs: build\n    if: ${{{{ github.event_name == 'push' && github.ref == 'refs/heads/{}' }}}}\n    runs-on: ubuntu-24.04\n    timeout-minutes: 15\n    permissions:\n      contents: write\n    steps:\n      - name: Download preview artifacts\n        uses: {}\n        with:\n          path: dist\n          merge-multiple: true\n      - name: Replace rolling preview\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          gh release view preview >/dev/null 2>&1 || gh release create preview --prerelease --title "Rolling preview"\n          gh release edit preview --target "${{{{ github.sha }}}}" --prerelease\n          gh release upload preview dist/* --clobber\n"#,
        yaml_scalar(&config.default_branch),
        ActionPin::Checkout.reference(),
        ActionPin::Sccache.reference(),
        yaml_scalar(&release.package),
        yaml_scalar(&release.binary),
        yaml_scalar(&release.package),
        yaml_scalar(&release.binary),
        ActionPin::Attest.reference(),
        ActionPin::UploadArtifact.reference(),
        config.default_branch,
        ActionPin::DownloadArtifact.reference(),
        paths = release_watch_paths(config),
        matrix = matrix,
        matrix_runner = matrix_runner,
    )
    .replace(
        "permissions:\\n  contents: read\\n\\njobs:\\n  build:",
        "permissions:\\n  contents: read\\n  id-token: write\\n  attestations: write\\n\\njobs:\\n  build:",
    )
    .replace(
        "          persist-credentials: false\\n      - name: Set up sccache",
        &format!(
            "          persist-credentials: false\\n      - name: Enforce workflow policy\\n        env:\\n          EVENT_NAME: ${{{{ github.event_name }}}}\\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\\n{toolchain_steps}      - name: Set up sccache"
        ),
    )
    .replace(
        "      - name: Build preview binary",
        "      - name: Add Rust target\\n        run: rustup target add \"${{ matrix.target }}\"\\n      - name: Build preview binary",
    )
    .replace(
        "      - name: Replace rolling preview",
        "      - name: Verify preview provenance\\n        env:\\n          GH_TOKEN: ${{ github.token }}\\n        run: |\\n          set -euo pipefail\\n          for artifact in dist/*.tar.gz; do gh attestation verify \"$artifact\" --repo \"$GITHUB_REPOSITORY\"; done\\n      - name: Replace rolling preview",
    )
    .replace(
        "    timeout-minutes: 15\\n    permissions:",
        "    timeout-minutes: 15\\n    environment: github-preview\\n    permissions:",
    )
    .replace("\\n", "\n")
    .replace(
        &format!("    runs-on: {matrix_runner}\n    timeout-minutes: 75"),
        &format!(
            "    runs-on: {matrix_runner}\n    if: ${{{{ {} }}}}\n    timeout-minutes: 75",
            trusted_release_runner_gate(&config.default_branch)
        ),
    )
    .replace(
        "      - name: Enforce workflow policy\n",
        &format!(
            "{}      - name: Enforce workflow policy\n",
            workflow_runtime_setup_for_config(config)
        ),
    )
    .replacen(
        "runs-on: ubuntu-24.04",
        &format!("runs-on: {}", selected_runner(config)),
        1,
    )
    .replace(
        "name: ${{ matrix.target }}",
        "name: ${{ matrix.lane }}-${{ matrix.target }}",
    )
    .replace(
        "          path: dist\n          merge-multiple: true",
        &format!(
            "          path: dist\n          pattern: {}-*\n          merge-multiple: true",
            canonical_lane(config)
        ),
    )
    .replace("run: cargo build ", "run: mbx build ");
    // The tarball lane has no identity job, so its guest payload keeps the
    // unbound shape; the native preview lane renders its own guest below.
    if let Some(guest) = render_guest_payload_job(config, release, false) {
        output = output.replace("jobs:\n  build:", &format!("jobs:\n{guest}\n  build:"));
    }
    output
}

pub(crate) fn render_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    if !release_contract_complete(release) {
        return format!(
            "{GENERATED_HEADER}# Release omitted: artifact, platform, registry, or signer contract is incomplete.\n"
        );
    }
    match release.kind.as_str() {
        "crates" => render_crates_release(config, release),
        "rust-binary" => render_binary_release(config, release),
        "native" => render_native_release(config, release),
        "pages" => render_pages_release(config, release),
        "homebrew" => render_homebrew_release(config, release),
        "apt" => render_apt_release(config, release),
        _ => format!(
            "{GENERATED_HEADER}# Release omitted: this publisher requires a separately verified contract.\n"
        ),
    }
}

fn render_release_unit_jobs(config: &ProjectConfig) -> (String, Vec<String>) {
    let workflow = WorkflowIr::from_config(config);
    let lanes = match config.runners {
        RunnerMode::Github => vec![RunnerMode::Github],
        RunnerMode::Velnor => vec![RunnerMode::Velnor],
        RunnerMode::Both => vec![RunnerMode::Github, RunnerMode::Velnor],
    };
    let mut output = String::new();
    let mut job_ids = Vec::new();
    for lane in lanes {
        for unit in config
            .units
            .iter()
            .filter(|unit| lane_supports_unit(lane, unit))
        {
            let id = format!("release-{}-{}", lane.as_str(), unit.id);
            let mut needs = vec!["verify".to_owned()];
            needs.extend(unit.depends_on.iter().filter_map(|dependency| {
                config
                    .units
                    .iter()
                    .find(|unit| unit.id == *dependency)
                    .filter(|unit| lane_supports_unit(lane, unit))
                    .map(|_| format!("release-{}-{}", lane.as_str(), dependency))
            }));
            let runner = workflow.runner_for_unit(lane, unit);
            let job_name = yaml_scalar(&format!(
                "{} / Release / {}",
                lane.display_name(),
                unit.label
            ));
            let verify_name = yaml_scalar(&unit.label);
            let dispatch_gate = if lane == RunnerMode::Velnor {
                format!(
                    "    if: ${{{{ {} }}}}\n",
                    trusted_release_runner_gate(&config.default_branch)
                )
            } else {
                String::new()
            };
            let _ = writeln!(
                output,
                "  {id}:\n    name: {job_name}\n{dispatch_gate}    needs: [{}]\n    runs-on: {runner}\n    timeout-minutes: 60\n    steps:",
                needs.join(", ")
            );
            let _ = writeln!(
                output,
                "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false",
                ActionPin::Checkout.reference()
            );
            WorkflowIr::render_workflow_runtime_setup(&mut output, lane);
            workflow.render_tool_provisioning(&mut output, lane, unit, false);
            let cargo_cache_restored = CacheBackend::Detected
                .lane_enables_actions_cache(lane, &workflow, unit)
                && unit.cache.is_some();
            if cargo_cache_restored && let Some(cache) = &unit.cache {
                render_retained_output_cache_note(&mut output, &workflow, unit, cache);
                let (paths, key) = rendered_cache_values(cache);
                let _ = writeln!(
                    output,
                    "      - name: Restore {} cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: ci-release-${{{{ runner.os }}}}-{}-${{{{ hashFiles({key}) }}}}",
                    verify_name,
                    ActionPin::CacheRestore.reference(),
                    unit.id
                );
            }
            let skip_when_offline_ready = lane == RunnerMode::Velnor
                && unit
                    .cache
                    .as_ref()
                    .is_some_and(super::cache::cache_is_velnor_host_persistent);
            render_cargo_source_preparation(
                &mut output,
                &[unit],
                &unit.id,
                false,
                cargo_cache_restored,
                skip_when_offline_ready,
            );
            let cargo_offline = checks_env(unit);
            let _ = writeln!(
                output,
                "      - name: Run {verify_name} checks\n        env:\n          CI_SCOPE: full\n          CI_UNIT_ID: {}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}{cargo_offline}\n        run: velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit {}\n",
                yaml_scalar(&unit.id),
                yaml_scalar(&unit.id)
            );
            job_ids.push(id);
        }
    }
    (output, job_ids)
}

fn render_crates_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = String::from(GENERATED_HEADER);
    output.push_str(
        "name: Release\nrun-name: Release · ${{ github.ref_name }}\n\non:\n  push:\n    tags: [\"v*\"]\n\nconcurrency:\n  group: release-${{ github.ref }}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n  verify:\n    name: Verify release\n    runs-on: ubuntu-24.04\n    timeout-minutes: 60\n    steps:\n",
    );
    let _ = writeln!(
        output,
        "      - name: Checkout\n        uses: {}\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0\n      - name: Verify tag\n        run: velnor-workflow release verify-tag\n      - name: Run full CI\n        run: velnor-workflow run --config .github/ci/project.toml --scope full\n      - name: Package declared crates\n        run: cargo package --workspace --locked\n\n  publish:\n    name: Publish crates.io packages\n    needs: verify\n    runs-on: ubuntu-24.04\n    timeout-minutes: 45\n    environment: crates.io\n    permissions:\n      contents: read\n      id-token: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Authenticate to crates.io\n        id: auth\n        uses: {}\n      - name: Publish in dependency order\n        env:\n          CARGO_REGISTRY_TOKEN: {}\n        run: |\n          set -euo pipefail\n",
        ActionPin::Checkout.reference(),
        ActionPin::Sccache.reference(),
        ActionPin::Checkout.reference(),
        ActionPin::CratesAuth.reference(),
        github_expression("steps.auth.outputs.token")
    );
    gate_velnor_release_verify(&mut output, config);
    let (unit_jobs, unit_job_ids) = render_release_unit_jobs(config);
    output = output.replace("\n  publish:", &format!("\n{unit_jobs}\n  publish:"));
    output = output.replace(
        "      - name: Run full CI\n        run: velnor-workflow run --config .github/ci/project.toml --scope full\n",
        "",
    );
    let release_needs = std::iter::once("verify".to_owned())
        .chain(unit_job_ids)
        .collect::<Vec<_>>()
        .join(", ");
    output = output.replace(
        "    needs: verify\n",
        &format!("    needs: [{release_needs}]\n"),
    );
    output = output.replace(
        "    timeout-minutes: 60\n    steps:\n",
        "    timeout-minutes: 60\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    steps:\n",
    );
    let crate_artifact_steps = format!(
        "      - name: Attest crate packages\n        uses: {}\n        with:\n          subject-path: target/package/*.crate\n      - name: Upload crate packages\n        uses: {}\n        with:\n          name: crate-packages\n          path: target/package/*.crate\n          if-no-files-found: error\n          retention-days: 2\n",
        ActionPin::Attest.reference(),
        ActionPin::UploadArtifact.reference(),
    );
    output = output.replace(
        "      - name: Package declared crates\n        run: cargo package --workspace --locked\n",
        &format!(
            "      - name: Package declared crates\n        run: cargo package --workspace --locked\n{crate_artifact_steps}"
        ),
    );
    let crate_verify_steps = format!(
        "      - name: Download crate packages\n        uses: {}\n        with:\n          name: crate-packages\n          path: target/package\n      - name: Verify crate provenance\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          for artifact in target/package/*.crate; do gh attestation verify \"$artifact\" --repo \"$GITHUB_REPOSITORY\"; done\n      - name: Authenticate to crates.io\n",
        ActionPin::DownloadArtifact.reference(),
    );
    output = output.replace(
        "      - name: Authenticate to crates.io\n",
        &crate_verify_steps,
    );
    output = output.replace(
        "      - name: Set up sccache\n",
        &format!(
            "{}      - name: Enforce workflow policy\n        env:\n          EVENT_NAME: ${{{{ github.event_name }}}}\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n      - name: Set up sccache\n",
            workflow_runtime_setup_for_config(config)
        ),
    );
    output = output.replace(
        "runs-on: ubuntu-24.04",
        &format!("runs-on: {}", selected_runner(config)),
    );
    output = output.replace(
        "run: velnor-workflow release verify-tag\n",
        &format!(
            "run: velnor-workflow release verify-tag --branch {} --package {}\n",
            shell_quote(&config.default_branch),
            shell_quote(&release.packages[0])
        ),
    );
    for package in &release.packages {
        let _ = writeln!(
            output,
            "          mbx publish --locked --package {}",
            shell_quote(package)
        );
    }
    output = output.replace("cargo package ", "mbx package ");
    let _ = writeln!(
        output,
        "# Release tags are cut from the protected {} branch.",
        config.default_branch
    );
    output
}

fn render_binary_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = String::from(GENERATED_HEADER);
    output.push_str(
        "name: Release\nrun-name: Release · ${{ github.ref_name }}\n\non:\n  push:\n    tags: [\"v*\"]\n\nconcurrency:\n  group: release-${{ github.ref }}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n  verify:\n    name: Verify release\n    runs-on: ubuntu-24.04\n    timeout-minutes: 60\n    steps:\n",
    );
    let _ = writeln!(
        output,
        "      - name: Checkout\n        uses: {}\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0\n      - name: Verify tag\n        run: velnor-workflow release verify-tag\n      - name: Run full CI\n        run: velnor-workflow run --config .github/ci/project.toml --scope full\n",
        ActionPin::Checkout.reference(),
        ActionPin::Sccache.reference(),
    );
    gate_velnor_release_verify(&mut output, config);
    output = output.replace(
        "      - name: Run full CI\n        run: velnor-workflow run --config .github/ci/project.toml --scope full\n",
        "",
    );
    output = output.replace(
        "run: velnor-workflow release verify-tag\n",
        &format!(
            "run: velnor-workflow release verify-tag --branch {} --package {}\n",
            shell_quote(&config.default_branch),
            shell_quote(&release.package)
        ),
    );
    output = output.replace(
        "      - name: Set up sccache\n",
        &format!(
            "{}      - name: Enforce workflow policy\n        env:\n          EVENT_NAME: ${{{{ github.event_name }}}}\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n      - name: Set up sccache\n",
            workflow_runtime_setup_for_config(config)
        ),
    );
    output.push_str(
        "\n  build:\n    name: Build / ${{ matrix.target }}\n    needs: verify\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n",
    );
    let (unit_jobs, unit_job_ids) = render_release_unit_jobs(config);
    output = output.replace("\n  build:", &format!("\n{unit_jobs}\n  build:"));
    let release_needs = std::iter::once("verify".to_owned())
        .chain(unit_job_ids)
        .collect::<Vec<_>>()
        .join(", ");
    output = output.replace(
        "    needs: verify\n",
        &format!("    needs: [{release_needs}]\n"),
    );
    for target in &release.targets {
        for (lane, runner) in release_lanes(config, target) {
            let _ = writeln!(
                output,
                "          - target: {}\n            lane: {lane}\n            runner: {runner}",
                yaml_scalar(target),
            );
        }
    }
    let matrix_runner = release_matrix_runner(config, &release.targets);
    let _ = writeln!(
        output,
        "    runs-on: {matrix_runner}\n    timeout-minutes: 90\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0\n      - name: Add Rust target\n        run: rustup target add \"${{{{ matrix.target }}}}\"\n      - name: Build release binary\n        env:\n          CARGO_INCREMENTAL: \"0\"\n          RUSTC_WRAPPER: sccache\n        run: cargo build --locked --release --package {} --bin {} --target \"${{{{ matrix.target }}}}\"\n      - name: Package release binary\n        env:\n          VERSION: ${{{{ github.ref_name }}}}\n        run: |\n          set -euo pipefail\n          velnor-workflow release package-binary --target \"${{{{ matrix.target }}}}\" --version \"${{VERSION#v}}\" --package {} --binary {}\n      - name: Attest release artifact\n        uses: {}\n        with:\n          subject-path: dist/*.tar.gz\n      - name: Upload release artifact\n        uses: {}\n        with:\n          name: ${{{{ matrix.target }}}}\n          path: dist/*\n          if-no-files-found: error\n          retention-days: 2\n\n  publish:\n    name: Publish GitHub release\n    needs: [verify, build]\n    runs-on: ubuntu-24.04\n    timeout-minutes: 20\n    environment: github-release\n    permissions:\n      contents: write\n    steps:\n      - name: Download release artifacts\n        uses: {}\n        with:\n          path: dist\n          merge-multiple: true\n      - name: Verify archive checksums\n        run: |\n          set -euo pipefail\n          cd dist\n          for checksum in *.sha256; do sha256sum --check \"$checksum\"; done\n      - name: Publish immutable GitHub release\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: gh release create \"${{{{ github.ref_name }}}}\" dist/* --verify-tag --generate-notes\n",
        ActionPin::Checkout.reference(),
        ActionPin::Sccache.reference(),
        yaml_scalar(&release.package),
        yaml_scalar(&release.binary),
        yaml_scalar(&release.package),
        yaml_scalar(&release.binary),
        ActionPin::Attest.reference(),
        ActionPin::UploadArtifact.reference(),
        ActionPin::DownloadArtifact.reference(),
        matrix_runner = matrix_runner,
    );
    if let Some((prefix, build)) = output.split_once("\n  build:") {
        let build = build.replace(
            "      - name: Set up sccache\n",
            &format!(
                "{}      - name: Set up sccache\n",
                workflow_runtime_setup_for_config(config)
            ),
        );
        let build = build.replace(
            &format!("    runs-on: {matrix_runner}\n    timeout-minutes: 90"),
            &format!(
                "    runs-on: {matrix_runner}\n    if: ${{{{ {} }}}}\n    timeout-minutes: 90",
                trusted_release_runner_gate(&config.default_branch)
            ),
        );
        output = format!("{prefix}\n  build:{build}");
    }
    output = output.replace(
        "      - name: Verify archive checksums\n",
        "      - name: Verify artifact provenance\n        env:\n          GH_TOKEN: ${{ github.token }}\n        run: |\n          set -euo pipefail\n          for artifact in dist/*.tar.gz; do gh attestation verify \"$artifact\" --repo \"$GITHUB_REPOSITORY\"; done\n      - name: Verify archive checksums\n",
    );
    output = output.replace("run: cargo build ", "run: mbx build ");
    if !config.default_branch.is_empty() {
        // Keep the branch policy visible in generated release metadata.
        let _ = writeln!(
            output,
            "# Release tags are cut from the protected {} branch.",
            config.default_branch
        );
    }
    output = output.replace(
        "runs-on: ubuntu-24.04",
        &format!("runs-on: {}", selected_runner(config)),
    );
    output
        .replace(
            "name: ${{ matrix.target }}",
            "name: ${{ matrix.lane }}-${{ matrix.target }}",
        )
        .replace(
            "          path: dist\n          merge-multiple: true",
            &format!(
                "          path: dist\n          pattern: {}-*\n          merge-multiple: true",
                canonical_lane(config)
            ),
        )
}

#[allow(clippy::format_push_string)]
/// One resolved release version for the whole file: the tag without its
/// `v` prefix, which is also the container tag the estate consumes.
fn inject_native_verify_outputs(
    output: &str,
    config: &ProjectConfig,
    release: &ReleaseSpec,
) -> String {
    let output = output.replace(
        "  verify:\n    name: Verify release\n",
        "  verify:\n    name: Verify release\n    outputs:\n      version: ${{ steps.version.outputs.version }}\n",
    );
    let verify_tag_run = format!(
        "run: velnor-workflow release verify-tag --branch {} --package {}\n",
        shell_quote(&config.default_branch),
        shell_quote(&release.package)
    );
    output.replace(
        &verify_tag_run,
        &format!(
            "{verify_tag_run}      - name: Resolve release version\n        id: version\n        env:\n          TAG: ${{{{ github.ref_name }}}}\n        run: |\n          set -euo pipefail\n          case \"$TAG\" in\n            v[0-9]*) ;;\n            *) echo \"::error::release tag $TAG must match v[0-9]*\" >&2; exit 1 ;;\n          esac\n          version=\"${{TAG#v}}\"\n          case \"$version\" in\n            ''|*['/ ']*) echo \"::error::release version is not portable: $version\" >&2; exit 1 ;;\n          esac\n          echo \"version=$version\" >> \"$GITHUB_OUTPUT\"\n"
        ),
    )
}

fn inject_native_build_identity(output: &str, release: &ReleaseSpec) -> String {
    let build_step = format!(
        "      - name: Build release binary\n        env:\n          CARGO_INCREMENTAL: \"0\"\n          RUSTC_WRAPPER: sccache\n        run: mbx build --locked --release --package {} --bin {} --target \"${{{{ matrix.target }}}}\"",
        yaml_scalar(&release.package),
        yaml_scalar(&release.binary)
    );
    output.replace(
        &build_step,
        &build_step
            .replace(
                "RUSTC_WRAPPER: sccache\n",
                "RUSTC_WRAPPER: sccache\n          VELNOR_RELEASE_BUILD: \"1\"\n",
            )
            .replace(
                " --target \"${{ matrix.target }}\"",
                " --features release-build --target \"${{ matrix.target }}\"",
            ),
    )
}

/// Record the raw release binary's digest next to its tarball: the deb lane
/// reuses these exact bytes and the release record binds this digest. A
/// target without a Debian architecture fails closed instead of recording
/// under a guessed name.
fn inject_native_binary_digest(output: &str, release: &ReleaseSpec) -> String {
    let mut arms = String::new();
    for target in &release.targets {
        let arch = match target.as_str() {
            "x86_64-unknown-linux-gnu" => "amd64",
            "aarch64-unknown-linux-gnu" => "arm64",
            _ => continue,
        };
        let _ = writeln!(arms, "            {target}) arch={arch} ;;");
    }
    let step = format!(
        "      - name: Record release binary digest\n        run: |\n          set -euo pipefail\n          case \"${{{{ matrix.target }}}}\" in\n{arms}            *) echo \"::error::native release record needs a Debian architecture for ${{{{ matrix.target }}}}\" >&2; exit 1 ;;\n          esac\n          binary=\"target/${{{{ matrix.target }}}}/release/{binary}\"\n          test -f \"$binary\" || {{ echo \"::error::missing release binary $binary\" >&2; exit 1; }}\n          digest=\"$(sha256sum \"$binary\" | awk '{{print $1}}')\"\n          case \"$digest\" in\n            ''|*[!0-9a-f]*) echo \"::error::binary digest is not lowercase hex\" >&2; exit 1 ;;\n          esac\n          [ \"${{#digest}}\" -eq 64 ] || {{ echo \"::error::binary digest has invalid length\" >&2; exit 1; }}\n          printf '%s\\n' \"$digest\" > \"dist/{binary}-$arch.bin.sha256\"\n",
        binary = yaml_scalar(&release.binary),
    );
    output.replace(
        "      - name: Attest release artifact\n",
        &format!("{step}      - name: Attest release artifact\n"),
    )
}

/// The native image jobs: the record-bound multi-arch lane (admission
/// gate, per-arch staging builds, one immutable version index) for
/// identity contracts, or the plain single-tag publisher for contracts
/// without release identity, which have no record to bind.
fn native_image_jobs(config: &ProjectConfig, release: &ReleaseSpec, debian: bool) -> String {
    if debian {
        let mut jobs = render_image_admission_job(config, release);
        jobs.push('\n');
        jobs.push_str(&render_image_platform_job(config, release));
        jobs.push('\n');
        jobs.push_str(&render_image_index_job(config, release));
        jobs.push('\n');
        jobs
    } else {
        let image_tag = yaml_scalar(&format!(
            "{}:${{{{ needs.verify.outputs.version }}}}",
            release.image
        ));
        format!(
            "  image:\n    name: Publish container image\n    needs: [admit-runner, verify, build]\n    runs-on: {}\n    timeout-minutes: 120\n    permissions:\n      contents: read\n      packages: write\n      id-token: write\n      attestations: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Log in to GHCR\n        uses: {}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ github.token }}}}\n      - name: Build and push image\n        uses: {}\n        with:\n          context: .\n          push: true\n          tags: {image_tag}\n          provenance: true\n          sbom: true\n",
            yaml_scalar(&config.github_runner),
            ActionPin::Checkout.reference(),
            ActionPin::DockerLogin.reference(),
            ActionPin::DockerBuild.reference(),
        )
    }
}

#[allow(clippy::format_push_string)]
fn render_native_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = render_binary_release(config, release);
    let github_runner = yaml_scalar(&config.github_runner);
    let identity = native_identity_release(config, release);
    let debian = native_debian_release(config, release);
    output = inject_native_verify_outputs(&output, config, release);
    if identity {
        output = inject_native_build_identity(&output, release);
    }
    if debian {
        output = inject_native_binary_digest(&output, release);
    }
    let admit = format!(
        "  admit-runner:\n    name: Admit release runner\n    runs-on: {github_runner}\n    timeout-minutes: 5\n    steps:\n      - name: Reject Velnor-only native release\n        if: ${{{{ github.event_name == 'workflow_dispatch' && github.event.inputs.runner == 'velnor' }}}}\n        run: |\n          echo 'native release publishes from GitHub only; Velnor-only dispatch is unsupported' >&2\n          exit 1\n"
    );
    output = output.replace("jobs:\n  verify:", &format!("jobs:\n{admit}\n  verify:"));
    output = output.replace(
        "    needs: [verify, build]\n",
        "    needs: [admit-runner, verify, build]\n",
    );
    let mut extra = String::new();
    let mut publish_needs = vec![
        "admit-runner".to_owned(),
        "verify".to_owned(),
        "build".to_owned(),
    ];
    if !release.image.is_empty() {
        extra.push_str(&native_image_jobs(config, release, debian));
        publish_needs.push("image".to_owned());
    }
    if debian {
        extra.push_str(&render_release_metadata_job(config, release, false));
        extra.push('\n');
        publish_needs.push("metadata".to_owned());
    }
    let guest_job = render_guest_payload_job(config, release, false);
    if let Some(guest_job) = &guest_job {
        extra.push_str(guest_job);
        extra.push('\n');
        publish_needs.push("guest-payload".to_owned());
    }
    if !release.consumer_repository.is_empty() {
        if debian {
            extra.push_str(&render_identity_debian_job(
                config,
                release,
                guest_job.is_some(),
                false,
            ));
        } else {
            extra.push_str(&render_debian_job(config, release, guest_job.is_some()));
        }
        publish_needs.push("debian".to_owned());
    }
    if debian && let Some(sign_job) = render_sign_deb_job(config, release, false) {
        extra.push_str(&sign_job);
        extra.push('\n');
        publish_needs.push("sign-deb".to_owned());
    }
    if !extra.is_empty() {
        output = output.replace("\n  publish:", &format!("\n{extra}\n  publish:"));
    }
    if debian {
        let publish = render_native_publish_job(config, release, &publish_needs);
        if let Some((prefix, _)) = output.split_once("\n  publish:") {
            output = prefix.to_owned();
            output.push('\n');
            output.push_str(&publish);
            if !config.default_branch.is_empty() {
                output.push_str(&format!(
                    "\n# Release tags are cut from the protected {} branch.\n",
                    config.default_branch
                ));
            }
        }
    } else {
        output = output.replace(
            "  publish:\n    name: Publish GitHub release\n    needs: [admit-runner, verify, build]\n",
            &format!(
                "  publish:\n    name: Publish GitHub release\n    needs: [{}]\n",
                publish_needs.join(", ")
            ),
        );
    }
    // The recovery digest input exists only with the admission gate that
    // reads it; contracts without the multi-arch lane keep their dispatch
    // surface unchanged.
    let recovery_input = if debian && !release.image.is_empty() {
        "      existing-image-digest:\n        description: Exact OCI index digest for an explicitly verified failed-run recovery.\n        type: string\n        required: false\n        default: ''\n"
    } else {
        ""
    };
    output.replace(
        "on:\n  push:\n    tags: [\"v*\"]\n",
        &format!(
            "on:\n  push:\n    tags: [\"v*\"]\n  workflow_dispatch:\n    inputs:\n      runner:\n        description: Execution backend\n        required: false\n        default: github\n        type: choice\n        options:\n          - github\n          - velnor\n          - both\n{recovery_input}",
        ),
    )
}

fn render_homebrew_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    render_package_feed(
        config,
        "homebrew",
        &release.package,
        &release.source_repository,
    )
}

fn render_apt_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    render_package_feed(
        config,
        "apt",
        &release.package,
        &release.consumer_repository,
    )
}

fn render_package_feed(
    config: &ProjectConfig,
    kind: &str,
    package: &str,
    coordinate: &str,
) -> String {
    let runner = yaml_scalar(&config.github_runner);
    let package = yaml_scalar(package);
    let coordinate = yaml_scalar(coordinate);
    format!(
        "{GENERATED_HEADER}name: Package feed\nrun-name: Package feed · {kind} · ${{{{ github.event_name }}}}\n\non:\n  schedule:\n    - cron: '17 4 * * *'\n  workflow_dispatch:\n    inputs:\n      runner:\n        description: Execution backend\n        required: false\n        default: github\n        type: choice\n        options:\n          - github\n          - velnor\n          - both\n      channel:\n        description: Package channel\n        required: false\n        default: stable\n        type: choice\n        options:\n          - stable\n          - preview\n\nconcurrency:\n  group: package-feed-{kind}-${{{{ github.repository }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n  admit-runner:\n    name: Admit feed runner\n    runs-on: {runner}\n    timeout-minutes: 5\n    steps:\n      - name: Reject Velnor-only feed mutation\n        if: ${{{{ github.event_name == 'workflow_dispatch' && github.event.inputs.runner == 'velnor' }}}}\n        run: |\n          echo '{kind} feed mutation publishes from GitHub only' >&2\n          exit 1\n  verify:\n    name: Verify {kind} feed\n    needs: [admit-runner]\n    runs-on: {runner}\n    timeout-minutes: 30\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Enforce workflow policy\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n      - name: Verify feed inputs\n        run: velnor-workflow release verify-feed --kind {kind} --package {package} --coordinate {coordinate}\n  mutate:\n    name: Update {kind} feed\n    needs: [admit-runner, verify]\n    if: ${{{{ github.ref == 'refs/heads/{branch}' && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') && github.event.inputs.runner != 'velnor' }}}}\n    runs-on: {runner}\n    timeout-minutes: 30\n    environment: package-feed\n    permissions:\n      contents: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Update feed\n        env:\n          CHANNEL: ${{{{ github.event.inputs.channel || 'stable' }}}}\n        run: velnor-workflow release update-feed --kind {kind} --package {package} --coordinate {coordinate} --channel \"$CHANNEL\"\n",
        ActionPin::Checkout.reference(),
        ActionPin::Checkout.reference(),
        branch = config.default_branch,
    )
}

fn render_pages_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = String::from(GENERATED_HEADER);
    let _ = writeln!(
        output,
        "name: Release\nrun-name: Release · documentation\n\non:\n  push:\n    branches: [{}]\n  workflow_dispatch:\n\nconcurrency:\n  group: pages-${{{{ github.repository }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n  deploy:\n    name: Publish documentation\n    if: ${{{{ github.event_name == 'push' && github.ref == 'refs/heads/{}' }}}}\n    runs-on: ubuntu-24.04\n    timeout-minutes: 30\n    environment: github-pages\n    permissions:\n      contents: read\n      pages: write\n      id-token: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Set up Bun\n        uses: {}\n        with:\n          cache: true\n      - name: Build documentation\n        run: bun run scripts/generate-docs.ts\n      - name: Configure Pages\n        uses: {}\n      - name: Upload Pages artifact\n        uses: {}\n        with:\n          path: {}\n      - name: Deploy Pages\n        uses: {}\n",
        yaml_scalar(&config.default_branch),
        config.default_branch,
        ActionPin::Checkout.reference(),
        ActionPin::Bun.reference(),
        ActionPin::ConfigurePages.reference(),
        ActionPin::UploadPages.reference(),
        yaml_scalar(&release.artifact_path),
        ActionPin::DeployPages.reference(),
    );
    let verify = format!(
        "  verify:\n    name: Verify documentation release\n    runs-on: ubuntu-24.04\n    timeout-minutes: 15\n    steps:\n      - name: Checkout workflow data\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Enforce workflow policy\n        env:\n          EVENT_NAME: ${{{{ github.event_name }}}}\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n",
        ActionPin::Checkout.reference(),
    );
    let verify = if config.runners == RunnerMode::Velnor {
        let gate = trusted_release_runner_gate(&config.default_branch);
        verify.replace(
            "  verify:\n    name: Verify documentation release\n",
            &format!(
                "  verify:\n    name: Verify documentation release\n    if: ${{{{ {gate} }}}}\n"
            ),
        )
    } else {
        verify
    };
    output = output.replace(
        "\njobs:\n  deploy:",
        &format!("\njobs:\n{verify}\n  deploy:"),
    );
    output = output.replace(
        "  deploy:\n    name: Publish documentation",
        "  deploy:\n    name: Publish documentation\n    needs: verify",
    );
    let push_condition = format!(
        "if: ${{{{ github.event_name == 'push' && github.ref == 'refs/heads/{}' }}}}",
        config.default_branch
    );
    let deploy_condition = format!(
        "if: ${{{{ ((github.event_name == 'workflow_dispatch' || github.event_name == 'push') && github.ref == 'refs/heads/{0}') || github.event_name == 'schedule' }}}}",
        config.default_branch
    );
    output = output.replace(&push_condition, &deploy_condition);
    let (unit_jobs, unit_job_ids) = render_release_unit_jobs(config);
    output = output.replace("\n  deploy:", &format!("\n{unit_jobs}\n  deploy:"));
    let release_needs = std::iter::once("verify".to_owned())
        .chain(unit_job_ids)
        .collect::<Vec<_>>()
        .join(", ");
    output = output.replace(
        "    needs: verify\n",
        &format!("    needs: [{release_needs}]\n"),
    );
    output = output.replace(
        "      - name: Set up Bun\n",
        "      - name: Enforce workflow policy\n        env:\n          EVENT_NAME: ${{ github.event_name }}\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n      - name: Set up Bun\n",
    );
    output = output.replace(
        "      - name: Enforce workflow policy\n",
        &format!(
            "{}      - name: Enforce workflow policy\n",
            workflow_runtime_setup_for_config(config)
        ),
    );
    output = output.replace(
        "runs-on: ubuntu-24.04",
        &format!("runs-on: {}", selected_runner(config)),
    );
    output
}

fn trusted_release_runner_gate(default_branch: &str) -> String {
    format!(
        "(github.event_name == 'push' && (github.ref_type == 'tag' || github.ref == 'refs/heads/{default_branch}')) || github.event_name == 'schedule' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{default_branch}')"
    )
}

fn gate_velnor_release_verify(output: &mut String, config: &ProjectConfig) {
    if config.runners == RunnerMode::Velnor {
        let gate = trusted_release_runner_gate(&config.default_branch);
        *output = output.replace(
            "  verify:\n    name: Verify release\n",
            &format!("  verify:\n    name: Verify release\n    if: ${{{{ {gate} }}}}\n"),
        );
    }
}

const MAINTENANCE_WORKFLOW: &str = r#"name: Maintenance
run-name: Maintenance · ${{ github.event_name }}

on:
  pull_request:
    types: [closed]
  schedule:
    - cron: '31 3 * * *'
  workflow_dispatch:
    inputs:
      pull_request_number:
        description: Optional closed PR number whose merge cache should be removed
        required: false
        type: string

permissions:
  actions: write
  contents: read

concurrency:
  group: maintenance-${{ github.repository }}-${{ github.event.pull_request.number || inputs.pull_request_number || github.run_id }}
  cancel-in-progress: false

jobs:
  prune-pr-cache:
    name: Prune closed-PR cache
    if: ${{ github.event_name == 'pull_request' || inputs.pull_request_number != '' }}
    runs-on: __MAINTENANCE_PRUNE_RUNNER__
    timeout-minutes: 15
    steps:
      - name: Delete merge-ref cache namespace
        env:
          GH_TOKEN: ${{ github.token }}
          PR_NUMBER: ${{ github.event.pull_request.number || inputs.pull_request_number }}
        run: |
          set -euo pipefail
          ref="refs/pull/$PR_NUMBER/merge"
          encoded="$(printf '%s' "$ref" | jq -sRr @uri)"
          cache_ids="$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/caches?ref=$encoded" --jq '.actions_caches[].id')"
          if [[ -z "$cache_ids" ]]; then
            echo "No merge-ref cache entries found for $ref"
            exit 0
          fi
          # shellcheck disable=SC2016
          if ! printf '%s\n' "$cache_ids" | xargs -r -P 4 -n 1 bash -c '
            repo="$1"
            id="$2"
            for delay in 1 2 4 8; do
              if gh api --method DELETE "repos/$repo/actions/caches/$id" >/dev/null 2>&1; then
                exit 0
              fi
              sleep "$delay"
            done
            printf "::warning::failed to delete cache id %s after retries\\n" "$id" >&2
            exit 1
          ' _ "$GITHUB_REPOSITORY"; then
            echo "::warning::some closed-PR cache entries could not be deleted; rerun maintenance"
          fi
  cache-budget:
    name: Cache retention
    if: ${{ github.event_name == 'schedule' || github.event_name == 'workflow_dispatch' }}
    runs-on: __MAINTENANCE_CACHE_RUNNER__
    timeout-minutes: 10
    permissions:
      contents: read
      actions: write
    steps:
VELNOR_RUNTIME_SETUP_STEPS      - name: Collect Actions cache account
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
          mkdir -p "$RUNNER_TEMP/cache-retention"
          gh api --paginate "repos/$GITHUB_REPOSITORY/actions/caches?per_page=100" \
            --jq '.actions_caches[] | {id, key, size_in_bytes, created_at, last_accessed_at}' \
            > "$RUNNER_TEMP/cache-retention/entries.jsonl"
          jq -s '.' "$RUNNER_TEMP/cache-retention/entries.jsonl" \
            > "$RUNNER_TEMP/cache-retention/entries.json"
          # `gh api --paginate --jq` runs the filter once per page and
          # concatenates the outputs: summing inside the filter prints one
          # number per page, and the total silently understates the account.
          # Slurp the page stream first, then take one total over every entry.
          total="$(jq '[.[].size_in_bytes] | add // 0' "$RUNNER_TEMP/cache-retention/entries.json")"
          count="$(jq 'length' "$RUNNER_TEMP/cache-retention/entries.json")"
          headroom="$(( "$(velnor-workflow cache-plan --mode=budget)" - total ))"
          jq -n --argjson total "$total" --argjson count "$count" --argjson headroom "$headroom" \
            --arg captured_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            '{captured_at: $captured_at, cache_count: $count, total_bytes: $total, headroom_bytes: $headroom}' \
            > "$RUNNER_TEMP/cache-retention/summary.json"
          cat "$RUNNER_TEMP/cache-retention/summary.json" >> "$GITHUB_STEP_SUMMARY"
      - name: Plan retention evictions
        run: |
          set -euo pipefail
          # The plan is the generator's own retention policy, executed - never
          # a shell copy of it: per-class budgets, bounded generations per
          # variant class, and the protected classes (toolchain seeds, Cargo
          # source bundles, the Docker seed baseline) reserved before rolling
          # compiler snapshots are touched. An access timestamp is not a
          # lease. The newest generation of a variant stays out of reach
          # inside the producer window; a superseded generation of the same
          # variant is eligible, because the newer save is the producer
          # signal that the older entry is no longer being written.
          velnor-workflow cache-plan \
            --now "$(date -u +%s)" \
            --entries "$RUNNER_TEMP/cache-retention/entries.json" \
            > "$RUNNER_TEMP/cache-retention/plan.json"
          if jq -e 'length > 0' "$RUNNER_TEMP/cache-retention/plan.json" > /dev/null; then
            {
              echo "Retention plan (evict oldest first: bound, class budget, global budget):"
              jq -r 'sort_by(.class, .reason) | group_by(.class, .reason)[] | "  \(.[0].class) / \(.[0].reason): \(length) entries, \(map(.size_in_bytes) | add) bytes"' \
                "$RUNNER_TEMP/cache-retention/plan.json"
            } >> "$GITHUB_STEP_SUMMARY"
          else
            echo "Retention plan: nothing to evict" >> "$GITHUB_STEP_SUMMARY"
          fi
      - name: Apply retention evictions
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
          evicted=0
          freed=0
          failed=0
          # The plan is applied verbatim, in its own order: generations beyond
          # their class bound first, then classes over budget, then the global
          # sweep - which never touches a protected class.
          while IFS=$'\t' read -r class reason id size key; do
            if gh api --method DELETE "repos/$GITHUB_REPOSITORY/actions/caches/$id" >/dev/null; then
              evicted=$((evicted + 1))
              freed=$((freed + size))
              echo "evicted id=$id class=$class reason=$reason size=$size key=$key"
            else
              failed=$((failed + 1))
              echo "::warning::failed to evict cache id $id (class $class, key $key)" >&2
            fi
          done < <(jq -r '.[] | [.class, .reason, .id, .size_in_bytes, .key] | @tsv' \
            "$RUNNER_TEMP/cache-retention/plan.json")
          # Every eviction is recorded under its cache class and its reason,
          # so a later cold run can be correlated with the eviction that
          # caused it.
          {
            echo "Evictions by cache class:"
            jq -r 'group_by(.class)[] | "  \(.[0].class): \(length) evictions, \(map(.size_in_bytes) | add) bytes"' \
              "$RUNNER_TEMP/cache-retention/plan.json"
          } >> "$GITHUB_STEP_SUMMARY"
          jq -n --argjson evicted "$evicted" --argjson freed "$freed" --argjson failed "$failed" \
            '{evicted_caches: $evicted, failed_evictions: $failed, freed_bytes: $freed}' \
            >> "$GITHUB_STEP_SUMMARY"
          # A failed eviction is a loud failure, never a swallowed warning:
          # silent DELETE failures leave the account over budget while the
          # run reports success.
          if (( failed > 0 )); then
            echo "::error::$failed retention evictions failed; rerun maintenance" >&2
            exit 1
          fi
      - name: Publish retention evidence
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: cache-retention-${{ github.run_id }}
          path: ${{ runner.temp }}/cache-retention
          if-no-files-found: error
          retention-days: 14
      - name: Enforce cache budget
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
          # Re-query live state: the enforcement decision must reflect what the
          # account actually holds after eviction, not the pre-eviction
          # snapshot. The same page-streaming rule as collection applies: the
          # sum is taken after slurping every page.
          total="$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/caches?per_page=100" \
            --jq '.actions_caches[].size_in_bytes' | jq -s 'add // 0')"
          budget="$(velnor-workflow cache-plan --mode=budget)"
          if (( total > budget )); then
            echo "::error::Actions cache account exceeds budget: $total > $budget bytes" >&2
            exit 1
          fi
"#;

/// Maintenance follows the configured CI mode: both prune and cache-budget
/// stay on Velnor when `runners = "velnor"`, so no GitHub-hosted `runs-on`
/// leaks into a Velnor-only surface. Hosted otherwise. `uses:` stays on the
/// `SOURCE_REV` pin (GitHub Actions rejects expressions in `uses:` versions).
/// `rev:` uses a context-gated `${{ github.sha }}` with a static fallback
/// when this repository owns the setup action.
fn render_maintenance(config: &ProjectConfig) -> String {
    let cache_lane = if config.runners == RunnerMode::Velnor {
        RunnerMode::Velnor
    } else {
        RunnerMode::Github
    };
    let prune_gate = format!(
        "github.event_name == 'pull_request' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{}' && inputs.pull_request_number != '')",
        config.default_branch
    );
    let cache_gate = if cache_lane == RunnerMode::Velnor {
        format!(
            "github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')",
            config.default_branch
        )
    } else {
        "github.event_name == 'schedule' || github.event_name == 'workflow_dispatch'".to_owned()
    };
    let setup = if cache_lane == RunnerMode::Github {
        workflow_runtime_setup_with_install_rev(
            RunnerMode::Github,
            &workflow_setup_install_rev(&config.repository),
        )
    } else {
        String::new()
    };

    MAINTENANCE_WORKFLOW
        .replace("VELNOR_RUNTIME_SETUP_STEPS", &setup)
        .replace(
            "__MAINTENANCE_PRUNE_RUNNER__",
            &configured_runner(config, cache_lane),
        )
        .replace(
            "__MAINTENANCE_CACHE_RUNNER__",
            &configured_runner(config, cache_lane),
        )
        .replace(
            "if: ${{ github.event_name == 'pull_request' || inputs.pull_request_number != '' }}",
            &format!("if: ${{{{ {prune_gate} }}}}"),
        )
        .replace(
            "if: ${{ github.event_name == 'schedule' || github.event_name == 'workflow_dispatch' }}",
            &format!("if: ${{{{ {cache_gate} }}}}"),
        )
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]

    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use sha2::{Digest as _, Sha256};

    use super::*;

    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn must_some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("{context}"),
        }
    }

    /// The digest of a rendered workflow, as the hex the `sha256sum` output
    /// spells: the pin the legacy-render test compares against.
    fn digest_of(content: &str) -> String {
        Sha256::digest(content.as_bytes()).iter().fold(
            String::with_capacity(64),
            |mut output, byte| {
                let _ = write!(output, "{byte:02x}");
                output
            },
        )
    }

    fn rendered(surface: &super::super::Surface, file: &str) -> String {
        let path = PathBuf::from(".github/workflows").join(file);
        must_some(
            surface.files.get(&path),
            &format!("the surface is missing {file}"),
        )
        .clone()
    }

    /// The `id:` job body, from the job key through the line before the next
    /// top-level job.
    fn yaml_job<'a>(workflow: &'a str, id: &str) -> &'a str {
        let header = format!("  {id}:");
        let mut start = None;
        let mut end = None;
        let mut offset = 0;
        for line in workflow.split_inclusive('\n') {
            let content = line.trim_end_matches('\n');
            if start.is_none() {
                if content == header {
                    start = Some(offset);
                }
            } else if content.starts_with("  ")
                && !content.starts_with("   ")
                && content.ends_with(':')
            {
                end = Some(offset);
                break;
            }
            offset += line.len();
        }
        let start = must_some(start, &format!("{id} job"));
        must_some(
            workflow.get(start..end.unwrap_or(workflow.len())),
            &format!("{id} job bytes"),
        )
    }

    fn assert_cache_retention_has_actions_write(workflow: &str) {
        let job = yaml_job(workflow, "cache-budget");
        assert!(
            job.contains("name: Cache retention"),
            "cache-budget must be the Cache retention job: {job}"
        );
        assert!(
            job.contains("permissions:\n      contents: read\n      actions: write"),
            "Cache retention must grant actions: write: {job}"
        );
    }

    fn assert_maintenance_is_github_hosted(workflow: &str, config: &ProjectConfig) {
        let hosted = format!("runs-on: {}", yaml_scalar(&config.github_runner));
        let runs_on: Vec<&str> = workflow
            .lines()
            .filter(|line| line.trim_start().starts_with("runs-on:"))
            .map(str::trim)
            .collect();
        assert_eq!(
            runs_on.as_slice(),
            [hosted.as_str(), hosted.as_str()],
            "both maintenance jobs must stay GitHub-hosted: {workflow}"
        );
        assert!(
            workflow.contains("setup-velnor-workflow"),
            "maintenance must install the hosted workflow runtime: {workflow}"
        );
        assert_eq!(
            crate::VELNOR_WORKFLOW_SOURCE_REV,
            "7fa4a0731ee8bedc5b02d90507d6dbe8b719153a"
        );
        let uses_line = must_some(
            workflow.lines().find(|line| {
                line.contains(&format!("uses: {}", crate::VELNOR_WORKFLOW_SETUP_ACTION))
            }),
            "setup-velnor-workflow uses line",
        );
        assert!(
            uses_line.contains("@7fa4a0731ee8bedc5b02d90507d6dbe8b719153a"),
            "uses: must pin SOURCE_REV: {uses_line}"
        );
        assert!(
            !uses_line.contains("github.sha"),
            "GitHub Actions forbids expressions in uses: versions: {uses_line}"
        );
        let expected_rev = crate::workflow_setup_install_rev(&config.repository);
        assert!(
            workflow.contains(&format!("rev: {expected_rev}")),
            "maintenance install rev follows setup-action ownership: {workflow}"
        );
        if config.repository != crate::workflow_setup_action_repository() {
            assert!(
                !workflow.contains(&format!("rev: {}", github_expression("github.sha"))),
                "foreign maintenance must not cargo-install a foreign github.sha: {workflow}"
            );
        }
        assert!(
            !workflow.contains("[self-hosted,"),
            "maintenance must not select the Velnor lane: {workflow}"
        );
        for label in &config.velnor_labels {
            if label == "self-hosted" {
                continue;
            }
            assert!(
                !workflow.contains(label.as_str()),
                "maintenance must not name Velnor label `{label}`: {workflow}"
            );
        }
        let prune_if = format!(
            "github.event_name == 'pull_request' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{}' && inputs.pull_request_number != '')",
            config.default_branch
        );
        assert!(
            workflow.contains(&prune_if),
            "closed-PR prune must stay live: {workflow}"
        );
        assert!(
            workflow.contains(
                "github.event_name == 'schedule' || github.event_name == 'workflow_dispatch'"
            ),
            "cache retention must keep schedule and dispatch: {workflow}"
        );
        if config.repository == crate::workflow_setup_action_repository() {
            assert!(
                workflow.contains("github.event_name == 'push'")
                    && workflow.contains("github.event.repository.default_branch")
                    && workflow.contains(crate::VELNOR_WORKFLOW_SOURCE_REV),
                "owned maintenance must use the context-gated runtime revision: {workflow}"
            );
        }
    }

    /// Every maintenance step that calls `gh api` must carry `GH_TOKEN`: an
    /// unauthenticated call fails, and where DELETE stderr is discarded the
    /// failure is silent retention drift.
    fn assert_maintenance_gh_api_steps_carry_token(workflow: &str) {
        let mut authenticated: Vec<String> = Vec::new();
        let mut current: Option<(String, String)> = None;
        let mut flush = |current: &mut Option<(String, String)>| {
            if let Some((name, body)) = current.take()
                && body.contains("gh api")
            {
                assert!(
                    body.contains("GH_TOKEN:"),
                    "maintenance step `{name}` calls gh api without GH_TOKEN: {body}"
                );
                authenticated.push(name);
            }
        };
        for line in workflow.lines() {
            if let Some(name) = line.strip_prefix("      - name: ") {
                flush(&mut current);
                current = Some((name.trim().to_owned(), String::new()));
            } else if let Some((_, body)) = current.as_mut() {
                body.push_str(line);
                body.push('\n');
            }
        }
        flush(&mut current);
        assert_eq!(
            authenticated.as_slice(),
            [
                "Delete merge-ref cache namespace",
                "Collect Actions cache account",
                "Apply retention evictions",
                "Enforce cache budget",
            ],
            "the gh api step set drifted; carry the token assertion to the new shape: {workflow}"
        );
    }

    fn assert_maintenance_runner_split(workflow: &str, config: &ProjectConfig) {
        let hosted = format!("runs-on: {}", yaml_scalar(&config.github_runner));
        let velnor = format!("runs-on: {}", configured_runner(config, RunnerMode::Velnor));
        let runs_on: Vec<&str> = workflow
            .lines()
            .filter(|line| line.trim_start().starts_with("runs-on:"))
            .map(str::trim)
            .collect();
        assert_eq!(
            runs_on.as_slice(),
            [velnor.as_str(), velnor.as_str()],
            "velnor-only maintenance must not emit a hosted runs-on: {workflow}"
        );
        assert!(
            !workflow.contains(&hosted),
            "velnor-only maintenance must not mention the hosted runner: {workflow}"
        );
        assert!(
            !workflow
                .lines()
                .any(|line| line.trim_start().starts_with("runs-on: ubuntu-")),
            "velnor-only maintenance must not emit runs-on: ubuntu-: {workflow}"
        );
        assert!(!workflow.contains("setup-velnor-workflow"));
        let prune_if = format!(
            "github.event_name == 'pull_request' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{}' && inputs.pull_request_number != '')",
            config.default_branch
        );
        assert!(
            workflow.contains(&prune_if),
            "closed-PR prune must stay live: {workflow}"
        );
        let cache_gate = format!(
            "github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')",
            config.default_branch
        );
        assert!(
            workflow.contains(&cache_gate),
            "Velnor cache retention must stay on the trusted lane: {workflow}"
        );
        assert!(!workflow.contains("\n  push:"));
    }

    /// A scanned throwaway repository: the only way to obtain a real shape.
    fn scanned_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-release-{name}-{}",
            crate::unique_suffix()
        ));
        must(fs::create_dir_all(&root), "create release test repository");
        must(
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"example\"\nversion = \"0.1.0\"\n",
            ),
            "write release fixture manifest",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.91.1\"\n",
            ),
            "write release fixture toolchain pin",
        );
        root
    }

    fn unit(id: &str) -> crate::Unit {
        crate::Unit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: crate::UnitKind::Rust,
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
            // The scan guarantees this fact on every Rust unit of a real
            // repository; the test config carries the same contract, pinning
            // the targets the fixture's release contract builds for.
            toolchain: Some(crate::RustToolchain {
                channel: "1.91.1".to_owned(),
                components: Vec::new(),
                targets: vec![
                    "x86_64-unknown-linux-gnu".to_owned(),
                    "aarch64-unknown-linux-gnu".to_owned(),
                ],
                profile: None,
            }),
            services: Vec::new(),
            workflow_file: None,
            requires_trusted: false,
            workspace_check: false,
        }
    }

    fn binary_spec() -> ReleaseSpec {
        ReleaseSpec {
            kind: "rust-binary".to_owned(),
            package: "example".to_owned(),
            packages: Vec::new(),
            binary: "example".to_owned(),
            targets: vec![
                "x86_64-unknown-linux-gnu".to_owned(),
                "aarch64-unknown-linux-gnu".to_owned(),
            ],
            image: String::new(),
            source_repository: String::new(),
            consumer_repository: String::new(),
            artifact_path: String::new(),
            description: String::new(),
            manifest_schema: String::new(),
        }
    }

    /// A native publisher with a package consumer and a declared consumer
    /// manifest schema. The schema URN is fixture-local: the generic crate
    /// never names a real consumer schema.
    fn native_spec() -> ReleaseSpec {
        ReleaseSpec {
            kind: "native".to_owned(),
            package: "example".to_owned(),
            packages: Vec::new(),
            binary: "example".to_owned(),
            targets: vec![
                "x86_64-unknown-linux-gnu".to_owned(),
                "aarch64-unknown-linux-gnu".to_owned(),
            ],
            image: "ghcr.io/example/app".to_owned(),
            source_repository: "example/app".to_owned(),
            consumer_repository: "example/apt".to_owned(),
            artifact_path: String::new(),
            description: String::new(),
            manifest_schema: "example.test/consumer-manifest-v1".to_owned(),
        }
    }

    /// The native config with the scan's `release-build` marker for the
    /// release package, the only input that arms the identity lane.
    fn native_identity_config(workflow_files: &[&str]) -> ProjectConfig {
        let mut config = config(workflow_files, Some(native_spec()));
        config.analysis.detected = vec!["release-build:example".to_owned()];
        config
    }

    fn config(workflow_files: &[&str], release: Option<ReleaseSpec>) -> ProjectConfig {
        crate::ProjectConfig {
            repository: String::new(),
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
            runners: crate::RunnerMode::Both,
            automatic: crate::RunnerMode::Github,
            github_runner: "ubuntu-24.04".to_owned(),
            macos_runner: "macos-15".to_owned(),
            velnor_labels: vec!["self-hosted".to_owned(), "example-runner".to_owned()],
            release_enabled: release.is_some(),
            release_reason: String::new(),
            release,
            units: vec![unit("rust-example")],
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            package_update_channels: None,
            velnor_runner_group: None,
            velnor_trusted_label: None,
            pull_request_on_velnor: false,
            default_dispatch_runner: crate::DEFAULT_DISPATCH_RUNNER.to_owned(),
            automatic_lanes: crate::DEFAULT_AUTOMATIC_LANES.to_owned(),
            velnor_rust_needs: crate::VelnorRustNeeds::Parallel,
            velnor_concurrency_group: None,
            velnor_serial_stack_groups: false,
            static_files: Vec::new(),
            declared_surface: false,
            mise_lock_keys: BTreeSet::new(),
        }
    }

    fn generate(
        root: &Path,
        config: &ProjectConfig,
        generation: Option<&str>,
    ) -> super::super::Surface {
        must(
            try_generate(root, config, generation),
            "generate release surface",
        )
    }

    fn try_generate(
        root: &Path,
        config: &ProjectConfig,
        generation: Option<&str>,
    ) -> Result<super::super::Surface, GeneratorError> {
        let shape = must(
            crate::scan::scan_shape(root, crate::RunnerMode::Both, "main", &[]),
            "scan release fixture",
        );
        let generation = generation.map(|rows| {
            let directory = root.join(crate::config::GENERATION_CONFIG_PATH);
            must(
                fs::create_dir_all(directory.parent().unwrap_or(root)),
                "create generation config directory",
            );
            must(
                fs::write(
                    &directory,
                    format!(
                        "schema = 1\n\n[generator]\nrepository = \"example/declared\"\n\n{rows}"
                    ),
                ),
                "write declared config",
            );
            let Some(generation) = must(crate::config::discover(root), "discover declared config")
            else {
                panic!("the declared config must be discovered");
            };
            generation
        });
        super::super::generate(root, &shape, config, generation.as_ref())
    }

    /// The default rows render the release surface, pinned to the bytes the
    /// reviewed renderer produced. The digests are the expectation, not a
    /// second call into the same code, so a renderer change shows up here and
    /// has to be carried into the pin deliberately; the structural assertions
    /// below say what the pinned bytes are for. The artifact signer and the
    /// policy provider are not pinned here: they are a repository's declared
    /// static surface, not part of the generic renderer.
    #[test]
    fn default_rows_render_the_legacy_release_surface() {
        const PINNED: &[(&str, &str)] = &[
            (
                "release.yml",
                "a66f1d3e5d50e7ccb4e8ce66771341d6c2013efb5b2dac951a4a9e9072d23d7d",
            ),
            (
                "preview.yml",
                "bce3395c072ee7451e2a0068a680e9e64b0f62652f30205ed43ac3dce410e49d",
            ),
            (
                "maintenance.yml",
                "3b68b122986adf2945fdb0edc08fc22b4f8269cec95c862842292c2de09a7dd9",
            ),
            (
                "ci-release-package-signer.yml",
                "63e76d5e5615192d52e34bba0b8bd51ddcec3b610e934633dd282c585d9721f1",
            ),
        ];
        let root = scanned_root("default");
        let config = config(
            &[
                "release.yml",
                "preview.yml",
                "maintenance.yml",
                "ci-release-package-signer.yml",
            ],
            Some(binary_spec()),
        );
        let surface = generate(&root, &config, None);
        let divergent: Vec<String> = PINNED
            .iter()
            .filter_map(|(file, digest)| {
                let rendered = rendered(&surface, file);
                let actual = digest_of(&rendered);
                (actual != *digest).then(|| format!("{file}: {actual}"))
            })
            .collect();
        assert!(
            divergent.is_empty(),
            "pinned renders diverged:\n  {}",
            divergent.join("\n  ")
        );
        // The publisher verifies before it publishes; the rolling preview and
        // the signer stay tag- and attestation-driven.
        let release = rendered(&surface, "release.yml");
        assert!(
            !release.contains("cargo fetch --locked      - name:"),
            "cargo fetch must terminate the run block before the next step: {release}"
        );
        assert!(release.contains("name: Release"), "{release}");
        assert!(release.contains("Publish GitHub release"), "{release}");
        assert!(release.contains("Verify archive checksums"), "{release}");
        assert!(!release.contains("inputs.unit"), "{release}");
        for (file, marker) in [
            ("preview.yml", "Preview"),
            ("maintenance.yml", "cache"),
            ("ci-release-package-signer.yml", "attest"),
        ] {
            let rendered = rendered(&surface, file);
            assert!(
                rendered.contains(marker),
                "{file} must still carry its `{marker}` contract: {rendered}"
            );
            assert!(
                rendered.starts_with(crate::GENERATED_HEADER),
                "{file} must carry the generated header: {rendered}"
            );
        }
        assert!(surface.added_files.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    /// The identity release surface, pinned like the legacy one: any renderer
    /// change shows up here and has to be carried into the pin deliberately.
    #[test]
    fn identity_rows_render_the_pinned_native_surface() {
        const PINNED: &[(&str, &str)] = &[
            (
                "release.yml",
                "196eb3bf081be5f8e893a69464f3f65202aa2be1a66337b535a3c482f0110aa3",
            ),
            (
                "preview.yml",
                "3fc09285bf9d971005bb1c2a24e5be2f99b059b3d1202eda294992d30bae6445",
            ),
        ];
        let root = scanned_root("identity-pinned");
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let surface = generate(&root, &config, None);
        let divergent: Vec<String> = PINNED
            .iter()
            .filter_map(|(file, digest)| {
                let rendered = rendered(&surface, file);
                let actual = digest_of(&rendered);
                (actual != *digest).then(|| format!("{file}: {actual}"))
            })
            .collect();
        assert!(
            divergent.is_empty(),
            "pinned identity renders diverged:\n  {}",
            divergent.join("\n  ")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn velnor_release_surfaces_have_no_hosted_runner_branch() {
        let mut config = config(&["release.yml", "preview.yml"], Some(binary_spec()));
        config.runners = RunnerMode::Velnor;
        let Some(release) = config.release.as_ref() else {
            panic!("release fixture must carry a release contract")
        };
        let rendered = [
            super::render_release(&config, release),
            super::render_preview(&config, Some(release)),
        ];

        for workflow in rendered {
            assert!(workflow.contains("runs-on: [self-hosted, example-runner]"));
            assert!(!workflow.contains("runs-on: ubuntu-24.04"), "{workflow}");
            assert!(!workflow.contains("github-hosted"), "{workflow}");
        }
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn release_lanes_honor_the_configured_runner_labels() {
        let mut spec = binary_spec();
        spec.targets.push("aarch64-apple-darwin".to_owned());
        let mut cfg = config(&["preview.yml", "release.yml"], Some(spec));
        cfg.runners = RunnerMode::Github;
        cfg.github_runner = "ubuntu-custom".to_owned();
        cfg.macos_runner = "macos-custom".to_owned();
        let Some(release) = cfg.release.as_ref() else {
            panic!("release fixture must carry a release contract");
        };
        for workflow in [
            super::render_preview(&cfg, Some(release)),
            super::render_release(&cfg, release),
        ] {
            assert!(
                workflow.contains("runner: ubuntu-custom"),
                "linux lanes must use the configured github runner: {workflow}"
            );
            assert!(
                workflow.contains("runner: macos-custom"),
                "apple lanes must use the configured macos runner: {workflow}"
            );
            assert!(
                !workflow.contains("macos-15"),
                "no hardcoded macos label may survive: {workflow}"
            );
            assert!(
                !workflow.contains("ubuntu-24.04-arm"),
                "no hardcoded arm label may survive: {workflow}"
            );
        }
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn dual_lane_release_publishers_stay_hosted_while_verification_stays_dual_lane() {
        let cfg = config(&["preview.yml", "release.yml"], Some(binary_spec()));
        let Some(release) = cfg.release.as_ref() else {
            panic!("release fixture must carry a release contract");
        };
        let preview = super::render_preview(&cfg, Some(release));
        let release_workflow = super::render_release(&cfg, release);

        for workflow in [&preview, &release_workflow] {
            assert!(workflow.contains("lane: github"), "{workflow}");
            assert!(
                !workflow.contains("lane: velnor"),
                "release-side artifact matrices must stay hosted: {workflow}"
            );
            assert!(
                !workflow.contains("runner: { group:"),
                "release-side artifact matrices must not carry a dynamic Velnor runner: {workflow}"
            );
            assert!(
                !workflow.contains("runs-on: ${{ matrix.runner }}"),
                "hosted-only release-side matrices must use a literal runner: {workflow}"
            );
            assert!(
                workflow.contains("runs-on: ubuntu-24.04"),
                "hosted release-side runner must remain configured: {workflow}"
            );
        }
        assert!(
            release_workflow.contains("release-velnor-rust-example"),
            "dual-lane release verification must retain its literal Velnor job: {release_workflow}"
        );
    }

    #[test]
    fn maintenance_splits_prune_and_cache_when_runners_are_velnor() {
        let mut cfg = config(&["maintenance.yml"], None);
        cfg.runners = RunnerMode::Velnor;

        let direct = super::render_maintenance(&cfg);
        assert_maintenance_runner_split(&direct, &cfg);
        assert_cache_retention_has_actions_write(&direct);

        let root = scanned_root("maintenance-hosted");
        let surface = generate(&root, &cfg, None);
        let generated = rendered(&surface, "maintenance.yml");
        assert!(
            generated.starts_with(crate::GENERATED_HEADER),
            "generated maintenance.yml must carry the header: {generated}"
        );
        assert_maintenance_runner_split(&generated, &cfg);
        assert_cache_retention_has_actions_write(&generated);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn generated_cache_retention_job_has_actions_write() {
        let cfg = config(&["maintenance.yml"], None);
        let root = scanned_root("cache-retention-permissions");
        let surface = generate(&root, &cfg, None);
        let generated = rendered(&surface, "maintenance.yml");
        assert_cache_retention_has_actions_write(&generated);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn maintenance_gh_api_steps_carry_gh_token() {
        for runners in [RunnerMode::Github, RunnerMode::Velnor, RunnerMode::Both] {
            let mut cfg = config(&["maintenance.yml"], None);
            cfg.runners = runners;
            let workflow = super::render_maintenance(&cfg);
            assert_maintenance_gh_api_steps_carry_token(&workflow);
            assert!(
                workflow.contains("if (( failed > 0 ))"),
                "failed evictions must fail the step loudly: {workflow}"
            );
        }

        let mut cfg = config(&["maintenance.yml"], None);
        cfg.runners = RunnerMode::Velnor;
        let root = scanned_root("maintenance-token");
        let surface = generate(&root, &cfg, None);
        assert_maintenance_gh_api_steps_carry_token(&rendered(&surface, "maintenance.yml"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn maintenance_uses_configured_runners_for_github_hosted_modes() {
        for runners in [RunnerMode::Github, RunnerMode::Both] {
            let mut cfg = config(&["maintenance.yml"], None);
            cfg.runners = runners;
            let workflow = super::render_maintenance(&cfg);
            assert_maintenance_is_github_hosted(&workflow, &cfg);
        }
    }

    #[test]
    fn maintenance_installs_head_when_this_repository_owns_setup() {
        let mut cfg = config(&["maintenance.yml"], None);
        cfg.repository = crate::workflow_setup_action_repository().to_owned();
        let workflow = super::render_maintenance(&cfg);
        assert_maintenance_is_github_hosted(&workflow, &cfg);
        assert!(
            workflow.contains(&format!(
                "rev: {}",
                crate::workflow_setup_install_rev(crate::workflow_setup_action_repository())
            )),
            "the setup-action owner uses a context-gated HEAD fallback: {workflow}"
        );
    }

    #[test]
    fn maintenance_keeps_github_runner_out_of_runs_on_when_runners_are_velnor() {
        let mut cfg = config(&["maintenance.yml"], None);
        cfg.runners = RunnerMode::Velnor;
        cfg.github_runner = "ubuntu-22.04".to_owned();
        cfg.default_branch = "trunk".to_owned();

        let workflow = super::render_maintenance(&cfg);
        assert_maintenance_runner_split(&workflow, &cfg);
        assert_cache_retention_has_actions_write(&workflow);
        assert!(
            !workflow.contains("runs-on: ubuntu-22.04"),
            "velnor-only maintenance must not leak config.github_runner into runs-on: {workflow}"
        );
        assert!(
            !workflow.contains("runs-on: ubuntu-24.04"),
            "velnor-only maintenance must not emit a hosted runs-on: {workflow}"
        );
    }

    /// An incomplete contract omits the publisher exactly as the legacy path
    /// does: the preview file carries the omission, the publisher is absent.
    #[test]
    fn an_incomplete_contract_omits_the_publisher_like_the_legacy_path() {
        let root = scanned_root("omitted");
        let config = config(&["release.yml", "preview.yml"], None);
        let surface = generate(&root, &config, None);
        let legacy = must(crate::generated_files(&config), "generate legacy files");
        let preview = PathBuf::from(".github/workflows/preview.yml");
        assert_eq!(
            surface.files.get(&preview),
            legacy.get(&preview),
            "the omitted preview must match the legacy comment"
        );
        let release = PathBuf::from(".github/workflows/release.yml");
        assert!(!surface.files.contains_key(&release));
        assert!(!legacy.contains_key(&release));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_declared_release_row_adds_the_publisher() {
        let root = scanned_root("declared-release");
        let config = config(&["preview.yml"], None);
        let surface = generate(
            &root,
            &config,
            Some(
                "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                 [declare.args]\nkind = \"rust-binary\"\npackage = \"example\"\n\
                 binary = \"example\"\n\
                 targets = [\"x86_64-unknown-linux-gnu\", \"aarch64-unknown-linux-gnu\"]\n",
            ),
        );
        let release = surface
            .files
            .get(&PathBuf::from(".github/workflows/release.yml"))
            .unwrap_or_else(|| panic!("a declared release row must render release.yml"));
        assert!(release.contains("name: Release"), "{release}");
        assert!(release.contains("Publish GitHub release"), "{release}");
        assert!(
            release.contains("target: aarch64-unknown-linux-gnu"),
            "{release}"
        );
        assert_eq!(surface.added_files, vec!["release.yml".to_owned()]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn github_release_unit_cache_restore_declares_cache_step_id() {
        let mut config = config(&["release.yml"], Some(binary_spec()));
        config.units[0].pr_commands = vec!["mbx nextest run --locked".to_owned()];
        config.units[0].full_commands = vec!["mbx nextest run --locked".to_owned()];
        config.units[0].cache = Some(crate::CacheSpec {
            key_files: vec!["Cargo.lock".to_owned()],
            paths: vec!["~/.cargo/registry".to_owned()],
            purpose: crate::CachePurpose::CargoSources,
            mbx_output_cache_justification: None,
            mutable_mount_seed: false,
        });
        let Some(release) = config.release.as_ref() else {
            panic!("binary fixture must carry a release contract");
        };
        let workflow = super::render_release(&config, release);
        assert!(
            workflow.contains("      - name: Restore rust-example cache\n        id: cache\n"),
            "restore must expose steps.cache for the fetch gate: {workflow}"
        );
        assert!(
            workflow.contains("        if: ${{ steps.cache.outputs.cache-hit != 'true' }}\n"),
            "Prepare Cargo sources must key off the restore id: {workflow}"
        );
    }

    #[test]
    fn a_declared_native_release_admits_github_writer_only() {
        for (name, package, image, consumer) in [
            (
                "native-release",
                "example",
                "ghcr.io/example/app",
                "example/apt",
            ),
            (
                "native-release-acme",
                "widget",
                "ghcr.io/acme/widget",
                "acme/apt",
            ),
        ] {
            let root = scanned_root(name);
            let config = config(&["preview.yml"], None);
            let surface = generate(
                &root,
                &config,
                Some(&format!(
                    "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                     [declare.args]\nkind = \"native\"\npackage = \"{package}\"\n\
                     binary = \"{package}\"\n\
                     targets = [\"x86_64-unknown-linux-gnu\", \"aarch64-unknown-linux-gnu\"]\n\
                     image = \"{image}\"\n\
                     consumer_repository = \"{consumer}\"\n"
                )),
            );
            let release = surface
                .files
                .get(&PathBuf::from(".github/workflows/release.yml"))
                .unwrap_or_else(|| panic!("a declared native release must render release.yml"));
            assert!(release.contains("name: Admit release runner"), "{release}");
            assert!(
                release.contains("native release publishes from GitHub only"),
                "{release}"
            );
            assert!(release.contains("Publish container image"), "{release}");
            assert!(release.contains(image), "{release}");
            assert!(release.contains("github.ref_name"), "{release}");
            assert!(
                !release.contains(
                    "  image:\n    name: Publish container image\n    needs: [admit-runner, verify, build, image]"
                ),
                "image must not depend on itself: {release}"
            );
            assert!(release.contains("Package Debian artifacts"), "{release}");
            assert!(
                release.contains(&format!("--package {package}")),
                "{release}"
            );
            assert!(release.contains("default: github"), "{release}");
            assert!(!release.contains("default: velnor"), "{release}");
            assert!(
                !release.contains("guest-payload"),
                "native release without a scanned guest image must not emit guest-payload: {release}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_identity_release_wires_release_build_and_deb_publishing() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        // One resolved version, stripped of the tag prefix.
        assert!(
            workflow.contains("version: ${{ steps.version.outputs.version }}"),
            "{workflow}"
        );
        assert!(workflow.contains("Resolve release version"), "{workflow}");
        assert!(workflow.contains("version=\"${TAG#v}\""), "{workflow}");
        // The container tag follows the resolved version, never the raw ref:
        // the index job promotes the admitted image under GHCR_IMAGE:VERSION.
        let image = yaml_job(&workflow, "image");
        assert!(image.contains("GHCR_IMAGE: ghcr.io/example/app"), "{image}");
        assert!(
            image.contains("VERSION: ${{ needs.verify.outputs.version }}"),
            "{image}"
        );
        assert!(
            image.contains("--tag \"${GHCR_IMAGE}:${VERSION}\""),
            "{image}"
        );
        assert!(
            !workflow.contains("ghcr.io/example/app:${{ github.ref_name }}"),
            "{workflow}"
        );
        // Tarball builds carry release identity.
        let build = yaml_job(&workflow, "build");
        assert!(build.contains("VELNOR_RELEASE_BUILD: \"1\""), "{build}");
        assert!(build.contains("--features release-build"), "{build}");
        // The metadata job exports the acyclic identity files once.
        let metadata = yaml_job(&workflow, "metadata");
        assert!(metadata.contains("needs: [verify]"), "{metadata}");
        assert!(
            metadata.contains("release export > build-identity.json"),
            "{metadata}"
        );
        assert!(
            metadata.contains("capabilities export > manifest.json"),
            "{metadata}"
        );
        assert!(metadata.contains("name: release-metadata"), "{metadata}");
        // The debian job prebuilds with identity, stages the record inputs,
        // and packages exactly one renamed consumer deb per arch.
        let debian = yaml_job(&workflow, "debian");
        assert!(
            debian.contains("needs: [admit-runner, verify, build, metadata]"),
            "{debian}"
        );
        assert!(debian.contains("VELNOR_RELEASE_BUILD: \"1\""), "{debian}");
        assert!(debian.contains("--features release-build"), "{debian}");
        assert!(debian.contains("cargo install cargo-deb"), "{debian}");
        assert!(debian.contains("release emit"), "{debian}");
        assert!(
            debian.contains("--no-build true --asset-name \"example-${{ needs.verify.outputs.version }}-${{ matrix.arch }}.deb\""),
            "{debian}"
        );
        assert!(debian.contains("empty-deb-incident"), "{debian}");
        assert!(debian.contains("dist/*.deb.sha256"), "{debian}");
        // Debs attest through the shared signer, never inline.
        let sign = yaml_job(&workflow, "sign-deb");
        assert!(
            sign.contains("uses: ./.github/workflows/ci-release-package-signer.yml"),
            "{sign}"
        );
        assert!(
            sign.contains(
                "subject-path: example-${{ needs.verify.outputs.version }}-${{ matrix.arch }}.deb"
            ),
            "{sign}"
        );
        assert!(sign.contains("source-ref: refs/tags/"), "{sign}");
        assert!(!debian.contains("Attest Debian packages"), "{debian}");
        // Publish attaches both lanes plus the consumer manifest.
        let publish = yaml_job(&workflow, "publish");
        assert!(publish.contains("name: debian-packages"), "{publish}");
        assert!(publish.contains("name: release-metadata"), "{publish}");
        assert!(
            publish.contains("--signer-workflow \"$GITHUB_REPOSITORY/.github/workflows/ci-release-package-signer.yml\""),
            "{publish}"
        );
        assert!(publish.contains("SHA256SUMS"), "{publish}");
        assert!(
            publish.contains("--arg schema 'example.test/consumer-manifest-v1'"),
            "{publish}"
        );
        assert!(publish.contains("release-manifest.json"), "{publish}");
        assert!(
            publish.contains("artifacts/example-${{ needs.verify.outputs.version }}-amd64.deb"),
            "{publish}"
        );
        assert!(
            publish.contains(
                "needs: [admit-runner, verify, build, image, metadata, debian, sign-deb]"
            ),
            "{publish}"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_identity_image_admission_inspects_without_mutation() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        // Admission inspects the version tag without mutating it: absent
        // opens the lane, present is adopted only with a matching release
        // or an explicitly supplied recovery digest.
        let admission = yaml_job(&workflow, "image-admission");
        assert!(
            admission.contains("needs: [admit-runner, verify]"),
            "{admission}"
        );
        assert!(
            admission.contains("existing: ${{ steps.inspect.outputs.existing }}"),
            "{admission}"
        );
        assert!(
            admission.contains("index_digest: ${{ steps.inspect.outputs.index_digest }}"),
            "{admission}"
        );
        assert!(
            admission.contains("imagetools inspect \"$ref\" --format '{{json .}}'"),
            "{admission}"
        );
        assert!(
            admission.contains("inputs.existing-image-digest"),
            "{admission}"
        );
        assert!(
            admission.contains("refusing to adopt unknown bytes"),
            "{admission}"
        );
        assert!(
            admission.contains("refusing a fail-open publish"),
            "{admission}"
        );
        assert!(
            workflow.contains("existing-image-digest:"),
            "the recovery input must exist wherever admission reads it: {workflow}"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_identity_image_platform_builds_per_arch_staging_tags() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        // Each native builder pushes one platform under a disposable
        // commit-scoped staging tag; the child config carries the
        // record-bound labels.
        let platform = yaml_job(&workflow, "image-platform");
        assert!(
            platform.contains("needs: [admit-runner, verify, metadata, image-admission]"),
            "{platform}"
        );
        assert!(
            platform.contains("if: ${{ needs.image-admission.outputs.existing != 'true' }}"),
            "{platform}"
        );
        assert!(platform.contains("- arch: amd64"), "{platform}");
        assert!(platform.contains("platform: linux/amd64"), "{platform}");
        assert!(platform.contains("- arch: arm64"), "{platform}");
        assert!(platform.contains("platform: linux/arm64"), "{platform}");
        assert!(platform.contains("runner: ubuntu-24.04-arm"), "{platform}");
        assert!(
            platform.contains("runs-on: ${{ matrix.runner }}"),
            "{platform}"
        );
        assert!(
            platform.contains("ref: ${{ github.sha }}"),
            "platform builders pin the gated event commit: {platform}"
        );
        assert!(
            platform.contains("Build the workflow binary for the image"),
            "{platform}"
        );
        assert!(platform.contains("--package velnor-workflow"), "{platform}");
        assert!(
            platform.contains("release-binaries/${{ matrix.arch }}/velnor-workflow"),
            "{platform}"
        );
        assert!(
            platform.contains("file: docker/job-ubuntu.Dockerfile"),
            "{platform}"
        );
        assert!(
            platform.contains(
                "tags: ${{ env.GHCR_IMAGE }}:release-${{ env.COMMIT }}-${{ matrix.arch }}"
            ),
            "{platform}"
        );
        assert!(
            platform
                .contains("org.opencontainers.image.version=${{ needs.verify.outputs.version }}"),
            "{platform}"
        );
        assert!(
            platform.contains("org.opencontainers.image.revision=${{ github.sha }}"),
            "{platform}"
        );
        assert!(
            platform.contains("org.opencontainers.image.source=https://github.com/example/app"),
            "{platform}"
        );
        assert!(
            platform.contains(
                "org.velnor.manifest-sha256=${{ needs.metadata.outputs.manifest_sha256 }}"
            ),
            "{platform}"
        );
        assert!(
            platform.contains("expected one image manifest for architecture"),
            "{platform}"
        );
        assert!(
            platform.contains("name: image-platform-${{ matrix.arch }}"),
            "{platform}"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_identity_image_index_assembles_one_immutable_tag() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        // The index job assembles (or re-verifies) the immutable version
        // tag and exports the digest the record binds.
        let image = yaml_job(&workflow, "image");
        assert!(
            image.contains("needs.image-admission.result == 'success'"),
            "{image}"
        );
        assert!(
            image.contains(
                "(needs.image-platform.result == 'success' || needs.image-platform.result == 'skipped')"
            ),
            "{image}"
        );
        assert!(
            image.contains("index_digest: ${{ steps.push.outputs.index_digest }}"),
            "{image}"
        );
        assert!(
            image.contains("manifest_sha256: ${{ needs.metadata.outputs.manifest_sha256 }}"),
            "{image}"
        );
        assert!(image.contains("imagetools create"), "{image}");
        assert!(
            image.contains("--tag \"${GHCR_IMAGE}:${VERSION}\""),
            "{image}"
        );
        assert!(
            image.contains("${GHCR_IMAGE}:release-${COMMIT}-amd64"),
            "{image}"
        );
        assert!(
            image.contains("${GHCR_IMAGE}:release-${COMMIT}-arm64"),
            "{image}"
        );
        assert!(
            image.contains("does not reference both newly built platform digests"),
            "{image}"
        );
        assert!(image.contains("name: image-digests"), "{image}");
        // The manifest digest flows from the metadata job into labels,
        // the index outputs, and the record.
        let metadata = yaml_job(&workflow, "metadata");
        assert!(
            metadata.contains("manifest_sha256: ${{ steps.export.outputs.manifest_sha256 }}"),
            "{metadata}"
        );
        assert!(metadata.contains("id: export"), "{metadata}");
        assert!(
            metadata.contains("echo \"manifest_sha256=$sha256\" >> \"$GITHUB_OUTPUT\""),
            "{metadata}"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_identity_publish_assembles_and_binds_the_release_record() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        let publish = yaml_job(&workflow, "publish");
        // The record inputs arrive as job outputs and image coordinates.
        assert!(publish.contains("COMMIT: ${{ github.sha }}"), "{publish}");
        assert!(
            publish.contains("INDEX_DIGEST: ${{ needs.image.outputs.index_digest }}"),
            "{publish}"
        );
        assert!(
            publish.contains("MANIFEST_SHA256: ${{ needs.image.outputs.manifest_sha256 }}"),
            "{publish}"
        );
        assert!(
            publish.contains("GHCR_IMAGE: ghcr.io/example/app"),
            "{publish}"
        );
        assert!(
            publish.contains("SOURCE_URL: https://github.com/example/app"),
            "{publish}"
        );
        assert!(publish.contains("packages: read"), "{publish}");
        assert!(publish.contains("name: image-digests"), "{publish}");
        // The record is assembled from downloaded bytes, never trusted.
        assert!(
            publish.contains("Assemble the release record from downloaded artifacts"),
            "{publish}"
        );
        assert!(publish.contains("velnor.release-record/v1"), "{publish}");
        assert!(publish.contains("oci_index_digest"), "{publish}");
        assert!(publish.contains("oci_platform_digest"), "{publish}");
        assert!(
            publish.contains("artifacts/image-digests.json"),
            "{publish}"
        );
        assert!(
            publish.contains("release assemble"),
            "the candidate must pass through the release tool: {publish}"
        );
        assert!(
            publish.contains("--record record.candidate.json"),
            "{publish}"
        );
        assert!(publish.contains("--artifacts artifacts"), "{publish}");
        assert!(publish.contains("--out release-record.json"), "{publish}");
        // Packaged bytes must match the record before publication.
        assert!(
            publish.contains("Verify packaged runner identity before release creation"),
            "{publish}"
        );
        assert!(
            publish.contains("./usr/bin/example | sha256sum"),
            "{publish}"
        );
        assert!(publish.contains("digest != release record"), "{publish}");
        // The OCI index and the git tag must not have moved.
        assert!(
            publish.contains("Verify OCI index stayed immutable before publication"),
            "{publish}"
        );
        assert!(
            publish.contains("Verify release tag stayed immutable before publication"),
            "{publish}"
        );
        assert!(
            publish.contains("git ls-remote --exit-code origin"),
            "{publish}"
        );
        // The release is created once and never clobbered; a re-run
        // re-verifies the published bytes instead of uploading a rebuild.
        assert!(
            publish.contains("Create release once — no clobber, record-verified idempotency"),
            "{publish}"
        );
        assert!(publish.contains("gh release view \"$tag\""), "{publish}");
        assert!(
            publish.contains("gh release download \"$tag\""),
            "{publish}"
        );
        assert!(publish.contains("refusing to touch"), "{publish}");
        assert!(
            publish.contains(
                "gh release create \"$tag\" --verify-tag --target \"$COMMIT\" --title \"$tag\" --generate-notes"
            ),
            "{publish}"
        );
        assert_eq!(
            publish.matches("--clobber").count(),
            1,
            "only the idempotency re-download into a fresh directory may clobber: {publish}"
        );
        assert!(
            publish.contains("Stage package subjects for hosted signer"),
            "{publish}"
        );
        assert!(publish.contains("name: package-subjects"), "{publish}");
        assert!(
            publish.contains("release-record.json \\\n              release-record.json.sha256"),
            "{publish}"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_identity_build_records_binary_digest_and_debian_reuses_it() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        // The build job records the raw binary digest next to its tarball.
        let build = yaml_job(&workflow, "build");
        assert!(build.contains("Record release binary digest"), "{build}");
        assert!(build.contains("-$arch.bin.sha256"), "{build}");
        assert!(
            build.contains("x86_64-unknown-linux-gnu) arch=amd64"),
            "{build}"
        );
        assert!(
            build.contains("aarch64-unknown-linux-gnu) arch=arm64"),
            "{build}"
        );
        assert!(build.contains("needs a Debian architecture"), "{build}");
        // The stable deb packages the build job's exact bytes: no second
        // build that merely should agree.
        let debian = yaml_job(&workflow, "debian");
        assert!(debian.contains("Download release binary"), "{debian}");
        assert!(
            debian.contains("name: github-${{ matrix.target }}"),
            "{debian}"
        );
        assert!(
            debian.contains("Reuse the build job's release binary"),
            "{debian}"
        );
        assert!(
            debian.contains("reused runner binary does not match the recorded digest"),
            "{debian}"
        );
        assert!(
            !debian.contains("Build release runner binary"),
            "stable debian must reuse, never rebuild: {debian}"
        );
        // The preview lane is record-free by design: it keeps its own
        // build and learns nothing about records or OCI indexes.
        let preview = super::render_preview(&config, Some(release));
        let preview_debian = yaml_job(&preview, "debian");
        assert!(
            preview_debian.contains("Build release runner binary"),
            "{preview_debian}"
        );
        assert!(
            !preview.contains("Reuse the build job's release binary"),
            "{preview}"
        );
        assert!(!preview.contains("release-record"), "{preview}");
        assert!(!preview.contains("imagetools"), "{preview}");
        assert!(!preview.contains("image-digests"), "{preview}");
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_image_lane_with_unmapped_target_fails_closed() {
        let mut config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_mut() else {
            panic!("identity fixture must carry a release contract")
        };
        release.targets.push("aarch64-apple-darwin".to_owned());
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        // A partial multi-arch index must never ship: the platform lane
        // fails closed while the read-only admission gate stays intact.
        let platform = yaml_job(&workflow, "image-platform");
        assert!(
            platform.contains("Reject non-multi-arch image contract"),
            "{platform}"
        );
        let admission = yaml_job(&workflow, "image-admission");
        assert!(
            admission.contains("Inspect version tag without mutation"),
            "{admission}"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_preview_produces_debs_with_rolling_identity() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let preview = super::render_preview(&config, Some(release));
        // A newer main must cancel a stale preview so it cannot publish after
        // the current tip; only one run is active, so replaces cannot race.
        assert!(preview.contains("cancel-in-progress: true"), "{preview}");
        // Identity resolves one preview version from the crate manifest.
        let identity = yaml_job(&preview, "identity");
        assert!(identity.contains("name=Preview $version"), "{identity}");
        assert!(
            identity.contains("~preview.${RUN_NUMBER}+${short_commit}"),
            "{identity}"
        );
        // Metadata and debs bind the preview source commit.
        let metadata = yaml_job(&preview, "metadata");
        assert!(metadata.contains("needs: [identity]"), "{metadata}");
        assert!(
            metadata.contains("VELNOR_PREVIEW_SOURCE_SHA: ${{ needs.identity.outputs.commit }}"),
            "{metadata}"
        );
        let debian = yaml_job(&preview, "debian");
        assert!(debian.contains("needs: [identity, metadata]"), "{debian}");
        assert!(
            debian.contains("VERSION: ${{ needs.identity.outputs.version }}"),
            "{debian}"
        );
        assert!(
            debian.contains("--asset-name \"example-preview-${{ needs.identity.outputs.version }}-${{ matrix.arch }}.deb\""),
            "{debian}"
        );
        // The rolling release is replaced under its Preview title, never
        // moved backward, and its published shape is verified.
        let publish = yaml_job(&preview, "publish");
        assert!(
            publish.contains("needs: [identity, debian, sign-deb]"),
            "{publish}"
        );
        assert!(
            publish.contains(
                "gh release create preview --target \"$COMMIT\" --prerelease --title \"$NAME\""
            ),
            "{publish}"
        );
        assert!(
            publish.contains("refusing to move the rolling preview backward"),
            "{publish}"
        );
        assert!(
            publish.contains("Verify the published rolling preview"),
            "{publish}"
        );
        assert!(!preview.contains("Rolling preview"), "{preview}");
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_without_detected_feature_keeps_plain_builds() {
        let mut spec = native_spec();
        spec.image = "ghcr.io/example/plain".to_owned();
        let config = config(&["release.yml", "preview.yml"], Some(spec));
        let Some(release) = config.release.as_ref() else {
            panic!("plain fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        // The resolved version and the coherent container tag are native
        // contract behavior, independent of the release-build feature.
        assert!(
            workflow.contains("ghcr.io/example/plain:${{ needs.verify.outputs.version }}"),
            "{workflow}"
        );
        // Everything else stays plain: no identity env, no feature flag, no
        // metadata job, no signer, and the legacy deb publisher.
        assert!(!workflow.contains("VELNOR_RELEASE_BUILD"), "{workflow}");
        assert!(!workflow.contains("--features release-build"), "{workflow}");
        assert!(
            !workflow.contains("Compile release metadata once"),
            "{workflow}"
        );
        assert!(!workflow.contains("sign-deb"), "{workflow}");
        assert!(workflow.contains("Package Debian artifacts"), "{workflow}");
        let preview = super::render_preview(&config, Some(release));
        assert!(!preview.contains("VELNOR_RELEASE_BUILD"), "{preview}");
        assert!(!preview.contains("Resolve preview identity"), "{preview}");
        assert!(preview.contains("Publish rolling preview"), "{preview}");
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_debian_without_manifest_schema_fails_closed() {
        let mut config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_mut() else {
            panic!("identity fixture must carry a release contract")
        };
        release.manifest_schema.clear();
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        for workflow in [
            super::render_release(&config, release),
            super::render_preview(&config, Some(release)),
        ] {
            assert!(
                workflow
                    .contains("needs manifest_schema; declare the consumer manifest schema URN"),
                "{workflow}"
            );
        }
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_debian_with_unmapped_target_fails_closed() {
        let mut config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_mut() else {
            panic!("identity fixture must carry a release contract")
        };
        release.targets.push("aarch64-apple-darwin".to_owned());
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        for workflow in [
            super::render_release(&config, release),
            super::render_preview(&config, Some(release)),
        ] {
            assert!(
                workflow.contains("Reject undebianable target"),
                "{workflow}"
            );
            assert!(!workflow.contains("Build Debian packages"), "{workflow}");
        }
    }

    #[test]
    fn native_guest_payload_uses_scanned_guest_bins() {
        for (name, package) in [("guest-widget", "widget"), ("guest-acme", "acme-box")] {
            let agent = format!("{package}-guest-agent");
            let image = format!("{package}-guest-image");
            let root = scanned_root(name);
            must(
                fs::write(
                    root.join("Cargo.toml"),
                    format!(
                        "[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                         [[bin]]\nname = \"{package}\"\npath = \"src/main.rs\"\n\n\
                         [[bin]]\nname = \"{agent}\"\npath = \"src/bin/agent.rs\"\n\n\
                         [[bin]]\nname = \"{image}\"\npath = \"src/bin/image.rs\"\n"
                    ),
                ),
                "write guest crate manifest",
            );
            must(fs::create_dir_all(root.join("src/bin")), "create bin dir");
            must(
                fs::write(root.join("src/main.rs"), "fn main() {}\n"),
                "write main",
            );
            must(
                fs::write(root.join("src/bin/agent.rs"), "fn main() {}\n"),
                "write agent",
            );
            must(
                fs::write(root.join("src/bin/image.rs"), "fn main() {}\n"),
                "write image",
            );
            must(fs::create_dir_all(root.join("microvm")), "create microvm");
            must(
                fs::write(
                    root.join("microvm/pins.json"),
                    "{\n  \"kernel_tarball\": \"https://example.invalid/linux.tar.xz\",\n  \"kernel_tarball_sha256\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n}\n",
                ),
                "write pins",
            );
            let shape = must(
                crate::scan::scan_shape(&root, crate::RunnerMode::Github, "main", &[]),
                "scan guest fixture",
            );
            let mut scanned = crate::ProjectConfig::from(shape.clone());
            for unit in &mut scanned.units {
                if !unit.watch.iter().any(|path| path.contains("microvm")) {
                    unit.watch.push("microvm/**".to_owned());
                }
            }
            scanned.workflow_files = vec!["release.yml".to_owned(), "preview.yml".to_owned()];
            scanned.release_enabled = true;
            scanned.release = Some(ReleaseSpec {
                kind: "native".to_owned(),
                package: package.to_owned(),
                packages: Vec::new(),
                binary: package.to_owned(),
                targets: vec![
                    "x86_64-unknown-linux-gnu".to_owned(),
                    "aarch64-unknown-linux-gnu".to_owned(),
                ],
                image: format!("ghcr.io/{package}/app"),
                source_repository: String::new(),
                consumer_repository: format!("{package}/apt"),
                artifact_path: String::new(),
                description: String::new(),
                manifest_schema: String::new(),
            });
            let surface = must(
                super::super::generate(&root, &shape, &scanned, None),
                "generate guest surface",
            );
            let preview = rendered(&surface, "preview.yml");
            let release = rendered(&surface, "release.yml");
            must(
                crate::validate_guest_seed_lifecycle(&preview, "main"),
                "preview guest seed lifecycle",
            );
            must(
                crate::validate_guest_seed_lifecycle(&release, "main"),
                "release guest seed lifecycle",
            );
            assert!(preview.contains(&format!("--bin {agent}")), "{preview}");
            assert!(preview.contains(&format!("--bin {image}")), "{preview}");
            assert!(
                preview.contains("runs-on: ${{ matrix.runner }}"),
                "guest payload must honor the per-arch runner: {preview}"
            );
            assert!(
                preview.contains("runner: ubuntu-24.04-arm"),
                "aarch64 guest payload must build on hosted arm64: {preview}"
            );
            assert!(
                preview.contains("libc6-dev-arm64-cross"),
                "aarch64 guest payload must install the cross sysroot: {preview}"
            );
            assert!(
                preview.contains("ln -sf /usr/bin/ld.bfd /usr/bin/ld"),
                "kernel build must restore GNU ld after mold: {preview}"
            );
            assert!(!preview.contains("velnor-guest-"), "{preview}");
            assert!(
                !preview.contains("velnor-workflow release package-guest"),
                "{preview}"
            );
            assert!(
                release.contains("needs: [admit-runner, verify, build, guest-payload]"),
                "{release}"
            );
            assert!(release.contains(&format!("--bin {image}")), "{release}");
            assert!(
                release.contains("stage --root \"$root\" --arch"),
                "{release}"
            );
            assert!(release.contains("--rootfs-sha256"), "{release}");
            assert!(release.contains("--guest-agent-sha256"), "{release}");
            assert!(release.contains(".tarballs[$a].url"), "{release}");
            assert!(release.contains("--guest dist/microvm"), "{release}");
            assert!(!release.contains("velnor-guest-"), "{release}");
            let _ = fs::remove_dir_all(root);
        }
    }

    /// The identity lane arms from scan evidence alone: a fixture package
    /// declaring `release-build` renders the wired release and preview
    /// without any hand-placed detection marker.
    #[test]
    fn native_identity_arms_from_scanned_release_build_feature() {
        for (name, package) in [("identity-widget", "widget"), ("identity-acme", "acme-box")] {
            let agent = format!("{package}-guest-agent");
            let image = format!("{package}-guest-image");
            let root = scanned_root(name);
            must(
                fs::write(
                    root.join("Cargo.toml"),
                    format!(
                        "[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                         [features]\nrelease-build = []\n\n\
                         [[bin]]\nname = \"{package}\"\npath = \"src/main.rs\"\n\n\
                         [[bin]]\nname = \"{agent}\"\npath = \"src/bin/agent.rs\"\n\n\
                         [[bin]]\nname = \"{image}\"\npath = \"src/bin/image.rs\"\n"
                    ),
                ),
                "write identity fixture manifest",
            );
            must(fs::create_dir_all(root.join("src/bin")), "create bin dir");
            must(
                fs::write(root.join("src/main.rs"), "fn main() {}\n"),
                "write main",
            );
            must(
                fs::write(root.join("src/bin/agent.rs"), "fn main() {}\n"),
                "write agent",
            );
            must(
                fs::write(root.join("src/bin/image.rs"), "fn main() {}\n"),
                "write image",
            );
            must(fs::create_dir_all(root.join("microvm")), "create microvm");
            must(
                fs::write(
                    root.join("microvm/pins.json"),
                    "{\n  \"kernel_tarball\": \"https://example.invalid/linux.tar.xz\",\n  \"kernel_tarball_sha256\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n}\n",
                ),
                "write pins",
            );
            let shape = must(
                crate::scan::scan_shape(&root, crate::RunnerMode::Github, "main", &[]),
                "scan identity fixture",
            );
            let mut scanned = crate::ProjectConfig::from(shape.clone());
            assert!(
                scanned
                    .analysis
                    .detected
                    .contains(&format!("release-build:{package}")),
                "the scan must mark the declaring package: {:?}",
                scanned.analysis.detected
            );
            for unit in &mut scanned.units {
                if !unit.watch.iter().any(|path| path.contains("microvm")) {
                    unit.watch.push("microvm/**".to_owned());
                }
            }
            scanned.workflow_files = vec!["release.yml".to_owned(), "preview.yml".to_owned()];
            scanned.release_enabled = true;
            scanned.release = Some(ReleaseSpec {
                kind: "native".to_owned(),
                package: package.to_owned(),
                packages: Vec::new(),
                binary: package.to_owned(),
                targets: vec![
                    "x86_64-unknown-linux-gnu".to_owned(),
                    "aarch64-unknown-linux-gnu".to_owned(),
                ],
                image: format!("ghcr.io/{package}/app"),
                source_repository: format!("{package}/app"),
                consumer_repository: format!("{package}/apt"),
                artifact_path: String::new(),
                description: String::new(),
                manifest_schema: "example.test/consumer-manifest-v1".to_owned(),
            });
            let surface = must(
                super::super::generate(&root, &shape, &scanned, None),
                "generate identity surface",
            );
            let release = rendered(&surface, "release.yml");
            let preview = rendered(&surface, "preview.yml");
            for workflow in [&release, &preview] {
                assert!(
                    workflow.contains("VELNOR_RELEASE_BUILD: \"1\""),
                    "{workflow}"
                );
                assert!(workflow.contains("--features release-build"), "{workflow}");
                assert!(workflow.contains("release emit"), "{workflow}");
                assert!(
                    workflow.contains("ci-release-package-signer.yml"),
                    "{workflow}"
                );
            }
            assert!(
                release.contains(&format!("GHCR_IMAGE: ghcr.io/{package}/app")),
                "{release}"
            );
            assert!(
                release.contains("--tag \"${GHCR_IMAGE}:${VERSION}\""),
                "{release}"
            );
            assert!(
                preview.contains("VELNOR_PREVIEW_SOURCE_SHA: ${{ needs.identity.outputs.commit }}"),
                "{preview}"
            );
            assert!(
                preview.contains(
                    "gh release create preview --target \"$COMMIT\" --prerelease --title \"$NAME\""
                ),
                "{preview}"
            );
            assert!(
                release.contains(&format!("--bin {agent}"))
                    && preview.contains(&format!("--bin {agent}")),
                "guest agent builds stay on the scanned bin"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn a_declared_homebrew_feed_mutates_from_github_only() {
        for (name, package, source) in [
            ("homebrew-feed", "example", "example/app"),
            ("homebrew-feed-acme", "widget", "acme/widget"),
        ] {
            let root = scanned_root(name);
            let surface = generate(
                &root,
                &config(&[], None),
                Some(&format!(
                    "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                     [declare.args]\nkind = \"homebrew\"\npackage = \"{package}\"\n\
                     source_repository = \"{source}\"\n"
                )),
            );
            let release = surface
                .files
                .get(&PathBuf::from(".github/workflows/release.yml"))
                .unwrap_or_else(|| panic!("a homebrew feed must render release.yml"));
            assert!(release.contains("Package feed"), "{release}");
            assert!(release.contains("homebrew"), "{release}");
            assert!(
                release.contains(&format!("--package {package}")),
                "{release}"
            );
            assert!(release.contains(source), "{release}");
            assert!(release.contains("default: github"), "{release}");
            assert!(
                release.contains("feed mutation publishes from GitHub only"),
                "{release}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn a_declared_apt_feed_mutates_from_github_only() {
        for (name, package, consumer) in [
            ("apt-feed", "example", "example/apt"),
            ("apt-feed-acme", "widget", "acme/apt"),
        ] {
            let root = scanned_root(name);
            let surface = generate(
                &root,
                &config(&[], None),
                Some(&format!(
                    "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                     [declare.args]\nkind = \"apt\"\npackage = \"{package}\"\n\
                     consumer_repository = \"{consumer}\"\n"
                )),
            );
            let release = surface
                .files
                .get(&PathBuf::from(".github/workflows/release.yml"))
                .unwrap_or_else(|| panic!("an apt feed must render release.yml"));
            assert!(release.contains("Package feed"), "{release}");
            assert!(release.contains("--kind apt"), "{release}");
            assert!(
                release.contains(&format!("--package {package}")),
                "{release}"
            );
            assert!(release.contains(consumer), "{release}");
            assert!(release.contains("default: github"), "{release}");
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn a_declared_static_workflow_is_rejected() {
        let root = scanned_root("static");
        let config = config(&[], None);
        let error = match try_generate(
            &root,
            &config,
            Some(
                "[[declare]]\nprimitive = \"static-workflow\"\nfile = \"custom.yml\"\n\n\
                 [declare.args]\ntemplate = '''\nname: Custom\n'''\n",
            ),
        ) {
            Ok(_) => panic!("static-workflow must fail closed"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("static-workflow") && error.contains("not supported"),
            "{error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Every incomplete or misdirected declaration fails closed.
    #[test]
    fn incomplete_or_misdirected_release_rows_fail_closed() {
        let cases: Vec<(&str, &str, &str)> = vec![
            (
                "release without targets",
                "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                 [declare.args]\nkind = \"rust-binary\"\npackage = \"example\"\nbinary = \"example\"\n",
                "incomplete release contract",
            ),
            (
                "preview without binary",
                "[[declare]]\nprimitive = \"preview\"\nfile = \"preview.yml\"\n\n\
                 [declare.args]\npackage = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\n",
                "incomplete release contract",
            ),
            (
                "release without kind",
                "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                 [declare.args]\npackage = \"example\"\n",
                "needs `kind`",
            ),
            (
                "static workflow without template",
                "[[declare]]\nprimitive = \"static-workflow\"\nfile = \"custom.yml\"\n",
                "not supported",
            ),
            (
                "renamed release file",
                "[[declare]]\nprimitive = \"release\"\nfile = \"renamed.yml\"\n\n\
                 [declare.args]\nkind = \"crates\"\npackages = [\"example\"]\n",
                "must declare `release.yml`",
            ),
            (
                "release row naming units",
                "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\nunits = [\"rust-example\"]\n\n\
                 [declare.args]\nkind = \"crates\"\npackages = [\"example\"]\n",
                "takes no `units`",
            ),
        ];
        for (name, rows, expected) in cases {
            let root = scanned_root("invalid");
            let config = config(&[], None);
            let error = match try_generate(&root, &config, Some(rows)) {
                Ok(_) => panic!("`{name}` must fail closed, and did not"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains(expected),
                "`{name}` must name the problem: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    /// A declared file that a non-surface renderer owns is rejected at
    /// declaration time. The nested per-unit workflows are rendered by the
    /// legacy renderer for every scanned unit, so a row that slips past the
    /// pipeline family — here narrowed with `coverage = "explicit"` — would
    /// otherwise be emitted and then silently overwritten.

    #[test]
    fn a_declared_file_a_nested_unit_renderer_owns_is_rejected() {
        let root = scanned_root("owned-nested");
        let mut config = config(&[], None);
        config.units = vec![unit("rust-example"), unit("rust-other")];
        let error = match try_generate(
            &root,
            &config,
            Some(
                "[[declare]]\nprimitive = \"rust-crate-pipeline\"\nunits = [\"rust-example\"]\nfile = \"ci-rust-example.yml\"\n\n\
                 [declare.args]\ncoverage = \"explicit\"\n\n\
                 [[declare]]\nprimitive = \"static-workflow\"\nfile = \"ci-rust-other.yml\"\n\n\
                 [declare.args]\ntemplate = '''\nname: Custom\n'''\n",
            ),
        ) {
            Ok(_) => panic!("a declared nested-unit file must fail closed, and did not"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("`ci-unit-rust.yml`, not `ci-rust-example.yml`"),
            "the error must name the colliding path and its renderer: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }
}
