//! The release-side families: publishing, rolling preview, maintenance, and
//! the static surfaces a repository declares verbatim.
//!
//! Every family here renders one workflow file from the scanned shape, the
//! resolved config, and — for the families a repository may drive itself — its
//! declared arguments. A release contract is explicit input: the generic
//! publishers render only what a config or catalog declares, never a guess
//! from a manifest. A repository whose publishing surface is reviewed as a
//! whole declares it as a `static-workflow` row and owns the bytes.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::{
    checks_env, render_cargo_source_preparation, render_pinned_toolchain_steps,
    render_retained_output_cache_note, Args, CacheBackend, Primitive, RenderCtx, Rendered,
    WorkflowIr, MAINTENANCE, PREVIEW, RELEASE, RELEASE_SIGNER, STATIC_WORKFLOW,
};
use crate::{
    github_expression, lane_supports_unit, rendered_cache_values, shell_quote, velnor_runner,
    velnor_runner_group, workflow_runtime_setup, workflow_runtime_setup_with_install_rev,
    yaml_scalar, ActionPin, GeneratorError, ProjectConfig, ReleaseSpec, RunnerMode,
    GENERATED_HEADER, VELNOR_RELEASE_PACKAGE_SIGNER_TEMPLATE,
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

/// The `preview.yml` content for a config. Every contract renders the generic
/// rolling preview; a repository with a reviewed native preview declares its
/// bytes as a static workflow instead.
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

/// A reviewed workflow body, declared verbatim, rendered through the static
/// template machinery: source pins, action pins, and the Mr. Boxington command
/// contract are refreshed from the generator's reviewed tables at generation
/// time, so the declared body stays revision-independent.
pub(crate) struct StaticWorkflow;

impl Primitive for StaticWorkflow {
    fn id(&self) -> &'static str {
        STATIC_WORKFLOW
    }

    fn schema(&self) -> &'static [&'static str] {
        &["template"]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let file = ctx
            .file
            .filter(|file| !file.is_empty())
            .ok_or_else(|| {
                GeneratorError::usage(format!(
                    "`{STATIC_WORKFLOW}` renders one workflow file and needs `file`"
                ))
            })?
            .to_owned();
        let template = args.string("template")?.ok_or_else(|| {
            GeneratorError::usage(format!(
                "`{STATIC_WORKFLOW}` declares `{file}`, which needs a `template`; the workflow body is explicit input, never a guess"
            ))
        })?;
        let content = crate::render_static_template_for_config(ctx.config, &file, &template)?;
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
        "rust-binary" => {
            !release.package.is_empty()
                && !release.binary.is_empty()
                && targets_are_real(&release.targets)
        }
        "native" => {
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

fn release_runner(target: &str) -> &'static str {
    if target.ends_with("-apple-darwin") {
        "macos-15"
    } else if target.starts_with("aarch64-") {
        "ubuntu-24.04-arm"
    } else {
        "ubuntu-24.04"
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
    let github = || yaml_scalar(release_runner(target));
    let velnor = || configured_runner(config, RunnerMode::Velnor);
    match config.runners {
        RunnerMode::Github => vec![("github", github())],
        RunnerMode::Velnor => vec![("velnor", velnor())],
        RunnerMode::Both => vec![("github", github()), ("velnor", velnor())],
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
    let Some(release) = release.filter(|release| release.kind == "rust-binary") else {
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
    format!(
        r#"{GENERATED_HEADER}name: Preview\nrun-name: Preview · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\non:\n  push:\n    branches: [{}]\n    paths:\n{paths}  workflow_dispatch:\n\nconcurrency:\n  group: preview-${{{{ github.repository }}}}\n  cancel-in-progress: true\n\npermissions:\n  contents: read\n\njobs:\n  build:\n    name: Preview / ${{{{ matrix.target }}}}\n    runs-on: ${{{{ matrix.runner }}}}\n    timeout-minutes: 75\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0\n      - name: Build preview binary\n        env:\n          CARGO_INCREMENTAL: "0"\n          RUSTC_WRAPPER: sccache\n        run: cargo build --locked --release --package {} --bin {} --target "${{{{ matrix.target }}}}"\n      - name: Package preview binary\n        run: velnor-workflow release package-binary --target "${{{{ matrix.target }}}}" --version preview --package {} --binary {}\n      - name: Attest preview artifact\n        uses: {}\n        with:\n          subject-path: dist/*.tar.gz\n      - name: Upload preview artifact\n        uses: {}\n        with:\n          name: ${{{{ matrix.target }}}}\n          path: dist/*\n          if-no-files-found: error\n          retention-days: 1\n\n  publish:\n    name: Publish rolling preview\n    needs: build\n    if: ${{{{ github.event_name == 'push' && github.ref == 'refs/heads/{}' }}}}\n    runs-on: ubuntu-24.04\n    timeout-minutes: 15\n    permissions:\n      contents: write\n    steps:\n      - name: Download preview artifacts\n        uses: {}\n        with:\n          path: dist\n          merge-multiple: true\n      - name: Replace rolling preview\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          gh release view preview >/dev/null 2>&1 || gh release create preview --prerelease --title "Rolling preview"\n          gh release edit preview --target "${{{{ github.sha }}}}" --prerelease\n          gh release upload preview dist/* --clobber\n"#,
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
        "    runs-on: ${{ matrix.runner }}\n    timeout-minutes: 75",
        &format!(
            "    runs-on: ${{{{ matrix.runner }}}}\n    if: ${{{{ {} }}}}\n    timeout-minutes: 75",
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
    .replace("run: cargo build ", "run: mbx build ")
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
            if CacheBackend::Detected.enables_actions_cache(&workflow, unit)
                && let Some(cache) = &unit.cache
            {
                render_retained_output_cache_note(&mut output, &workflow, unit, cache);
                let (paths, key) = rendered_cache_values(cache);
                let _ = writeln!(
                    output,
                    "      - name: Restore {} cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: ci-release-${{{{ runner.os }}}}-{}-${{{{ hashFiles({key}) }}}}",
                    verify_name,
                    ActionPin::CacheRestore.reference(),
                    unit.id
                );
            }
            render_cargo_source_preparation(&mut output, unit);
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
    let _ = writeln!(
        output,
        "    runs-on: ${{{{ matrix.runner }}}}\n    timeout-minutes: 90\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0\n      - name: Add Rust target\n        run: rustup target add \"${{{{ matrix.target }}}}\"\n      - name: Build release binary\n        env:\n          CARGO_INCREMENTAL: \"0\"\n          RUSTC_WRAPPER: sccache\n        run: cargo build --locked --release --package {} --bin {} --target \"${{{{ matrix.target }}}}\"\n      - name: Package release binary\n        env:\n          VERSION: ${{{{ github.ref_name }}}}\n        run: |\n          set -euo pipefail\n          velnor-workflow release package-binary --target \"${{{{ matrix.target }}}}\" --version \"${{VERSION#v}}\" --package {} --binary {}\n      - name: Attest release artifact\n        uses: {}\n        with:\n          subject-path: dist/*.tar.gz\n      - name: Upload release artifact\n        uses: {}\n        with:\n          name: ${{{{ matrix.target }}}}\n          path: dist/*\n          if-no-files-found: error\n          retention-days: 2\n\n  publish:\n    name: Publish GitHub release\n    needs: [verify, build]\n    runs-on: ubuntu-24.04\n    timeout-minutes: 20\n    environment: github-release\n    permissions:\n      contents: write\n    steps:\n      - name: Download release artifacts\n        uses: {}\n        with:\n          path: dist\n          merge-multiple: true\n      - name: Verify archive checksums\n        run: |\n          set -euo pipefail\n          cd dist\n          for checksum in *.sha256; do sha256sum --check \"$checksum\"; done\n      - name: Publish immutable GitHub release\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: gh release create \"${{{{ github.ref_name }}}}\" dist/* --verify-tag --generate-notes\n",
        ActionPin::Checkout.reference(),
        ActionPin::Sccache.reference(),
        yaml_scalar(&release.package),
        yaml_scalar(&release.binary),
        yaml_scalar(&release.package),
        yaml_scalar(&release.binary),
        ActionPin::Attest.reference(),
        ActionPin::UploadArtifact.reference(),
        ActionPin::DownloadArtifact.reference(),
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
            "    runs-on: ${{ matrix.runner }}\n    timeout-minutes: 90",
            &format!(
                "    runs-on: ${{{{ matrix.runner }}}}\n    if: ${{{{ {} }}}}\n    timeout-minutes: 90",
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

fn render_native_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = render_binary_release(config, release);
    let github_runner = yaml_scalar(&config.github_runner);
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
        let image_tag = yaml_scalar(&format!("{}:${{{{ github.ref_name }}}}", release.image));
        extra.push_str(&format!(
            "  image:\n    name: Publish container image\n    needs: [admit-runner, verify, build]\n    runs-on: {github_runner}\n    timeout-minutes: 120\n    permissions:\n      contents: read\n      packages: write\n      id-token: write\n      attestations: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Log in to GHCR\n        uses: {}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ github.token }}}}\n      - name: Build and push image\n        uses: {}\n        with:\n          context: .\n          push: true\n          tags: {image_tag}\n          provenance: true\n          sbom: true\n",
            ActionPin::Checkout.reference(),
            ActionPin::DockerLogin.reference(),
            ActionPin::DockerBuild.reference(),
        ));
        publish_needs.push("image".to_owned());
    }
    if !release.consumer_repository.is_empty() {
        let package = yaml_scalar(&release.package);
        extra.push_str(&format!(
            "  debian:\n    name: Package Debian artifacts\n    needs: [admit-runner, verify, build]\n    runs-on: {github_runner}\n    timeout-minutes: 45\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Download release artifacts\n        uses: {}\n        with:\n          path: dist\n          merge-multiple: true\n      - name: Build Debian packages\n        env:\n          VERSION: ${{{{ github.ref_name }}}}\n        run: |\n          set -euo pipefail\n          velnor-workflow release package-deb --package {package} --version \"${{VERSION#v}}\"\n      - name: Attest Debian packages\n        uses: {}\n        with:\n          subject-path: dist/*.deb\n      - name: Upload Debian packages\n        uses: {}\n        with:\n          name: debian-packages\n          path: dist/*.deb\n          if-no-files-found: error\n          retention-days: 2\n",
            ActionPin::Checkout.reference(),
            ActionPin::DownloadArtifact.reference(),
            ActionPin::Attest.reference(),
            ActionPin::UploadArtifact.reference(),
        ));
        publish_needs.push("debian".to_owned());
    }
    let guest = config
        .units
        .iter()
        .any(|unit| unit.watch.iter().any(|path| path.contains("microvm")));
    if guest {
        extra.push_str(&format!(
            "  guest:\n    name: Guest kernel/rootfs ${{{{ matrix.arch }}}}\n    needs: [admit-runner, verify]\n    runs-on: {github_runner}\n    timeout-minutes: 180\n    strategy:\n      fail-fast: false\n      matrix:\n        arch: [x86_64, aarch64]\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Build guest image\n        run: velnor-workflow release package-guest --arch \"${{{{ matrix.arch }}}}\"\n      - name: Upload guest image\n        uses: {}\n        with:\n          name: guest-${{{{ matrix.arch }}}}\n          path: dist/guest-*\n          if-no-files-found: error\n          retention-days: 2\n",
            ActionPin::Checkout.reference(),
            ActionPin::UploadArtifact.reference(),
        ));
        publish_needs.push("guest".to_owned());
    }
    if !extra.is_empty() {
        output = output.replace("\n  publish:", &format!("\n{extra}\n  publish:"));
    }
    output = output.replace(
        "  publish:\n    name: Publish GitHub release\n    needs: [admit-runner, verify, build]\n",
        &format!(
            "  publish:\n    name: Publish GitHub release\n    needs: [{}]\n",
            publish_needs.join(", ")
        ),
    );
    output.replace(
        "on:\n  push:\n    tags: [\"v*\"]\n",
        "on:\n  push:\n    tags: [\"v*\"]\n  workflow_dispatch:\n    inputs:\n      runner:\n        description: Execution backend\n        required: false\n        default: github\n        type: choice\n        options:\n          - github\n          - velnor\n          - both\n",
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
    runs-on: ubuntu-24.04
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
    runs-on: ubuntu-24.04
    timeout-minutes: 10
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
        run: |
          set -euo pipefail
          evicted=0
          freed=0
          failed=0
          # The plan is applied verbatim, in its own order: generations beyond
          # their class bound first, then classes over budget, then the global
          # sweep - which never touches a protected class.
          while IFS=$'\t' read -r class reason id size key; do
            if gh api --method DELETE "repos/$GITHUB_REPOSITORY/actions/caches/$id" >/dev/null 2>&1; then
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

/// Maintenance is GitHub cache-API hygiene, not a Velnor job. Both jobs stay
/// on the hosted image even when CI lanes select `runners = "velnor"`. `uses:`
/// stays on the SOURCE_REV pin (GitHub Actions rejects expressions in `uses:`
/// versions). `rev:` is `${{ github.sha }}` so default-branch dispatch installs
/// HEAD through an action yaml that includes CONTROLLED_BOOTSTRAP for
/// `workflow_dispatch`.
fn render_maintenance(config: &ProjectConfig) -> String {
    MAINTENANCE_WORKFLOW
        .replace(
            "VELNOR_RUNTIME_SETUP_STEPS",
            &workflow_runtime_setup_with_install_rev(
                RunnerMode::Github,
                &github_expression("github.sha"),
            ),
        )
        .replace(
            "runs-on: ubuntu-24.04",
            &format!("runs-on: {}", yaml_scalar(&config.github_runner)),
        )
        .replace(
            "if: ${{ github.event_name == 'pull_request' || inputs.pull_request_number != '' }}",
            &format!(
                "if: ${{{{ github.event_name == 'pull_request' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{}' && inputs.pull_request_number != '') }}}}",
                config.default_branch
            ),
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
            "172845cf0307d99b2af1e29f58d4880519c6fb31"
        );
        let uses_line = must_some(
            workflow.lines().find(|line| {
                line.contains(&format!("uses: {}", crate::VELNOR_WORKFLOW_SETUP_ACTION))
            }),
            "setup-velnor-workflow uses line",
        );
        assert!(
            uses_line.contains("@172845cf0307d99b2af1e29f58d4880519c6fb31"),
            "uses: must pin SOURCE_REV: {uses_line}"
        );
        assert!(
            !uses_line.contains("github.sha"),
            "GitHub Actions forbids expressions in uses: versions: {uses_line}"
        );
        let head_rev = github_expression("github.sha");
        assert!(
            workflow.contains(&format!("rev: {head_rev}")),
            "maintenance cache-plan must install HEAD: {workflow}"
        );
        assert!(
            !workflow.contains(&format!("rev: {}", crate::VELNOR_WORKFLOW_SOURCE_REV)),
            "maintenance must not pin cache-plan to SOURCE_REV: {workflow}"
        );
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
        assert!(
            !workflow.contains("github.event_name == 'push'"),
            "maintenance must not inherit the Velnor trusted-event gate: {workflow}"
        );
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
        }
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
            pull_request_on_velnor: false,
            static_files: Vec::new(),
            declared_surface: false,
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
                "2912ae5a02086b8d0f892ab87adc2fed4f19e7d0904743b10e37bec48d5d02f1",
            ),
            (
                "preview.yml",
                "a0ab79fff849c4b9ef76f3d362d33a53625fb52d94b92974d52618dc790f99c8",
            ),
            (
                "maintenance.yml",
                "ee336d15bffe519c307b4b6c38bed1c35a1912b684eafffcbc97ca990c0dd626",
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
        assert!(release.contains("name: Release"), "{release}");
        assert!(release.contains("Publish GitHub release"), "{release}");
        assert!(release.contains("Verify archive checksums"), "{release}");
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
    fn maintenance_stays_github_hosted_when_runners_are_velnor() {
        let mut cfg = config(&["maintenance.yml"], None);
        cfg.runners = RunnerMode::Velnor;

        let direct = super::render_maintenance(&cfg);
        assert_maintenance_is_github_hosted(&direct, &cfg);

        let root = scanned_root("maintenance-hosted");
        let surface = generate(&root, &cfg, None);
        let generated = rendered(&surface, "maintenance.yml");
        assert!(
            generated.starts_with(crate::GENERATED_HEADER),
            "generated maintenance.yml must carry the header: {generated}"
        );
        assert_maintenance_is_github_hosted(&generated, &cfg);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn maintenance_uses_configured_runners_for_all_hosted_modes() {
        for runners in [RunnerMode::Github, RunnerMode::Velnor, RunnerMode::Both] {
            let mut cfg = config(&["maintenance.yml"], None);
            cfg.runners = runners;
            let workflow = super::render_maintenance(&cfg);
            assert_maintenance_is_github_hosted(&workflow, &cfg);
        }
    }

    #[test]
    fn maintenance_uses_configured_github_runner_when_runners_are_velnor() {
        let mut cfg = config(&["maintenance.yml"], None);
        cfg.runners = RunnerMode::Velnor;
        cfg.github_runner = "ubuntu-22.04".to_owned();
        cfg.default_branch = "trunk".to_owned();

        let workflow = super::render_maintenance(&cfg);
        assert_maintenance_is_github_hosted(&workflow, &cfg);
        assert!(
            workflow.contains("runs-on: ubuntu-22.04"),
            "maintenance must honor config.github_runner: {workflow}"
        );
        assert!(
            !workflow.contains("runs-on: ubuntu-24.04"),
            "maintenance must not keep the template runner when github_runner differs: {workflow}"
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
    fn a_declared_static_workflow_renders_the_declared_body() {
        let root = scanned_root("static");
        let config = config(&[], None);
        let surface = generate(
            &root,
            &config,
            Some(
                "[[declare]]\nprimitive = \"static-workflow\"\nfile = \"custom.yml\"\n\n\
                 [declare.args]\ntemplate = '''\nname: Custom\n\njobs:\n  probe:\n    runs-on: __VELNOR_GITHUB_RUNNER__\n    steps:\n      - run: velnor-workflow --rev __VELNOR_WORKFLOW_SOURCE_REV__\n'''\n",
            ),
        );
        let custom = surface
            .files
            .get(&PathBuf::from(".github/workflows/custom.yml"))
            .unwrap_or_else(|| panic!("a declared static workflow must render its file"));
        assert!(
            custom.starts_with(crate::GENERATED_HEADER),
            "the rendered body carries the generated header: {custom}"
        );
        assert!(custom.contains("runs-on: ubuntu-24.04"), "{custom}");
        assert!(
            custom.contains(crate::VELNOR_WORKFLOW_SOURCE_REV),
            "source pins are refreshed: {custom}"
        );
        assert_eq!(surface.added_files, vec!["custom.yml".to_owned()]);
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
                "needs a `template`",
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
