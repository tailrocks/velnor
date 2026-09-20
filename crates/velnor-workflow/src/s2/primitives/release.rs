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
    checks_env, docker_build_token_env_for_members, render_cargo_source_preparation,
    render_pinned_toolchain_steps, render_retained_output_cache_note, Args, CacheBackend,
    Primitive, RenderCtx, Rendered, WorkflowIr, MAINTENANCE, PACKAGE_RELEASE, PREVIEW, RELEASE,
    RELEASE_SIGNER, STATIC_WORKFLOW,
};
use crate::s2::provider::{runs_on_for, ProviderId};
use crate::s2::{
    github_expression, provider_supports_unit, rendered_cache_values, selector_runs_on_yaml,
    shell_quote, unit_display_label, workflow_runtime_setup,
    workflow_runtime_setup_with_install_rev, workflow_setup_install_rev, yaml_scalar, ActionPin,
    GeneratorError, ProjectConfig, ReleaseJobSpec, ReleaseSpec, Unit, UnitKind, GENERATED_HEADER,
    MACOS_HOSTED_RUNS_ON, VELNOR_RELEASE_PACKAGE_SIGNER_TEMPLATE,
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
    canonical_release_side_file(primitive).is_some()
        || primitive == PACKAGE_RELEASE
        || primitive == STATIC_WORKFLOW
}

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
pub(crate) fn release_signer_content(revision: &str) -> String {
    crate::s2::render_static_template(VELNOR_RELEASE_PACKAGE_SIGNER_TEMPLATE, revision)
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
            "archive_checksum",
            "archive_members",
            "archive_retention_days",
            "artifact_path",
            "assert_tasks",
            "binary",
            "build_tasks",
            "consumer_repository",
            "context",
            "dockerfile",
            "image",
            "image_package",
            "kind",
            "manifest_schema",
            "modes",
            "name",
            "package",
            "packages",
            "platforms",
            "producer_conclusion",
            "producer_workflow",
            "publish_group",
            "pull_request_paths",
            "push_paths",
            "registry",
            "registry_password_secret",
            "registry_username_secret",
            "source_repository",
            "tag_pattern",
            "targets",
            "version_gate_tasks",
            "version_manifest",
            "version_prefix",
        ]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        // The main-branch-driven publisher bypasses the tag-driven contract:
        // its version comes from a manifest plus a PR gate, not from a tag,
        // so it parses and renders from its own spec.
        if args.string("kind")?.as_deref() == Some(VERSIONED_TOOL_KIND) {
            let file = ctx.file.filter(|file| !file.is_empty()).ok_or_else(|| {
                GeneratorError::usage(format!(
                    "`{}` renders a versioned-tool publisher and needs `file`",
                    ctx.family
                ))
            })?;
            let spec = declared_versioned_tool_spec(ctx.family, file, args)?;
            if !versioned_tool_contract_complete(&spec) {
                return Err(incomplete_versioned_tool_contract(ctx.family, file, &spec));
            }
            let content = render_versioned_tool_release(ctx.config, &spec);
            return render_file(ctx, file, content);
        }
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
        &[
            "archive_checksum",
            "archive_members",
            "archive_retention_days",
            "binary",
            "modes",
            "package",
            "producer_conclusion",
            "producer_workflow",
            "targets",
        ]
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

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let _ = args;
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
            release_signer_content(&ctx.config.workflow_revision),
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
        Some("crates" | "rust-binary" | "native" | "pages" | "homebrew" | "apt" | "docker") => {
            args.string("kind")?.unwrap_or_default()
        }
        Some(other) => {
            return Err(GeneratorError::usage(format!(
                "`{family}` `kind` must be `crates`, `rust-binary`, `native`, `pages`, `homebrew`, `apt`, or `docker`, found `{other}`"
            )))
        }
        None => {
            return Err(GeneratorError::usage(format!(
                "`{family}` needs `kind`; one of `crates`, `rust-binary`, `native`, `pages`, `homebrew`, `apt`, or `docker`"
            )))
        }
    };
    let (registry, registry_username_secret, registry_password_secret) = declared_registry_auth(
        family,
        &kind,
        args.string("registry")?.as_deref(),
        args.string("registry_username_secret")?.as_deref(),
        args.string("registry_password_secret")?.as_deref(),
    )?;
    Ok(ReleaseSpec {
        kind,
        package: args.string("package")?.unwrap_or_default(),
        packages: args.strings("packages")?.unwrap_or_default(),
        binary: args.string("binary")?.unwrap_or_default(),
        targets: args.strings("targets")?.unwrap_or_default(),
        image: args.string("image")?.unwrap_or_default(),
        image_package: args.string("image_package")?.unwrap_or_default(),
        source_repository: args.string("source_repository")?.unwrap_or_default(),
        consumer_repository: args.string("consumer_repository")?.unwrap_or_default(),
        artifact_path: args.string("artifact_path")?.unwrap_or_default(),
        description: String::new(),
        manifest_schema: args.string("manifest_schema")?.unwrap_or_default(),
        dockerfile: args.string("dockerfile")?.unwrap_or_default(),
        context: args.string("context")?.unwrap_or_default(),
        platforms: args.strings("platforms")?.unwrap_or_default(),
        producer_workflow: args.string("producer_workflow")?.unwrap_or_default(),
        producer_conclusion: declared_producer_conclusion(
            family,
            args.string("producer_conclusion")?.as_deref(),
            args.string("producer_workflow")?.as_deref(),
        )?,
        modes: declared_modes(family, args.strings("modes")?.unwrap_or_default())?,
        archive_members: declared_archive_members(
            family,
            args.strings("archive_members")?.unwrap_or_default(),
        )?,
        archive_checksum: declared_archive_checksum(
            family,
            args.string("archive_checksum")?.as_deref(),
        )?,
        archive_retention_days: declared_retention_days(
            family,
            args.integer("archive_retention_days")?,
        )?,
        credentials: Vec::new(),
        tag_pattern: declared_tag_pattern(family, args.string("tag_pattern")?.as_deref())?,
        registry,
        registry_username_secret,
        registry_password_secret,
        jobs: Vec::new(),
    })
}

/// Parse a declared rolling-preview contract: the preview lane publishes a
/// Rust binary, so the contract is the binary publisher's without a `kind`.
fn declared_preview_spec(args: &Args<'_>) -> Result<ReleaseSpec, GeneratorError> {
    let family = PREVIEW;
    let modes = declared_modes(family, args.strings("modes")?.unwrap_or_default())?;
    Ok(ReleaseSpec {
        kind: "rust-binary".to_owned(),
        package: args.string("package")?.unwrap_or_default(),
        packages: Vec::new(),
        binary: args.string("binary")?.unwrap_or_default(),
        targets: args.strings("targets")?.unwrap_or_default(),
        image: String::new(),
        image_package: String::new(),
        source_repository: String::new(),
        consumer_repository: String::new(),
        artifact_path: String::new(),
        description: String::new(),
        manifest_schema: String::new(),
        dockerfile: String::new(),
        context: String::new(),
        platforms: Vec::new(),
        producer_workflow: args.string("producer_workflow")?.unwrap_or_default(),
        producer_conclusion: declared_producer_conclusion(
            family,
            args.string("producer_conclusion")?.as_deref(),
            args.string("producer_workflow")?.as_deref(),
        )?,
        modes,
        archive_members: declared_archive_members(
            family,
            args.strings("archive_members")?.unwrap_or_default(),
        )?,
        archive_checksum: declared_archive_checksum(
            family,
            args.string("archive_checksum")?.as_deref(),
        )?,
        archive_retention_days: declared_retention_days(
            family,
            args.integer("archive_retention_days")?,
        )?,
        credentials: Vec::new(),
        // Rolling previews trigger on push, never on tags: no pattern.
        tag_pattern: String::new(),
        // Rolling previews never log in to a registry.
        registry: String::new(),
        registry_username_secret: String::new(),
        registry_password_secret: String::new(),
        jobs: Vec::new(),
    })
}

/// The main-branch-driven versioned publisher: a tool whose version comes
/// from a manifest plus a PR gate, published as immutable
/// `<version_prefix><version>` releases. Unlike the tag-triggered publishers
/// it renders to the row's own file with the row's own workflow name.
pub(crate) const VERSIONED_TOOL_KIND: &str = "versioned-tool";

/// Whether a release `kind` is driven by the main branch instead of tags.
/// Only these rows may render outside `release.yml`; tag-triggered kinds
/// stay pinned to the canonical file.
pub(crate) fn is_main_branch_driven_release_kind(kind: &str) -> bool {
    kind == VERSIONED_TOOL_KIND
}

/// The workflow `name:` a release row renders: tag-driven publishers share
/// `Release`, while a main-branch-driven publisher takes its row's `name`
/// or, when the row names none, the file stem — so every publisher owns a
/// distinct identity.
pub(crate) fn release_workflow_name(file: &str, args: &Args<'_>) -> Result<String, GeneratorError> {
    let kind = args.string("kind")?.unwrap_or_default();
    if !is_main_branch_driven_release_kind(&kind) {
        return Ok("Release".to_owned());
    }
    if let Some(name) = args.string("name")? {
        return Ok(name);
    }
    Ok(file
        .strip_suffix(".yml")
        .or_else(|| file.strip_suffix(".yaml"))
        .unwrap_or(file)
        .to_owned())
}

/// A declared main-branch-driven publisher contract. It bypasses
/// `ReleaseSpec`: that record carries the tag-driven contract, and a
/// manifest version plus gate tasks is not one.
struct VersionedToolSpec {
    name: String,
    package: String,
    binary: String,
    targets: Vec<String>,
    version_manifest: String,
    version_prefix: String,
    publish_group: String,
    version_gate_tasks: Vec<String>,
    assert_tasks: Vec<String>,
    build_tasks: Vec<String>,
    push_paths: Vec<String>,
    pull_request_paths: Vec<String>,
}

/// Whether `prefix` is a well-formed version tag prefix: a non-empty stem
/// over the tag alphabet ending in `-v`, so `gh release create` mints tags
/// like `example-tool-v0.1.0`.
fn valid_version_prefix(prefix: &str) -> bool {
    prefix.strip_suffix("-v").is_some_and(|stem| {
        !stem.is_empty()
            && stem
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    })
}

/// Validate one declared versioned-tool task list against the same predicate
/// the scheduled checks use: every entry renders verbatim into
/// `mise run <task>`.
fn declared_versioned_tool_tasks(
    family: &str,
    file: &str,
    key: &str,
    tasks: Vec<String>,
) -> Result<Vec<String>, GeneratorError> {
    for task in &tasks {
        if !crate::s2::config::valid_check_profile_task(task) {
            return Err(GeneratorError::usage(format!(
                "`{family}` file `{file}` `{key}` names task `{task}`, which is not a plain task reference; use names such as check-smoke without whitespace or shell syntax"
            )));
        }
    }
    Ok(tasks)
}

/// Validate one declared versioned-tool path list: every entry renders into
/// a trigger `paths:` block, so empty and multi-line entries fail closed.
fn declared_versioned_tool_paths(
    family: &str,
    file: &str,
    key: &str,
    paths: Vec<String>,
) -> Result<Vec<String>, GeneratorError> {
    for path in &paths {
        if path.is_empty() || path.contains(['\n', '\r']) {
            return Err(GeneratorError::usage(format!(
                "`{family}` file `{file}` `{key}` must not contain an empty or multi-line path"
            )));
        }
    }
    Ok(paths)
}

/// Parse a declared versioned-tool contract. Shapes fail here with the
/// offending value; missing fields fail in the completeness check, which
/// names every one of them at once.
fn declared_versioned_tool_spec(
    family: &str,
    file: &str,
    args: &Args<'_>,
) -> Result<VersionedToolSpec, GeneratorError> {
    let name = release_workflow_name(file, args)?;
    if name.is_empty() || name.contains(['\n', '\r']) {
        return Err(GeneratorError::usage(format!(
            "`{family}` file `{file}` `name` must be one non-empty line"
        )));
    }
    let version_manifest = args.string("version_manifest")?.unwrap_or_default();
    if version_manifest.contains(['\n', '\r']) {
        return Err(GeneratorError::usage(format!(
            "`{family}` file `{file}` `version_manifest` must be one line, found a multi-line value"
        )));
    }
    let version_prefix = args.string("version_prefix")?.unwrap_or_default();
    if !version_prefix.is_empty() && !valid_version_prefix(&version_prefix) {
        return Err(GeneratorError::usage(format!(
            "`{family}` file `{file}` `version_prefix` must be a tag prefix ending in `-v` (letters, digits, `_`, `.`, `-`), found `{version_prefix}`"
        )));
    }
    let publish_group = args.string("publish_group")?.unwrap_or_default();
    if publish_group.contains(['\n', '\r']) {
        return Err(GeneratorError::usage(format!(
            "`{family}` file `{file}` `publish_group` must be one line, found a multi-line value"
        )));
    }
    Ok(VersionedToolSpec {
        name,
        package: args.string("package")?.unwrap_or_default(),
        binary: args.string("binary")?.unwrap_or_default(),
        targets: args.strings("targets")?.unwrap_or_default(),
        version_manifest,
        version_prefix,
        publish_group,
        version_gate_tasks: declared_versioned_tool_tasks(
            family,
            file,
            "version_gate_tasks",
            args.strings("version_gate_tasks")?.unwrap_or_default(),
        )?,
        assert_tasks: declared_versioned_tool_tasks(
            family,
            file,
            "assert_tasks",
            args.strings("assert_tasks")?.unwrap_or_default(),
        )?,
        build_tasks: declared_versioned_tool_tasks(
            family,
            file,
            "build_tasks",
            args.strings("build_tasks")?.unwrap_or_default(),
        )?,
        push_paths: declared_versioned_tool_paths(
            family,
            file,
            "push_paths",
            args.strings("push_paths")?.unwrap_or_default(),
        )?,
        pull_request_paths: declared_versioned_tool_paths(
            family,
            file,
            "pull_request_paths",
            args.strings("pull_request_paths")?.unwrap_or_default(),
        )?,
    })
}

/// Whether a declared versioned-tool contract renders: the binary publisher
/// core (package, binary, real targets) plus the manifest version source,
/// the tag prefix, the publish mutex, the gate and build tasks, and both
/// trigger path lists. Assert tasks stay optional: the assert job's generic
/// published-reuse check renders with or without them.
fn versioned_tool_contract_complete(spec: &VersionedToolSpec) -> bool {
    !spec.package.is_empty()
        && !spec.binary.is_empty()
        && targets_are_real(&spec.targets)
        && !spec.version_manifest.is_empty()
        && valid_version_prefix(&spec.version_prefix)
        && !spec.publish_group.is_empty()
        && !spec.version_gate_tasks.is_empty()
        && !spec.build_tasks.is_empty()
        && !spec.push_paths.is_empty()
        && !spec.pull_request_paths.is_empty()
}

fn incomplete_versioned_tool_contract(
    family: &str,
    file: &str,
    spec: &VersionedToolSpec,
) -> GeneratorError {
    let mut missing = Vec::new();
    if spec.package.is_empty() {
        missing.push("`package`");
    }
    if spec.binary.is_empty() {
        missing.push("`binary`");
    }
    if !targets_are_real(&spec.targets) {
        missing.push("`targets`");
    }
    if spec.version_manifest.is_empty() {
        missing.push("`version_manifest`");
    }
    if !valid_version_prefix(&spec.version_prefix) {
        missing.push("`version_prefix`");
    }
    if spec.publish_group.is_empty() {
        missing.push("`publish_group`");
    }
    if spec.version_gate_tasks.is_empty() {
        missing.push("`version_gate_tasks`");
    }
    if spec.build_tasks.is_empty() {
        missing.push("`build_tasks`");
    }
    if spec.push_paths.is_empty() {
        missing.push("`push_paths`");
    }
    if spec.pull_request_paths.is_empty() {
        missing.push("`pull_request_paths`");
    }
    GeneratorError::usage(format!(
        "`{family}` file `{file}` declares an incomplete versioned-tool contract; missing: {}",
        missing.join(", ")
    ))
}

/// Validate a declared producer conclusion: only `success` binds, and a
/// conclusion without a producer workflow binds nothing.
fn declared_producer_conclusion(
    family: &str,
    conclusion: Option<&str>,
    producer: Option<&str>,
) -> Result<String, GeneratorError> {
    match conclusion {
        None => Ok(String::new()),
        Some("success") if producer.is_some_and(|value| !value.is_empty()) => {
            Ok("success".to_owned())
        }
        Some("success") => Err(GeneratorError::usage(format!(
            "`{family}` `producer_conclusion` needs `producer_workflow`: a required conclusion without a trusted producer binds nothing"
        ))),
        Some(other) => Err(GeneratorError::usage(format!(
            "`{family}` `producer_conclusion` must be `success`, found `{other}`"
        ))),
    }
}

/// Validate declared dispatch modes: every mode must be one the renderer
/// offers, and `publish` is never a dispatch option.
/// Validate a declared tag-trigger filter: one non-empty line with no
/// whitespace. Absent selects `v*` at render.
fn declared_tag_pattern(family: &str, pattern: Option<&str>) -> Result<String, GeneratorError> {
    match pattern {
        None => Ok(String::new()),
        Some(pattern) if crate::s2::runtime::valid_tag_pattern(pattern) => Ok(pattern.to_owned()),
        Some(pattern) => Err(GeneratorError::usage(format!(
            "`{family}` `tag_pattern` must be one non-empty line with no whitespace, found `{pattern}`"
        ))),
    }
}

/// Validate a declared registry-auth triple: all three members travel
/// together, the host is a lowercase hostname, and both credentials are
/// secret names. Absent selects the GHCR automatic-token login at render.
/// The triple renders only for the `docker` publisher — every other kind
/// either never logs in or is bound to GHCR by its attestation flow.
fn declared_registry_auth(
    family: &str,
    kind: &str,
    registry: Option<&str>,
    username_secret: Option<&str>,
    password_secret: Option<&str>,
) -> Result<(String, String, String), GeneratorError> {
    if registry.is_none() && username_secret.is_none() && password_secret.is_none() {
        return Ok((String::new(), String::new(), String::new()));
    }
    for (name, value) in [
        ("registry", registry),
        ("registry_username_secret", username_secret),
        ("registry_password_secret", password_secret),
    ] {
        if value.is_none() {
            return Err(GeneratorError::usage(format!(
                "`{family}` `{name}` is missing: registry auth declares `registry`, `registry_username_secret`, and `registry_password_secret` together"
            )));
        }
    }
    let registry = registry.unwrap_or_default();
    if !crate::s2::runtime::valid_registry_host(registry) {
        return Err(GeneratorError::usage(format!(
            "`{family}` `registry` must be a lowercase host with an optional `:port`, found `{registry}`"
        )));
    }
    crate::s2::config::validate_secret_name(
        &format!("`{family}` `registry_username_secret`"),
        username_secret.unwrap_or_default(),
        "REGISTRY_USERNAME",
    )?;
    crate::s2::config::validate_secret_name(
        &format!("`{family}` `registry_password_secret`"),
        password_secret.unwrap_or_default(),
        "REGISTRY_PASSWORD",
    )?;
    if kind != "docker" {
        return Err(GeneratorError::usage(format!(
            "`{family}` registry auth renders only for kind `docker`, not `{kind}`"
        )));
    }
    Ok((
        registry.to_owned(),
        username_secret.unwrap_or_default().to_owned(),
        password_secret.unwrap_or_default().to_owned(),
    ))
}

/// The tag filter the push trigger matches: the declared pattern, or `v*`
/// when the contract declares none.
fn release_tag_pattern(release: &ReleaseSpec) -> &str {
    if release.tag_pattern.is_empty() {
        "v*"
    } else {
        &release.tag_pattern
    }
}

/// The `on: push: tags:` trigger block. The base renders and the dispatch
/// injectors share it, so a declared pattern flows to both the rendered
/// trigger and the match the injector looks for.
fn release_trigger_block(release: &ReleaseSpec) -> String {
    format!(
        "on:\n  push:\n    tags: [{}]\n",
        yaml_scalar(release_tag_pattern(release))
    )
}

/// The `release.yml` trigger header: the workflow name plus the tag-filtered
/// push trigger, through the control-plane verify job's step list.
fn release_trigger_header(release: &ReleaseSpec) -> String {
    format!(
        "name: Release\nrun-name: Release · ${{{{ github.ref_name }}}}\n\n{trigger}\nconcurrency:\n  group: release-${{{{ github.ref }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n  verify:\n    name: Control / Verify release\n    runs-on: ubuntu-24.04\n    timeout-minutes: 60\n    steps:\n",
        trigger = release_trigger_block(release),
    )
}

fn declared_modes(family: &str, modes: Vec<String>) -> Result<Vec<String>, GeneratorError> {
    for mode in &modes {
        if !crate::s2::config::is_release_mode(mode) {
            return Err(GeneratorError::usage(format!(
                "`{family}` `modes` must be one of {}, found `{mode}`; publish is tag-triggered, never a dispatch option",
                crate::s2::config::RELEASE_MODES.join(", ")
            )));
        }
    }
    Ok(modes)
}

/// Validate declared archive members: portable bare file names, never paths.
fn declared_archive_members(
    family: &str,
    members: Vec<String>,
) -> Result<Vec<String>, GeneratorError> {
    for member in &members {
        if member.is_empty()
            || !member
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(GeneratorError::usage(format!(
                "`{family}` `archive_members` must be portable file names without directories, found `{member}`"
            )));
        }
    }
    Ok(members)
}

/// Validate a declared archive checksum algorithm: only `sha256` exists.
fn declared_archive_checksum(
    family: &str,
    checksum: Option<&str>,
) -> Result<String, GeneratorError> {
    match checksum {
        None => Ok(String::new()),
        Some("sha256") => Ok("sha256".to_owned()),
        Some(other) => Err(GeneratorError::usage(format!(
            "`{family}` `archive_checksum` must be `sha256`, found `{other}`"
        ))),
    }
}

/// Validate a declared archive retention: GitHub's 1-90 day window.
fn declared_retention_days(family: &str, retention: Option<i64>) -> Result<u32, GeneratorError> {
    match retention {
        None => Ok(0),
        Some(days) if (1..=90).contains(&days) => Ok(u32::try_from(days).unwrap_or(0)),
        Some(days) => Err(GeneratorError::usage(format!(
            "`{family}` `archive_retention_days` must be 1-90, found `{days}`"
        ))),
    }
}

fn incomplete_contract(family: &str, spec: &ReleaseSpec) -> GeneratorError {
    let missing = match spec.kind.as_str() {
        "crates" => "`packages`",
        "rust-binary" => "`package`, `binary`, and `targets`",
        "pages" => "`artifact_path`",
        "docker" => "`image`",
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

/// Whether every declared target is a real Rust target triple the release
/// lanes build for: Linux GNU or Apple Darwin, never a bare arch.
fn targets_are_real(targets: &[String]) -> bool {
    !targets.is_empty()
        && targets.iter().all(|target| {
            target.ends_with("-unknown-linux-gnu") || target.ends_with("-apple-darwin")
        })
}

pub(crate) fn release_contract_complete(release: &ReleaseSpec) -> bool {
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
        "docker" => {
            !release.image.is_empty()
                && crate::s2::config::valid_docker_platforms(&release.platforms)
        }
        "tasks" => !release.jobs.is_empty(),
        // `versioned-tool` included: it never arrives as a `ReleaseSpec` —
        // the declare row parses it into its own spec, whose completeness
        // gate is `versioned_tool_contract_complete`.
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
            _ => hosted_selector_runs_on(config),
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
        workflow.render_tool_provisioning(&mut setup, ProviderId::GithubHosted, unit, true);
        "mbx"
    } else if let Some(toolchain) = crate::s2::config_rust_toolchain(config) {
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
        "  guest-payload:\n    name: Guest payload ${{{{ matrix.arch }}}}\n{needs}    runs-on: ${{{{ matrix.runner }}}}\n    timeout-minutes: 180\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    env:\n      TARGET: ${{{{ matrix.target }}}}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n{checkout_ref}          persist-credentials: false\n{setup}{steps}      - name: Upload guest payload\n        uses: {upload}\n        with:\n          name: guest-payload-${{{{ matrix.arch }}}}\n          path: |\n            dist/microvm/vmlinux\n            dist/microvm/rootfs.ext4\n            dist/microvm/rootfs.sha256\n            dist/microvm/{agent_bin}\n            dist/microvm/guest-agent.sha256\n          if-no-files-found: error\n",
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
        workflow.render_tool_provisioning(&mut setup, ProviderId::GithubHosted, unit, false);
    }
    let mut needs = vec![
        "admit-provider".to_owned(),
        "verify".to_owned(),
        "build".to_owned(),
    ];
    if guest {
        needs.push("guest-payload".to_owned());
    }
    let needs = needs.join(", ");
    let mut steps = format!(
        "      - name: Checkout\n        uses: {checkout}\n        with:\n          persist-credentials: false\n{setup}      - name: Download release artifacts\n        uses: {download}\n        with:\n          path: dist\n          pattern: {}-*\n          merge-multiple: true\n",
        canonical_provider(config),
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
            runner = hosted_selector_runs_on(config),
        )
    } else {
        format!(
            "  debian:\n    name: Package Debian artifacts\n    needs: [{needs}]\n    runs-on: {}\n    timeout-minutes: 45\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    steps:\n",
            hosted_selector_runs_on(config),
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
        workflow.render_tool_provisioning(&mut setup, ProviderId::GithubHosted, unit, false);
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
        runner = hosted_selector_runs_on(config),
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
        runner = hosted_selector_runs_on(config),
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
        "      - name: Download release binary\n        uses: {download}\n        with:\n          name: {provider}-${{{{ matrix.target }}}}\n          path: build-artifacts\n      - name: Reuse the build job's release binary\n        run: |\n          set -euo pipefail\n          tarball=\"build-artifacts/{binary}-$VERSION-${{{{ matrix.target }}}}.tar.gz\"\n          bin_sidecar=\"build-artifacts/{binary}-${{{{ matrix.arch }}}}.bin.sha256\"\n          test -f \"$tarball\" || {{ echo \"::error::missing build tarball $tarball\" >&2; exit 1; }}\n          test -f \"$tarball.sha256\" || {{ echo \"::error::missing tarball checksum $tarball.sha256\" >&2; exit 1; }}\n          test -f \"$bin_sidecar\" || {{ echo \"::error::missing binary checksum $bin_sidecar\" >&2; exit 1; }}\n          digest=\"$(sha256sum \"$tarball\" | awk '{{print $1}}')\"\n          sidecar=\"$(awk 'NF {{print $1; exit}}' \"$tarball.sha256\")\"\n          [ \"$digest\" = \"$sidecar\" ] || {{ echo \"::error::build tarball fails its sidecar checksum\" >&2; exit 1; }}\n          recorded=\"$(awk 'NF {{print $1; exit}}' \"$bin_sidecar\")\"\n          case \"$recorded\" in\n            ''|*[!0-9a-f]*) echo \"::error::recorded binary digest is not lowercase hex\" >&2; exit 1 ;;\n          esac\n          [ \"${{#recorded}}\" -eq 64 ] || {{ echo \"::error::recorded binary digest has invalid length\" >&2; exit 1; }}\n          mkdir -p \"target/$TARGET/release\"\n          tar -xzf \"$tarball\" -C \"target/$TARGET/release\" \"{binary}\"\n          reused=\"target/$TARGET/release/{binary}\"\n          chmod 0755 \"$reused\"\n          test -x \"$reused\"\n          actual=\"$(sha256sum \"$reused\" | awk '{{print $1}}')\"\n          [ \"$actual\" = \"$recorded\" ] || {{ echo \"::error::reused runner binary does not match the recorded digest\" >&2; exit 1; }}\n",
        download = ActionPin::DownloadArtifact.reference(),
        provider = canonical_provider(config),
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
        workflow.render_tool_provisioning(&mut setup, ProviderId::GithubHosted, unit, false);
        "mbx"
    } else {
        "cargo"
    };
    let mut needs = if preview {
        vec!["identity".to_owned(), "metadata".to_owned()]
    } else {
        vec![
            "admit-provider".to_owned(),
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
        // Provenance signing is publish-only: drilled modes build and
        // verify the packages but sign nothing.
        let gate = if has_release_modes(release) {
            "    if: ${{ needs.verify.outputs.mode == 'publish' }}\n".to_owned()
        } else {
            String::new()
        };
        (
            "Sign ${{ matrix.arch }} Debian package",
            "    needs: [verify, debian]\n",
            gate,
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
            "amd64" => hosted_selector_runs_on(config),
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
        "  image-admission:\n    needs: [admit-provider, verify]\n    name: Admit immutable image tag\n    timeout-minutes: 10\n    runs-on: {runner}\n    permissions:\n      contents: read\n      packages: read\n    outputs:\n      existing: ${{{{ steps.inspect.outputs.existing }}}}\n      index_digest: ${{{{ steps.inspect.outputs.index_digest }}}}\n    steps:\n      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n      - name: Log in to GHCR\n        uses: {login}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ secrets.GITHUB_TOKEN }}}}\n      - name: Inspect version tag without mutation\n        id: inspect\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n          GHCR_IMAGE: {image}\n          VERSION: ${{{{ needs.verify.outputs.version }}}}\n          REPOSITORY: ${{{{ github.repository }}}}\n          RECOVERY_INDEX_DIGEST: ${{{{ github.event_name == 'workflow_dispatch' && inputs.existing-image-digest || '' }}}}\n        run: |\n          set -euo pipefail\n          ref=\"${{GHCR_IMAGE}}:${{VERSION}}\"\n          error_file=\"$(mktemp)\"\n          if docker buildx imagetools inspect \"$ref\" --format '{{{{json .}}}}' > image.json 2>\"$error_file\"; then\n            index_digest=\"$(jq -er '.manifest.digest' image.json)\"\n            case \"$index_digest\" in\n              sha256:[0-9a-fA-F]*) ;;\n              *) echo \"::error::existing image tag $ref returned an invalid index digest\" >&2; exit 1 ;;\n            esac\n            if ! gh release view \"v${{VERSION}}\" --repo \"$REPOSITORY\" >/dev/null 2>&1; then\n              if [ -z \"$RECOVERY_INDEX_DIGEST\" ] || [ \"$index_digest\" != \"$RECOVERY_INDEX_DIGEST\" ]; then\n                echo \"::error::OCI tag $ref already exists without a matching GitHub release; refusing to adopt unknown bytes\" >&2\n                exit 1\n              fi\n              echo \"adopting explicitly supplied recovery index $index_digest\"\n            fi\n            {{\n              echo \"existing=true\"\n              echo \"index_digest=$index_digest\"\n            }} >> \"$GITHUB_OUTPUT\"\n          elif grep -Eiq 'manifest unknown|no such manifest|not found|name unknown' \"$error_file\"; then\n            {{\n              echo \"existing=false\"\n              echo \"index_digest=\"\n            }} >> \"$GITHUB_OUTPUT\"\n          else\n            cat \"$error_file\" >&2\n            echo \"::error::could not determine whether OCI tag $ref exists; refusing a fail-open publish\" >&2\n            exit 1\n          fi\n",
        runner = hosted_selector_runs_on(config),
        image = yaml_scalar(&release.image),
    )
}

/// The per-arch OCI platform lane: each native builder compiles the binary
/// the image embeds (the declared image package, else the release package),
/// then builds and pushes its platform image under a disposable
/// commit-scoped staging tag. Version tags are admitted and promoted only
/// by the index job; staging tags can never overwrite a consumer-facing
/// tag. The lane standardizes the Dockerfile handoff points — the binary
/// at `release-binaries/<arch>/<package>`, a `VERSION` build arg, and the
/// automatic token as the `github_token` build secret — while Dockerfiles
/// and image contents stay consumer-owned.
fn render_image_platform_job(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let needs = "admit-provider, verify, metadata, image-admission";
    // Staging pushes write to the registry, so drilled modes skip the lane
    // instead of pushing drill bytes under commit tags, and a modeless lane
    // never pushes from a dispatch: without a mode gate a dispatch-on-tag
    // would stage platforms the index job then assembles into an orphaned
    // version tag no later tag push can adopt.
    let gate = if has_release_modes(release) {
        "    if: ${{ needs.image-admission.outputs.existing != 'true' && needs.verify.outputs.mode == 'publish' }}\n"
    } else {
        "    if: ${{ needs.image-admission.outputs.existing != 'true' && github.event_name != 'workflow_dispatch' }}\n"
    };
    let Some(matrix) = image_platform_matrix(config, &release.targets) else {
        return format!(
            "  image-platform:\n    name: Build ${{{{ matrix.arch }}}} GHCR image\n    needs: [{needs}]\n{gate}    runs-on: {runner}\n    timeout-minutes: 5\n    steps:\n      - name: Reject non-multi-arch image contract\n        run: |\n          echo '::error::native OCI lane needs exactly x86_64+aarch64 linux targets' >&2\n          exit 1\n",
            runner = hosted_selector_runs_on(config),
        );
    };
    let checkout = ActionPin::Checkout.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let buildx = ActionPin::DockerBuildx.reference();
    let login = ActionPin::DockerLogin.reference();
    let build = ActionPin::DockerBuild.reference();
    // The binary the Dockerfile copies from `release-binaries/<arch>/`: the
    // declared image package, else the release package. Both empty means the
    // contract names no binary, so the lane rejects instead of building a
    // hardcoded package that only exists in one repository.
    let workflow_package = if release.image_package.is_empty() {
        release.package.as_str()
    } else {
        release.image_package.as_str()
    };
    if workflow_package.is_empty() {
        return format!(
            "  image-platform:\n    name: Build ${{{{ matrix.arch }}}} GHCR image\n    needs: [{needs}]\n{gate}    runs-on: {runner}\n    timeout-minutes: 5\n    steps:\n      - name: Reject imageless binary contract\n        run: |\n          echo '::error::native OCI lane needs `package` or `image_package` naming the binary the image embeds' >&2\n          exit 1\n",
            runner = hosted_selector_runs_on(config),
        );
    }
    let mut setup = String::new();
    let workflow = WorkflowIr::from_config(config);
    let cargo_cmd = if let Some(unit) = rust_package_unit(config, workflow_package) {
        workflow.render_tool_provisioning(&mut setup, ProviderId::GithubHosted, unit, false);
        "mbx"
    } else {
        "cargo"
    };
    format!(
        "  image-platform:\n    name: Build ${{{{ matrix.arch }}}} GHCR image\n    needs: [{needs}]\n{gate}    timeout-minutes: 60\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    runs-on: ${{{{ matrix.runner }}}}\n    permissions:\n      contents: read\n      packages: write\n      id-token: write\n      attestations: write\n    env:\n      GHCR_IMAGE: {image}\n      VERSION: ${{{{ needs.verify.outputs.version }}}}\n      COMMIT: ${{{{ github.sha }}}}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n          ref: ${{{{ github.sha }}}}\n          fetch-depth: 1\n          persist-credentials: false\n{setup}      - name: Build the image binary\n        env:\n          CARGO_INCREMENTAL: \"0\"\n        run: |\n          set -euo pipefail\n          {cargo_cmd} build --locked --release --package {workflow_package}\n          binary=\"release-binaries/${{{{ matrix.arch }}}}/{workflow_package}\"\n          mkdir -p \"release-binaries/${{{{ matrix.arch }}}}\"\n          cp \"target/release/{workflow_package}\" \"$binary\"\n          chmod 0755 \"$binary\"\n          test -x \"$binary\"\n      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n          keep-state: true\n      - name: Log in to GHCR\n        uses: {login}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ secrets.GITHUB_TOKEN }}}}\n      - name: Build + push platform image\n        id: push\n        uses: {build}\n        with:\n          context: {context}\n          file: {dockerfile}\n          platforms: ${{{{ matrix.platform }}}}\n          push: true\n          provenance: true\n          sbom: true\n          cache-from: |\n            type=registry,ref=${{{{ env.GHCR_IMAGE }}}}:buildcache-${{{{ matrix.arch }}}}\n            type=gha,scope={scope}-${{{{ matrix.arch }}}}\n          cache-to: |\n            type=registry,ref=${{{{ env.GHCR_IMAGE }}}}:buildcache-${{{{ matrix.arch }}}},mode=max\n            type=gha,scope={scope}-${{{{ matrix.arch }}}},mode=max\n          secrets: |\n            github_token=${{{{ github.token }}}}\n          build-args: |\n            VERSION=${{{{ needs.verify.outputs.version }}}}\n          tags: ${{{{ env.GHCR_IMAGE }}}}:release-${{{{ env.COMMIT }}}}-${{{{ matrix.arch }}}}\n          labels: |\n            org.opencontainers.image.version=${{{{ needs.verify.outputs.version }}}}\n            org.opencontainers.image.revision=${{{{ github.sha }}}}\n            org.opencontainers.image.source={source_url}\n            org.velnor.manifest-sha256=${{{{ needs.metadata.outputs.manifest_sha256 }}}}\n      - name: Record platform digest\n        env:\n          ARCH: ${{{{ matrix.arch }}}}\n        run: |\n          set -euo pipefail\n          docker buildx imagetools inspect \\\n            \"${{GHCR_IMAGE}}:release-${{COMMIT}}-${{ARCH}}\" \\\n            --format '{{{{json .}}}}' > image-inspect.json\n          PLATFORM_DIGEST=\"$(jq -er --arg arch \"$ARCH\" '\n            [.manifest.manifests[]\n             | select(.platform.architecture == $arch and .platform.os == \"linux\")\n             | select((.annotations[\"vnd.docker.reference.type\"] // \"\") != \"attestation-manifest\")\n             | .digest]\n            | if length == 1 then .[0] else error(\"expected one image manifest for architecture\") end\n          ' image-inspect.json)\"\n          case \"$PLATFORM_DIGEST\" in\n            sha256:[0-9a-fA-F]*) ;;\n            *) echo \"::error::staging image inspection did not return a platform digest\" >&2; exit 1 ;;\n          esac\n          printf '%s\\n' \"$PLATFORM_DIGEST\" > \"image-${{{{ matrix.arch }}}}.digest\"\n      - name: Upload platform digest\n        uses: {upload}\n        with:\n          name: image-platform-${{{{ matrix.arch }}}}\n          path: image-${{{{ matrix.arch }}}}.digest\n          if-no-files-found: error\n          retention-days: 2\n",
        image = yaml_scalar(&release.image),
        source_url = release_source_url(release),
        workflow_package = workflow_package,
        dockerfile = yaml_scalar(docker_dockerfile(release)),
        context = yaml_scalar(docker_context(release)),
        scope = docker_cache_scope(&release.image),
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
    // A drilled mode assembles no version tag: the immutable index push is
    // publish-only, while skipped staging builds stay skipped. A modeless
    // lane refuses to assemble off a dispatch (assembling a version index
    // off a dispatch orphans a tag no later tag push can adopt) but may
    // adopt an already-admitted index: the gate opens on dispatches only
    // when admission verified an existing tag, and that branch inspects
    // without pushing, so no dispatch can mint version bytes.
    let mode_gate = if has_release_modes(release) {
        " && needs.verify.outputs.mode == 'publish'"
    } else {
        " && (github.event_name != 'workflow_dispatch' || needs.image-admission.outputs.existing == 'true')"
    };
    format!(
        "  image:\n    if: ${{{{ always() && needs.admit-provider.result == 'success' && needs.verify.result == 'success' && needs.metadata.result == 'success' && needs.image-admission.result == 'success' && (needs.image-platform.result == 'success' || needs.image-platform.result == 'skipped'){mode_gate} }}}}\n    needs: [admit-provider, verify, metadata, image-platform, image-admission]\n    name: Assemble one multi-platform GHCR image\n    timeout-minutes: 15\n    runs-on: {runner}\n    permissions:\n      contents: read\n      packages: write\n    outputs:\n      index_digest: ${{{{ steps.push.outputs.index_digest }}}}\n      manifest_sha256: ${{{{ needs.metadata.outputs.manifest_sha256 }}}}\n    env:\n      GHCR_IMAGE: {image}\n      SOURCE_URL: {source_url}\n      VERSION: ${{{{ needs.verify.outputs.version }}}}\n      COMMIT: ${{{{ github.sha }}}}\n    steps:\n      - name: Download platform digests\n        if: ${{{{ needs.image-admission.outputs.existing != 'true' }}}}\n        uses: {download}\n        with:\n          pattern: image-platform-*\n          path: image-artifacts\n          merge-multiple: true\n      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n      - name: Log in to GHCR\n        uses: {login}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ secrets.GITHUB_TOKEN }}}}\n      - name: Assemble and inspect immutable image index\n        id: push\n        env:\n          AMD64_DIGEST_FILE: image-artifacts/image-amd64.digest\n          ARM64_DIGEST_FILE: image-artifacts/image-arm64.digest\n          IMAGE_ALREADY_EXISTS: ${{{{ needs.image-admission.outputs.existing }}}}\n          EXPECTED_EXISTING_INDEX_DIGEST: ${{{{ needs.image-admission.outputs.index_digest }}}}\n        run: |\n          set -euo pipefail\n          if [ \"$IMAGE_ALREADY_EXISTS\" = true ]; then\n            docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > image-digests.json\n            index_digest=\"$(jq -er '.manifest.digest' image-digests.json)\"\n            [ \"$index_digest\" = \"$EXPECTED_EXISTING_INDEX_DIGEST\" ] || {{\n              echo \"::error::version tag moved from $EXPECTED_EXISTING_INDEX_DIGEST to $index_digest during admission\" >&2\n              exit 1\n            }}\n          else\n            for path in \"$AMD64_DIGEST_FILE\" \"$ARM64_DIGEST_FILE\"; do\n              test -s \"$path\"\n              digest=\"$(tr -d '[:space:]' < \"$path\")\"\n              case \"$digest\" in\n                sha256:[0-9a-fA-F]*) ;;\n                *) echo \"::error::invalid platform digest in $path\" >&2; exit 1 ;;\n              esac\n            done\n            amd64_digest=\"$(tr -d '[:space:]' < \"$AMD64_DIGEST_FILE\")\"\n            arm64_digest=\"$(tr -d '[:space:]' < \"$ARM64_DIGEST_FILE\")\"\n            docker buildx imagetools create \\\n              --tag \"${{GHCR_IMAGE}}:${{VERSION}}\" \\\n              \"${{GHCR_IMAGE}}:release-${{COMMIT}}-amd64\" \\\n              \"${{GHCR_IMAGE}}:release-${{COMMIT}}-arm64\"\n            docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > image-digests.json\n            jq -e --arg amd \"$amd64_digest\" --arg arm \"$arm64_digest\" '\n              any(.manifest.manifests[]; .digest == $amd and .platform.architecture == \"amd64\") and\n              any(.manifest.manifests[]; .digest == $arm and .platform.architecture == \"arm64\")\n            ' image-digests.json >/dev/null || {{\n              echo \"::error::version tag does not reference both newly built platform digests\" >&2\n              exit 1\n            }}\n            [ \"$(jq -r '[.manifest.manifests[] | select((.annotations[\"vnd.docker.reference.type\"] // \"\") != \"attestation-manifest\")] | length' image-digests.json)\" = \"2\" ] || {{\n              echo \"::error::version tag carries an unexpected platform set\" >&2\n              exit 1\n            }}
          fi\n          docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > image-digests.json\n          index_digest=\"$(jq -er '.manifest.digest' image-digests.json)\"\n          case \"$index_digest\" in\n            sha256:[0-9a-fA-F]*) ;;\n            *) echo \"::error::manifest inspection did not return an index digest\" >&2; exit 1 ;;\n          esac\n          printf 'index_digest=%s\\n' \"$index_digest\" >> \"$GITHUB_OUTPUT\"\n          printf '%s\\n' \"$index_digest\" > image-index.digest\n      - name: Download release metadata\n        uses: {download}\n        with:\n          name: release-metadata\n      - name: Upload image digests\n        uses: {upload}\n        with:\n          name: image-digests\n          path: |\n            image-digests.json\n            image-index.digest\n          if-no-files-found: error\n          retention-days: 2\n",
        runner = hosted_selector_runs_on(config),
        image = yaml_scalar(&release.image),
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

/// Re-verify the release tag still points at the event commit, immediately
/// before publication. `gh release create --verify-tag` only checks the tag
/// exists, so a tag moved between verify and publish would otherwise bind
/// the release to bytes built from a different commit. Every immutable
/// publisher carries this step; the tag, the bytes, and the record agree or
/// the lane refuses.
const TAG_IMMUTABILITY_STEP: &str = r#"      - name: Verify release tag stayed immutable before publication
        env:
          EXPECTED_TAG_REF: ${{ github.ref }}
          EXPECTED_TAG_COMMIT: ${{ github.sha }}
        run: |
          set -euo pipefail
          remote_tag_refs="$(git ls-remote --exit-code origin "$EXPECTED_TAG_REF" "$EXPECTED_TAG_REF^{}")"
          remote_tag_commit="$(printf '%s\n' "$remote_tag_refs" | awk -v expected="$EXPECTED_TAG_REF" '
            $2 == expected "^{}" { peeled=$1; found_peeled=1; next }
            $2 == expected && !found_peeled { raw=$1 }
            END {
              if (found_peeled) print peeled
              else if (raw != "") print raw
            }
          ')"
          case "$remote_tag_commit" in
            *[!0-9a-f]*|'') echo "::error::release tag $EXPECTED_TAG_REF did not resolve to lowercase hex" >&2; exit 1 ;;
          esac
          [ "${#remote_tag_commit}" -eq 40 ] || { echo "::error::release tag $EXPECTED_TAG_REF did not resolve to one commit" >&2; exit 1; }
          [ "$remote_tag_commit" = "$EXPECTED_TAG_COMMIT" ] || {
            echo "::error::release tag $EXPECTED_TAG_REF moved from $EXPECTED_TAG_COMMIT to $remote_tag_commit" >&2
            exit 1
          }
"#;

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
    // Publication — including its no-clobber reconciliation — is
    // publish-only: drilled modes stop after the local assembly.
    let mode_gate = if has_release_modes(release) {
        "    if: ${{ needs.verify.outputs.mode == 'publish' }}\n"
    } else {
        ""
    };
    format!(
        "  publish:\n    name: Control / Publish\n    needs: [{needs}]\n{mode_gate}    runs-on: {runner}\n    timeout-minutes: 20\n    environment: github-release\n    permissions:\n      contents: write\n      packages: read\n    env:\n      VERSION: {version}\n      SOURCE_REF: ${{{{ github.ref }}}}\n      SOURCE_COMMIT: ${{{{ github.sha }}}}\n      COMMIT: ${{{{ github.sha }}}}\n      INDEX_DIGEST: ${{{{ needs.image.outputs.index_digest }}}}\n      MANIFEST_SHA256: ${{{{ needs.image.outputs.manifest_sha256 }}}}\n      GHCR_IMAGE: {image}\n      SOURCE_URL: {source_url}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n          persist-credentials: false\n      - name: Download release artifacts\n        uses: {download}\n        with:\n          path: artifacts\n          pattern: {provider}-*\n          merge-multiple: true\n      - name: Download Debian packages\n        uses: {download}\n        with:\n          name: debian-packages\n          path: artifacts\n      - name: Download release metadata\n        uses: {download}\n        with:\n          name: release-metadata\n          path: artifacts\n      - name: Download image digests\n        uses: {download}\n        with:\n          name: image-digests\n          path: artifacts\n      - name: Verify tarball provenance\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          for artifact in artifacts/*.tar.gz; do gh attestation verify \"$artifact\" --repo \"$GITHUB_REPOSITORY\"; done\n      - name: Verify deb provenance\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          for artifact in artifacts/*.deb; do gh attestation verify \"$artifact\" --repo \"$GITHUB_REPOSITORY\" --signer-workflow \"$GITHUB_REPOSITORY/.github/workflows/ci-release-package-signer.yml\"; done\n{record_assembly}{record_reverify}      - name: Assemble independent checksums\n        run: |\n          set -euo pipefail\n          shopt -s nullglob\n          subjects=(artifacts/*.tar.gz artifacts/*.deb)\n          test \"${{#subjects[@]}}\" -eq {subject_count}\n          : > SHA256SUMS\n          : > assets.jsonl\n          for subject in \"${{subjects[@]}}\"; do\n            name=$(basename \"$subject\")\n            digest=$(sha256sum \"$subject\" | awk '{{print $1}}')\n            sidecar=\"$(awk 'NF {{print $1; exit}}' \"${{subject}}.sha256\")\"\n            [[ \"$digest\" =~ ^[0-9a-f]{{64}}$ && \"$sidecar\" = \"$digest\" ]] \\\n              || {{ echo \"::error::$name sidecar does not match its payload\" >&2; exit 1; }}\n            printf '%s  %s\\n' \"$digest\" \"$name\" >> SHA256SUMS\n            jq -cn --arg name \"$name\" --arg sha256 \"$digest\" '{{name:$name,sha256:$sha256}}' >> assets.jsonl\n          done\n          test \"$(wc -l < SHA256SUMS | tr -d ' ')\" -eq {subject_count}\n          (cd artifacts && sha256sum --check --strict ../SHA256SUMS)\n{manifest_step}      - name: Stage package subjects for hosted signer\n        run: |\n          set -euo pipefail\n          mkdir signer-input\n          cp artifacts/{binary}-*.tar.gz artifacts/{package}-*.deb signer-input/\n{packaged_identity}      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n      - name: Log in to GHCR for immutable image verification\n        uses: {login}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ secrets.GITHUB_TOKEN }}}}\n      - name: Verify OCI index stayed immutable before publication\n        env:\n          EXPECTED_INDEX_DIGEST: ${{{{ needs.image.outputs.index_digest }}}}\n        run: |\n          set -euo pipefail\n          docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > published-image.json\n          published_index=\"$(jq -er '.manifest.digest' published-image.json)\"\n          [ \"$published_index\" = \"$EXPECTED_INDEX_DIGEST\" ] || {{\n            echo \"::error::OCI version tag moved from $EXPECTED_INDEX_DIGEST to $published_index before release publication\" >&2\n            exit 1\n          }}\n{tag_check}{create_verify}      - name: Upload package subjects\n        uses: {upload}\n        with:\n          name: package-subjects\n          path: signer-input\n          if-no-files-found: error\n          retention-days: 2\n",
        runner = selected_runner(config),
        provider = canonical_provider(config),
        image = yaml_scalar(&release.image),
        source_url = release_source_url(release),
        binary = release.binary,
        package = release.package,
        tag_check = TAG_IMMUTABILITY_STEP,
    )
}

/// The preview identity job: one resolved `~preview.N+sha7` version, bound
/// to exactly the commit that triggered the run and never re-resolved from
/// the moving branch tip. Every downstream job consumes these outputs.
fn render_preview_identity_job(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let checkout = ActionPin::Checkout.reference();
    // The identity job below always runs on the hosted github runner, so its
    // runtime install is keyed to that placement — never to the repo lane.
    let setup = workflow_runtime_setup(
        ProviderId::GithubHosted,
        &config.repository,
        &config.workflow_revision,
    );
    let manifest = match rust_package_unit(config, &release.package) {
        Some(unit) if unit.root == "." || unit.root.is_empty() => "Cargo.toml".to_owned(),
        Some(unit) => format!("{}/Cargo.toml", unit.root.trim_end_matches('/')),
        None => "Cargo.toml".to_owned(),
    };
    let manifest = shell_quote(&manifest);
    format!(
        "  identity:\n    name: Resolve preview identity\n    if: ${{{{ github.ref == 'refs/heads/{}' }}}}\n    timeout-minutes: 10\n    runs-on: {runner}\n    outputs:\n      version: ${{{{ steps.identity.outputs.version }}}}\n      crate_version: ${{{{ steps.identity.outputs.crate_version }}}}\n      name: ${{{{ steps.identity.outputs.name }}}}\n      commit: ${{{{ steps.identity.outputs.commit }}}}\n      short_commit: ${{{{ steps.identity.outputs.short_commit }}}}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n          ref: ${{{{ github.sha }}}}\n{POLICY_CHECKOUT_WITH}{setup}{policy}      - name: Resolve the preview version from the crate manifest\n        id: identity\n        env:\n          EVENT_SHA: ${{{{ github.sha }}}}\n          RUN_NUMBER: ${{{{ github.run_number }}}}\n        run: |\n          set -euo pipefail\n          commit=\"$(git rev-parse HEAD)\"\n          case \"$commit\" in\n            *[!0-9a-f]*|'') echo \"::error::HEAD did not resolve to lowercase hex\" >&2; exit 1 ;;\n          esac\n          [ \"${{#commit}}\" -eq 40 ] || {{ echo \"::error::HEAD is not a 40-hex commit\" >&2; exit 1; }}\n          [ \"$commit\" = \"$EVENT_SHA\" ] || {{ echo \"::error::checkout $commit != event commit $EVENT_SHA\" >&2; exit 1; }}\n          case \"$RUN_NUMBER\" in\n            ''|*[!0-9]*) echo \"::error::invalid workflow run number $RUN_NUMBER\" >&2; exit 1 ;;\n          esac\n          [ \"$RUN_NUMBER\" -gt 0 ] || {{ echo \"::error::run number must be positive\" >&2; exit 1; }}\n          crate=\"$(sed -n 's/^version = \"\\(.*\\)\"/\\1/p' {manifest} | head -n1)\"\n          case \"$crate\" in\n            ''|*[!0-9.]*) echo \"::error::crate version $crate is not an X.Y.Z version\" >&2; exit 1 ;;\n          esac\n          [[ \"$crate\" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+$ ]] \\\n            || {{ echo \"::error::crate version $crate is not an X.Y.Z version\" >&2; exit 1; }}\n          short_commit=\"${{commit:0:7}}\"\n          version=\"${{crate}}~preview.${{RUN_NUMBER}}+${{short_commit}}\"\n          [[ \"$version\" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+~preview\\.[0-9]+\\+[0-9a-f]{{7}}$ ]] \\\n            || {{ echo \"::error::preview version $version violates the preview contract\" >&2; exit 1; }}\n          {{\n            echo \"version=$version\"\n            echo \"crate_version=$crate\"\n            echo \"name=Preview $version\"\n            echo \"commit=$commit\"\n            echo \"short_commit=$short_commit\"\n          }} >> \"$GITHUB_OUTPUT\"\n",
        config.default_branch,
        runner = hosted_selector_runs_on(config),
        policy = policy_enforcement_step(),
    )
}

/// The `with:` body of a checkout in a job that runs the policy validator:
/// full history and no persisted credentials. `pin-reachable` walks from the
/// audited head to the declared pin, so a validator-running job never checks
/// out shallow; `validate_policy_jobs_check_out_full_history` refuses a tree
/// where one does, whichever renderer wrote its checkout.
const POLICY_CHECKOUT_WITH: &str =
    "          fetch-depth: 0\n          persist-credentials: false\n";

/// The step that runs `velnor-workflow policy` against the checked-out tree.
/// It is rendered only after a checkout carrying [`POLICY_CHECKOUT_WITH`].
fn policy_enforcement_step() -> &'static str {
    "      - name: Enforce workflow policy\n        env:\n          EVENT_NAME: ${{ github.event_name }}\n        run: velnor-workflow policy --workflow-root \"$GITHUB_WORKSPACE\"\n"
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
        "  publish:\n    needs: [{needs}]\n    name: Replace the rolling preview release\n    if: ${{{{ github.event_name == 'push' && github.ref == 'refs/heads/{branch}' }}}}\n    timeout-minutes: 20\n    runs-on: {runner}\n    permissions:\n      contents: write\n    env:\n      VERSION: ${{{{ needs.identity.outputs.version }}}}\n      NAME: ${{{{ needs.identity.outputs.name }}}}\n      COMMIT: ${{{{ needs.identity.outputs.commit }}}}\n      GH_REPO: ${{{{ github.repository }}}}\n    steps:\n      - name: Download preview debs\n        uses: {download}\n        with:\n          name: debian-packages\n          path: artifacts\n      - name: Download preview metadata\n        uses: {download}\n        with:\n          name: preview-metadata\n          path: preview-metadata\n      - name: Assemble independent checksums\n        run: |\n          set -euo pipefail\n          shopt -s nullglob\n          subjects=(artifacts/*-preview-*.deb)\n          test \"${{#subjects[@]}}\" -eq {count}\n          : > SHA256SUMS\n          : > assets.jsonl\n          for subject in \"${{subjects[@]}}\"; do\n            name=$(basename \"$subject\")\n            digest=$(sha256sum \"$subject\" | awk '{{print $1}}')\n            sidecar=\"$(awk 'NF {{print $1; exit}}' \"${{subject}}.sha256\")\"\n            [[ \"$digest\" =~ ^[0-9a-f]{{64}}$ && \"$sidecar\" = \"$digest\" ]] \\\n              || {{ echo \"::error::$name sidecar does not match its payload\" >&2; exit 1; }}\n            printf '%s  %s\\n' \"$digest\" \"$name\" >> SHA256SUMS\n            jq -cn --arg name \"$name\" --arg sha256 \"$digest\" '{{name:$name,sha256:$sha256}}' >> assets.jsonl\n          done\n          test \"$(wc -l < SHA256SUMS | tr -d ' ')\" -eq {count}\n          (cd artifacts && sha256sum --check --strict ../SHA256SUMS)\n          chmod +x preview-metadata/{binary}-release-tool\n          tmpdir=\"$(mktemp -d)\"\n          trap 'rm -rf -- \"$tmpdir\"' EXIT\n          for arch in {arch_list}; do\n            deb=\"artifacts/{package}-preview-${{VERSION}}-${{arch}}.deb\"\n            [ -f \"$deb\" ] || {{ echo \"::error::missing preview deb for $arch\" >&2; exit 1; }}\n            [ \"$(dpkg-deb -f \"$deb\" Version)\" = \"$VERSION\" ] \\\n              || {{ echo \"::error::published deb version != preview identity ($arch)\" >&2; exit 1; }}\n            [ \"$(dpkg-deb -f \"$deb\" Architecture)\" = \"$arch\" ] \\\n              || {{ echo \"::error::published deb arch mismatch ($arch)\" >&2; exit 1; }}\n            record=\"$tmpdir/package-record-$arch.json\"\n            record_path=\"$(dpkg-deb -c \"$deb\" | awk '$NF ~ /(^|\\/)package-record\\.json$/ {{ print $NF }}')\"\n            [ -n \"$record_path\" ] || {{ echo \"::error::deb missing its package record ($arch)\" >&2; exit 1; }}\n            dpkg-deb --fsys-tarfile \"$deb\" | tar -xOf - \"$record_path\" > \"$record\"\n            record_digest=\"$(sha256sum \"$record\" | awk '{{print $1}}')\"\n            preview-metadata/{binary}-release-tool release verify-record \\\n              --record \"$record\" --sha256 \"$record_digest\" >/dev/null \\\n              || {{ echo \"::error::packaged package record is incoherent ($arch)\" >&2; exit 1; }}\n            packaged_binary=\"$(dpkg-deb --fsys-tarfile \"$deb\" | tar -xOf - ./usr/bin/{binary} | sha256sum | awk '{{print $1}}')\"\n            jq -e --arg commit \"$COMMIT\" --arg version \"$VERSION\" --arg arch \"$arch\" --arg binary \"$packaged_binary\" \\\n              '.build.kind == \"preview\" and .build.commit == $commit and\n               .build.debian_version == $version and\n               .architecture.arch == $arch and .architecture.binary_sha256 == $binary' \\\n              \"$record\" >/dev/null \\\n              || {{ echo \"::error::packaged package record does not name this preview identity ($arch)\" >&2; exit 1; }}\n          done\n{manifest_step}      - name: Replace the rolling preview release atomically\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          live_response=\"$(mktemp)\"\n          trap 'rm -f -- \"$live_response\"' EXIT\n          live_status=0\n          gh api -i \"repos/$GITHUB_REPOSITORY/releases/tags/preview\" > \"$live_response\" 2>/dev/null || live_status=$?\n          live_http=\"$(awk 'NR == 1 {{print $2; exit}}' \"$live_response\")\"\n          if [ \"$live_status\" -ne 0 ]; then\n            [ \"$live_http\" = \"404\" ] \\\n              || {{ echo \"::error::could not read the live preview release (gh api exit $live_status, HTTP ${{live_http:-unknown}}); refusing to replace it\" >&2; exit 1; }}\n          else\n            live_body=\"$(awk 'body {{print; next}} /^\\r?$/ {{body = 1}}' \"$live_response\")\"\n            if ! live_name=\"$(jq -er '.name | strings' <<<\"$live_body\" 2>/dev/null)\"; then\n              echo \"::error::live preview release response carries no name; refusing to replace it\" >&2\n              exit 1\n            fi\n            [ -n \"$live_name\" ] || {{ echo \"::error::live preview release has an empty name; refusing to replace it\" >&2; exit 1; }}\n            live_version=\"${{live_name#Preview }}\"\n            [[ \"$live_name\" = \"Preview $live_version\" && \"$live_version\" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+~preview\\.[0-9]+\\+[0-9a-f]{{7}}$ ]] \\\n              || {{ echo \"::error::live preview release names '$live_name', which is not 'Preview <version>' under the preview contract; refusing to delete it\" >&2; exit 1; }}\n            if dpkg --compare-versions \"$live_version\" eq \"$VERSION\"; then\n              :\n            elif dpkg --compare-versions \"$live_version\" gt \"$VERSION\"; then\n              echo \"::error::live preview $live_version is newer than candidate $VERSION; refusing to move the rolling preview backward — re-run the LATEST preview run instead\" >&2\n              exit 1\n            fi\n          fi\n          gh release delete preview --cleanup-tag --yes || true\n          gh release create preview --target \"$COMMIT\" --prerelease --title \"$NAME\" \\\n            {create_assets}\n      - name: Verify the published rolling preview\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          gh api \"repos/$GITHUB_REPOSITORY/releases/tags/preview\" > published.json\n          asset_version=\"${{VERSION//'~'/'.'}}\"\n          jq -e \\\n            --arg name \"$NAME\" --arg commit \"$COMMIT\" --arg asset_version \"$asset_version\" '\n              (.draft | not) and .prerelease == true and\n              .tag_name == \"preview\" and .name == $name and\n              .target_commitish == $commit and\n              ([.assets[].name] | sort) ==\n              ([\n                {expected_assets}\n              ] | sort)\n            ' published.json >/dev/null\n          jq -e --arg version \"$VERSION\" \\\n            '.version == $version and .source_ref == \"refs/heads/{branch}\" and\n             (.assets | length) == {count}' release-manifest.json >/dev/null\n",
        branch = config.default_branch,
        runner = selected_runner(config),
        count = arches.len(),
        package = release.package,
    )
}

/// The rolling-lane trigger bindings: a `workflow_run` block for the bound
/// producer ahead of dispatch, and the declared drill modes on dispatch.
/// No-op without a producer or modes.
fn inject_preview_triggers(output: &str, config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = output.to_owned();
    if has_producer_binding(release) {
        output = output.replacen(
            "  workflow_dispatch:\n",
            &format!(
                "{}  workflow_dispatch:\n",
                workflow_run_trigger(config, release)
            ),
            1,
        );
    }
    inject_dispatch_modes(&output, release)
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
    let output = format!(
        "{GENERATED_HEADER}name: Preview\nrun-name: Preview · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}\n\non:\n  push:\n    branches: [{}]\n    paths:\n{paths}  workflow_dispatch:\n\nconcurrency:\n  group: preview-${{{{ github.repository }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n{jobs}",
        yaml_scalar(&config.default_branch),
        paths = release_watch_paths(config),
    );
    inject_native_preview_bindings(&output, config, release)
}

/// The Debian preview bindings: trigger bindings with the source and gate
/// jobs, the identity job rewired onto the resolved source, and the rolling
/// publish admitted by the gate. Archive and credential contracts bind the
/// tarball package steps, which the Debian lane has none of, so a declared
/// contract there is a visible note instead of a silent skip: the lane
/// mounts no credential it would have to tear down.
fn inject_native_preview_bindings(
    output: &str,
    config: &ProjectConfig,
    release: &ReleaseSpec,
) -> String {
    let mut output = inject_preview_triggers(output, config, release);
    output = inject_binding_jobs(&output, config, release);
    if has_producer_binding(release) {
        output = output.replacen(
            "  identity:\n    name: Resolve preview identity\n",
            "  identity:\n    name: Resolve preview identity\n    needs: [source]\n",
            1,
        );
        output = output.replacen(
            "          ref: ${{ github.sha }}\n",
            "          ref: ${{ needs.source.outputs.sha }}\n",
            1,
        );
        output = output.replacen(
            "          EVENT_SHA: ${{ github.sha }}\n",
            "          EVENT_SHA: ${{ needs.source.outputs.sha }}\n",
            1,
        );
        output = output.replacen(
            "  publish:\n    needs: [",
            "  publish:\n    needs: [publish-gate, ",
            1,
        );
        let publish_gate = format!(
            "    if: ${{{{ github.ref == 'refs/heads/{}' && needs.publish-gate.outputs.admitted == 'true' && needs.publish-gate.outputs.mode == 'publish' }}}}\n    timeout-minutes: 20\n",
            config.default_branch,
        );
        output = output.replacen(
            &format!(
                "    if: ${{{{ github.event_name == 'push' && github.ref == 'refs/heads/{}' }}}}\n    timeout-minutes: 20\n",
                config.default_branch,
            ),
            &publish_gate,
            1,
        );
    }
    if has_archive_contract(release) || !release.credentials.is_empty() {
        output = output.replacen(
            "permissions:\n  contents: read\n\njobs:",
            "permissions:\n  contents: read\n\n# Archive and credential contracts bind the tarball package steps; the Debian preview lane packages with its packager and mounts no credential.\njobs:",
            1,
        );
    }
    output
}

fn release_runner(config: &ProjectConfig, target: &str) -> String {
    if target.ends_with("-apple-darwin") {
        yaml_scalar(MACOS_HOSTED_RUNS_ON)
    } else {
        hosted_selector_runs_on(config)
    }
}

/// The hosted selector value, for the one-writer release-side jobs.
fn hosted_selector_runs_on(config: &ProjectConfig) -> String {
    config
        .selectors
        .get(&ProviderId::GithubHosted)
        .map(selector_runs_on_yaml)
        .unwrap_or_default()
}

/// Release-side builders are one writer on the release-side provider:
/// hosted when the universe contains it, otherwise the canonical-first local
/// provider, so no hosted `runs-on` leaks into a local-only surface.
fn selected_runner(config: &ProjectConfig) -> String {
    let provider = release_provider(config);
    config
        .selectors
        .get(&provider)
        .map(selector_runs_on_yaml)
        .unwrap_or_default()
}

fn workflow_runtime_setup_for_config(config: &ProjectConfig) -> String {
    if release_provider(config) == ProviderId::GithubHosted {
        workflow_runtime_setup(
            ProviderId::GithubHosted,
            &config.repository,
            &config.workflow_revision,
        )
    } else {
        String::new()
    }
}

/// Release and preview artifact builders are one writer: hosted. Core CI
/// still fans out over the universe, and release verification jobs run per
/// provider, while the release-side matrix must not carry a self-hosted
/// value behind `matrix.runner`: the pinned policy runtime validates that
/// raw value and cannot prove a matrix entry is the approved runner mapping.
/// The one-writer release-side provider: hosted when the universe contains
/// it, otherwise the canonical-first local provider, so no hosted `runs-on`
/// leaks into a local-only surface. Explicit local-only repositories retain
/// their trusted local release lane.
fn release_provider(config: &ProjectConfig) -> ProviderId {
    if config.providers.contains(&ProviderId::GithubHosted) {
        ProviderId::GithubHosted
    } else {
        config
            .providers
            .iter()
            .next()
            .copied()
            .unwrap_or(ProviderId::GithubHosted)
    }
}

fn release_providers(config: &ProjectConfig, target: &str) -> Vec<(&'static str, String)> {
    let provider = release_provider(config);
    let runner = if target.ends_with("-apple-darwin") {
        yaml_scalar(MACOS_HOSTED_RUNS_ON)
    } else if provider == ProviderId::GithubHosted {
        release_runner(config, target)
    } else {
        config
            .selectors
            .get(&provider)
            .map(selector_runs_on_yaml)
            .unwrap_or_default()
    };
    vec![(provider.as_str(), runner)]
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
        .flat_map(|target| release_providers(config, target).into_iter())
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

fn canonical_provider(config: &ProjectConfig) -> &'static str {
    release_provider(config).as_str()
}

/// Whether the contract binds a trusted producer workflow: a non-empty
/// producer renders the `workflow_run` trigger with its source-resolution
/// and publish-gate jobs.
fn has_producer_binding(release: &ReleaseSpec) -> bool {
    !release.producer_workflow.is_empty()
}

/// Whether the contract declares dispatch modes: a non-empty list renders
/// the `mode` dispatch input with a resolver step and publish gates.
fn has_release_modes(release: &ReleaseSpec) -> bool {
    !release.modes.is_empty()
}

/// Whether the contract declares archive contents: a non-empty member list
/// renders deterministic multi-member packaging with the declared retention.
fn has_archive_contract(release: &ReleaseSpec) -> bool {
    !release.archive_members.is_empty()
}

/// Whether the contract declares anything only the tarball publishers
/// (`rust-binary`, `native`) and the preview lane implement.
fn has_tarball_bindings(release: &ReleaseSpec) -> bool {
    has_producer_binding(release)
        || has_release_modes(release)
        || has_archive_contract(release)
        || !release.credentials.is_empty()
}

/// The `workflow_run` trigger block for a bound producer: the rolling lane
/// publishes only after the trusted producer completes on the default
/// branch. The runtime publish gate re-checks the producer name and
/// conclusion; the trigger alone never admits a run.
fn workflow_run_trigger(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    format!(
        "  workflow_run:\n    workflows: [{}]\n    types: [completed]\n    branches: [{}]\n",
        yaml_scalar(&release.producer_workflow),
        yaml_scalar(&config.default_branch),
    )
}

/// The dispatch `mode` input: the declared drill modes. `publish` is never
/// an option; publication stays tag-triggered (stable) or
/// admitted-producer-triggered (rolling).
fn dispatch_mode_input(release: &ReleaseSpec) -> String {
    let mut options = String::new();
    for mode in &release.modes {
        let _ = writeln!(options, "          - {mode}");
    }
    let default = release.modes.first().map_or("validate", String::as_str);
    format!(
        "    inputs:\n      mode:\n        description: Release drill mode (publication is tag-triggered, never dispatched)\n        required: false\n        default: {default}\n        type: choice\n        options:\n{options}"
    )
}

/// Expand a bare `workflow_dispatch:` trigger with the declared mode input.
/// No-op without declared modes, so legacy triggers keep their bytes.
fn inject_dispatch_modes(output: &str, release: &ReleaseSpec) -> String {
    if !has_release_modes(release) {
        return output.to_owned();
    }
    output.replacen(
        "  workflow_dispatch:\n",
        &format!("  workflow_dispatch:\n{}", dispatch_mode_input(release)),
        1,
    )
}

/// The source-resolution job: one 40-hex revision for the whole lane. A
/// `workflow_run` event builds the producer run's head SHA, every other
/// event builds its own SHA; the runtime refuses anything else.
fn render_binding_source_job(config: &ProjectConfig) -> String {
    let setup = workflow_runtime_setup_for_config(config);
    format!(
        "  source:\n    name: Resolve release source\n    runs-on: {}\n    timeout-minutes: 5\n    outputs:\n      sha: ${{{{ steps.resolve.outputs.sha }}}}\n    steps:\n{setup}      - name: Resolve source revision\n        id: resolve\n        env:\n          EVENT: ${{{{ github.event_name }}}}\n          SHA: ${{{{ github.sha }}}}\n          RUN_SHA: ${{{{ github.event.workflow_run.head_sha }}}}\n        run: |\n          set -euo pipefail\n          sha=\"$(velnor-workflow release resolve-source --event \"$EVENT\" --sha \"$SHA\" --run-sha \"$RUN_SHA\")\"\n          echo \"sha=$sha\" >> \"$GITHUB_OUTPUT\"\n",
        selected_runner(config),
    )
}

/// The rolling publish gate: `workflow_run` admits only the trusted
/// producer at `success`, dispatches resolve their drill mode immediately,
/// and anything else fails closed. A dispatch rehearsal finishes here
/// without waiting for default-branch CI — the gate resolves, never polls,
/// so no wait loop can strand a feature branch.
fn render_binding_gate_job(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let setup = workflow_runtime_setup_for_config(config);
    format!(
        "  publish-gate:\n    name: Admit rolling publish\n    needs: source\n    runs-on: {}\n    timeout-minutes: 10\n    outputs:\n      admitted: ${{{{ steps.admit.outputs.admitted }}}}\n      mode: ${{{{ steps.admit.outputs.mode }}}}\n      sha: ${{{{ needs.source.outputs.sha }}}}\n    steps:\n{setup}      - name: Admit producer or resolve drill mode\n        id: admit\n        env:\n          EVENT: ${{{{ github.event_name }}}}\n          REF: ${{{{ github.ref }}}}\n          PRODUCER: ${{{{ github.event.workflow_run.name }}}}\n          CONCLUSION: ${{{{ github.event.workflow_run.conclusion }}}}\n          MODE_INPUT: ${{{{ github.event_name == 'workflow_dispatch' && inputs.mode || '' }}}}\n          EXPECTED: {}\n          BRANCH: {}\n        run: |\n          set -euo pipefail\n          if [ \"$EVENT\" = \"workflow_run\" ]; then\n            velnor-workflow release admit-producer --producer \"$PRODUCER\" --expected \"$EXPECTED\" --conclusion \"$CONCLUSION\"\n            mode=publish\n          elif [ \"$EVENT\" = \"workflow_dispatch\" ]; then\n            mode=\"$(velnor-workflow release resolve-mode --event \"$EVENT\" --input \"${{MODE_INPUT:-validate}}\")\"\n          elif [ \"$EVENT\" = \"push\" ]; then\n            mode=\"$(velnor-workflow release resolve-mode --event push --ref \"$REF\" --rolling true --branch \"$BRANCH\")\"\n          else\n            echo \"::error::unsupported rolling event '$EVENT'\" >&2\n            exit 1\n          fi\n          {{\n            echo \"admitted=true\"\n            echo \"mode=$mode\"\n          }} >> \"$GITHUB_OUTPUT\"\n",
        selected_runner(config),
        yaml_scalar(&release.producer_workflow),
        yaml_scalar(&config.default_branch),
    )
}

/// Prepend the source-resolution and publish-gate jobs. No-op without a
/// bound producer.
fn inject_binding_jobs(output: &str, config: &ProjectConfig, release: &ReleaseSpec) -> String {
    if !has_producer_binding(release) {
        return output.to_owned();
    }
    let jobs = format!(
        "{}{}",
        render_binding_source_job(config),
        render_binding_gate_job(config, release)
    );
    output.replacen("jobs:\n", &format!("jobs:\n{jobs}"), 1)
}

/// Sanitize a credential name into a shell function suffix.
fn credential_function(name: &str) -> String {
    let mut sanitized: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        sanitized.push_str("credential");
    }
    format!("teardown_{sanitized}")
}

/// Indent a multi-line repository command into a `run:` block.
fn indent_command(command: &str) -> String {
    let mut indented = String::new();
    for line in command.lines() {
        indented.push_str("            ");
        indented.push_str(line);
        indented.push('\n');
    }
    indented
}

/// The credential setup steps: each setup runs with its teardown trapped
/// first, so a partially materialized credential is cleaned inside the
/// step. Teardown commands must be idempotent: the lane runs them again
/// before supply-chain sidecars and once more under `if: always()`.
fn render_credential_setup_steps(release: &ReleaseSpec) -> String {
    let mut steps = String::new();
    for credential in &release.credentials {
        let function = credential_function(&credential.name);
        let _ = writeln!(
            steps,
            "      - name: Mount {} credential\n        run: |\n          set -euo pipefail\n          {function}() {{\n{}          }}\n          trap {function} EXIT\n{}",
            yaml_scalar(&credential.name),
            indent_command(&credential.teardown),
            indent_command(&credential.setup),
        );
    }
    steps
}

/// The explicit teardown call before supply-chain sidecars: secrets leave
/// the host before attestation, manifests, and uploads run.
fn render_credential_unmount_steps(release: &ReleaseSpec) -> String {
    let mut steps = String::new();
    for credential in &release.credentials {
        let _ = writeln!(
            steps,
            "      - name: Unmount {} credential before supply-chain sidecars\n        run: |\n          set -euo pipefail\n{}",
            yaml_scalar(&credential.name),
            indent_command(&credential.teardown),
        );
    }
    steps
}

/// The final teardown under `if: always()`: success, failure, cancellation,
/// and timeout all restore host state, including paths where the explicit
/// unmount step never ran.
fn render_credential_restore_steps(release: &ReleaseSpec) -> String {
    let mut steps = String::new();
    for credential in &release.credentials {
        let _ = writeln!(
            steps,
            "      - name: Restore {} credential state\n        if: always()\n        run: |\n          set -euo pipefail\n{}",
            yaml_scalar(&credential.name),
            indent_command(&credential.teardown),
        );
    }
    steps
}

/// Inject the credential pairing into a tarball build section: setup before
/// the target install, explicit unmount before attestation, and the
/// `always()` restore after the artifact upload. Anchors are the build
/// job's own step names; no-op without declared credentials.
fn inject_credential_pairing(output: &str, release: &ReleaseSpec, attest: &str) -> String {
    if release.credentials.is_empty() {
        return output.to_owned();
    }
    let output = output.replacen(
        "      - name: Add Rust target\n",
        &format!(
            "{}      - name: Add Rust target\n",
            render_credential_setup_steps(release)
        ),
        1,
    );
    let output = output.replacen(
        attest,
        &format!("{}{attest}", render_credential_unmount_steps(release)),
        1,
    );
    // The `always()` restore follows the build upload: the upload is the
    // only step in the section carrying a retention, so its line ends the
    // upload block.
    if let Some(position) = output.find("          retention-days: ") {
        let end = output[position..]
            .find('\n')
            .map_or(output.len(), |offset| position + offset + 1);
        let mut patched = output.clone();
        patched.insert_str(end, &render_credential_restore_steps(release));
        patched
    } else {
        output
    }
}

/// The deterministic flag selection for one archive row: GNU tar lanes
/// package reproducibly, Apple lanes keep their platform tar. Rendered as
/// shell over the matrix target because one job covers both runners.
fn archive_deterministic_selection() -> &'static str {
    "          case \"${{ matrix.target }}\" in\n            *-apple-darwin) deterministic=false ;;\n            *) deterministic=true ;;\n          esac\n"
}

/// The extra `package-binary` flags for a declared archive contract.
fn archive_package_flags(release: &ReleaseSpec) -> String {
    if !has_archive_contract(release) {
        return String::new();
    }
    format!(
        " --members {} --deterministic \"$deterministic\"",
        shell_quote(&release.archive_members.join(",")),
    )
}

/// Override the tarball upload retention with the declared window. Only the
/// build upload — the first retention in the file — is the release
/// archive lane; intermediate digests keep their own windows.
fn inject_archive_retention(output: &str, release: &ReleaseSpec) -> String {
    if release.archive_retention_days == 0 {
        return output.to_owned();
    }
    if let Some(position) = output.find("          retention-days: ") {
        let end = output[position..]
            .find('\n')
            .map_or(output.len(), |offset| position + offset);
        let mut patched = output.to_owned();
        patched.replace_range(
            position..end,
            &format!(
                "          retention-days: {}",
                release.archive_retention_days
            ),
        );
        patched
    } else {
        output.to_owned()
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
    let Some(toolchain) = crate::s2::config_rust_toolchain(config) else {
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
        for (provider, runner) in release_providers(config, target) {
            let _ = writeln!(
                matrix,
                "          - target: {}\n            provider: {provider}\n            runner: {runner}",
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
        "        with:\\n          persist-credentials: false\\n      - name: Set up sccache",
        &format!(
            "        with:\n{POLICY_CHECKOUT_WITH}{}{toolchain_steps}      - name: Set up sccache",
            policy_enforcement_step()
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
            // The rolling lane has no verify job to resolve the drill, so a
            // declared rehearse reads the dispatch input directly: the arm
            // can only enable a drill build, never the push-gated publish.
            if has_release_modes(release) {
                format!(
                    "{} || (github.event_name == 'workflow_dispatch' && inputs.mode == 'rehearse')",
                    trusted_release_runner_gate(&config.default_branch)
                )
            } else {
                trusted_release_runner_gate(&config.default_branch)
            }
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
        "name: ${{ matrix.provider }}-${{ matrix.target }}",
    )
    .replace(
        "          path: dist\n          merge-multiple: true",
        &format!(
            "          path: dist\n          pattern: {}-*\n          merge-multiple: true",
            canonical_provider(config)
        ),
    )
    .replace("run: cargo build ", "run: mbx build ");
    // The tarball lane has no identity job, so its guest payload keeps the
    // unbound shape; the native preview lane renders its own guest below.
    if let Some(guest) = render_guest_payload_job(config, release, false) {
        output = output.replace("jobs:\n  build:", &format!("jobs:\n{guest}\n  build:"));
    }
    inject_tarball_preview_bindings(&output, config, release)
}

/// The tarball preview bindings: trigger bindings with the source and gate
/// jobs, the build (and guest) checkouts pinned to the resolved source, the
/// rolling publish admitted by the gate, declared archive packaging with
/// its manifest assembly, and the credential pairing.
fn inject_tarball_preview_bindings(
    output: &str,
    config: &ProjectConfig,
    release: &ReleaseSpec,
) -> String {
    let mut output = inject_preview_triggers(output, config, release);
    output = inject_binding_jobs(&output, config, release);
    if has_producer_binding(release) {
        output = output.replacen(
            "  build:\n    name: Preview / ${{ matrix.target }}\n",
            "  build:\n    name: Preview / ${{ matrix.target }}\n    needs: [source]\n",
            1,
        );
        output = output.replacen(
            "        with:\n          fetch-depth: 0\n          persist-credentials: false\n",
            "        with:\n          ref: ${{ needs.source.outputs.sha }}\n          fetch-depth: 0\n          persist-credentials: false\n",
            1,
        );
        if output.contains("  guest-payload:\n") {
            output = output.replacen(
                "  guest-payload:\n    name: Guest payload ${{ matrix.arch }}\n    runs-on:",
                "  guest-payload:\n    name: Guest payload ${{ matrix.arch }}\n    needs: [source]\n    runs-on:",
                1,
            );
            output = output.replacen(
                "        with:\n          persist-credentials: false\n",
                "        with:\n          ref: ${{ needs.source.outputs.sha }}\n          persist-credentials: false\n",
                1,
            );
        }
        let legacy_publish = format!(
            "    needs: build\n    if: ${{{{ github.event_name == 'push' && github.ref == 'refs/heads/{}' }}}}\n",
            config.default_branch,
        );
        output = output.replacen(
            &legacy_publish,
            "    needs: [build, publish-gate]\n    if: ${{ needs.publish-gate.outputs.admitted == 'true' && needs.publish-gate.outputs.mode == 'publish' }}\n",
            1,
        );
    }
    if has_archive_contract(release) {
        output = output.replacen(
            "        run: velnor-workflow release package-binary",
            &format!(
                "        run: |\n          set -euo pipefail\n{}          velnor-workflow release package-binary",
                archive_deterministic_selection()
            ),
            1,
        );
        let package_tail = format!(" --binary {}\n", yaml_scalar(&release.binary));
        output = output.replacen(
            &package_tail,
            &format!(
                " --binary {}{}\n",
                yaml_scalar(&release.binary),
                archive_package_flags(release)
            ),
            1,
        );
        if !release.manifest_schema.is_empty() {
            let subjects = release
                .targets
                .iter()
                .map(|target| format!("{}-preview-{target}.tar.gz", release.binary))
                .collect::<Vec<_>>()
                .join(",");
            let commit = if has_producer_binding(release) {
                "${{ needs.publish-gate.outputs.sha }}"
            } else {
                "${{ github.sha }}"
            };
            output = output.replacen(
                "      - name: Replace rolling preview\n",
                &format!(
                    "{}      - name: Assemble release manifest\n        run: |\n          set -euo pipefail\n          velnor-workflow release assemble-manifest --dir dist --subjects \"{subjects}\" --schema {} --repository \"${{{{ github.repository }}}}\" --ref \"${{{{ github.ref }}}}\" --commit \"{commit}\" --version preview\n      - name: Replace rolling preview\n",
                    workflow_runtime_setup_for_config(config),
                    shell_quote(&release.manifest_schema),
                ),
                1,
            );
        }
    }
    output = inject_credential_pairing(&output, release, "      - name: Attest preview artifact\n");
    inject_archive_retention(&output, release)
}

/// The providers a versioned-tool dispatch offers: exactly the
/// repository's providers in canonical order, plus `all` when the
/// repository has more than one.
fn versioned_tool_providers(config: &ProjectConfig) -> Vec<&'static str> {
    let mut providers = config
        .providers
        .iter()
        .map(ProviderId::as_str)
        .collect::<Vec<_>>();
    if providers.len() > 1 {
        providers.push("all");
    }
    providers
}

/// Render `value` as a JSON string: the runner-configs the version job emits
/// travel through bash into `fromJSON`, so every label is escaped.
fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other if other.is_control() => {
                let _ = write!(escaped, "\\u{:04x}", other as u32);
            }
            other => escaped.push(other),
        }
    }
    escaped.push('"');
    escaped
}

/// The `runs-on` JSON value for one provider: a single label renders as
/// a string, several as an array — the same value `runs-on` takes.
fn versioned_tool_runner_json(config: &ProjectConfig, provider: ProviderId) -> String {
    let labels = runs_on_for(&config.selectors, provider)
        .map(|labels| {
            labels
                .iter()
                .map(|label| json_string(label))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    match labels.as_slice() {
        [single] => single.clone(),
        labels => format!("[{}]", labels.join(",")),
    }
}

/// The runner-configs array for one `providers` input value: one
/// `{provider, runner}` object per provider the build matrix fans out to.
fn versioned_tool_provider_configs(config: &ProjectConfig, providers: &str) -> String {
    let selected: Vec<ProviderId> = if providers == "all" {
        config.providers.iter().copied().collect()
    } else {
        ProviderId::parse(providers).into_iter().collect()
    };
    // Exactly one provider uploads: the first local provider when one runs,
    // else the single provider. Every cell still builds (provider
    // redundancy validates the compile), but only the writer attests and
    // uploads, so two providers never publish the same tarball name twice.
    let writer = selected
        .iter()
        .find(|provider| provider.is_local())
        .or(selected.first())
        .copied();
    let configs = selected
        .iter()
        .map(|provider| {
            format!(
                "{{\"provider\":\"{}\",\"runner\":{},\"writer\":{}}}",
                provider.as_str(),
                versioned_tool_runner_json(config, *provider),
                Some(*provider) == writer,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{configs}]")
}

/// Pinned Mise provisioning for a versioned-tool job: hosted providers
/// install through the pinned action, while a local-only universe uses its
/// preinstalled binary. Mirrors the scheduled-check tool steps; these jobs
/// run named tasks and nothing else product-shaped.
fn render_versioned_tool_mise_setup(config: &ProjectConfig) -> String {
    if !config.providers.contains(&ProviderId::GithubHosted) {
        return String::new();
    }
    render_mise_setup()
}

/// Pinned Mise provisioning for one typed release job. The job's selected
/// runner owns this decision: GitHub-hosted Linux and macOS lanes need the
/// setup action, while Velnor lanes use the preinstalled binary.
fn render_mise_setup_for_runner(runner: &str) -> String {
    if matches!(runner, "github" | "macos") {
        render_mise_setup()
    } else {
        String::new()
    }
}

fn render_mise_setup() -> String {
    format!(
        "      - name: Set up Mise\n        uses: {}\n        with:\n          install: false\n",
        ActionPin::Mise.reference()
    )
}

/// One `mise run` step per named task, exactly the scheduled-check shape,
/// with an optional job-expression gate on every step.
fn render_versioned_tool_task_steps(tasks: &[String], gate: Option<&str>) -> String {
    let mut steps = String::new();
    for task in tasks {
        if let Some(gate) = gate {
            let _ = writeln!(
                steps,
                "      - name: Run {task}\n        if: {gate}\n        run: mise run {task}"
            );
        } else {
            let _ = writeln!(
                steps,
                "      - name: Run {task}\n        run: mise run {task}"
            );
        }
    }
    steps
}

/// The runner selector for one typed tasks-release job. macOS is a fixed
/// hosted lane in schema 2; the other aliases resolve through the explicit
/// provider selectors that config validation requires.
fn tasks_job_runs_on(config: &ProjectConfig, job: &ReleaseJobSpec) -> String {
    match job.runner.as_str() {
        "macos" => yaml_scalar(MACOS_HOSTED_RUNS_ON),
        "velnor" => config
            .selectors
            .get(&ProviderId::Velnor)
            .map(selector_runs_on_yaml)
            .unwrap_or_default(),
        _ => hosted_selector_runs_on(config),
    }
}

/// Dispatch drills run `validate` jobs; tag pushes run `publish` jobs. Jobs
/// declaring both modes or neither are useful on both event classes.
fn tasks_job_gate(job: &ReleaseJobSpec) -> Option<&'static str> {
    let validate = job.modes.iter().any(|mode| mode == "validate");
    let publish = job.modes.iter().any(|mode| mode == "publish");
    match (validate, publish) {
        (true, false) => Some("${{ github.event_name == 'workflow_dispatch' }}"),
        (false, true) => Some("${{ github.event_name != 'workflow_dispatch' }}"),
        _ => None,
    }
}

/// Render one typed tasks-release job. Product build/sign/publish behavior
/// stays in named mise tasks; Velnor owns only the job graph, event gates,
/// runner routing, and protected execution metadata.
fn render_tasks_release_job(config: &ProjectConfig, job: &ReleaseJobSpec) -> String {
    let mut output = format!(
        "  {}:\n    name: {}\n",
        yaml_scalar(&job.id),
        yaml_scalar(&job.name)
    );
    if !job.needs.is_empty() {
        let needs = job
            .needs
            .iter()
            .map(|dependency| yaml_scalar(dependency))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(output, "    needs: [{needs}]");
    }
    if let Some(gate) = tasks_job_gate(job) {
        let _ = writeln!(output, "    if: {gate}");
    }
    let _ = writeln!(output, "    runs-on: {}", tasks_job_runs_on(config, job));
    let _ = writeln!(output, "    timeout-minutes: {}", job.timeout_minutes);
    if !job.environment.is_empty() {
        let _ = writeln!(output, "    environment: {}", yaml_scalar(&job.environment));
    }
    if !job.permissions.is_empty() || !job.attest_subjects.is_empty() {
        // A job-level permissions map replaces the workflow-level map in
        // GitHub Actions. Preserve checkout access when a release author
        // asks for an additional scope, while config validation rejects an
        // explicit `contents: none` override. Attestation subjects also
        // require the OIDC and attestations scopes at this job boundary.
        let mut permissions = job.permissions.clone();
        permissions
            .entry("contents".to_owned())
            .or_insert_with(|| "read".to_owned());
        if !job.attest_subjects.is_empty() {
            permissions
                .entry("id-token".to_owned())
                .or_insert_with(|| "write".to_owned());
            permissions
                .entry("attestations".to_owned())
                .or_insert_with(|| "write".to_owned());
        }
        output.push_str("    permissions:\n");
        for (scope, level) in &permissions {
            let _ = writeln!(output, "      {scope}: {level}");
        }
    }
    if !job.env.is_empty() {
        output.push_str("    env:\n");
        for (key, value) in &job.env {
            let _ = writeln!(output, "      {key}: {}", yaml_scalar(value));
        }
    }
    let _ = writeln!(
        output,
        "    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n{}",
        ActionPin::Checkout.reference(),
        render_mise_setup_for_runner(&job.runner),
    );
    output.push_str(&render_versioned_tool_task_steps(&job.tasks, None));
    if !job.attest_subjects.is_empty() {
        let mut subjects = String::new();
        for subject in &job.attest_subjects {
            let _ = writeln!(subjects, "            {subject}");
        }
        let _ = writeln!(
            output,
            "      - name: Attest release artifacts\n        uses: {}\n        with:\n          subject-path: |\n{subjects}",
            ActionPin::Attest.reference(),
        );
    }
    output
}

/// The schema-2 `tasks` publisher: tag pushes plus dispatch drills over
/// repository-owned named tasks. Publication remains tag-triggered; job mode
/// gates keep validation/signing tasks distinct on dispatch versus tag runs.
fn render_tasks_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let trigger = format!("{}  workflow_dispatch:\n", release_trigger_block(release));
    let mut output = format!(
        "{GENERATED_HEADER}name: Release\nrun-name: Release · ${{{{ github.ref_name }}}}\n\n{trigger}\nconcurrency:\n  group: release-${{{{ github.ref }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n"
    );
    output = inject_dispatch_modes(&output, release);
    for job in &release.jobs {
        output.push_str(&render_tasks_release_job(config, job));
    }
    output
}

/// The PR-only version gate: fails the pull request when the product's own
/// version task reports artifact inputs changed without a version bump.
///
/// Selection-vs-enforcement split: `version_bump_matches` in `runtime.rs`
/// stays the single cargo-specific classifier for *local-run selection*
/// (the `workflow.version_bump_units` allowlist); it is deliberately NOT
/// ported into this YAML. The CI gate is *enforcement* via the product's
/// named task — generic code classifies nothing here, so no second
/// classifier exists.
fn render_version_gate_job(config: &ProjectConfig, spec: &VersionedToolSpec) -> String {
    format!(
        "  validate-version:\n    name: Validate version bump\n    if: ${{{{ github.event_name == 'pull_request' }}}}\n    runs-on: {}\n    timeout-minutes: 15\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n{}{}",
        selected_runner(config),
        ActionPin::Checkout.reference(),
        render_versioned_tool_mise_setup(config),
        render_versioned_tool_task_steps(&spec.version_gate_tasks, None),
    )
}

/// The version job: one manifest version plus the runner-configs the build
/// matrix fans out to, resolved from the dispatch providers input.
fn render_versioned_tool_version_job(config: &ProjectConfig, spec: &VersionedToolSpec) -> String {
    let mut provider_arms = String::new();
    for provider in versioned_tool_providers(config) {
        let configs = shell_quote(&versioned_tool_provider_configs(config, provider));
        let _ = writeln!(provider_arms, "          {provider}) configs={configs} ;;");
    }
    format!(
        "  version:\n    name: Resolve tool version\n    runs-on: {}\n    timeout-minutes: 10\n    outputs:\n      version: ${{{{ steps.resolve.outputs.version }}}}\n      runner-configs: ${{{{ steps.resolve.outputs.runner-configs }}}}\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n{}{}      - name: Resolve the tool version from the manifest\n        id: resolve\n        env:\n          PROVIDERS: ${{{{ github.event_name == 'workflow_dispatch' && inputs.providers || '{}' }}}}\n          MANIFEST: {}\n        run: |\n          set -euo pipefail\n          version=\"$(sed -n 's/^version = \"\\(.*\\)\"/\\1/p' \"$MANIFEST\" | head -n1)\"\n          case \"$version\" in\n            ''|*[!0-9.]*) echo \"::error::tool version $version is not an X.Y.Z version\" >&2; exit 1 ;;\n          esac\n          [[ \"$version\" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+$ ]] \\\n            || {{ echo \"::error::tool version $version is not an X.Y.Z version\" >&2; exit 1; }}\n          case \"$PROVIDERS\" in\n{}            *) echo \"::error::unknown providers '$PROVIDERS'\" >&2; exit 1 ;;\n          esac\n          {{\n            echo \"version=$version\"\n            echo \"runner-configs=$configs\"\n          }} >> \"$GITHUB_OUTPUT\"\n",
        selected_runner(config),
        ActionPin::Checkout.reference(),
        workflow_runtime_setup_for_config(config),
        policy_enforcement_step(),
        canonical_provider(config),
        yaml_scalar(&spec.version_manifest),
        provider_arms,
    )
}

/// The assert job: on the default branch the published-reuse check reports
/// whether the version already shipped, and the declared assert tasks run
/// their product checks (formula and the like); off the branch the job
/// reports unpublished without touching the release API.
fn render_versioned_tool_assert_job(config: &ProjectConfig, spec: &VersionedToolSpec) -> String {
    let mut steps = format!(
        "      - name: Check whether the version is already published\n        id: published\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n          VERSION: ${{{{ needs.version.outputs.version }}}}\n          REF: ${{{{ github.ref }}}}\n        run: |\n          set -euo pipefail\n          tag=\"{}$VERSION\"\n          if [ \"$REF\" = \"refs/heads/{}\" ] && gh release view \"$tag\" --json tagName --jq .tagName >/dev/null 2>&1; then\n            echo \"published=true\" >> \"$GITHUB_OUTPUT\"\n          else\n            echo \"published=false\" >> \"$GITHUB_OUTPUT\"\n          fi\n",
        spec.version_prefix, config.default_branch,
    );
    if !spec.assert_tasks.is_empty() {
        let gate = format!(
            "${{{{ github.ref == 'refs/heads/{}' }}}}",
            config.default_branch
        );
        let _ = write!(
            steps,
            "      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n{}{}",
            ActionPin::Checkout.reference(),
            render_versioned_tool_mise_setup(config),
            render_versioned_tool_task_steps(&spec.assert_tasks, Some(&gate)),
        );
    }
    format!(
        "  assert-version:\n    name: Assert the version is unpublished\n    needs: [version]\n    runs-on: {}\n    timeout-minutes: 10\n    outputs:\n      published: ${{{{ steps.published.outputs.published }}}}\n    steps:\n{}",
        selected_runner(config),
        steps,
    )
}

/// The matrix build: declared targets crossed with the version job's provider
/// configs. The declared build tasks own the compile — cross-builders like
/// zigbuild stay named tasks, never generic code — and generic code
/// packages the conventional `target/<triple>/release/<binary>` output,
/// attests it, and uploads it per provider and target.
fn render_versioned_tool_build_job(config: &ProjectConfig, spec: &VersionedToolSpec) -> String {
    let mut targets = String::new();
    for target in &spec.targets {
        let _ = writeln!(targets, "          - {}", yaml_scalar(target));
    }
    let build_steps = format!(
        "{}{}",
        render_versioned_tool_mise_setup(config),
        render_versioned_tool_task_steps(&spec.build_tasks, None),
    );
    format!(
        "  build:\n    name: Build / ${{{{ matrix.target }}}} / ${{{{ matrix.config.provider }}}}\n    needs: [version, assert-version]\n    if: ${{{{ github.event_name != 'pull_request' && needs.assert-version.outputs.published != 'true' }}}}\n    runs-on: ${{{{ matrix.config.runner }}}}\n    timeout-minutes: 90\n    strategy:\n      fail-fast: false\n      matrix:\n        target:\n{}        config: ${{{{ fromJSON(needs.version.outputs.runner-configs) }}}}\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    env:\n      VERSION: ${{{{ needs.version.outputs.version }}}}\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n{}{}      - name: Package tool binary\n        run: |\n          set -euo pipefail\n          velnor-workflow release package-binary --target \"${{{{ matrix.target }}}}\" --version \"$VERSION\" --package {} --binary {}\n      - name: Attest tool artifact\n        if: ${{{{ matrix.config.writer }}}}\n        uses: {}\n        with:\n          subject-path: dist/*.tar.gz\n      - name: Upload tool artifact\n        if: ${{{{ matrix.config.writer }}}}\n        uses: {}\n        with:\n          name: ${{{{ matrix.config.provider }}}}-${{{{ matrix.target }}}}\n          path: dist/*\n          if-no-files-found: error\n          retention-days: 2\n",
        targets,
        ActionPin::Checkout.reference(),
        workflow_runtime_setup_for_config(config),
        build_steps,
        yaml_scalar(&spec.package),
        yaml_scalar(&spec.binary),
        ActionPin::Attest.reference(),
        ActionPin::UploadArtifact.reference(),
    )
}

/// The mutexed immutable publish: downloads every provider's archives,
/// re-verifies provenance and checksums, and creates the versioned release
/// once — no clobber, and no tag check, because no tag triggered this flow.
fn render_versioned_tool_publish_job(config: &ProjectConfig, spec: &VersionedToolSpec) -> String {
    format!(
        "  publish:\n    name: Publish immutable tool release\n    needs: [version, assert-version, build]\n    if: ${{{{ github.ref == 'refs/heads/{}' && needs.assert-version.outputs.published != 'true' }}}}\n    runs-on: {}\n    timeout-minutes: 20\n    environment: github-release\n    concurrency:\n      group: {}\n      cancel-in-progress: false\n    permissions:\n      contents: write\n    env:\n      VERSION: ${{{{ needs.version.outputs.version }}}}\n      COMMIT: ${{{{ github.sha }}}}\n    steps:\n      - name: Download tool artifacts\n        uses: {}\n        with:\n          path: dist\n          pattern: \"*-*\"\n          merge-multiple: true\n      - name: Verify artifact provenance\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          for artifact in dist/*.tar.gz; do gh attestation verify \"$artifact\" --repo \"$GITHUB_REPOSITORY\"; done\n      - name: Verify archive checksums\n        run: |\n          set -euo pipefail\n          cd dist\n          for checksum in *.sha256; do sha256sum --check \"$checksum\"; done\n      - name: Publish immutable tool release\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: |\n          set -euo pipefail\n          tag=\"{}$VERSION\"\n          gh release create \"$tag\" dist/* --target \"$COMMIT\" --title \"$tag\" --generate-notes\n",
        config.default_branch,
        selected_runner(config),
        yaml_scalar(&spec.publish_group),
        ActionPin::DownloadArtifact.reference(),
        spec.version_prefix,
    )
}

/// The main-branch-driven versioned-tool publisher: the row's own workflow
/// name, push-main plus pull-request triggers over declared paths, a
/// providers dispatch, per-ref concurrency that cancels PR runs only, and
/// the five-job version graph — gate, version, assert, matrix build, and
/// the mutexed immutable publish. No tag checks render: no tag triggers
/// this file, so there is nothing to verify.
fn render_versioned_tool_release(config: &ProjectConfig, spec: &VersionedToolSpec) -> String {
    let mut provider_options = String::new();
    for provider in versioned_tool_providers(config) {
        let _ = writeln!(provider_options, "          - {provider}");
    }
    let mut push_paths = String::new();
    for path in &spec.push_paths {
        let _ = writeln!(push_paths, "      - {}", yaml_scalar(path));
    }
    let mut pull_request_paths = String::new();
    for path in &spec.pull_request_paths {
        let _ = writeln!(pull_request_paths, "      - {}", yaml_scalar(path));
    }
    format!(
        "{GENERATED_HEADER}name: {}\nrun-name: {}\n\non:\n  push:\n    branches: [{}]\n    paths:\n{}  pull_request:\n    paths:\n{}  workflow_dispatch:\n    inputs:\n      providers:\n        description: Build providers (all builds every provider)\n        required: false\n        default: {}\n        type: choice\n        options:\n{}concurrency:\n  group: ${{{{ github.workflow }}}}-${{{{ github.ref }}}}\n  cancel-in-progress: ${{{{ github.event_name == 'pull_request' }}}}\n\npermissions:\n  contents: read\n\njobs:\n{}{}{}{}{}",
        yaml_scalar(&spec.name),
        yaml_scalar(&format!(
            "{} · ${{{{ github.event_name }}}} · ${{{{ github.ref_name }}}}",
            spec.name
        )),
        yaml_scalar(&config.default_branch),
        push_paths,
        pull_request_paths,
        canonical_provider(config),
        provider_options,
        render_version_gate_job(config, spec),
        render_versioned_tool_version_job(config, spec),
        render_versioned_tool_assert_job(config, spec),
        render_versioned_tool_build_job(config, spec),
        render_versioned_tool_publish_job(config, spec),
    )
}

pub(crate) fn render_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    if !release_contract_complete(release) {
        return format!(
            "{GENERATED_HEADER}# Release omitted: artifact, platform, registry, or signer contract is incomplete.\n"
        );
    }
    let bindings_supported = matches!(release.kind.as_str(), "rust-binary" | "native")
        || (release.kind.as_str() == "tasks"
            && !has_producer_binding(release)
            && !has_archive_contract(release)
            && release.credentials.is_empty());
    if has_tarball_bindings(release) && !bindings_supported {
        return format!(
            "{GENERATED_HEADER}# Release omitted: producer bindings, dispatch modes, archive contracts, and credential pairings render only for the `rust-binary` and `native` publishers (`tasks` renders dispatch modes only).\n"
        );
    }
    match release.kind.as_str() {
        "crates" => render_crates_release(config, release),
        "rust-binary" => render_binary_release(config, release),
        "native" => render_native_release(config, release),
        "tasks" => render_tasks_release(config, release),
        "pages" => render_pages_release(config, release),
        "homebrew" => render_homebrew_release(config, release),
        "apt" => render_apt_release(config, release),
        "docker" => render_docker_release(config, release),
        _ => format!(
            "{GENERATED_HEADER}# Release omitted: this publisher requires a separately verified contract.\n"
        ),
    }
}

/// One release unit job for `(provider, unit)`; returns its job id.
fn render_release_unit_job(
    output: &mut String,
    config: &ProjectConfig,
    workflow: &WorkflowIr,
    provider: ProviderId,
    unit: &Unit,
) -> String {
    let id = format!("release-{}-{}", provider.as_str(), unit.id);
    let mut needs = vec!["verify".to_owned()];
    needs.extend(unit.depends_on.iter().filter_map(|dependency| {
        config
            .units
            .iter()
            .find(|unit| unit.id == *dependency)
            .filter(|unit| provider_supports_unit(provider, unit))
            .map(|_| format!("release-{}-{}", provider.as_str(), dependency))
    }));
    let runner = release_unit_runs_on(workflow, provider, unit);
    let job_name = yaml_scalar(&crate::s2::comparison_job_name(provider, unit));
    let verify_name = yaml_scalar(&unit.label);
    let dispatch_gate = if provider.is_local() {
        trusted_release_runner_gate(&config.default_branch)
    } else {
        String::new()
    };
    let dispatch_gate = if dispatch_gate.is_empty() {
        String::new()
    } else {
        format!("    if: ${{{{ {dispatch_gate} }}}}\n")
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
    workflow.render_workflow_runtime_setup(output, provider);
    // The generator's own unit runs `--plain --check` with the network
    // restricted, so its D19 guard needs the pinned policy binary before the
    // first command. Hosted release jobs already acquire the runtime product
    // at the pin (the guard finds it on PATH by closure); local providers
    // run the packaged fleet runtime and provision the pinned product into
    // the host's persistent executable store instead.
    if provider.is_local() && super::ir::unit_runs_workflow_plain_check(unit) {
        output.push_str(&crate::s2::workflow_pinned_policy_runtime_local(
            &workflow.workflow_revision,
            "${{ github.workspace }}",
        ));
    }
    workflow.render_tool_provisioning(output, provider, unit, false);
    let cargo_cache_restored = CacheBackend::Detected
        .provider_enables_actions_cache(provider, workflow, unit)
        && unit.cache.is_some();
    if cargo_cache_restored && let Some(cache) = &unit.cache {
        render_retained_output_cache_note(output, workflow, unit, cache);
        let (paths, key) = rendered_cache_values(cache);
        let _ = writeln!(
            output,
            "      - name: Restore {} cache\n        id: cache\n        uses: {}\n        with:\n          path: |\n{paths}\n          key: ci-release-${{{{ runner.os }}}}-{}-{}-{}-{}-${{{{ hashFiles({key}) }}}}",
            verify_name,
            ActionPin::CacheRestore.reference(),
            provider.as_str(),
            unit.platform.as_str(),
            unit.trust.as_str(),
            unit.id
        );
    }
    let skip_when_offline_ready = provider.is_local()
        && unit
            .cache
            .as_ref()
            .is_some_and(super::cache::cache_is_local_host_persistent);
    render_cargo_source_preparation(
        output,
        &[unit],
        &unit.id,
        false,
        cargo_cache_restored,
        skip_when_offline_ready,
    );
    let cargo_offline = checks_env(unit);
    let token_env = docker_build_token_env_for_members(&[unit]);
    // `run` requires the planned-selection file CI Planning materializes
    // for reusable legs; release legs have no Planning job, so each lane
    // plans its own full selection first (full scope short-circuits the
    // diff, so no history is needed). Without this step every lane fails
    // reading the missing selection file.
    let _ = writeln!(
        output,
        "      - name: Plan full release selection\n        env:\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}\n          VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection\n        run: |\n          set -euo pipefail\n          mkdir -p .velnor-ci-selection\n          velnor-workflow plan --config .github/ci/project.toml\n"
    );
    let _ = writeln!(
        output,
        "      - name: Run {verify_name} checks\n        env:\n          CI_SCOPE: full\n          CI_UNIT_ID: {}\n          EVENT_NAME: ${{{{ github.event_name }}}}\n          HEAD_SHA: ${{{{ github.sha }}}}{cargo_offline}{token_env}\n        run: velnor-workflow run --config .github/ci/project.toml --scope \"$CI_SCOPE\" --unit {}\n",
        yaml_scalar(&unit.id),
        yaml_scalar(&unit.id)
    );
    id
}

/// The `runs-on:` for one release verification job: the provider's
/// selector, except macOS-platform units on hosted, which need the
/// GitHub-owned macOS image.
fn release_unit_runs_on(workflow: &WorkflowIr, provider: ProviderId, unit: &Unit) -> String {
    if provider == ProviderId::GithubHosted
        && unit.platform == crate::s2::provider::Platform::MacosArm64
    {
        return yaml_scalar(MACOS_HOSTED_RUNS_ON);
    }
    workflow.runs_on_yaml(provider)
}

fn render_release_unit_jobs(config: &ProjectConfig) -> (String, Vec<String>) {
    let workflow = WorkflowIr::from_config(config);
    let mut output = String::new();
    let mut job_ids = Vec::new();
    for provider in &config.providers {
        for unit in config
            .units
            .iter()
            .filter(|unit| provider_supports_unit(*provider, unit))
        {
            job_ids.push(render_release_unit_job(
                &mut output,
                config,
                &workflow,
                *provider,
                unit,
            ));
        }
    }
    (output, job_ids)
}

fn render_crates_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = String::from(GENERATED_HEADER);
    output.push_str(&release_trigger_header(release));
    let _ = writeln!(
        output,
        "      - name: Checkout\n        uses: {}\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0\n      - name: Verify tag\n        run: velnor-workflow release verify-tag\n      - name: Run full CI\n        run: velnor-workflow run --config .github/ci/project.toml --scope full\n      - name: Package declared crates\n        run: cargo package --workspace --locked\n\n  publish:\n    name: Publish crates.io packages\n    needs: verify\n    runs-on: ubuntu-24.04\n    timeout-minutes: 45\n    environment: crates.io\n    permissions:\n      contents: read\n      id-token: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Authenticate to crates.io\n        id: auth\n        uses: {}\n      - name: Publish in dependency order\n        env:\n          CARGO_REGISTRY_TOKEN: {}\n        run: |\n          set -euo pipefail\n",
        ActionPin::Checkout.reference(),
        ActionPin::Sccache.reference(),
        ActionPin::Checkout.reference(),
        ActionPin::CratesAuth.reference(),
        github_expression("steps.auth.outputs.token")
    );
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
            "{}{}      - name: Set up sccache\n",
            workflow_runtime_setup_for_config(config),
            policy_enforcement_step()
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

/// The `build` job gate for native releases: `always()` plus explicit
/// result checks, so a tag dispatch can declare the github-hosted
/// bootstrap scope while tag pushes keep the full gate. Hosted lanes
/// are ungated (they run on every event) and required on every event;
/// local lanes are trusted-gated (they skip a tag dispatch) and
/// required off dispatches. A dispatch additionally declares
/// github-hosted-only scope (the admit job rejects anything else
/// loudly) plus an adoptable image index, so a fresh-version dispatch
/// fails closed at the build instead of after an hour of wasted
/// binaries. Without rendered hosted lanes no dispatch scope can
/// satisfy the gate, so a local-only publisher cannot bootstrap past
/// zero qualification. Rehearse drills keep their arm verbatim inside
/// the gate.
fn native_release_build_gate(
    config: &ProjectConfig,
    unit_job_ids: &[String],
    release: &ReleaseSpec,
) -> String {
    fn lane_results(
        providers: &crate::s2::provider::ProviderSet,
        unit_job_ids: &[String],
        local: bool,
    ) -> String {
        let required: Vec<String> = providers
            .iter()
            .filter(|provider| provider.is_local() == local)
            .flat_map(|provider| {
                let prefix = format!("release-{}-", provider.as_str());
                unit_job_ids
                    .iter()
                    .filter(move |id| id.starts_with(&prefix))
                    .map(|id| format!("needs.{id}.result == 'success'"))
            })
            .collect();
        if required.is_empty() {
            "true".to_owned()
        } else {
            required.join(" && ")
        }
    }
    let hosted = lane_results(&config.providers, unit_job_ids, false);
    let local = lane_results(&config.providers, unit_job_ids, true);
    let has_hosted_lanes = hosted != "true";
    let mut clauses = vec![
        "needs.verify.result == 'success'".to_owned(),
        format!("({hosted})"),
        format!("(github.event_name == 'workflow_dispatch' || ({local}))"),
    ];
    if has_hosted_lanes {
        let mut scope = vec![
            "github.ref_type == 'tag'".to_owned(),
            "contains(format(',{0},', github.event.inputs.providers), ',github-hosted,')"
                .to_owned(),
        ];
        for provider in &config.providers {
            if provider.is_local() {
                scope.push(format!(
                    "!contains(format(',{{0}},', github.event.inputs.providers), ',{},')",
                    provider.as_str()
                ));
            }
        }
        clauses.push(format!(
            "(github.event_name != 'workflow_dispatch' || ({}))",
            scope.join(" && ")
        ));
    } else {
        clauses.push("(github.event_name != 'workflow_dispatch')".to_owned());
    }
    clauses.push("(github.event_name != 'workflow_dispatch' || needs.image-admission.outputs.existing == 'true')".to_owned());
    let core = clauses.join(" && ");
    if has_release_modes(release) {
        format!("always() && ((github.event_name == 'workflow_dispatch' && needs.verify.outputs.mode == 'rehearse') || ({core}))")
    } else {
        format!("always() && {core}")
    }
}

fn render_binary_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = String::from(GENERATED_HEADER);
    output.push_str(&release_trigger_header(release));
    let _ = writeln!(
        output,
        "      - name: Checkout\n        uses: {}\n        with:\n          fetch-depth: 0\n          persist-credentials: false\n      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0\n      - name: Verify tag\n        run: velnor-workflow release verify-tag\n      - name: Run full CI\n        run: velnor-workflow run --config .github/ci/project.toml --scope full\n",
        ActionPin::Checkout.reference(),
        ActionPin::Sccache.reference(),
    );
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
            "{}{}      - name: Set up sccache\n",
            workflow_runtime_setup_for_config(config),
            policy_enforcement_step()
        ),
    );
    output.push_str(
        "\n  build:\n    name: Build / ${{ matrix.target }}\n    needs: verify\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n",
    );
    let (unit_jobs, unit_job_ids) = render_release_unit_jobs(config);
    output = output.replace("\n  build:", &format!("\n{unit_jobs}\n  build:"));
    // Native builds additionally wait for image admission: the gate reads
    // its adoptable-index output, and admission is read-only beside the
    // lanes, so no critical-path latency is added.
    let release_needs = if release.kind == "native" {
        std::iter::once("verify".to_owned())
            .chain(std::iter::once("image-admission".to_owned()))
            .chain(unit_job_ids.iter().cloned())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        std::iter::once("verify".to_owned())
            .chain(unit_job_ids.iter().cloned())
            .collect::<Vec<_>>()
            .join(", ")
    };
    output = output.replace(
        "    needs: verify\n",
        &format!("    needs: [{release_needs}]\n"),
    );
    for target in &release.targets {
        for (provider, runner) in release_providers(config, target) {
            let _ = writeln!(
                output,
                "          - target: {}\n            provider: {provider}\n            runner: {runner}",
                yaml_scalar(target),
            );
        }
    }
    let matrix_runner = release_matrix_runner(config, &release.targets);
    let _ = writeln!(
        output,
        "    runs-on: {matrix_runner}\n    timeout-minutes: 90\n    permissions:\n      contents: read\n      id-token: write\n      attestations: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Set up sccache\n        uses: {}\n        with:\n          version: v0.16.0\n      - name: Add Rust target\n        run: rustup target add \"${{{{ matrix.target }}}}\"\n      - name: Build release binary\n        env:\n          CARGO_INCREMENTAL: \"0\"\n          RUSTC_WRAPPER: sccache\n        run: cargo build --locked --release --package {} --bin {} --target \"${{{{ matrix.target }}}}\"\n      - name: Package release binary\n        env:\n          VERSION: ${{{{ github.ref_name }}}}\n        run: |\n          set -euo pipefail\n          velnor-workflow release package-binary --target \"${{{{ matrix.target }}}}\" --version \"${{VERSION#v}}\" --package {} --binary {}\n      - name: Attest release artifact\n        uses: {}\n        with:\n          subject-path: dist/*.tar.gz\n      - name: Upload release artifact\n        uses: {}\n        with:\n          name: ${{{{ matrix.target }}}}\n          path: dist/*\n          if-no-files-found: error\n          retention-days: 2\n\n  publish:\n    name: Control / Publish\n    needs: [verify, build]\n    runs-on: ubuntu-24.04\n    timeout-minutes: 20\n    environment: github-release\n    permissions:\n      contents: write\n    steps:\n      - name: Download release artifacts\n        uses: {}\n        with:\n          path: dist\n          merge-multiple: true\n      - name: Verify archive checksums\n        run: |\n          set -euo pipefail\n          cd dist\n          for checksum in *.sha256; do sha256sum --check \"$checksum\"; done\n      - name: Checkout\n        uses: {publish_checkout}\n        with:\n          persist-credentials: false\n{tag_check}      - name: Publish immutable GitHub release\n        env:\n          GH_TOKEN: ${{{{ github.token }}}}\n        run: gh release create \"${{{{ github.ref_name }}}}\" dist/* --verify-tag --generate-notes\n",
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
        publish_checkout = ActionPin::Checkout.reference(),
        tag_check = TAG_IMMUTABILITY_STEP,
    );
    if let Some((prefix, build)) = output.split_once("\n  build:") {
        let build = build.replace(
            "      - name: Set up sccache\n",
            &format!(
                "{}      - name: Set up sccache\n",
                workflow_runtime_setup_for_config(config)
            ),
        );
        // A declared rehearse drill finishes its declared work on the
        // feature branch it was dispatched from: the trusted gate alone
        // would skip every build off main, green without building. Publish
        // stays tag-gated downstream, so the extra arm can only rehearse.
        // Native releases carry the stricter always()-plus-results gate
        // (declared provider subsets on tag dispatch; full gate on push);
        // other publishers keep the legacy trusted gate verbatim.
        let build_gate = if release.kind == "native" {
            native_release_build_gate(config, &unit_job_ids, release)
        } else if has_release_modes(release) {
            format!(
                "{} || (github.event_name == 'workflow_dispatch' && needs.verify.outputs.mode == 'rehearse')",
                trusted_release_runner_gate(&config.default_branch)
            )
        } else {
            trusted_release_runner_gate(&config.default_branch)
        };
        let build = build.replace(
            &format!("    runs-on: {matrix_runner}\n    timeout-minutes: 90"),
            &format!("    runs-on: {matrix_runner}\n    if: ${{{{ {build_gate} }}}}\n    timeout-minutes: 90"),
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
    let output = output
        .replace(
            "name: ${{ matrix.target }}",
            "name: ${{ matrix.provider }}-${{ matrix.target }}",
        )
        .replace(
            "          path: dist\n          merge-multiple: true",
            &format!(
                "          path: dist\n          pattern: {}-*\n          merge-multiple: true",
                canonical_provider(config)
            ),
        );
    inject_binary_bindings(&output, config, release)
}

/// The stable-publisher bindings: a dispatch trigger carrying the declared
/// drill modes, a mode resolver in the verify job, a publish-only gate on
/// the tag check and the publish job, declared archive packaging with its
/// manifest assembly, and the credential pairing. Every injection is gated
/// on its declared contract; an undeclared contract keeps every byte.
fn inject_binary_bindings(output: &str, config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = output.to_owned();
    if has_release_modes(release) {
        let trigger = release_trigger_block(release);
        output = output.replacen(
            &trigger,
            &format!(
                "{trigger}  workflow_dispatch:\n{}",
                dispatch_mode_input(release)
            ),
            1,
        );
        output = output.replacen(
            "  verify:\n    name: Control / Verify release\n",
            "  verify:\n    name: Control / Verify release\n    outputs:\n      mode: ${{ steps.mode.outputs.mode }}\n",
            1,
        );
        output = output.replacen(
            "      - name: Verify tag\n",
            "      - name: Resolve release mode\n        id: mode\n        env:\n          MODE_INPUT: ${{ github.event_name == 'workflow_dispatch' && inputs.mode || '' }}\n        run: |\n          mode=\"$(velnor-workflow release resolve-mode --event \"${{ github.event_name }}\" --ref \"${{ github.ref }}\" --input \"${MODE_INPUT:-validate}\")\"\n          echo \"mode=$mode\" >> \"$GITHUB_OUTPUT\"\n      - name: Verify tag\n        if: ${{ steps.mode.outputs.mode == 'publish' }}\n",
            1,
        );
        output = output.replacen(
            "  publish:\n    name: Control / Publish\n    needs: [verify, build]\n",
            "  publish:\n    name: Control / Publish\n    needs: [verify, build]\n    if: ${{ needs.verify.outputs.mode == 'publish' }}\n",
            1,
        );
    }
    if has_archive_contract(release) {
        output = output.replacen(
            "          velnor-workflow release package-binary",
            &format!(
                "{}          velnor-workflow release package-binary",
                archive_deterministic_selection()
            ),
            1,
        );
        let package_tail = format!(" --binary {}\n", yaml_scalar(&release.binary));
        output = output.replacen(
            &package_tail,
            &format!(
                " --binary {}{}\n",
                yaml_scalar(&release.binary),
                archive_package_flags(release)
            ),
            1,
        );
        if !release.manifest_schema.is_empty() {
            let subjects = release
                .targets
                .iter()
                .map(|target| format!("{}-${{VERSION#v}}-{target}.tar.gz", release.binary))
                .collect::<Vec<_>>()
                .join(",");
            output = output.replacen(
                "      - name: Publish immutable GitHub release\n",
                &format!(
                    "{}      - name: Assemble release manifest\n        env:\n          VERSION: ${{{{ github.ref_name }}}}\n        run: |\n          set -euo pipefail\n          velnor-workflow release assemble-manifest --dir dist --subjects \"{subjects}\" --schema {} --repository \"${{{{ github.repository }}}}\" --ref \"${{{{ github.ref }}}}\" --commit \"${{{{ github.sha }}}}\" --version \"${{VERSION#v}}\"\n      - name: Publish immutable GitHub release\n",
                    workflow_runtime_setup_for_config(config),
                    shell_quote(&release.manifest_schema),
                ),
                1,
            );
        }
    }
    output = inject_credential_pairing(&output, release, "      - name: Attest release artifact\n");
    inject_archive_retention(&output, release)
}

#[allow(clippy::format_push_string)]
/// One resolved release version for the whole file: the tag without its
/// `v` prefix, which is also the container tag the estate consumes.
fn inject_native_verify_outputs(
    output: &str,
    config: &ProjectConfig,
    release: &ReleaseSpec,
) -> String {
    // With declared modes the verify job already carries a mode output
    // block; the version merges into it instead of opening a second block.
    let output = if has_release_modes(release) {
        output.replacen(
            "    outputs:\n",
            "    outputs:\n      version: ${{ steps.version.outputs.version }}\n",
            1,
        )
    } else {
        output.replace(
            "  verify:\n    name: Control / Verify release\n",
            "  verify:\n    name: Control / Verify release\n    outputs:\n      version: ${{ steps.version.outputs.version }}\n",
        )
    };
    let verify_tag_run = format!(
        "run: velnor-workflow release verify-tag --branch {} --package {}\n",
        shell_quote(&config.default_branch),
        shell_quote(&release.package)
    );
    let version_gate = if has_release_modes(release) {
        "        if: ${{ steps.mode.outputs.mode == 'publish' }}\n"
    } else {
        ""
    };
    output.replace(
        &verify_tag_run,
        &format!(
            "{verify_tag_run}      - name: Resolve release version\n        id: version\n{version_gate}        env:\n          TAG: ${{{{ github.ref_name }}}}\n        run: |\n          set -euo pipefail\n          case \"$TAG\" in\n            v[0-9]*) ;;\n            *) echo \"::error::release tag $TAG must match v[0-9]*\" >&2; exit 1 ;;\n          esac\n          version=\"${{TAG#v}}\"\n          case \"$version\" in\n            ''|*['/ ']*) echo \"::error::release version is not portable: $version\" >&2; exit 1 ;;\n          esac\n          echo \"version=$version\" >> \"$GITHUB_OUTPUT\"\n"
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
            "  image:\n    name: Publish container image\n    needs: [admit-provider, verify, build]\n    runs-on: {}\n    timeout-minutes: 120\n    permissions:\n      contents: read\n      packages: write\n      id-token: write\n      attestations: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Log in to GHCR\n        uses: {}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ github.token }}}}\n      - name: Build and push image\n        uses: {}\n        with:\n          context: .\n          push: true\n          tags: {image_tag}\n          provenance: true\n          sbom: true\n",
            hosted_selector_runs_on(config),
            ActionPin::Checkout.reference(),
            ActionPin::DockerLogin.reference(),
            ActionPin::DockerBuild.reference(),
        )
    }
}

/// The `docker` publisher's platform rows: `(arch, platform)` per declared
/// platform, defaulting to both Linux architectures when the contract
/// declares none. Rows dedupe by arch so one platform can never gain two
/// authorized publishers. Unknown platforms fail closed at contract
/// validation, so this mapping is total over complete contracts.
fn docker_platform_arches(release: &ReleaseSpec) -> Vec<(&'static str, String)> {
    let platforms: Vec<String> = if release.platforms.is_empty() {
        crate::s2::config::DOCKER_PLATFORMS
            .iter()
            .map(|platform| (*platform).to_owned())
            .collect()
    } else {
        release.platforms.clone()
    };
    let mut arches = Vec::new();
    for platform in &platforms {
        let arch = match platform.as_str() {
            "linux/amd64" => "amd64",
            "linux/arm64" => "arm64",
            _ => continue,
        };
        if !arches.iter().any(|(known, _)| *known == arch) {
            arches.push((arch, platform.clone()));
        }
    }
    arches
}

/// The consumer-owned Dockerfile the `docker` publisher builds.
fn docker_dockerfile(release: &ReleaseSpec) -> &str {
    if release.dockerfile.is_empty() {
        "Dockerfile"
    } else {
        &release.dockerfile
    }
}

/// The build context the `docker` publisher builds from.
fn docker_context(release: &ReleaseSpec) -> &str {
    if release.context.is_empty() {
        "."
    } else {
        &release.context
    }
}

/// The GHA cache scope stem for one image: the lowercased image slug, so
/// repositories publishing several images never share a `BuildKit` cache
/// scope across images. The matrix arch suffixes it per platform row.
fn docker_cache_scope(image: &str) -> String {
    let slug: String = image
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "image".to_owned()
    } else {
        slug.to_owned()
    }
}

/// The multi-arch platform matrix: one native builder per declared
/// platform. Arm64 builders run on the hosted arm64 label; anything else
/// would silently emulate under QEMU, so there is no fallback.
fn docker_platform_matrix(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut matrix = String::new();
    for (arch, platform) in docker_platform_arches(release) {
        let runner = match arch {
            "amd64" => hosted_selector_runs_on(config),
            // GitHub's hosted arm64 label; the release contract names it and
            // no second hosted label exists to configure.
            _ => "ubuntu-24.04-arm".to_owned(),
        };
        let _ = writeln!(
            matrix,
            "          - arch: {arch}\n            platform: {platform}\n            runner: {runner}"
        );
    }
    matrix
}

/// The source URL stamped into the `docker` publisher's OCI labels: the
/// repository's own address on github.com, never a declared guess.
fn docker_source_url(config: &ProjectConfig) -> String {
    format!("https://github.com/{}", config.repository)
}

/// The registry login step every docker job runs: the GHCR automatic-token
/// login by default, or the declared registry with its credential secret
/// names. Credentials travel by reference; secret values never appear as
/// workflow text.
fn docker_login_step(release: &ReleaseSpec) -> String {
    let login = ActionPin::DockerLogin.reference();
    if release.registry.is_empty() {
        format!(
            "      - name: Log in to GHCR\n        uses: {login}\n        with:\n          registry: ghcr.io\n          username: ${{{{ github.actor }}}}\n          password: ${{{{ secrets.GITHUB_TOKEN }}}}\n"
        )
    } else {
        format!(
            "      - name: Log in to {}\n        uses: {login}\n        with:\n          registry: {}\n          username: ${{{{ secrets.{} }}}}\n          password: ${{{{ secrets.{} }}}}\n",
            release.registry,
            yaml_scalar(&release.registry),
            release.registry_username_secret,
            release.registry_password_secret,
        )
    }
}

fn render_docker_admit_job(config: &ProjectConfig) -> String {
    format!(
        "  admit-provider:\n    name: Control / Admit release\n    runs-on: {runner}\n    timeout-minutes: 5\n    steps:\n      - name: Reject Velnor-only docker release\n        if: ${{{{ github.event_name == 'workflow_dispatch' && github.event.inputs.providers != '' && !contains(format(',{{0}},', github.event.inputs.providers), ',github-hosted,') }}}}\n        run: |\n          echo 'docker release publishes from GitHub only; Velnor-only dispatch is unsupported' >&2\n          exit 1\n",
        runner = hosted_selector_runs_on(config),
    )
}

/// The tag gate: the tag must equal the protected branch tip (via
/// `verify-tag`, without a Cargo package: the `docker` publisher has none),
/// and one resolved version flows to every downstream job.
fn render_docker_verify_job(config: &ProjectConfig) -> String {
    let checkout = ActionPin::Checkout.reference();
    // The verify job always runs on the hosted selector, so its runtime
    // install is keyed to that placement — never to the release provider.
    let setup = workflow_runtime_setup(
        ProviderId::GithubHosted,
        &config.repository,
        &config.workflow_revision,
    );
    format!(
        "  verify:\n    name: Control / Verify release\n    runs-on: {runner}\n    timeout-minutes: 30\n    outputs:\n      version: ${{{{ steps.version.outputs.version }}}}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n{POLICY_CHECKOUT_WITH}{setup}{policy}      - name: Verify tag\n        run: velnor-workflow release verify-tag --branch {branch}\n      - name: Resolve release version\n        id: version\n        env:\n          TAG: ${{{{ github.ref_name }}}}\n        run: |\n          set -euo pipefail\n          case \"$TAG\" in\n            v[0-9]*) ;;\n            *) echo \"::error::release tag $TAG must match v[0-9]*\" >&2; exit 1 ;;\n          esac\n          version=\"${{TAG#v}}\"\n          case \"$version\" in\n            ''|*['/ ']*) echo \"::error::release version is not portable: $version\" >&2; exit 1 ;;\n          esac\n          echo \"version=$version\" >> \"$GITHUB_OUTPUT\"\n",
        runner = hosted_selector_runs_on(config),
        policy = policy_enforcement_step(),
        branch = shell_quote(&config.default_branch),
    )
}

/// The image admission gate: inspect the version tag without mutating it. An
/// absent tag opens the platform lane; a present tag is adopted only with an
/// explicitly supplied recovery digest naming its exact index, so a version
/// tag is never clobbered and unknown bytes are never adopted.
fn render_docker_admission_job(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let buildx = ActionPin::DockerBuildx.reference();
    format!(
        "  image-admission:\n    needs: [admit-provider, verify]\n    name: Admit immutable image tag\n    timeout-minutes: 10\n    runs-on: {runner}\n    permissions:\n      contents: read\n      packages: read\n    outputs:\n      existing: ${{{{ steps.inspect.outputs.existing }}}}\n      index_digest: ${{{{ steps.inspect.outputs.index_digest }}}}\n    steps:\n      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n{login_step}      - name: Inspect version tag without mutation\n        id: inspect\n        env:\n          GHCR_IMAGE: {image}\n          VERSION: ${{{{ needs.verify.outputs.version }}}}\n          RECOVERY_INDEX_DIGEST: ${{{{ github.event_name == 'workflow_dispatch' && inputs.existing-image-digest || '' }}}}\n        run: |\n          set -euo pipefail\n          ref=\"${{GHCR_IMAGE}}:${{VERSION}}\"\n          error_file=\"$(mktemp)\"\n          if docker buildx imagetools inspect \"$ref\" --format '{{{{json .}}}}' > image.json 2>\"$error_file\"; then\n            index_digest=\"$(jq -er '.manifest.digest' image.json)\"\n            case \"$index_digest\" in\n              sha256:[0-9a-fA-F]*) ;;\n              *) echo \"::error::existing image tag $ref returned an invalid index digest\" >&2; exit 1 ;;\n            esac\n            if [ -z \"$RECOVERY_INDEX_DIGEST\" ] || [ \"$index_digest\" != \"$RECOVERY_INDEX_DIGEST\" ]; then\n              echo \"::error::OCI tag $ref already exists with an unverified index digest; refusing to adopt unknown bytes (supply existing-image-digest to resume a verified run)\" >&2\n              exit 1\n            fi\n            echo \"adopting explicitly supplied recovery index $index_digest\"\n            {{\n              echo \"existing=true\"\n              echo \"index_digest=$index_digest\"\n            }} >> \"$GITHUB_OUTPUT\"\n          elif grep -Eiq 'manifest unknown|no such manifest|not found|name unknown' \"$error_file\"; then\n            {{\n              echo \"existing=false\"\n              echo \"index_digest=\"\n            }} >> \"$GITHUB_OUTPUT\"\n          else\n            cat \"$error_file\" >&2\n            echo \"::error::could not determine whether OCI tag $ref exists; refusing a fail-open publish\" >&2\n            exit 1\n          fi\n",
        runner = hosted_selector_runs_on(config),
        image = yaml_scalar(&release.image),
        login_step = docker_login_step(release),
    )
}

/// The per-arch OCI platform lane: each native builder pushes its platform
/// by digest — never under a tag — with `BuildKit` caches plus SBOM and
/// provenance attestations, then records the digest the manifest job
/// assembles. The `tags:` input names the bare repository the digest push
/// targets; only the manifest job writes a version tag.
fn render_docker_platform_job(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let checkout = ActionPin::Checkout.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let buildx = ActionPin::DockerBuildx.reference();
    let build = ActionPin::DockerBuild.reference();
    let matrix = docker_platform_matrix(config, release);
    format!(
        "  image-platform:\n    name: Build ${{{{ matrix.arch }}}} image\n    needs: [admit-provider, verify, image-admission]\n    if: ${{{{ needs.image-admission.outputs.existing != 'true' }}}}\n    timeout-minutes: 60\n    strategy:\n      fail-fast: false\n      matrix:\n        include:\n{matrix}    runs-on: ${{{{ matrix.runner }}}}\n    permissions:\n      contents: read\n      packages: write\n      id-token: write\n      attestations: write\n    env:\n      GHCR_IMAGE: {image}\n      VERSION: ${{{{ needs.verify.outputs.version }}}}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n          ref: ${{{{ github.sha }}}}\n          fetch-depth: 1\n          persist-credentials: false\n      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n          keep-state: true\n{login_step}      - name: Build + push platform image by digest\n        id: build\n        uses: {build}\n        with:\n          context: {context}\n          file: {dockerfile}\n          platforms: ${{{{ matrix.platform }}}}\n          outputs: type=image,push-by-digest=true,name-canonical=true,push=true\n          provenance: true\n          sbom: true\n          cache-from: |\n            type=registry,ref=${{{{ env.GHCR_IMAGE }}}}:buildcache-${{{{ matrix.arch }}}}\n            type=gha,scope={scope}-${{{{ matrix.arch }}}}\n          cache-to: |\n            type=registry,ref=${{{{ env.GHCR_IMAGE }}}}:buildcache-${{{{ matrix.arch }}}},mode=max\n            type=gha,scope={scope}-${{{{ matrix.arch }}}},mode=max\n          build-args: |\n            VERSION=${{{{ needs.verify.outputs.version }}}}\n          tags: ${{{{ env.GHCR_IMAGE }}}}\n          labels: |\n            org.opencontainers.image.version=${{{{ needs.verify.outputs.version }}}}\n            org.opencontainers.image.revision=${{{{ github.sha }}}}\n            org.opencontainers.image.source={source_url}\n      - name: Record platform digest\n        run: |\n          set -euo pipefail\n          digest=\"${{{{ steps.build.outputs.digest }}}}\"\n          hex=\"${{digest#sha256:}}\"\n          case \"$digest\" in\n            sha256:*) ;;\n            *) echo \"::error::platform build did not return a digest\" >&2; exit 1 ;;\n          esac\n          case \"$hex\" in\n            ''|*[!0-9a-f]*) echo \"::error::platform digest is not lowercase hex\" >&2; exit 1 ;;\n          esac\n          [ \"${{#hex}}\" -eq 64 ] || {{ echo \"::error::platform digest has invalid length\" >&2; exit 1; }}\n          printf '%s\\n' \"$digest\" > \"image-${{{{ matrix.arch }}}}.digest\"\n      - name: Upload platform digest\n        uses: {upload}\n        with:\n          name: image-platform-${{{{ matrix.arch }}}}\n          path: image-${{{{ matrix.arch }}}}.digest\n          if-no-files-found: error\n          retention-days: 2\n",
        image = yaml_scalar(&release.image),
        context = yaml_scalar(docker_context(release)),
        dockerfile = yaml_scalar(docker_dockerfile(release)),
        scope = docker_cache_scope(&release.image),
        source_url = docker_source_url(config),
        login_step = docker_login_step(release),
    )
}

/// The manifest job: assemble the verified platform digests into one
/// immutable multi-arch version tag (or re-verify an admitted one), and
/// export the index digest. The job runs whenever its inputs are
/// trustworthy — including when the platform lane correctly skipped — but
/// never when admission itself failed. Nothing else in the file writes a
/// tag: this job is the exactly-one authorized publisher.
fn render_docker_manifest_job(
    config: &ProjectConfig,
    release: &ReleaseSpec,
    needs: &[String],
) -> String {
    let checkout = ActionPin::Checkout.reference();
    let download = ActionPin::DownloadArtifact.reference();
    let upload = ActionPin::UploadArtifact.reference();
    let buildx = ActionPin::DockerBuildx.reference();
    // The manifest job always runs on the hosted selector, so its runtime
    // install is keyed to that placement — never to the release provider.
    let setup = workflow_runtime_setup(
        ProviderId::GithubHosted,
        &config.repository,
        &config.workflow_revision,
    );
    let arches = docker_platform_arches(release);
    let mut reads = String::new();
    let mut sources = Vec::new();
    let mut jq_args = String::new();
    let mut clauses = Vec::new();
    for (arch, _) in &arches {
        let _ = writeln!(
            reads,
            "            {arch}_digest=\"$(tr -d '[:space:]' < \"image-artifacts/image-{arch}.digest\")\""
        );
        sources.push(format!("              \"${{GHCR_IMAGE}}@${arch}_digest\""));
        let _ = write!(jq_args, " --arg {arch} \"${arch}_digest\"");
        clauses.push(format!(
            "any(.manifest.manifests[]; .digest == ${arch} and .platform.architecture == \"{arch}\")"
        ));
    }
    let sources = sources.join(" \\\n");
    let clauses = clauses.join(" and\n              ");
    let arch_list = arches
        .iter()
        .map(|(arch, _)| (*arch).to_owned())
        .collect::<Vec<_>>()
        .join(",");
    let needs = needs.join(", ");
    format!(
        "  image:\n    if: ${{{{ always() && needs.admit-provider.result == 'success' && needs.verify.result == 'success' && needs.image-admission.result == 'success' && (needs.image-platform.result == 'success' || needs.image-platform.result == 'skipped') }}}}\n    needs: [{needs}]\n    name: Assemble one multi-platform image\n    timeout-minutes: 15\n    runs-on: {runner}\n    permissions:\n      contents: read\n      packages: write\n    outputs:\n      index_digest: ${{{{ steps.push.outputs.index_digest }}}}\n    env:\n      GHCR_IMAGE: {image}\n      VERSION: ${{{{ needs.verify.outputs.version }}}}\n    steps:\n      - name: Checkout\n        uses: {checkout}\n        with:\n          persist-credentials: false\n{setup}      - name: Download platform digests\n        if: ${{{{ needs.image-admission.outputs.existing != 'true' }}}}\n        uses: {download}\n        with:\n          pattern: image-platform-*\n          path: image-artifacts\n          merge-multiple: true\n      - name: Verify the complete platform digest set\n        if: ${{{{ needs.image-admission.outputs.existing != 'true' }}}}\n        run: velnor-workflow release verify-digests --dir image-artifacts --archs {arch_list}\n      - name: Set up Docker Buildx\n        uses: {buildx}\n        with:\n          cleanup: false\n{login_step}      - name: Assemble and inspect immutable image index\n        id: push\n        env:\n          IMAGE_ALREADY_EXISTS: ${{{{ needs.image-admission.outputs.existing }}}}\n          EXPECTED_EXISTING_INDEX_DIGEST: ${{{{ needs.image-admission.outputs.index_digest }}}}\n        run: |\n          set -euo pipefail\n          if [ \"$IMAGE_ALREADY_EXISTS\" = true ]; then\n            docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > image-digests.json\n            index_digest=\"$(jq -er '.manifest.digest' image-digests.json)\"\n            [ \"$index_digest\" = \"$EXPECTED_EXISTING_INDEX_DIGEST\" ] || {{\n              echo \"::error::version tag moved from $EXPECTED_EXISTING_INDEX_DIGEST to $index_digest during admission\" >&2\n              exit 1\n            }}\n          else\n{reads}            docker buildx imagetools create \\\n              --tag \"${{GHCR_IMAGE}}:${{VERSION}}\" \\\n{sources}\n            docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > image-digests.json\n            jq -e{jq_args} '\n              {clauses}\n            ' image-digests.json >/dev/null || {{\n              echo \"::error::version tag does not reference the verified platform digests\" >&2\n              exit 1\n            }}\n            [ \"$(jq -r '[.manifest.manifests[] | select((.annotations[\"vnd.docker.reference.type\"] // \"\") != \"attestation-manifest\")] | length' image-digests.json)\" = \"{count}\" ] || {{\n              echo \"::error::version tag carries an unexpected platform set\" >&2\n              exit 1\n            }}\n          fi\n          docker buildx imagetools inspect \"${{GHCR_IMAGE}}:${{VERSION}}\" --format '{{{{json .}}}}' > image-digests.json\n          index_digest=\"$(jq -er '.manifest.digest' image-digests.json)\"\n          case \"$index_digest\" in\n            sha256:[0-9a-fA-F]*) ;;\n            *) echo \"::error::manifest inspection did not return an index digest\" >&2; exit 1 ;;\n          esac\n          printf 'index_digest=%s\\n' \"$index_digest\" >> \"$GITHUB_OUTPUT\"\n          printf '%s\\n' \"$index_digest\" > image-index.digest\n      - name: Upload image digests\n        uses: {upload}\n        with:\n          name: image-digests\n          path: |\n            image-digests.json\n            image-index.digest\n          if-no-files-found: error\n          retention-days: 2\n",
        runner = hosted_selector_runs_on(config),
        image = yaml_scalar(&release.image),
        count = arches.len(),
        login_step = docker_login_step(release),
    )
}

/// The standalone multi-arch OCI publisher for consumer-owned Dockerfiles:
/// one immutable version tag is admitted, built natively per declared
/// platform with caches and attestations, and assembled from the complete
/// verified digest set by the single manifest job. One concurrency group
/// per ref serializes publishers so a resume and a fresh publication can
/// never interleave on the same tag.
fn render_docker_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let (unit_jobs, unit_job_ids) = render_release_unit_jobs(config);
    let mut manifest_needs = vec![
        "admit-provider".to_owned(),
        "verify".to_owned(),
        "image-admission".to_owned(),
        "image-platform".to_owned(),
    ];
    manifest_needs.extend(unit_job_ids);
    format!(
        "{GENERATED_HEADER}name: Release\nrun-name: Release · ${{{{ github.ref_name }}}}\n\non:\n  push:\n    tags: [{tags}]\n  workflow_dispatch:\n    inputs:\n      providers:\n        description: Comma-separated provider subset (release publishes from github-hosted)\n        required: false\n        default: github-hosted\n        type: string\n      existing-image-digest:\n        description: Exact OCI index digest for an explicitly verified failed-run recovery.\n        type: string\n        required: false\n        default: ''\n\nconcurrency:\n  group: release-${{{{ github.ref }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n{admit}\n{verify}\n{unit_jobs}{admission}\n{platform}\n{manifest}",
        tags = yaml_scalar(release_tag_pattern(release)),
        admit = render_docker_admit_job(config),
        verify = render_docker_verify_job(config),
        unit_jobs = unit_jobs,
        admission = render_docker_admission_job(config, release),
        platform = render_docker_platform_job(config, release),
        manifest = render_docker_manifest_job(config, release, &manifest_needs),
    )
}

#[allow(clippy::format_push_string)]
fn render_native_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = render_binary_release(config, release);
    let hosted_runner = hosted_selector_runs_on(config);
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
        "  admit-provider:\n    name: Control / Admit release\n    runs-on: {hosted_runner}\n    timeout-minutes: 5\n    steps:\n      - name: Reject Velnor-only native release\n        if: ${{{{ github.event_name == 'workflow_dispatch' && github.event.inputs.providers != '' && !contains(format(',{{0}},', github.event.inputs.providers), ',github-hosted,') }}}}\n        run: |\n          echo 'native release publishes from GitHub only; Velnor-only dispatch is unsupported' >&2\n          exit 1\n      - name: Reject local providers on tag dispatch\n        if: ${{{{ github.event_name == 'workflow_dispatch' && github.ref_type == 'tag' && (contains(format(',{{0}},', github.event.inputs.providers), ',velnor,') || contains(format(',{{0}},', github.event.inputs.providers), ',github-self-hosted,')) }}}}\n        run: |\n          echo 'tag dispatch publishes github-hosted only; use a tag push for full provider scope' >&2\n          exit 1\n      - name: Declare release provider scope\n        if: ${{{{ github.event_name == 'workflow_dispatch' }}}}\n        env:\n          PROVIDERS: ${{{{ github.event.inputs.providers }}}}\n        run: |\n          set -euo pipefail\n          scope=\"${{PROVIDERS:-github-hosted}}\"\n          echo \"release provider scope: $scope\"\n          case \",$scope,\" in\n            *,velnor,*|*,github-self-hosted,*) echo 'full provider scope includes local lanes' ;;\n            *) echo '::notice::BOOTSTRAP release scope: github-hosted only (local qualification deferred)' ;;\n          esac\n"
    );
    output = output.replace("jobs:\n  verify:", &format!("jobs:\n{admit}\n  verify:"));
    output = output.replace(
        "    needs: [verify, build]\n",
        "    needs: [admit-provider, verify, build]\n",
    );
    let mut extra = String::new();
    let mut publish_needs = vec![
        "admit-provider".to_owned(),
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
            "  publish:\n    name: Control / Publish\n    needs: [admit-provider, verify, build]\n",
            &format!(
                "  publish:\n    name: Control / Publish\n    needs: [{}]\n",
                publish_needs.join(", ")
            ),
        );
    }
    inject_native_dispatch(&output, release, debian)
}

/// The native dispatch surface: the providers input (with the recovery digest
/// beside the admission gate that reads it) joins the mode-carrying
/// dispatch block when modes are declared, else the legacy block. Contracts
/// without the multi-arch lane keep their dispatch surface unchanged.
fn inject_native_dispatch(output: &str, release: &ReleaseSpec, debian: bool) -> String {
    let recovery_input = if debian && !release.image.is_empty() {
        "      existing-image-digest:\n        description: Exact OCI index digest for an explicitly verified failed-run recovery.\n        type: string\n        required: false\n        default: ''\n"
    } else {
        ""
    };
    let providers_input = format!(
        "      providers:\n        description: Comma-separated provider subset (release publishes from github-hosted)\n        required: false\n        default: github-hosted\n        type: string\n{recovery_input}"
    );
    if has_release_modes(release) {
        return output.replacen(
            "  workflow_dispatch:\n    inputs:\n",
            &format!("  workflow_dispatch:\n    inputs:\n{providers_input}"),
            1,
        );
    }
    let trigger = release_trigger_block(release);
    output.replace(
        &trigger,
        &format!("{trigger}  workflow_dispatch:\n    inputs:\n{providers_input}"),
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
    let runner = hosted_selector_runs_on(config);
    let package = yaml_scalar(package);
    let coordinate = yaml_scalar(coordinate);
    format!(
        "{GENERATED_HEADER}name: Package feed\nrun-name: Package feed · {kind} · ${{{{ github.event_name }}}}\n\non:\n  schedule:\n    - cron: '17 4 * * *'\n  workflow_dispatch:\n    inputs:\n      providers:\n        description: Comma-separated provider subset (release publishes from github-hosted)\n        required: false\n        default: github-hosted\n        type: string\n      channel:\n        description: Package channel\n        required: false\n        default: stable\n        type: choice\n        options:\n          - stable\n          - preview\n\nconcurrency:\n  group: package-feed-{kind}-${{{{ github.repository }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n  admit-provider:\n    name: Admit feed provider\n    runs-on: {runner}\n    timeout-minutes: 5\n    steps:\n      - name: Reject Velnor-only feed mutation\n        if: ${{{{ github.event_name == 'workflow_dispatch' && github.event.inputs.providers != '' && !contains(format(',{{0}},', github.event.inputs.providers), ',github-hosted,') }}}}\n        run: |\n          echo '{kind} feed mutation publishes from GitHub only' >&2\n          exit 1\n  verify:\n    name: Verify {kind} feed\n    needs: [admit-provider]\n    runs-on: {runner}\n    timeout-minutes: 30\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n{POLICY_CHECKOUT_WITH}{policy}      - name: Verify feed inputs\n        run: velnor-workflow release verify-feed --kind {kind} --package {package} --coordinate {coordinate}\n  mutate:\n    name: Update {kind} feed\n    needs: [admit-provider, verify]\n    if: ${{{{ github.ref == 'refs/heads/{branch}' && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch') && (github.event.inputs.providers == '' || contains(format(',{{0}},', github.event.inputs.providers), ',github-hosted,')) }}}}\n    runs-on: {runner}\n    timeout-minutes: 30\n    environment: package-feed\n    permissions:\n      contents: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n          persist-credentials: false\n      - name: Update feed\n        env:\n          CHANNEL: ${{{{ github.event.inputs.channel || 'stable' }}}}\n        run: velnor-workflow release update-feed --kind {kind} --package {package} --coordinate {coordinate} --channel \"$CHANNEL\"\n",
        ActionPin::Checkout.reference(),
        ActionPin::Checkout.reference(),
        branch = config.default_branch,
        policy = policy_enforcement_step(),
    )
}

fn render_pages_release(config: &ProjectConfig, release: &ReleaseSpec) -> String {
    let mut output = String::from(GENERATED_HEADER);
    let _ = writeln!(
        output,
        "name: Release\nrun-name: Release · documentation\n\non:\n  push:\n    branches: [{}]\n  workflow_dispatch:\n\nconcurrency:\n  group: pages-${{{{ github.repository }}}}\n  cancel-in-progress: false\n\npermissions:\n  contents: read\n\njobs:\n  deploy:\n    name: Publish documentation\n    if: ${{{{ github.event_name == 'push' && github.ref == 'refs/heads/{}' }}}}\n    runs-on: ubuntu-24.04\n    timeout-minutes: 30\n    environment: github-pages\n    permissions:\n      contents: read\n      pages: write\n      id-token: write\n    steps:\n      - name: Checkout\n        uses: {}\n        with:\n{POLICY_CHECKOUT_WITH}      - name: Set up Bun\n        uses: {}\n        with:\n          cache: true\n      - name: Build documentation\n        run: bun run scripts/generate-docs.ts\n      - name: Configure Pages\n        uses: {}\n      - name: Upload Pages artifact\n        uses: {}\n        with:\n          path: {}\n      - name: Deploy Pages\n        uses: {}\n",
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
        "  verify:\n    name: Verify documentation release\n    runs-on: ubuntu-24.04\n    timeout-minutes: 15\n    steps:\n      - name: Checkout workflow data\n        uses: {}\n        with:\n{POLICY_CHECKOUT_WITH}{}",
        ActionPin::Checkout.reference(),
        policy_enforcement_step(),
    );
    let _ = &config.default_branch;
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
        &format!("{}      - name: Set up Bun\n", policy_enforcement_step()),
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

/// Delete one Actions cache entry with bounded retries. A delete the API no
/// longer knows (HTTP 404) is progress a concurrent maintenance run already
/// made, not a failure; an authorization refusal (HTTP 401/403) aborts the
/// step instead of retrying a refusal that cannot succeed. Returns 0 when
/// the entry is deleted or already gone, 1 after bounded retries, and 2 on
/// authorization refusal.
const MAINTENANCE_DELETE_CACHE_FN: &str = r#"          delete_cache_id() {
            local id="$1" error attempt
            for attempt in 1 2 3; do
              error="$(gh api --method DELETE "repos/$GITHUB_REPOSITORY/actions/caches/$id" 2>&1 >/dev/null)" && return 0
              if grep -qi 'not found' <<<"$error"; then
                return 0
              fi
              if grep -Eq 'HTTP 40[13]' <<<"$error"; then
                echo "::error::cache delete refused for id $id ($error); refusing to retry an authorization failure" >&2
                return 2
              fi
              if (( attempt < 3 )); then
                sleep $((attempt * 2))
              fi
            done
            echo "::error::failed to delete cache id $id after bounded retries" >&2
            return 1
          }
"#;

const MAINTENANCE_WORKFLOW: &str = r#"name: Maintenance
run-name: Maintenance · ${{ github.event_name }}

on:
  pull_request:
    types: [closed]
  schedule:
    - cron: __MAINTENANCE_SCHEDULE__
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
__MAINTENANCE_DELETE_CACHE_FN__
          ref="refs/pull/$PR_NUMBER/merge"
          encoded="$(printf '%s' "$ref" | jq -sRr @uri)"
          listing="$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/caches?ref=$encoded" --jq '.actions_caches[].id')" || {
            echo "::error::failed to list cache entries for $ref" >&2
            exit 1
          }
          if [[ -z "$listing" ]]; then
            echo "No merge-ref cache entries found for $ref"
            exit 0
          fi
          mapfile -t cache_ids <<<"$listing"
          deleted=0
          failed=0
          for id in "${cache_ids[@]}"; do
            [[ -z "$id" ]] && continue
            if (( deleted + failed >= __MAINTENANCE_MAX_DELETES__ )); then
              echo "::error::maintenance delete bound reached (__MAINTENANCE_MAX_DELETES__ cache deletes); rerun maintenance to continue" >&2
              exit 1
            fi
            if delete_cache_id "$id"; then
              deleted=$((deleted + 1))
            else
              status=$?
              if (( status == 2 )); then
                exit 1
              fi
              failed=$((failed + 1))
            fi
          done
          if (( failed > 0 )); then
            echo "::error::$failed closed-PR cache entries could not be deleted; rerun maintenance" >&2
            exit 1
          fi
  cache-budget:
    name: Cache retention
    if: ${{ github.event_name == 'schedule' || github.event_name == 'workflow_dispatch' }}
    runs-on: __MAINTENANCE_CACHE_RUNNER__
    timeout-minutes: 10
    permissions:
      contents: read
      actions: write
      pull-requests: read
    steps:
      - name: Skip while CI producers are running
        id: retention-gate
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
          for workflow in __MAINTENANCE_PRODUCERS__; do
            if [[ "$(gh run list --repo "$GITHUB_REPOSITORY" --workflow "$workflow" --status in_progress --limit 1 --json databaseId --jq 'length')" != "0" ]]; then
              echo "skip=true" >> "$GITHUB_OUTPUT"
              echo "$workflow is in_progress; skipping cache retention" >> "$GITHUB_STEP_SUMMARY"
              exit 0
            fi
          done
          echo "skip=false" >> "$GITHUB_OUTPUT"
VELNOR_RUNTIME_SETUP_STEPS      - name: Sweep closed-PR merge-ref caches
        if: steps.retention-gate.outputs.skip != 'true'
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
__MAINTENANCE_DELETE_CACHE_FN__
          failed=0
          processed=0
          all_refs="$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/caches?per_page=100" \
            --jq '.actions_caches[].ref')" || {
            echo "::error::failed to list cache scopes" >&2
            exit 1
          }
          mapfile -t refs < <(printf '%s\n' "$all_refs" | grep -E '^refs/pull/[0-9]+/merge$' | sort -u)
          if ((${#refs[@]} == 0)); then
            echo "No merge-ref cache scopes found"
            exit 0
          fi
          for ref in "${refs[@]}"; do
            [[ -z "$ref" ]] && continue
            pr="${ref#refs/pull/}"
            pr="${pr%/merge}"
            state="$(gh pr view "$pr" --json state --jq .state 2>/dev/null || echo unknown)"
            if [[ "$state" != "CLOSED" ]]; then
              continue
            fi
            encoded="$(printf '%s' "$ref" | jq -sRr @uri)"
            listing="$(gh api --paginate "repos/$GITHUB_REPOSITORY/actions/caches?ref=$encoded" \
              --jq '.actions_caches[].id')" || {
              echo "::error::failed to list cache entries for $ref" >&2
              exit 1
            }
            if [[ -z "$listing" ]]; then
              continue
            fi
            mapfile -t cache_ids <<<"$listing"
            echo "Sweeping $ref ($pr): ${#cache_ids[@]} entries"
            for id in "${cache_ids[@]}"; do
              [[ -z "$id" ]] && continue
              if (( processed >= __MAINTENANCE_MAX_DELETES__ )); then
                echo "::error::maintenance delete bound reached (__MAINTENANCE_MAX_DELETES__ cache deletes); rerun maintenance to continue" >&2
                exit 1
              fi
              if delete_cache_id "$id"; then
                :
              else
                status=$?
                if (( status == 2 )); then
                  exit 1
                fi
                failed=$((failed + 1))
              fi
              processed=$((processed + 1))
            done
          done
          if (( failed > 0 )); then
            echo "::error::$failed closed-PR merge-ref cache entries could not be deleted; rerun maintenance" >&2
            exit 1
          fi
      - name: Collect Actions cache account
        if: steps.retention-gate.outputs.skip != 'true'
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
          velnor-workflow cache-plan --mode=budget --entries "$RUNNER_TEMP/cache-retention/entries.json" \
            > "$RUNNER_TEMP/cache-retention/budget.json"
          # `gh api --paginate --jq` runs the filter once per page and
          # concatenates the outputs: summing inside the filter prints one
          # number per page, and the total silently understates the account.
          # Slurp the page stream first, then take one total over every entry.
          total="$(jq '.total_held_bytes' "$RUNNER_TEMP/cache-retention/budget.json")"
          count="$(jq 'length' "$RUNNER_TEMP/cache-retention/entries.json")"
          headroom="$(jq '.headroom_bytes' "$RUNNER_TEMP/cache-retention/budget.json")"
          if (( headroom >= 0 && headroom < 536870912 )); then
            echo "::warning::Actions cache headroom below 512 MiB ($headroom bytes remaining)" >&2
          fi
          jq -n --argjson total "$total" --argjson count "$count" --argjson headroom "$headroom" \
            --slurpfile budget "$RUNNER_TEMP/cache-retention/budget.json" \
            --arg captured_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            '{captured_at: $captured_at, cache_count: $count, total_bytes: $total, headroom_bytes: $headroom, classes: $budget[0].classes}' \
            > "$RUNNER_TEMP/cache-retention/summary.json"
          {
            cat "$RUNNER_TEMP/cache-retention/summary.json"
            echo "Per-class totals:"
            jq -r '.classes[] | "  \(.id): \(.entry_count) entries, \(.held_bytes) bytes (budget \(.budget_bytes))"' \
              "$RUNNER_TEMP/cache-retention/budget.json"
          } >> "$GITHUB_STEP_SUMMARY"
      - name: Plan retention evictions
        if: steps.retention-gate.outputs.skip != 'true'
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
        if: steps.retention-gate.outputs.skip != 'true'
        env:
          GH_TOKEN: ${{ github.token }}
        run: |
          set -euo pipefail
__MAINTENANCE_DELETE_CACHE_FN__
          evicted=0
          freed=0
          failed=0
          # The plan is applied verbatim, in its own order: generations beyond
          # their class bound first, then classes over budget, then the global
          # sweep - which never touches a protected class. The per-run delete
          # bound stops the sweep instead of letting one run empty the
          # account; the next run continues where this one stopped.
          while IFS=$'\t' read -r class reason id size key; do
            if (( evicted + failed >= __MAINTENANCE_MAX_DELETES__ )); then
              echo "::error::maintenance delete bound reached (__MAINTENANCE_MAX_DELETES__ cache deletes); rerun maintenance to continue" >&2
              exit 1
            fi
            if delete_cache_id "$id"; then
              evicted=$((evicted + 1))
              freed=$((freed + size))
              echo "evicted id=$id class=$class reason=$reason size=$size key=$key"
            else
              status=$?
              if (( status == 2 )); then
                exit 1
              fi
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
        if: steps.retention-gate.outputs.skip != 'true'
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: cache-retention-${{ github.run_id }}
          path: ${{ runner.temp }}/cache-retention
          if-no-files-found: error
          retention-days: 14
      - name: Enforce cache budget
        if: steps.retention-gate.outputs.skip != 'true'
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

/// Maintenance follows the configured provider universe: both prune and
/// cache-budget stay on Velnor when `providers = ["velnor"]`, so no
/// GitHub-hosted `runs-on` leaks into a Velnor-only surface. Hosted
/// otherwise. `uses:` stays on the
/// `SOURCE_REV` pin (GitHub Actions rejects expressions in `uses:` versions).
/// `rev:` uses a context-gated `${{ github.sha }}` with a static fallback
/// when this repository owns the setup action.
/// Maintenance is one writer that follows the universe: hosted when the
/// universe contains it, otherwise the canonical-first local provider, so no
/// hosted `runs-on` leaks into a local-only surface.
fn maintenance_provider(config: &ProjectConfig) -> ProviderId {
    if config.providers.contains(&ProviderId::GithubHosted) {
        ProviderId::GithubHosted
    } else {
        config
            .providers
            .iter()
            .next()
            .copied()
            .unwrap_or(ProviderId::GithubHosted)
    }
}

fn render_maintenance(config: &ProjectConfig) -> String {
    let maintenance = maintenance_provider(config);
    let prune_gate = format!(
        "github.event_name == 'pull_request' || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/{}' && inputs.pull_request_number != '')",
        config.default_branch
    );
    let cache_gate = if maintenance == ProviderId::GithubHosted {
        "github.event_name == 'schedule' || github.event_name == 'workflow_dispatch'".to_owned()
    } else {
        format!(
            "github.ref == 'refs/heads/{}' && (github.event_name == 'push' || github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')",
            config.default_branch
        )
    };
    let setup = if maintenance == ProviderId::GithubHosted {
        let mut setup = workflow_runtime_setup_with_install_rev(
            ProviderId::GithubHosted,
            &config.repository,
            &config.workflow_revision,
            &workflow_setup_install_rev(&config.repository, &config.workflow_revision),
        );
        if crate::s2::workflow_setup_action_uses(&config.repository, &config.workflow_revision)
            == crate::s2::VELNOR_WORKFLOW_LOCAL_SETUP_ACTION
        {
            // The owner runs its own checkout of the setup action; the cache
            // retention job otherwise never checks the repository out.
            setup = format!(
                "      - name: Checkout setup action\n        uses: {}\n        with:\n          sparse-checkout: |\n            .github/actions\n          sparse-checkout-cone-mode: true\n          fetch-depth: 1\n          persist-credentials: false\n{setup}",
                ActionPin::Checkout.reference()
            );
        }
        setup = setup.replace(
            "      - name: Set trusted workflow policy revision\n        run:",
            "      - name: Set trusted workflow policy revision\n        if: steps.retention-gate.outputs.skip != 'true'\n        run:",
        );
        setup
    } else {
        String::new()
    };
    let maintenance_runs_on = config
        .selectors
        .get(&maintenance)
        .map(selector_runs_on_yaml)
        .unwrap_or_default();

    MAINTENANCE_WORKFLOW
        .replace("VELNOR_RUNTIME_SETUP_STEPS", &setup)
        .replace(
            "__MAINTENANCE_SCHEDULE__",
            &yaml_scalar(&config.maintenance.schedule),
        )
        .replace(
            "__MAINTENANCE_PRODUCERS__",
            &config.maintenance.producers.join(" "),
        )
        .replace(
            "__MAINTENANCE_MAX_DELETES__",
            &config.maintenance.max_deletes.to_string(),
        )
        .replace(
            "__MAINTENANCE_DELETE_CACHE_FN__",
            MAINTENANCE_DELETE_CACHE_FN.trim_end_matches('\n'),
        )
        .replace(
            "__MAINTENANCE_PRUNE_RUNNER__",
            &maintenance_runs_on,
        )
        .replace(
            "__MAINTENANCE_CACHE_RUNNER__",
            &maintenance_runs_on,
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
    use crate::s2::ReleaseCredential;

    /// A fixed generator pin so the pinned render digests below never move
    /// with the commit that builds the test binary.
    const FIXTURE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

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
    #[test]
    fn package_release_owns_declared_workflow_file() {
        assert_eq!(canonical_release_side_file(PACKAGE_RELEASE), None);
        assert!(is_release_side(PACKAGE_RELEASE));
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
            job.contains(
                "permissions:\n      contents: read\n      actions: write\n      pull-requests: read"
            ),
            "Cache retention must grant actions: write and pull-requests: read: {job}"
        );
    }

    fn assert_maintenance_is_github_hosted(workflow: &str, config: &ProjectConfig) {
        let hosted = format!("runs-on: {}", hosted_selector_runs_on(config));
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
        let uses_line = must_some(
            workflow
                .lines()
                .find(|line| line.contains("uses: ") && line.contains("setup-velnor-workflow")),
            "setup-velnor-workflow uses line",
        );
        if config.repository == crate::s2::workflow_setup_action_repository() {
            assert!(
                uses_line.contains(&format!(
                    "uses: {}",
                    crate::s2::VELNOR_WORKFLOW_LOCAL_SETUP_ACTION
                )),
                "the owner runs its own checkout of the action: {uses_line}"
            );
        } else {
            assert!(
                uses_line.contains(&format!("@{}", config.workflow_revision)),
                "uses: must pin SOURCE_REV: {uses_line}"
            );
        }
        assert!(
            !uses_line.contains("github.sha"),
            "GitHub Actions forbids expressions in uses: versions: {uses_line}"
        );
        assert!(
            workflow.contains(&format!("rev: {}", config.workflow_revision)),
            "maintenance install rev is the declared pin for every repository: {workflow}"
        );
        for line in workflow
            .lines()
            .filter(|line| line.trim_start().starts_with("rev:"))
        {
            assert!(
                !line.contains("github."),
                "no maintenance runtime identity may derive from the event context: {line}"
            );
        }
        assert!(
            !workflow.contains("[self-hosted,"),
            "maintenance must not select the Velnor lane: {workflow}"
        );
        let empty: &[String] = &[];
        for label in config
            .selectors
            .get(&ProviderId::Velnor)
            .map_or(empty, |selector| selector.runs_on.as_slice())
        {
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
                "Sweep closed-PR merge-ref caches",
                "Collect Actions cache account",
                "Apply retention evictions",
                "Enforce cache budget",
            ],
            "the gh api step set drifted; carry the token assertion to the new shape: {workflow}"
        );
    }

    fn assert_maintenance_runner_split(workflow: &str, config: &ProjectConfig) {
        let hosted = format!("runs-on: {}", hosted_selector_runs_on(config));
        let velnor = format!(
            "runs-on: {}",
            config
                .selectors
                .get(&ProviderId::Velnor)
                .map(selector_runs_on_yaml)
                .unwrap_or_default()
        );
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
            crate::s2::unique_suffix()
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

    fn unit(id: &str) -> crate::s2::Unit {
        crate::s2::Unit {
            id: id.to_owned(),
            label: id.to_owned(),
            kind: crate::s2::UnitKind::Rust,
            root: ".".to_owned(),
            pinned_lockfile: true,
            watch: vec!["Cargo.toml".to_owned()],
            pr_commands: vec!["cargo check".to_owned()],
            full_commands: vec!["cargo check".to_owned()],
            depends_on: Vec::new(),
            cache: None,
            tool_version: None,
            mise_tools: Vec::new(),
            // The scan guarantees this fact on every Rust unit of a real
            // repository; the test config carries the same contract, pinning
            // the targets the fixture's release contract builds for.
            toolchain: Some(crate::s2::RustToolchain {
                channel: "1.91.1".to_owned(),
                components: Vec::new(),
                targets: vec![
                    "x86_64-unknown-linux-gnu".to_owned(),
                    "aarch64-unknown-linux-gnu".to_owned(),
                ],
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
            image_package: String::new(),
            source_repository: String::new(),
            consumer_repository: String::new(),
            artifact_path: String::new(),
            description: String::new(),
            manifest_schema: String::new(),
            dockerfile: String::new(),
            context: String::new(),
            platforms: Vec::new(),
            producer_workflow: String::new(),
            producer_conclusion: String::new(),
            modes: Vec::new(),
            archive_members: Vec::new(),
            archive_checksum: String::new(),
            archive_retention_days: 0,
            credentials: Vec::new(),
            tag_pattern: String::new(),
            registry: String::new(),
            registry_username_secret: String::new(),
            registry_password_secret: String::new(),
            jobs: Vec::new(),
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
            image_package: String::new(),
            source_repository: "example/app".to_owned(),
            consumer_repository: "example/apt".to_owned(),
            artifact_path: String::new(),
            description: String::new(),
            manifest_schema: "example.test/consumer-manifest-v1".to_owned(),
            dockerfile: String::new(),
            context: String::new(),
            platforms: Vec::new(),
            producer_workflow: String::new(),
            producer_conclusion: String::new(),
            modes: Vec::new(),
            archive_members: Vec::new(),
            archive_checksum: String::new(),
            archive_retention_days: 0,
            credentials: Vec::new(),
            tag_pattern: String::new(),
            registry: String::new(),
            registry_username_secret: String::new(),
            registry_password_secret: String::new(),
            jobs: Vec::new(),
        }
    }

    /// A standalone multi-arch OCI publisher over a consumer-owned
    /// Dockerfile. All coordinates are fixture-local.
    fn docker_spec() -> ReleaseSpec {
        ReleaseSpec {
            kind: "docker".to_owned(),
            package: String::new(),
            packages: Vec::new(),
            binary: String::new(),
            targets: Vec::new(),
            image: "ghcr.io/example/app".to_owned(),
            image_package: String::new(),
            source_repository: String::new(),
            consumer_repository: String::new(),
            artifact_path: String::new(),
            description: String::new(),
            manifest_schema: String::new(),
            dockerfile: "Dockerfile".to_owned(),
            context: ".".to_owned(),
            platforms: vec!["linux/amd64".to_owned(), "linux/arm64".to_owned()],
            producer_workflow: String::new(),
            producer_conclusion: String::new(),
            modes: Vec::new(),
            archive_members: Vec::new(),
            archive_checksum: String::new(),
            archive_retention_days: 0,
            credentials: Vec::new(),
            tag_pattern: String::new(),
            registry: String::new(),
            registry_username_secret: String::new(),
            registry_password_secret: String::new(),
            jobs: Vec::new(),
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
        let mut selectors = crate::s2::scan::default_selectors();
        selectors.insert(
            crate::s2::provider::ProviderId::Velnor,
            crate::s2::provider::ProviderSelector {
                runs_on: vec!["self-hosted".to_owned(), "example-runner".to_owned()],
            },
        );
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
            selectors,
            release_enabled: release.is_some(),
            release_reason: String::new(),
            release,
            renovate_enabled: false,
            renovate_reason: String::new(),
            renovate: None,
            docs_enabled: false,
            docs_reason: String::new(),
            docs: None,
            check_profiles: Vec::new(),
            maintenance: crate::s2::MaintenanceSpec::default(),
            units: vec![unit("rust-example")],
            workflow_templates: BTreeMap::new(),
            adopted_workflow_surface: false,
            actionlint_config_variables_null: false,
            ci_required: true,
            ruleset_required_status_checks: Vec::new(),
            ruleset_external_status_checks: Vec::new(),
            package_update_channels: None,
            default_dispatch_providers: crate::s2::provider::ProviderId::ALL.into_iter().collect(),
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
        let providers: crate::s2::provider::ProviderSet =
            crate::s2::provider::ProviderId::ALL.into_iter().collect();
        let shape = must(
            crate::s2::scan::scan_shape(root, &providers, "main", &[]),
            "scan release fixture",
        );
        let generation = generation.map(|rows| {
            let directory = root.join(crate::s2::config::GENERATION_CONFIG_PATH);
            must(
                fs::create_dir_all(directory.parent().unwrap_or(root)),
                "create generation config directory",
            );
            must(
                fs::write(
                    &directory,
                    format!(
                        "schema = 2\n\n[generator]\nrepository = \"example/declared\"\n\n{rows}"
                    ),
                ),
                "write declared config",
            );
            let Some(generation) = must(
                crate::s2::config::discover(root),
                "discover declared config",
            ) else {
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
                "9bac3040c77b0dc9fb9acf31cabb426f20a8da419f86f976d34819b0c1bc5e75",
            ),
            (
                "preview.yml",
                "4c47335678cafe07eeb3dfd7f2aacdc8e57535f80fec2a1a9f8fcea7a1336afc",
            ),
            (
                "maintenance.yml",
                "3725b27e2f9fe196c7a5694f8e1255feef011f7b88ad52e214c47300809c0328",
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
        assert!(release.contains("Control / Publish"), "{release}");
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
                rendered.starts_with(crate::s2::GENERATED_HEADER),
                "{file} must carry the generated header: {rendered}"
            );
        }
        assert!(surface.added_files.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    /// Every release-side family, whichever kind renders it: a job that runs
    /// `velnor-workflow policy` checks out full history, and a hosted Mr.
    /// Boxington store is bounded ahead of the action. The shallow preview
    /// identity checkout that failed `Preview · push · main` was one kind's
    /// renderer; the rule is on every kind's output.
    #[test]
    fn every_release_kind_renders_validator_jobs_with_full_history_and_bounded_stores() {
        let mut pages = binary_spec();
        pages.kind = "pages".to_owned();
        pages.artifact_path = "site".to_owned();
        let mut apt = binary_spec();
        apt.kind = "apt".to_owned();
        apt.consumer_repository = "example/apt".to_owned();
        let mut homebrew = binary_spec();
        homebrew.kind = "homebrew".to_owned();
        homebrew.source_repository = "example/app".to_owned();
        let mut crates = binary_spec();
        crates.kind = "crates".to_owned();
        crates.packages = vec!["example".to_owned()];
        for spec in [binary_spec(), native_spec(), pages, apt, homebrew, crates] {
            let kind = spec.kind.clone();
            let root = scanned_root(&format!("validator-checkout-{kind}"));
            let config = config(&["release.yml", "preview.yml"], Some(spec));
            let surface = generate(&root, &config, None);
            let validator_jobs = surface
                .files
                .values()
                .filter(|rendered| rendered.contains("run: velnor-workflow policy"))
                .count();
            assert!(
                validator_jobs > 0,
                "the {kind} surface must run the validator somewhere"
            );
            must(
                crate::s2::validate_policy_jobs_check_out_full_history(&surface.files),
                &format!("{kind}: every validator-running job checks out full history"),
            );
            must(
                crate::s2::validate_hosted_mr_boxington_store_budget(&surface.files),
                &format!("{kind}: every hosted Mr. Boxington job bounds its store"),
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    /// The identity release surface, pinned like the legacy one: any renderer
    /// change shows up here and has to be carried into the pin deliberately.
    #[test]
    fn identity_rows_render_the_pinned_native_surface() {
        const PINNED: &[(&str, &str)] = &[
            (
                "release.yml",
                "be72b8ec1252d670495df4d184c26b50c08ae0e2847d1191688851d3e5eb9c2c",
            ),
            (
                "preview.yml",
                "4922e8b7aded3ec357e776fec51fad0ef4d408db8117801c87ec7dfeefb36368",
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
    fn native_preview_identity_provisions_the_hosted_runtime_on_the_velnor_lane() {
        let mut config = native_identity_config(&["preview.yml"]);
        config.providers = std::collections::BTreeSet::from([ProviderId::Velnor]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let preview = super::render_preview(&config, Some(release));
        let identity = yaml_job(&preview, "identity");
        assert!(
            identity.contains("runs-on: ubuntu-24.04"),
            "the identity job stays hosted whatever the lane: {identity}"
        );
        let setup = must_some(
            identity.find("Set up Velnor workflow runtime"),
            "identity runtime setup",
        );
        let enforce = must_some(identity.find("Enforce workflow policy"), "identity enforce");
        assert!(
            setup < enforce,
            "the hosted identity job must provision its runtime before enforcing policy: {identity}"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn velnor_release_surfaces_have_no_hosted_runner_branch() {
        let mut config = config(&["release.yml", "preview.yml"], Some(binary_spec()));
        config.providers = std::collections::BTreeSet::from([ProviderId::Velnor]);
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
    fn release_providers_honor_the_configured_runner_labels() {
        let mut spec = binary_spec();
        spec.targets.push("aarch64-apple-darwin".to_owned());
        let mut cfg = config(&["preview.yml", "release.yml"], Some(spec));
        cfg.providers = std::collections::BTreeSet::from([ProviderId::GithubHosted]);
        cfg.selectors.insert(
            ProviderId::GithubHosted,
            crate::s2::provider::ProviderSelector {
                runs_on: vec!["ubuntu-custom".to_owned()],
            },
        );
        let Some(release) = cfg.release.as_ref() else {
            panic!("release fixture must carry a release contract");
        };
        for workflow in [
            super::render_preview(&cfg, Some(release)),
            super::render_release(&cfg, release),
        ] {
            assert!(
                workflow.contains("runner: ubuntu-custom"),
                "linux builders must use the configured hosted selector: {workflow}"
            );
            assert!(
                workflow.contains("runner: macos-26"),
                "apple builders use the fixed GitHub-owned macos image: {workflow}"
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
    fn release_publishers_stay_hosted_while_verification_fans_out_per_provider() {
        let cfg = config(&["preview.yml", "release.yml"], Some(binary_spec()));
        let Some(release) = cfg.release.as_ref() else {
            panic!("release fixture must carry a release contract");
        };
        let preview = super::render_preview(&cfg, Some(release));
        let release_workflow = super::render_release(&cfg, release);

        for workflow in [&preview, &release_workflow] {
            assert!(workflow.contains("provider: github-hosted"), "{workflow}");
            assert!(
                !workflow.contains("provider: velnor"),
                "release-side artifact matrices must stay hosted: {workflow}"
            );
            assert!(
                !workflow.contains("provider: github-self-hosted"),
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
        for provider in ["github-hosted", "github-self-hosted", "velnor"] {
            assert!(
                release_workflow.contains(&format!("release-{provider}-rust-example")),
                "release verification must fan out to every provider: {release_workflow}"
            );
        }
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn release_docker_verification_exports_the_build_token() {
        let mut cfg = config(&["release.yml"], Some(binary_spec()));
        let mut docker = unit("docker-example");
        docker.kind = crate::s2::UnitKind::Docker;
        docker.full_commands = vec![
            "docker buildx build --load --file 'Dockerfile' --tag local-ci:dockerfile '.' --secret id=github_token,env=GITHUB_TOKEN"
                .to_owned(),
        ];
        cfg.units.push(docker);
        let Some(release) = cfg.release.as_ref() else {
            panic!("release fixture must carry a release contract")
        };
        let workflow = super::render_release(&cfg, release);
        let docker_job = yaml_job(&workflow, "release-github-hosted-docker-example");
        assert!(
            docker_job.contains("GITHUB_TOKEN: ${{ github.token }}"),
            "the release docker checks step provides the build secret's token: {docker_job}"
        );
        let rust_job = yaml_job(&workflow, "release-github-hosted-rust-example");
        assert!(
            !rust_job.contains("GITHUB_TOKEN: ${{ github.token }}"),
            "other kinds get no token: {rust_job}"
        );
    }

    #[test]
    fn maintenance_splits_prune_and_cache_when_runners_are_velnor() {
        let mut cfg = config(&["maintenance.yml"], None);
        cfg.providers = std::collections::BTreeSet::from([ProviderId::Velnor]);

        let direct = super::render_maintenance(&cfg);
        assert_maintenance_runner_split(&direct, &cfg);
        assert_cache_retention_has_actions_write(&direct);

        let root = scanned_root("maintenance-hosted");
        let surface = generate(&root, &cfg, None);
        let generated = rendered(&surface, "maintenance.yml");
        assert!(
            generated.starts_with(crate::s2::GENERATED_HEADER),
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
        for runners in [
            std::collections::BTreeSet::from([ProviderId::GithubHosted]),
            std::collections::BTreeSet::from([ProviderId::Velnor]),
            crate::s2::provider::ProviderId::ALL.into_iter().collect(),
        ] {
            let mut cfg = config(&["maintenance.yml"], None);
            cfg.providers = runners;
            let workflow = super::render_maintenance(&cfg);
            assert_maintenance_gh_api_steps_carry_token(&workflow);
            assert!(
                workflow.contains("if (( failed > 0 ))"),
                "failed evictions must fail the step loudly: {workflow}"
            );
        }

        let mut cfg = config(&["maintenance.yml"], None);
        cfg.providers = std::collections::BTreeSet::from([ProviderId::Velnor]);
        let root = scanned_root("maintenance-token");
        let surface = generate(&root, &cfg, None);
        assert_maintenance_gh_api_steps_carry_token(&rendered(&surface, "maintenance.yml"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn maintenance_uses_configured_runners_for_github_hosted_modes() {
        for runners in [
            std::collections::BTreeSet::from([ProviderId::GithubHosted]),
            crate::s2::provider::ProviderId::ALL.into_iter().collect(),
        ] {
            let mut cfg = config(&["maintenance.yml"], None);
            cfg.providers = runners;
            let workflow = super::render_maintenance(&cfg);
            assert_maintenance_is_github_hosted(&workflow, &cfg);
        }
    }

    #[test]
    fn maintenance_renders_declared_schedule_producers_and_bound() {
        let mut cfg = config(&["maintenance.yml"], None);
        cfg.maintenance = crate::s2::MaintenanceSpec {
            schedule: "17 4 * * *".to_owned(),
            producers: vec!["ci-main.yml".to_owned()],
            max_deletes: 50,
        };
        let workflow = super::render_maintenance(&cfg);
        assert!(
            workflow.contains("- cron: \"17 4 * * *\""),
            "declared schedule: {workflow}"
        );
        assert!(
            workflow.contains("for workflow in ci-main.yml; do"),
            "declared producers: {workflow}"
        );
        assert!(
            workflow.contains(">= 50"),
            "declared delete bound: {workflow}"
        );
        assert!(
            !workflow.contains("__MAINTENANCE_"),
            "no placeholder survives rendering: {workflow}"
        );
        let defaults = super::render_maintenance(&config(&["maintenance.yml"], None));
        assert!(defaults.contains("- cron: \"31 3 * * *\""), "{defaults}");
        assert!(
            defaults.contains("for workflow in ci-main.yml nightly.yml; do"),
            "{defaults}"
        );
        assert!(defaults.contains(">= 500"), "{defaults}");
    }

    #[test]
    fn maintenance_deletes_are_bounded_and_progress_aware() {
        let cfg = config(&["maintenance.yml"], None);
        let workflow = super::render_maintenance(&cfg);
        assert_eq!(
            workflow.matches("delete_cache_id() {").count(),
            3,
            "prune, sweep, and eviction share one delete helper: {workflow}"
        );
        for marker in [
            "grep -qi 'not found'",
            "grep -Eq 'HTTP 40[13]'",
            "refusing to retry an authorization failure",
            "after bounded retries",
            "maintenance delete bound reached",
            "rerun maintenance to continue",
        ] {
            assert!(workflow.contains(marker), "{marker}: {workflow}");
        }
        assert!(
            !workflow.contains("for delay in 1 2 4 8"),
            "the unbounded retry loop is gone: {workflow}"
        );
    }

    #[test]
    fn maintenance_passes_trusted_policy_audit() {
        let cfg = config(&["maintenance.yml"], None);
        let workflow = super::render_maintenance(&cfg);
        let root = scanned_root("maintenance-audit");
        let workflows = root.join(".github/workflows");
        must(fs::create_dir_all(&workflows), "create audited workflows");
        must(
            fs::write(workflows.join("maintenance.yml"), &workflow),
            "write audited maintenance.yml",
        );
        let audit = must(
            crate::s2::policy::audit_workflows(&root),
            "audit maintenance workflow",
        );
        assert!(
            audit.pull_request_target.is_empty(),
            "{:?}",
            audit.pull_request_target
        );
        assert!(audit.runners.is_empty(), "{:?}", audit.runners);
        assert!(audit.actions.is_empty(), "{:?}", audit.actions);
        assert!(audit.structure.is_empty(), "{:?}", audit.structure);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn maintenance_installs_pin_when_this_repository_owns_setup() {
        let mut cfg = config(&["maintenance.yml"], None);
        cfg.repository = crate::s2::workflow_setup_action_repository().to_owned();
        let workflow = super::render_maintenance(&cfg);
        assert_maintenance_is_github_hosted(&workflow, &cfg);
        assert!(
            workflow.contains(&format!("rev: {}", cfg.workflow_revision)),
            "the setup-action owner installs the declared pin like every consumer: {workflow}"
        );
        for line in workflow
            .lines()
            .filter(|line| line.trim_start().starts_with("rev:"))
        {
            assert!(
                !line.contains("github.sha"),
                "owned maintenance must not derive its runtime identity from github.sha: {line}"
            );
        }
    }

    #[test]
    fn maintenance_keeps_hosted_selector_out_of_runs_on_when_universe_is_velnor() {
        let mut cfg = config(&["maintenance.yml"], None);
        cfg.providers = std::collections::BTreeSet::from([ProviderId::Velnor]);
        cfg.selectors.insert(
            ProviderId::GithubHosted,
            crate::s2::provider::ProviderSelector {
                runs_on: vec!["ubuntu-22.04".to_owned()],
            },
        );
        cfg.default_branch = "trunk".to_owned();

        let workflow = super::render_maintenance(&cfg);
        assert_maintenance_runner_split(&workflow, &cfg);
        assert_cache_retention_has_actions_write(&workflow);
        assert!(
            !workflow.contains("runs-on: ubuntu-22.04"),
            "velnor-only maintenance must not leak the hosted selector into runs-on: {workflow}"
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
        let legacy = must(crate::s2::generated_files(&config), "generate legacy files");
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
        assert!(release.contains("Control / Publish"), "{release}");
        assert!(
            release.contains("target: aarch64-unknown-linux-gnu"),
            "{release}"
        );
        assert_eq!(surface.added_files, vec!["release.yml".to_owned()]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn schema2_tasks_release_renders_typed_desktop_jobs() {
        let root = scanned_root("tasks-release");
        must(
            fs::write(
                root.join("mise.toml"),
                "[tasks.desktop-build]\nrun = \"true\"\n\n[tasks.desktop-sign]\nrun = \"true\"\n",
            ),
            "write desktop task fixture",
        );
        must(
            fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        must(
            fs::write(
                root.join(crate::s2::config::GENERATION_CONFIG_PATH),
                "schema = 2\n\n[generator]\nrepository = \"example/declared\"\n\n[workflow]\nfiles = [\"release.yml\"]\n\n[release]\nenabled = true\nkind = \"tasks\"\nmodes = [\"validate\"]\ntag_pattern = \"v[0-9]*\"\n\n[[release.job]]\nid = \"build\"\ntasks = [\"desktop-build\"]\nrunner = \"github\"\n\n[[release.job]]\nid = \"sign\"\nname = \"Sign release\"\ntasks = [\"desktop-sign\"]\nneeds = [\"build\"]\nrunner = \"macos\"\nmodes = [\"publish\"]\nenvironment = \"release-macos\"\nattest_subjects = [\"dist/app.zip\"]\n\n[release.job.permissions]\nid-token = \"write\"\n\n[[release.job]]\nid = \"attest-defaults\"\ntasks = [\"desktop-sign\"]\nrunner = \"github\"\nattest_subjects = [\"dist/*.tar.gz\"]\n",
            ),
            "write tasks release config",
        );
        let tree = must(
            crate::s2::render_tree(&root, None, "main"),
            "render schema-2 tasks release tree",
        );
        let release = must_some(
            tree.files
                .get(&PathBuf::from(".github/workflows/release.yml")),
            "schema-2 tasks release workflow",
        );
        assert!(release.contains("tags: [\"v[0-9]*\"]"), "{release}");
        assert!(
            release.contains("workflow_dispatch:\n    inputs:\n      mode:"),
            "{release}"
        );
        let build = yaml_job(release, "build");
        assert!(build.contains("runs-on: ubuntu-24.04"), "{build}");
        assert!(build.contains("run: mise run desktop-build"), "{build}");
        let sign = yaml_job(release, "sign");
        assert!(sign.contains("needs: [build]"), "{sign}");
        assert!(
            sign.contains("if: ${{ github.event_name != 'workflow_dispatch' }}"),
            "{sign}"
        );
        assert!(sign.contains("runs-on: macos-26"), "{sign}");
        assert!(sign.contains("environment: release-macos"), "{sign}");
        assert!(
            sign.contains("contents: read"),
            "custom permissions must retain checkout access: {sign}"
        );
        assert!(sign.contains("id-token: write"), "{sign}");
        assert!(sign.contains("attestations: write"), "{sign}");
        assert!(sign.contains("run: mise run desktop-sign"), "{sign}");
        assert!(
            sign.contains("subject-path: |\n            dist/app.zip"),
            "{sign}"
        );
        let defaults = yaml_job(release, "attest-defaults");
        assert!(defaults.contains("contents: read"), "{defaults}");
        assert!(defaults.contains("id-token: write"), "{defaults}");
        assert!(defaults.contains("attestations: write"), "{defaults}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn schema2_tasks_release_mise_setup_follows_job_runner() {
        let mut config = config(&["release.yml"], None);
        config.providers = [ProviderId::Velnor].into_iter().collect();

        for (runner, needs_setup) in [("github", true), ("macos", true), ("velnor", false)] {
            let job = ReleaseJobSpec {
                id: "job".to_owned(),
                name: "Job".to_owned(),
                tasks: vec!["desktop-build".to_owned()],
                runner: runner.to_owned(),
                timeout_minutes: 10,
                ..ReleaseJobSpec::default()
            };
            let rendered = render_tasks_release_job(&config, &job);
            assert_eq!(
                rendered.contains("Set up Mise"),
                needs_setup,
                "Mise setup for runner {runner}: {rendered}"
            );
        }
    }

    #[test]
    fn schema2_tasks_release_quotes_yaml_reserved_job_ids_and_needs() {
        let root = scanned_root("tasks-release-reserved-id");
        must(
            fs::write(
                root.join("mise.toml"),
                "[tasks.desktop-build]\nrun = \"true\"\n\n[tasks.desktop-publish]\nrun = \"true\"\n",
            ),
            "write reserved-id task fixture",
        );
        must(
            fs::create_dir_all(root.join(".github-gen")),
            "create generation config directory",
        );
        must(
            fs::write(
                root.join(crate::s2::config::GENERATION_CONFIG_PATH),
                "schema = 2\n\n[generator]\nrepository = \"example/declared\"\n\n[workflow]\nfiles = [\"release.yml\"]\n\n[release]\nenabled = true\nkind = \"tasks\"\n\n[[release.job]]\nid = \"on\"\ntasks = [\"desktop-build\"]\n\n[[release.job]]\nid = \"publish\"\ntasks = [\"desktop-publish\"]\nneeds = [\"on\"]\n",
            ),
            "write reserved-id release config",
        );
        let tree = must(
            crate::s2::render_tree(&root, None, "main"),
            "render reserved-id tasks release tree",
        );
        let release = must_some(
            tree.files
                .get(&PathBuf::from(".github/workflows/release.yml")),
            "reserved-id tasks release workflow",
        );
        assert!(
            release.contains("  \"on\":\n"),
            "job ids must be YAML-safe: {release}"
        );
        assert!(
            release.contains("needs: [\"on\"]"),
            "needs entries must be YAML-safe: {release}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn github_release_unit_cache_restore_declares_cache_step_id() {
        let mut config = config(&["release.yml"], Some(binary_spec()));
        config.units[0].pr_commands = vec!["mbx nextest run --locked".to_owned()];
        config.units[0].full_commands = vec!["mbx nextest run --locked".to_owned()];
        config.units[0].cache = Some(crate::s2::CacheSpec {
            key_files: vec!["Cargo.lock".to_owned()],
            paths: vec!["~/.cargo/registry".to_owned()],
            purpose: crate::s2::CachePurpose::CargoSources,
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
            assert!(
                release.contains("name: Control / Admit release"),
                "{release}"
            );
            assert!(
                release.contains("native release publishes from GitHub only"),
                "{release}"
            );
            assert!(release.contains("Publish container image"), "{release}");
            assert!(release.contains(image), "{release}");
            assert!(release.contains("github.ref_name"), "{release}");
            assert!(
                !release.contains(
                    "  image:\n    name: Publish container image\n    needs: [admit-provider, verify, build, image]"
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
        assert!(
            image.contains("GHCR_IMAGE: \"ghcr.io/example/app\""),
            "{image}"
        );
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
            debian.contains("needs: [admit-provider, verify, build, metadata]"),
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
                "needs: [admit-provider, verify, build, image, metadata, debian, sign-deb]"
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
            admission.contains("needs: [admit-provider, verify]"),
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

    /// The genericity law's deny list, parsed out of its own source so the
    /// probes stay in sync without this file spelling a consumer name. The
    /// sibling parser in `tests/migration_contract.rs` reads the same
    /// source; the law file is the single source of truth for the tokens.
    fn deny_list_probes() -> (Vec<String>, (String, String)) {
        const LAW: &str = include_str!("../../../tests/generic_surface_literals.rs");
        const LIST_MARKER: &str = "const DENY_LIST";
        const REWRITE_MARKER: &str = ".replace(";
        fn quoted(line: &str) -> Vec<String> {
            let mut out = Vec::new();
            let mut rest = line;
            while let Some(open) = rest.find('"') {
                rest = &rest[open + 1..];
                let Some(close) = rest.find('"') else {
                    break;
                };
                out.push(rest[..close].to_lowercase());
                rest = &rest[close + 1..];
            }
            out
        }
        let start = LAW.find(LIST_MARKER).unwrap_or(0);
        let mut probes = Vec::new();
        let mut in_list = false;
        for line in LAW[start..].lines() {
            if !in_list {
                if line.contains('[') {
                    in_list = true;
                }
                continue;
            }
            if line.contains("];") {
                break;
            }
            probes.extend(quoted(line));
        }
        let mut rewrite = (String::new(), String::new());
        for line in LAW.lines() {
            if line.contains(REWRITE_MARKER) {
                let parts = quoted(line);
                if parts.len() >= 2 {
                    rewrite = (parts[0].clone(), parts[1].clone());
                }
                break;
            }
        }
        (probes, rewrite)
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
            platform.contains("needs: [admit-provider, verify, metadata, image-admission]"),
            "{platform}"
        );
        assert!(
            platform.contains("if: ${{ needs.image-admission.outputs.existing != 'true' && github.event_name != 'workflow_dispatch' }}"),
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
        assert!(platform.contains("Build the image binary"), "{platform}");
        // The lane derives every product value from the contract: the
        // release package names the binary, the Dockerfile convention
        // applies, and the cache scope slugifies the image. No
        // owner-repository literal may appear in a neutral render.
        assert!(platform.contains("--package example"), "{platform}");
        assert!(
            platform.contains("release-binaries/${{ matrix.arch }}/example"),
            "{platform}"
        );
        assert!(platform.contains("file: Dockerfile"), "{platform}");
        assert!(
            platform.contains("type=gha,scope=ghcr-io-example-app-${{ matrix.arch }}"),
            "{platform}"
        );
        assert!(
            platform.contains("github_token=${{ github.token }}"),
            "{platform}"
        );
        assert!(
            platform.contains("VERSION=${{ needs.verify.outputs.version }}"),
            "{platform}"
        );
        // No owner-repository literal may appear in a neutral render. The
        // probe names come from the genericity law's own deny list, parsed
        // from its source: this file must not spell a consumer name
        // literally, and the probes stay in sync with the law.
        let (forbidden, (admitted, replacement)) = deny_list_probes();
        assert!(
            !forbidden.is_empty(),
            "the genericity deny list parsed to no probes"
        );
        let text = platform.to_lowercase().replace(&admitted, &replacement);
        for name in &forbidden {
            assert!(
                !text.contains(name),
                "neutral render must not contain {name}: {platform}"
            );
        }
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
    fn image_package_overrides_the_release_package_for_the_image_lane() {
        let mut spec = native_spec();
        spec.image_package = "example-worker".to_owned();
        spec.dockerfile = "images/app/Dockerfile".to_owned();
        let config = config(&["release.yml"], Some(spec));
        let release = must_some(
            config.release.as_ref(),
            "config must carry the release contract",
        );
        let platform = super::render_image_platform_job(&config, release);
        assert!(platform.contains("--package example-worker"), "{platform}");
        assert!(
            platform.contains("release-binaries/${{ matrix.arch }}/example-worker"),
            "{platform}"
        );
        assert!(
            platform.contains("file: \"images/app/Dockerfile\""),
            "{platform}"
        );
    }

    #[test]
    fn image_lane_without_any_package_rejects_instead_of_guessing() {
        let mut spec = native_spec();
        spec.package = String::new();
        spec.binary = String::new();
        let config = config(&["release.yml"], Some(spec));
        let release = must_some(
            config.release.as_ref(),
            "config must carry the release contract",
        );
        let platform = super::render_image_platform_job(&config, release);
        assert!(
            platform.contains("Reject imageless binary contract"),
            "{platform}"
        );
        assert!(!platform.contains("--package"), "{platform}");
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

    /// A `docker` publisher config over a repository that owns its
    /// Dockerfile. The repository slug feeds the OCI source label.
    fn docker_config() -> ProjectConfig {
        let mut config = config(&["release.yml"], Some(docker_spec()));
        config.repository = "example/app".to_owned();
        config
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn docker_publisher_renders_one_native_builder_per_platform() {
        let config = docker_config();
        let Some(release) = config.release.as_ref() else {
            panic!("docker fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        let platform = yaml_job(&workflow, "image-platform");
        assert!(
            platform.contains("needs: [admit-provider, verify, image-admission]"),
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
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn docker_platform_pushes_by_digest_with_caches_and_attestations() {
        let config = docker_config();
        let Some(release) = config.release.as_ref() else {
            panic!("docker fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        let platform = yaml_job(&workflow, "image-platform");
        // Push-by-digest: no tag is written here; `tags:` names the bare
        // repository the digest push targets.
        assert!(
            platform
                .contains("outputs: type=image,push-by-digest=true,name-canonical=true,push=true"),
            "{platform}"
        );
        assert!(
            platform.contains("tags: ${{ env.GHCR_IMAGE }}"),
            "{platform}"
        );
        assert!(!platform.contains("imagetools create"), "{platform}");
        assert!(platform.contains("provenance: true"), "{platform}");
        assert!(platform.contains("sbom: true"), "{platform}");
        assert!(platform.contains("id-token: write"), "{platform}");
        assert!(platform.contains("attestations: write"), "{platform}");
        assert!(
            platform
                .contains("type=registry,ref=${{ env.GHCR_IMAGE }}:buildcache-${{ matrix.arch }}"),
            "{platform}"
        );
        assert!(
            platform.contains("type=gha,scope=ghcr-io-example-app-${{ matrix.arch }}"),
            "{platform}"
        );
        assert!(platform.contains("mode=max"), "{platform}");
        // The consumer-owned build inputs render verbatim.
        assert!(platform.contains("file: Dockerfile"), "{platform}");
        assert!(platform.contains("context: \".\""), "{platform}");
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
        // The recorded digest is strictly validated before transport.
        assert!(
            platform.contains("digest=\"${{ steps.build.outputs.digest }}\""),
            "{platform}"
        );
        assert!(
            platform.contains("platform digest is not lowercase hex"),
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
    fn docker_admission_reconciles_absent_resume_and_conflict() {
        let config = docker_config();
        let Some(release) = config.release.as_ref() else {
            panic!("docker fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        let admission = yaml_job(&workflow, "image-admission");
        assert!(
            admission.contains("needs: [admit-provider, verify]"),
            "{admission}"
        );
        assert!(
            admission.contains("imagetools inspect \"$ref\" --format '{{json .}}'"),
            "{admission}"
        );
        // Absent opens the lane.
        assert!(admission.contains("existing=false"), "{admission}");
        // Resume adopts only the explicitly supplied recovery index.
        assert!(
            admission.contains("inputs.existing-image-digest"),
            "{admission}"
        );
        assert!(
            admission.contains("adopting explicitly supplied recovery index"),
            "{admission}"
        );
        // Conflict fails closed instead of adopting unknown bytes.
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
    fn docker_manifest_assembles_only_the_complete_verified_set() {
        let config = docker_config();
        let Some(release) = config.release.as_ref() else {
            panic!("docker fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
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
        // The shared digest-set gate runs before any assembly.
        assert!(
            image.contains(
                "run: velnor-workflow release verify-digests --dir image-artifacts --archs amd64,arm64"
            ),
            "{image}"
        );
        // Digest-pinned sources; the version tag is created here only.
        assert!(image.contains("imagetools create"), "{image}");
        assert!(
            image.contains("--tag \"${GHCR_IMAGE}:${VERSION}\""),
            "{image}"
        );
        assert!(image.contains("\"${GHCR_IMAGE}@$amd64_digest\""), "{image}");
        assert!(image.contains("\"${GHCR_IMAGE}@$arm64_digest\""), "{image}");
        // Inclusion plus exclusivity: exactly the verified set.
        assert!(
            image.contains("does not reference the verified platform digests"),
            "{image}"
        );
        assert!(
            image.contains("carries an unexpected platform set"),
            "{image}"
        );
        assert!(image.contains("name: image-digests"), "{image}");
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn docker_publisher_writes_the_version_tag_exactly_once() {
        let config = docker_config();
        let Some(release) = config.release.as_ref() else {
            panic!("docker fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        assert_eq!(
            workflow.matches("imagetools create").count(),
            1,
            "exactly one job may create the version tag: {workflow}"
        );
        assert!(
            workflow.contains("group: release-${{ github.ref }}"),
            "{workflow}"
        );
        assert!(workflow.contains("cancel-in-progress: false"), "{workflow}");
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn docker_contract_defaults_to_both_linux_architectures() {
        let mut spec = docker_spec();
        spec.dockerfile.clear();
        spec.context.clear();
        spec.platforms.clear();
        let mut config = config(&["release.yml"], Some(spec));
        config.repository = "example/app".to_owned();
        let Some(release) = config.release.as_ref() else {
            panic!("docker fixture must carry a release contract")
        };
        assert!(super::release_contract_complete(release));
        let workflow = super::render_release(&config, release);
        let platform = yaml_job(&workflow, "image-platform");
        assert!(platform.contains("platform: linux/amd64"), "{platform}");
        assert!(platform.contains("platform: linux/arm64"), "{platform}");
        assert!(platform.contains("file: Dockerfile"), "{platform}");
        assert!(platform.contains("context: \".\""), "{platform}");
        let image = yaml_job(&workflow, "image");
        assert!(image.contains("--archs amd64,arm64"), "{image}");
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn docker_contract_rejects_unknown_platforms_and_missing_images() {
        let mut unknown = docker_spec();
        unknown.platforms = vec!["linux/riscv64".to_owned()];
        assert!(!super::release_contract_complete(&unknown));
        let unknown_config = config(&["release.yml"], Some(unknown));
        let Some(release) = unknown_config.release.as_ref() else {
            panic!("docker fixture must carry a release contract")
        };
        assert!(
            super::render_release(&unknown_config, release).contains("# Release omitted"),
            "an unknown platform must omit the publisher instead of silently dropping it"
        );
        let mut missing = docker_spec();
        missing.image.clear();
        assert!(!super::release_contract_complete(&missing));
        let missing_config = config(&["release.yml"], Some(missing));
        let Some(release) = missing_config.release.as_ref() else {
            panic!("docker fixture must carry a release contract")
        };
        assert!(
            super::render_release(&missing_config, release).contains("# Release omitted"),
            "a missing image must omit the publisher"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn docker_single_platform_renders_a_singleton_verified_set() {
        let mut spec = docker_spec();
        spec.platforms = vec!["linux/arm64".to_owned()];
        let mut config = config(&["release.yml"], Some(spec));
        config.repository = "example/app".to_owned();
        let Some(release) = config.release.as_ref() else {
            panic!("docker fixture must carry a release contract")
        };
        assert!(super::release_contract_complete(release));
        let workflow = super::render_release(&config, release);
        let platform = yaml_job(&workflow, "image-platform");
        assert!(!platform.contains("- arch: amd64"), "{platform}");
        assert!(platform.contains("- arch: arm64"), "{platform}");
        let image = yaml_job(&workflow, "image");
        assert!(image.contains("--archs arm64"), "{image}");
        assert!(
            image.contains(")\" = \"1\" ]"),
            "a singleton set must expect exactly one manifest: {image}"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_identity_platform_emits_provenance_and_sbom() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        let platform = yaml_job(&workflow, "image-platform");
        assert!(platform.contains("provenance: true"), "{platform}");
        assert!(platform.contains("sbom: true"), "{platform}");
        assert!(platform.contains("id-token: write"), "{platform}");
        assert!(platform.contains("attestations: write"), "{platform}");
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_identity_index_rejects_an_unexpected_platform_set() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        let image = yaml_job(&workflow, "image");
        assert!(
            image.contains("does not reference both newly built platform digests"),
            "{image}"
        );
        assert!(
            image.contains("carries an unexpected platform set"),
            "{image}"
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
            publish.contains("GHCR_IMAGE: \"ghcr.io/example/app\""),
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
    fn native_identity_publish_verifies_attestations_before_creating_the_release() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        let publish = yaml_job(&workflow, "publish");
        // Missing attestations fail the release: both provenance gates run
        // before record assembly and release creation, so `gh attestation
        // verify` exiting nonzero on an unattested subject aborts the
        // publish job before any mutation.
        let tarball_at = must_some(
            publish.find("name: Verify tarball provenance"),
            "tarball provenance gate renders",
        );
        let deb_at = must_some(
            publish.find("name: Verify deb provenance"),
            "deb provenance gate renders",
        );
        let assembly_at = must_some(
            publish.find("Assemble the release record from downloaded artifacts"),
            "record assembly renders",
        );
        let create_at = must_some(
            publish.find("gh release create"),
            "release creation renders",
        );
        assert!(
            tarball_at < deb_at && deb_at < assembly_at && assembly_at < create_at,
            "provenance gates must run before record assembly and release creation: {publish}"
        );
        // Tarballs verify against the producing repo; debs verify against
        // the pinned package-signer workflow, never an ambient signer.
        assert!(
            publish.contains("for artifact in artifacts/*.tar.gz; do gh attestation verify \"$artifact\" --repo \"$GITHUB_REPOSITORY\"; done"),
            "{publish}"
        );
        assert!(
            publish.contains("--signer-workflow \"$GITHUB_REPOSITORY/.github/workflows/ci-release-package-signer.yml\""),
            "{publish}"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_release_lanes_plan_their_full_selection_before_running() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        // `run` requires the planned-selection file; release legs have no
        // Planning job, so each lane plans its own full selection first.
        // Without this step every lane fails reading the missing file.
        for id in [
            "release-github-hosted-rust-example",
            "release-velnor-rust-example",
        ] {
            let leg = yaml_job(&workflow, id);
            let plan_at = must_some(
                leg.find("name: Plan full release selection"),
                "selection step renders",
            );
            let run_at = must_some(leg.find("velnor-workflow run --config"), "run renders");
            assert!(plan_at < run_at, "{leg}");
            assert!(
                leg.contains("VELNOR_SELECTION_FILE: .velnor-ci-selection/velnor-ci-selection"),
                "{leg}"
            );
            assert!(
                leg.contains("velnor-workflow plan --config .github/ci/project.toml"),
                "{leg}"
            );
        }
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_release_local_lanes_keep_the_legacy_trusted_gate() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        // Local lanes never read the dispatch provider subset: they run
        // on tag pushes via the trusted gate and skip tag dispatches, so
        // a dispatch can only ever declare the github-hosted scope. The
        // admit job rejects anything else loudly.
        let velnor = yaml_job(&workflow, "release-velnor-rust-example");
        assert!(!velnor.contains("event.inputs.providers"), "{velnor}");
        // Tag pushes keep the trusted gate.
        assert!(velnor.contains("(github.event_name == 'push'"), "{velnor}");
        // Hosted lanes stay ungated: the build gate enforces their scope.
        let hosted = yaml_job(&workflow, "release-github-hosted-rust-example");
        let header_end = must_some(hosted.find("    steps:"), "leg steps render");
        assert!(!hosted[..header_end].contains("if: ${{"), "{hosted}");
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_release_build_gate_declares_provider_scope_on_dispatch_and_full_scope_on_push() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        let build = yaml_job(&workflow, "build");
        // always() plus explicit result checks (the image job's
        // established pattern): static needs cannot express a declared
        // provider scope.
        assert!(
            build.contains("always() && needs.verify.result == 'success'"),
            "{build}"
        );
        // Hosted lanes run on every event and are required on every
        // event; local lanes skip tag dispatches and are required off
        // dispatches.
        assert!(
            build.contains("(needs.release-github-hosted-rust-example.result == 'success')"),
            "{build}"
        );
        assert!(
            build.contains("(github.event_name == 'workflow_dispatch' || ("),
            "{build}"
        );
        for id in [
            "release-velnor-rust-example",
            "release-github-self-hosted-rust-example",
        ] {
            assert!(
                build.contains(&format!("needs.{id}.result == 'success'")),
                "{build}"
            );
        }
        // A dispatch declares github-hosted-only scope at a tag (the
        // admit job rejects anything else loudly) plus an adoptable
        // image index, so a fresh-version dispatch fails closed at the
        // build.
        assert!(
            build.contains("github.ref_type == 'tag' && contains(format(',{0},', github.event.inputs.providers), ',github-hosted,')"),
            "{build}"
        );
        assert!(
            build.contains("!contains(format(',{0},', github.event.inputs.providers), ',velnor,')"),
            "{build}"
        );
        assert!(
            build.contains("(github.event_name != 'workflow_dispatch' || needs.image-admission.outputs.existing == 'true')"),
            "{build}"
        );
        // The build waits for admission (read-only beside the lanes) to
        // read its adoptable-index output.
        assert!(
            build.contains("needs: [verify, image-admission,"),
            "{build}"
        );
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_release_image_adopts_but_never_assembles_on_dispatch() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        // The index assembly opens on dispatches only when admission
        // verified an existing tag; that branch inspects without pushing,
        // so no dispatch can mint version bytes (and a fresh-version
        // dispatch still refuses outright).
        let image = yaml_job(&workflow, "image");
        assert!(
            image.contains("(github.event_name != 'workflow_dispatch' || needs.image-admission.outputs.existing == 'true')"),
            "{image}"
        );
        assert!(
            image.contains("name: Assemble and inspect immutable image index"),
            "{image}"
        );
        assert!(image.contains("imagetools create"), "{image}");
    }

    #[test]
    #[expect(
        clippy::panic,
        reason = "the fixture construction must fail loudly if it loses its release contract"
    )]
    fn native_release_admit_declares_the_dispatch_provider_scope() {
        let config = native_identity_config(&["release.yml", "preview.yml"]);
        let Some(release) = config.release.as_ref() else {
            panic!("identity fixture must carry a release contract")
        };
        let workflow = super::render_release(&config, release);
        let admit = yaml_job(&workflow, "admit-provider");
        assert!(
            admit.contains("Reject Velnor-only native release"),
            "{admit}"
        );
        // Local lanes on a tag dispatch fail loudly (local lanes skip
        // dispatches, so a silent skip would greenwash the request).
        assert!(
            admit.contains("Reject local providers on tag dispatch"),
            "{admit}"
        );
        assert!(
            admit.contains(
                "tag dispatch publishes github-hosted only; use a tag push for full provider scope"
            ),
            "{admit}"
        );
        assert!(admit.contains("Declare release provider scope"), "{admit}");
        assert!(
            admit.contains("::notice::BOOTSTRAP release scope: github-hosted only (local qualification deferred)"),
            "{admit}"
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
            debian.contains("name: github-hosted-${{ matrix.target }}"),
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
        // Guest/rootfs/deb can run 180 minutes. A newer main must wait, not
        // cancel, or the rolling replace never publishes.
        assert!(
            preview.contains(
                "concurrency:\n  group: preview-${{ github.repository }}\n  cancel-in-progress: false\n"
            ),
            "{preview}"
        );
        assert!(!preview.contains("cancel-in-progress: true"), "{preview}");
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
                crate::s2::scan::scan_shape(
                    &root,
                    &std::collections::BTreeSet::from([
                        crate::s2::provider::ProviderId::GithubHosted,
                    ]),
                    "main",
                    &[],
                ),
                "scan guest fixture",
            );
            let mut scanned = crate::s2::ProjectConfig::from(shape.clone());
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
                image_package: String::new(),
                source_repository: String::new(),
                consumer_repository: format!("{package}/apt"),
                artifact_path: String::new(),
                description: String::new(),
                manifest_schema: String::new(),
                dockerfile: String::new(),
                context: String::new(),
                platforms: Vec::new(),
                producer_workflow: String::new(),
                producer_conclusion: String::new(),
                modes: Vec::new(),
                archive_members: Vec::new(),
                archive_checksum: String::new(),
                archive_retention_days: 0,
                credentials: Vec::new(),
                tag_pattern: String::new(),
                registry: String::new(),
                registry_username_secret: String::new(),
                registry_password_secret: String::new(),
                jobs: Vec::new(),
            });
            let surface = must(
                super::super::generate(&root, &shape, &scanned, None),
                "generate guest surface",
            );
            let preview = rendered(&surface, "preview.yml");
            let release = rendered(&surface, "release.yml");
            must(
                crate::s2::validate_guest_seed_lifecycle(&preview, "main"),
                "preview guest seed lifecycle",
            );
            must(
                crate::s2::validate_guest_seed_lifecycle(&release, "main"),
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
                !preview.contains("ln -sf /usr/local/bin/mold"),
                "mold must not replace the system linker: {preview}"
            );
            assert!(
                !preview.contains("ln -sf /usr/bin/ld.bfd /usr/bin/ld"),
                "guest kernel build must not need a linker restore shim: {preview}"
            );
            assert!(!preview.contains("velnor-guest-"), "{preview}");
            assert!(
                !preview.contains("velnor-workflow release package-guest"),
                "{preview}"
            );
            assert!(
                release.contains("needs: [admit-provider, verify, build, guest-payload]"),
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
            // The upload must name the exact 5-file consumer contract: a
            // `dist/microvm/*` wildcard once walked build scratch into a
            // symlink escape onto a root-only host path (EACCES scandir).
            let upload = format!(
                "          path: |\n            dist/microvm/vmlinux\n            dist/microvm/rootfs.ext4\n            dist/microvm/rootfs.sha256\n            dist/microvm/{agent}\n            dist/microvm/guest-agent.sha256\n"
            );
            for workflow in [&preview, &release] {
                let payload = yaml_job(workflow, "guest-payload");
                assert!(
                    payload.contains(&upload),
                    "guest payload upload must list the exact consumer files: {payload}"
                );
                assert!(
                    !workflow.contains("dist/microvm/*"),
                    "guest payload upload must not use a wildcard: {workflow}"
                );
            }
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
                crate::s2::scan::scan_shape(
                    &root,
                    &std::collections::BTreeSet::from([
                        crate::s2::provider::ProviderId::GithubHosted,
                    ]),
                    "main",
                    &[],
                ),
                "scan identity fixture",
            );
            let mut scanned = crate::s2::ProjectConfig::from(shape.clone());
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
                image_package: String::new(),
                source_repository: format!("{package}/app"),
                consumer_repository: format!("{package}/apt"),
                artifact_path: String::new(),
                description: String::new(),
                manifest_schema: "example.test/consumer-manifest-v1".to_owned(),
                dockerfile: String::new(),
                context: String::new(),
                platforms: Vec::new(),
                producer_workflow: String::new(),
                producer_conclusion: String::new(),
                modes: Vec::new(),
                archive_members: Vec::new(),
                archive_checksum: String::new(),
                archive_retention_days: 0,
                credentials: Vec::new(),
                tag_pattern: String::new(),
                registry: String::new(),
                registry_username_secret: String::new(),
                registry_password_secret: String::new(),
                jobs: Vec::new(),
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
                release.contains(&format!("GHCR_IMAGE: \"ghcr.io/{package}/app\"")),
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
    /// A binary contract with every tarball binding declared: a trusted
    /// producer, drill modes, archive contents with a manifest schema and
    /// retention, and one credential pair. All names are fixture-local.
    fn bound_spec() -> ReleaseSpec {
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
            image_package: String::new(),
            source_repository: String::new(),
            consumer_repository: String::new(),
            artifact_path: String::new(),
            description: String::new(),
            manifest_schema: "example.test/consumer-manifest-v1".to_owned(),
            dockerfile: String::new(),
            context: String::new(),
            platforms: Vec::new(),
            producer_workflow: "CI".to_owned(),
            producer_conclusion: "success".to_owned(),
            modes: vec![
                "validate".to_owned(),
                "build".to_owned(),
                "rehearse".to_owned(),
            ],
            archive_members: vec!["example-role".to_owned()],
            archive_checksum: "sha256".to_owned(),
            archive_retention_days: 14,
            credentials: vec![ReleaseCredential {
                name: "store-fixture".to_owned(),
                setup: "setup-store-fixture".to_owned(),
                teardown: "teardown-store-fixture".to_owned(),
            }],
            tag_pattern: String::new(),
            registry: String::new(),
            registry_username_secret: String::new(),
            registry_password_secret: String::new(),
            jobs: Vec::new(),
        }
    }

    /// The bound producer renders the `workflow_run` trigger with its
    /// source-resolution and publish-gate jobs, and the rolling publish
    /// admits only the gate's `publish` mode.
    #[test]
    fn preview_producer_binding_renders_source_gate_and_wired_publish() {
        let root = scanned_root("preview-binding");
        let config = config(&["preview.yml"], Some(bound_spec()));
        let surface = generate(&root, &config, None);
        let preview = rendered(&surface, "preview.yml");
        assert!(
            preview.contains("  workflow_run:\n    workflows: [CI]\n    types: [completed]\n    branches: [main]\n"),
            "the bound producer must render its trigger: {preview}"
        );
        let source = yaml_job(&preview, "source");
        assert!(
            source.contains("release resolve-source"),
            "the source job must resolve one revision: {source}"
        );
        let gate = yaml_job(&preview, "publish-gate");
        assert!(
            gate.contains("release admit-producer") && gate.contains("EXPECTED: CI"),
            "the gate must admit the trusted producer: {gate}"
        );
        assert!(
            gate.contains("unsupported rolling event"),
            "unknown events must fail closed: {gate}"
        );
        assert!(
            !gate.contains("sleep") && !gate.contains("gh run list"),
            "the gate resolves, never polls: {gate}"
        );
        let build = yaml_job(&preview, "build");
        assert!(
            build.contains("    needs: [source]\n"),
            "the build must wait for the resolved source: {build}"
        );
        assert!(
            build.contains("          ref: ${{ needs.source.outputs.sha }}\n"),
            "the build must pin the resolved source: {build}"
        );
        let publish = yaml_job(&preview, "publish");
        assert!(
            publish.contains("    needs: [build, publish-gate]\n")
                && publish.contains(
                    "    if: ${{ needs.publish-gate.outputs.admitted == 'true' && needs.publish-gate.outputs.mode == 'publish' }}\n"
                ),
            "the rolling publish must admit only the gate's publish mode: {publish}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Dispatch modes resolve through the runtime matrix: `publish` is
    /// never an option, and a rehearsal finishes without waiting for
    /// default-branch CI because the gate has no wait loop at all.
    #[test]
    fn preview_dispatch_modes_resolve_without_waiting() {
        let root = scanned_root("preview-modes");
        let config = config(&["preview.yml"], Some(bound_spec()));
        let surface = generate(&root, &config, None);
        let preview = rendered(&surface, "preview.yml");
        assert!(
            preview.contains("      mode:\n")
                && preview.contains("          - validate\n")
                && preview.contains("          - build\n")
                && preview.contains("          - rehearse\n"),
            "dispatch must offer the declared drill modes: {preview}"
        );
        assert!(
            !preview.contains("          - publish\n"),
            "publish must never be a dispatch option: {preview}"
        );
        let gate = yaml_job(&preview, "publish-gate");
        assert!(
            gate.contains("resolve-mode --event \"$EVENT\" --input"),
            "dispatches must resolve their drill mode: {gate}"
        );
        assert!(
            !gate.contains("sleep"),
            "a rehearsal must finish without waiting: {gate}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Declared modes gate the tag check and the publish job on `publish`
    /// while the build runs trusted refs in every mode and rehearses a
    /// feature dispatch; the immutable publish path keeps its no-clobber
    /// create.
    #[test]
    fn release_modes_gate_tag_check_and_publish() {
        let root = scanned_root("release-modes");
        let config = config(&["release.yml"], Some(bound_spec()));
        let surface = generate(&root, &config, None);
        let release = rendered(&surface, "release.yml");
        assert!(
            release.contains("  workflow_dispatch:\n    inputs:\n      mode:\n"),
            "modes must render a dispatch mode input: {release}"
        );
        let verify = yaml_job(&release, "verify");
        assert!(
            verify.contains("      mode: ${{ steps.mode.outputs.mode }}\n")
                && verify.contains("      - name: Resolve release mode\n"),
            "verify must resolve and output the mode: {verify}"
        );
        assert!(
            verify.contains(
                "      - name: Verify tag\n        if: ${{ steps.mode.outputs.mode == 'publish' }}\n"
            ),
            "the tag check must be publish-only: {verify}"
        );
        let build = yaml_job(&release, "build");
        assert!(
            build.contains(&format!(
                "    if: ${{{{ {} || (github.event_name == 'workflow_dispatch' && needs.verify.outputs.mode == 'rehearse') }}}}",
                trusted_release_runner_gate("main")
            )),
            "the build must run trusted refs in every mode and rehearse on any ref: {build}"
        );
        let publish = yaml_job(&release, "publish");
        assert!(
            publish.contains("    if: ${{ needs.verify.outputs.mode == 'publish' }}\n")
                && publish.contains("gh release create")
                && !publish.contains("--clobber"),
            "publish must be a gated no-clobber create: {publish}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// A dispatch rehearsal on a feature branch builds but never publishes:
    /// the build `if:` carries a rehearse arm that passes off main, while
    /// the publish `if:` still demands the `publish` mode a dispatch can
    /// never resolve to.
    #[test]
    fn dispatch_rehearsal_on_a_feature_branch_builds_without_publishing() {
        let root = scanned_root("dispatch-rehearsal");
        let config = config(&["release.yml", "preview.yml"], Some(bound_spec()));
        let surface = generate(&root, &config, None);
        let release = rendered(&surface, "release.yml");
        let build = yaml_job(&release, "build");
        assert!(
            build.contains("github.event_name == 'workflow_dispatch' && needs.verify.outputs.mode == 'rehearse'"),
            "the stable build must rehearse a feature dispatch: {build}"
        );
        let publish = yaml_job(&release, "publish");
        assert!(
            publish.contains("    if: ${{ needs.verify.outputs.mode == 'publish' }}\n"),
            "the stable publish must stay skipped for a rehearsal: {publish}"
        );
        let preview = rendered(&surface, "preview.yml");
        let preview_build = yaml_job(&preview, "build");
        assert!(
            preview_build
                .contains("github.event_name == 'workflow_dispatch' && inputs.mode == 'rehearse'"),
            "the rolling build must rehearse a feature dispatch: {preview_build}"
        );
        let preview_publish = yaml_job(&preview, "publish");
        assert!(
            preview_publish.contains("needs.publish-gate.outputs.mode == 'publish'"),
            "the rolling publish must stay skipped for a rehearsal: {preview_publish}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// A modeless native image lane never publishes from a dispatch: the
    /// staging lane and the index assembly both exclude the dispatch event,
    /// so a dispatch-on-tag cannot orphan a version index no later tag push
    /// can adopt.
    #[test]
    fn modeless_native_image_lane_refuses_dispatch() {
        let root = scanned_root("modeless-image-dispatch");
        let config = native_identity_config(&["release.yml"]);
        let surface = generate(&root, &config, None);
        let release = rendered(&surface, "release.yml");
        let platform = yaml_job(&release, "image-platform");
        assert!(
            platform.contains("github.event_name != 'workflow_dispatch'"),
            "staging must skip dispatches: {platform}"
        );
        let image = yaml_job(&release, "image");
        assert!(
            image.contains("github.event_name != 'workflow_dispatch'"),
            "the index assembly must skip dispatches: {image}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// The native rolling publish replaces only on default-branch pushes: a
    /// dispatch to main must not move the rolling release. A producer-bound
    /// lane keeps its admitted-producer path, which publishes off the
    /// `workflow_run` event a push-only gate would refuse.
    #[test]
    fn native_preview_publish_replaces_on_push_only() {
        let root = scanned_root("native-preview-push-only");
        let config = native_identity_config(&["preview.yml"]);
        let surface = generate(&root, &config, None);
        let preview = rendered(&surface, "preview.yml");
        let publish = yaml_job(&preview, "publish");
        assert!(
            publish.contains(
                "    if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}\n"
            ),
            "the unbound rolling publish must require a push: {publish}"
        );
        let _ = fs::remove_dir_all(root);

        let root = scanned_root("native-preview-bound-publish");
        let mut config = native_identity_config(&["preview.yml"]);
        if let Some(release) = config.release.as_mut() {
            release.producer_workflow = "CI".to_owned();
            release.producer_conclusion = "success".to_owned();
        }
        let surface = generate(&root, &config, None);
        let preview = rendered(&surface, "preview.yml");
        let publish = yaml_job(&preview, "publish");
        assert!(
            publish.contains("needs.publish-gate.outputs.mode == 'publish'")
                && !publish.contains("github.event_name == 'push'"),
            "the bound rolling publish must admit the producer run: {publish}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// One `TAG_IMMUTABILITY_STEP` block, from its header through the line
    /// before the next step.
    fn immutability_step<'a>(publish: &'a str, lane: &str) -> &'a str {
        let header = "      - name: Verify release tag stayed immutable before publication\n";
        let start = must_some(
            publish.find(header),
            &format!("the {lane} publish must re-verify the tag: {publish}"),
        );
        let rest = &publish[start + header.len()..];
        let end = rest
            .find("\n      - name: ")
            .map_or(rest.len(), |offset| offset + 1);
        &publish[start..start + header.len() + end]
    }

    /// Every immutable publisher re-verifies the release tag immediately
    /// before creating the release: the binary and non-identity native lanes
    /// carry the same step the Debian lane pins, so a tag moved between
    /// verify and publish refuses instead of binding the release to bytes
    /// built from another commit.
    #[test]
    fn binary_publish_reverifies_the_release_tag_before_creating() {
        let root = scanned_root("binary-tag-immutability");
        let binary_config = config(&["release.yml"], Some(binary_spec()));
        let surface = generate(&root, &binary_config, None);
        let release = rendered(&surface, "release.yml");
        let publish = yaml_job(&release, "publish");
        let step = immutability_step(publish, "binary");
        assert!(
            step.contains("git ls-remote --exit-code origin")
                && step.contains("moved from $EXPECTED_TAG_COMMIT to $remote_tag_commit"),
            "the binary publish must refuse a moved tag: {step}"
        );
        let create = must_some(
            publish.find("      - name: Publish immutable GitHub release\n"),
            "the binary create step",
        );
        let check = must_some(
            publish.find("      - name: Verify release tag stayed immutable"),
            "the binary tag check",
        );
        assert!(
            check < create,
            "the tag check must run before the release create: {publish}"
        );
        assert!(
            publish.contains("      - name: Checkout\n        uses: actions/checkout@"),
            "the publish checkout must exist for ls-remote: {publish}"
        );
        let _ = fs::remove_dir_all(root);

        let root = scanned_root("native-tag-immutability");
        let identity_config = native_identity_config(&["release.yml"]);
        let surface = generate(&root, &identity_config, None);
        let native = rendered(&surface, "release.yml");
        let native_publish = yaml_job(&native, "publish");
        assert_eq!(
            immutability_step(native_publish, "native identity"),
            step,
            "both immutable lanes must carry the same tag check"
        );
        let _ = fs::remove_dir_all(root);

        let root = scanned_root("native-plain-tag-immutability");
        let plain_config = config(&["release.yml"], Some(native_spec()));
        let surface = generate(&root, &plain_config, None);
        let plain = rendered(&surface, "release.yml");
        let plain_publish = yaml_job(&plain, "publish");
        assert_eq!(
            immutability_step(plain_publish, "native without identity"),
            step,
            "the non-identity native lane must carry the same tag check"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// The archive contract renders multi-member deterministic packaging,
    /// manifest assembly over declared subjects, and the declared
    /// retention — on the archive lane only.
    #[test]
    fn archive_contract_renders_members_manifest_and_retention() {
        let root = scanned_root("archive-contract");
        let config = config(&["release.yml", "preview.yml"], Some(bound_spec()));
        let surface = generate(&root, &config, None);
        let release = rendered(&surface, "release.yml");
        assert!(
            release.contains("--members 'example-role' --deterministic \"$deterministic\""),
            "packaging must carry the declared members: {release}"
        );
        assert!(
            release.contains("*-apple-darwin) deterministic=false"),
            "Apple rows must keep their platform tar: {release}"
        );
        assert!(
            release.contains("release assemble-manifest --dir dist")
                && release.contains("example-${VERSION#v}-x86_64-unknown-linux-gnu.tar.gz")
                && release.contains("example.test/consumer-manifest-v1"),
            "publish must assemble the manifest over declared subjects: {release}"
        );
        let build = yaml_job(&release, "build");
        assert!(
            build.contains("          retention-days: 14\n"),
            "the archive lane must use the declared retention: {build}"
        );
        let preview = rendered(&surface, "preview.yml");
        assert!(
            preview.contains("example-preview-x86_64-unknown-linux-gnu.tar.gz"),
            "the rolling manifest must name preview subjects: {preview}"
        );
        assert!(
            preview.contains("--commit \"${{ needs.publish-gate.outputs.sha }}\""),
            "the rolling manifest must bind the resolved source: {preview}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Intermediate digests keep their own windows when the archive lane
    /// takes the declared retention.
    #[test]
    fn archive_retention_leaves_intermediate_digests_alone() {
        let root = scanned_root("archive-retention");
        let mut config = native_identity_config(&["release.yml"]);
        let mut spec = native_spec();
        spec.archive_members = vec!["example-role".to_owned()];
        spec.archive_retention_days = 14;
        config.release = Some(spec);
        let surface = generate(&root, &config, None);
        let release = rendered(&surface, "release.yml");
        let build = yaml_job(&release, "build");
        assert!(
            build.contains("          retention-days: 14\n"),
            "the archive lane must use the declared retention: {build}"
        );
        let image = yaml_job(&release, "image");
        assert!(
            image.contains("          retention-days: 2\n"),
            "intermediate digests must keep their own window: {image}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Every setup renders with its teardown trapped first, an explicit
    /// unmount before supply-chain sidecars, and an `if: always()` restore
    /// after the upload — so success, failure, cancellation, and timeout
    /// all restore host state.
    #[test]
    fn credential_pairing_traps_then_always_restores() {
        let root = scanned_root("credential-pairing");
        let config = config(&["release.yml", "preview.yml"], Some(bound_spec()));
        let surface = generate(&root, &config, None);
        for file in ["release.yml", "preview.yml"] {
            let workflow = rendered(&surface, file);
            let build = yaml_job(&workflow, "build");
            let mount = must_some(
                build.find("      - name: Mount store-fixture credential\n"),
                "mount step",
            );
            let trap = must_some(
                build[mount..].find("trap teardown_store_fixture EXIT"),
                "trap",
            );
            let setup = must_some(build[mount..].find("setup-store-fixture"), "setup");
            assert!(
                trap < setup,
                "the teardown must trap before the setup runs: {build}"
            );
            let unmount = must_some(
                build.find(
                    "      - name: Unmount store-fixture credential before supply-chain sidecars\n",
                ),
                "unmount step",
            );
            let attest = must_some(build.find("      - name: Attest"), "attest step");
            assert!(
                unmount < attest,
                "secrets must leave before supply-chain sidecars: {build}"
            );
            let restore = must_some(
                build.find(
                    "      - name: Restore store-fixture credential state\n        if: always()\n",
                ),
                "restore step",
            );
            let upload = must_some(build.find("      - name: Upload"), "upload step");
            assert!(
                restore > upload,
                "the always-restore must follow the upload: {build}"
            );
            assert!(
                build.matches("teardown-store-fixture").count() >= 3,
                "the teardown must appear in all three steps: {build}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Bindings only the tarball publishers implement omit closed on every
    /// other kind instead of rendering a silently weaker lane.
    #[test]
    fn bindings_on_other_kinds_omit_closed() {
        let config = config(&[], None);
        let crates = ReleaseSpec {
            kind: "crates".to_owned(),
            packages: vec!["example".to_owned()],
            modes: vec!["rehearse".to_owned()],
            ..ReleaseSpec::default()
        };
        let release = render_release(&config, &crates);
        assert!(
            release.contains("render only for the `rust-binary` and `native` publishers"),
            "crates with modes must omit closed: {release}"
        );
        let pages = ReleaseSpec {
            kind: "pages".to_owned(),
            artifact_path: "dist/site".to_owned(),
            producer_workflow: "CI".to_owned(),
            ..ReleaseSpec::default()
        };
        let release = render_release(&config, &pages);
        assert!(
            release.contains("render only for the `rust-binary` and `native` publishers"),
            "pages with a producer must omit closed: {release}"
        );
    }

    /// The Debian preview rebinds its identity job onto the resolved
    /// source; every downstream job already consumes the identity commit,
    /// and the rolling publish admits only the gate.
    #[test]
    fn native_preview_binding_rewires_identity_onto_resolved_source() {
        let root = scanned_root("native-preview-binding");
        let mut config = native_identity_config(&["preview.yml"]);
        let mut spec = native_spec();
        spec.producer_workflow = "CI".to_owned();
        spec.modes = vec!["validate".to_owned(), "rehearse".to_owned()];
        config.release = Some(spec);
        let surface = generate(&root, &config, None);
        let preview = rendered(&surface, "preview.yml");
        assert!(
            preview.contains("  workflow_run:\n    workflows: [CI]\n"),
            "the Debian preview must bind the producer trigger: {preview}"
        );
        let identity = yaml_job(&preview, "identity");
        assert!(
            identity.contains("    needs: [source]\n")
                && identity.contains("          ref: ${{ needs.source.outputs.sha }}\n")
                && identity.contains("          EVENT_SHA: ${{ needs.source.outputs.sha }}\n"),
            "identity must resolve from the bound source: {identity}"
        );
        let publish = yaml_job(&preview, "publish");
        assert!(
            publish.contains("    needs: [publish-gate, ")
                && publish.contains("needs.publish-gate.outputs.mode == 'publish'"),
            "the Debian rolling publish must admit only the gate: {publish}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Drilled native modes build and verify locally but write nothing
    /// externally: staging pushes, the version index, signing, and the
    /// release creation all gate on `publish`, and verify carries one
    /// outputs block.
    #[test]
    fn native_release_modes_gate_external_writes() {
        let root = scanned_root("native-release-modes");
        let mut config = native_identity_config(&["release.yml"]);
        let mut spec = native_spec();
        spec.modes = vec![
            "validate".to_owned(),
            "build".to_owned(),
            "rehearse".to_owned(),
        ];
        config.release = Some(spec);
        let surface = generate(&root, &config, None);
        let release = rendered(&surface, "release.yml");
        assert!(
            release.contains("      providers:\n") && release.contains("      mode:\n"),
            "dispatch must carry both provider and mode inputs: {release}"
        );
        let verify = yaml_job(&release, "verify");
        assert_eq!(
            verify.matches("    outputs:\n").count(),
            1,
            "verify must carry one outputs block: {verify}"
        );
        assert!(
            verify.contains("      version: ${{ steps.version.outputs.version }}\n")
                && verify.contains("      mode: ${{ steps.mode.outputs.mode }}\n"),
            "verify must output both version and mode: {verify}"
        );
        assert!(
            verify.contains(
                "      - name: Resolve release version\n        id: version\n        if: ${{ steps.mode.outputs.mode == 'publish' }}\n"
            ),
            "the version resolution must be publish-only: {verify}"
        );
        let platform = yaml_job(&release, "image-platform");
        assert!(
            platform.contains("needs.verify.outputs.mode == 'publish'"),
            "staging pushes must be publish-only: {platform}"
        );
        let image = yaml_job(&release, "image");
        assert!(
            image.contains("needs.verify.outputs.mode == 'publish'"),
            "the version index must be publish-only: {image}"
        );
        let sign = yaml_job(&release, "sign-deb");
        assert!(
            sign.contains("    if: ${{ needs.verify.outputs.mode == 'publish' }}\n"),
            "signing must be publish-only: {sign}"
        );
        let publish = yaml_job(&release, "publish");
        assert!(
            publish.contains("    if: ${{ needs.verify.outputs.mode == 'publish' }}\n"),
            "the native publish must be publish-only: {publish}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Declared binding shapes fail closed at declaration time: `publish`
    /// is never a dispatch mode, conclusions bind only `success` with a
    /// producer, members are portable names, and retention is 1-90.
    #[test]
    fn declared_binding_shapes_fail_closed() {
        let root = scanned_root("declared-bindings");
        let config = config(&[], None);
        for (name, args, expected) in [
            (
                "publish-mode",
                "kind = \"rust-binary\"\npackage = \"example\"\nbinary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\nmodes = [\"publish\"]\n",
                "never a dispatch option",
            ),
            (
                "bad-mode",
                "kind = \"rust-binary\"\npackage = \"example\"\nbinary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\nmodes = [\"ship\"]\n",
                "must be one of",
            ),
            (
                "bad-conclusion",
                "kind = \"rust-binary\"\npackage = \"example\"\nbinary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\nproducer_workflow = \"CI\"\nproducer_conclusion = \"completed\"\n",
                "must be `success`",
            ),
            (
                "lonely-conclusion",
                "kind = \"rust-binary\"\npackage = \"example\"\nbinary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\nproducer_conclusion = \"success\"\n",
                "needs `producer_workflow`",
            ),
            (
                "bad-member",
                "kind = \"rust-binary\"\npackage = \"example\"\nbinary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\narchive_members = [\"../escape\"]\n",
                "portable file names",
            ),
            (
                "bad-retention",
                "kind = \"rust-binary\"\npackage = \"example\"\nbinary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\narchive_retention_days = 91\n",
                "must be 1-90",
            ),
        ] {
            let error = match try_generate(
                &root,
                &config,
                Some(&format!(
                    "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n[declare.args]\n{args}"
                )),
            ) {
                Ok(_) => panic!("{name} must fail closed, and did not"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains(expected),
                "`{name}` must name the problem: {error}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    /// A declared preview with bindings renders them end to end through
    /// the `[[declare]]` surface.
    #[test]
    fn declared_preview_bindings_render_end_to_end() {
        let root = scanned_root("declared-preview-bindings");
        let config = config(&[], None);
        let surface = generate(
            &root,
            &config,
            Some(
                "[[declare]]\nprimitive = \"preview\"\nfile = \"preview.yml\"\n\n\
                 [declare.args]\npackage = \"example\"\nbinary = \"example\"\n\
                 targets = [\"x86_64-unknown-linux-gnu\"]\n\
                 producer_workflow = \"CI\"\nmodes = [\"validate\", \"rehearse\"]\n\
                 archive_members = [\"example-role\"]\narchive_retention_days = 7\n",
            ),
        );
        let preview = rendered(&surface, "preview.yml");
        assert!(
            preview.contains("  workflow_run:\n    workflows: [CI]\n")
                && preview.contains("  publish-gate:\n")
                && preview.contains("--members 'example-role'")
                && preview.contains("          retention-days: 7\n"),
            "declared preview bindings must render: {preview}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// The shared versioned-tool declare args over fixture-local names: the
    /// package, tag prefix, tasks, and paths below exist only in this test.
    fn versioned_tool_args() -> &'static str {
        "kind = \"versioned-tool\"\npackage = \"example-tool\"\nbinary = \"example-tool\"\n\
         targets = [\"x86_64-unknown-linux-gnu\", \"aarch64-unknown-linux-gnu\"]\n\
         version_manifest = \"crates/example-tool/Cargo.toml\"\n\
         version_prefix = \"example-tool-v\"\npublish_group = \"example-tap-publish\"\n\
         version_gate_tasks = [\"check-example-tool-version\"]\n\
         assert_tasks = [\"assert-example-tool-published\"]\n\
         build_tasks = [\"build-example-tool\"]\n\
         push_paths = [\"crates/example-tool/**\"]\n\
         pull_request_paths = [\"crates/example-tool/**\", \"Cargo.lock\"]\n"
    }

    fn versioned_tool_row(file: &str) -> String {
        format!(
            "[[declare]]\nprimitive = \"release\"\nfile = \"{file}\"\n\n[declare.args]\n{}",
            versioned_tool_args()
        )
    }

    /// A versioned-tool row renders its own file end to end: per-row name,
    /// main-branch triggers over declared paths, a providers dispatch,
    /// PR-only file concurrency, and the five-job version graph.
    #[test]
    fn versioned_tool_row_renders_the_five_job_graph() {
        let root = scanned_root("versioned-tool-graph");
        let config = config(&[], None);
        let surface = generate(
            &root,
            &config,
            Some(&versioned_tool_row("example-tool.yml")),
        );
        assert_eq!(surface.added_files, vec!["example-tool.yml".to_owned()]);
        let workflow = rendered(&surface, "example-tool.yml");
        assert!(
            workflow.contains("name: example-tool\n"),
            "the workflow name defaults to the file stem: {workflow}"
        );
        assert!(
            workflow.contains("  push:\n    branches: [main]\n    paths:\n      - \"crates/example-tool/**\"\n")
                && workflow.contains(
                    "  pull_request:\n    paths:\n      - \"crates/example-tool/**\"\n      - Cargo.lock\n"
                ),
            "push-main and pull-request triggers must carry the declared paths: {workflow}"
        );
        assert!(
            workflow.contains("      providers:\n")
                && workflow.contains("        default: github-hosted\n")
                && workflow.contains(
                    "          - github-hosted\n          - github-self-hosted\n          - velnor\n          - all\n"
                ),
            "the dispatch must offer the repository providers: {workflow}"
        );
        assert!(
            workflow.contains(
                "  group: ${{ github.workflow }}-${{ github.ref }}\n  cancel-in-progress: ${{ github.event_name == 'pull_request' }}"
            ),
            "file concurrency cancels PR runs only: {workflow}"
        );
        let gate = yaml_job(&workflow, "validate-version");
        assert!(
            gate.contains("    if: ${{ github.event_name == 'pull_request' }}")
                && gate.contains("fetch-depth: 0")
                && gate.contains("run: mise run check-example-tool-version"),
            "the gate is PR-only, full-history, and runs the declared task: {gate}"
        );
        let version = yaml_job(&workflow, "version");
        assert!(
            version.contains("runner-configs: ${{ steps.resolve.outputs.runner-configs }}")
                && version.contains("s/^version = ")
                && version.contains("configs='[{\"provider\":\"github-hosted\"")
                && version.contains("velnor) configs='[{\"provider\":\"velnor\""),
            "the version job resolves the manifest version and the provider configs: {version}"
        );
        let assert_job = yaml_job(&workflow, "assert-version");
        assert!(
            assert_job.contains("    needs: [version]")
                && assert_job.contains("gh release view \"$tag\"")
                && assert_job.contains("published=true")
                && assert_job.contains("run: mise run assert-example-tool-published")
                && assert_job.contains("if: ${{ github.ref == 'refs/heads/main' }}"),
            "the assert job checks reuse and gates product tasks to main: {assert_job}"
        );
        let build = yaml_job(&workflow, "build");
        assert!(
            build.contains("    needs: [version, assert-version]")
                && build.contains("github.event_name != 'pull_request' && needs.assert-version.outputs.published != 'true'")
                && build.contains("runs-on: ${{ matrix.config.runner }}")
                && build.contains("config: ${{ fromJSON(needs.version.outputs.runner-configs) }}")
                && build.contains("run: mise run build-example-tool")
                && build.contains("package-binary --target \"${{ matrix.target }}\" --version \"$VERSION\""),
            "the build crosses targets with provider configs and packages the task output: {build}"
        );
        let publish = yaml_job(&workflow, "publish");
        assert!(
            publish.contains("    needs: [version, assert-version, build]")
                && publish.contains("github.ref == 'refs/heads/main' && needs.assert-version.outputs.published != 'true'")
                && publish.contains("      group: example-tap-publish")
                && publish.contains("tag=\"example-tool-v$VERSION\"")
                && publish.contains("gh release create \"$tag\" dist/* --target \"$COMMIT\" --title \"$tag\" --generate-notes"),
            "the publish mints the immutable versioned release under its mutex: {publish}"
        );
        assert!(
            !workflow.contains("verify-tag") && !workflow.contains("clobber"),
            "no tag check and no clobber render for a main-branch file: {workflow}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// The row's `name` overrides the file stem, so two files can still be
    /// told apart when stems collide across extensions.
    #[test]
    fn versioned_tool_name_arg_overrides_the_file_stem() {
        let root = scanned_root("versioned-tool-name");
        let config = config(&[], None);
        let row = versioned_tool_row("example-tool.yml").replace(
            "kind = \"versioned-tool\"\n",
            "kind = \"versioned-tool\"\nname = \"Example Tool\"\n",
        );
        let surface = generate(&root, &config, Some(&row));
        let workflow = rendered(&surface, "example-tool.yml");
        assert!(
            workflow.contains("name: \"Example Tool\"\n"),
            "the declared name must render: {workflow}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Two versioned-tool rows render two identities: two files, two names,
    /// two tag streams.
    #[test]
    fn second_versioned_tool_row_renders_its_own_identity() {
        let root = scanned_root("versioned-tool-second");
        let config = config(&[], None);
        let other = versioned_tool_row("other-tool.yml").replace("example-tool", "other-tool");
        let rows = format!("{}\n{}", versioned_tool_row("example-tool.yml"), other);
        let surface = generate(&root, &config, Some(&rows));
        let first = rendered(&surface, "example-tool.yml");
        let second = rendered(&surface, "other-tool.yml");
        assert!(
            first.contains("name: example-tool\n")
                && first.contains("tag=\"example-tool-v$VERSION\""),
            "the first publisher keeps its identity: {first}"
        );
        assert!(
            second.contains("name: other-tool\n")
                && second.contains("tag=\"other-tool-v$VERSION\""),
            "the second publisher renders its own identity: {second}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Two publishers under one workflow name fail closed: the shared name
    /// would collapse two publishers into one status context.
    #[test]
    fn duplicate_versioned_tool_name_fails_closed() {
        let root = scanned_root("versioned-tool-duplicate-name");
        let config = config(&[], None);
        let other = versioned_tool_row("other-tool.yml")
            .replace(
                "version_prefix = \"example-tool-v\"",
                "version_prefix = \"other-tool-v\"",
            )
            .replace(
                "kind = \"versioned-tool\"\n",
                "kind = \"versioned-tool\"\nname = \"example-tool\"\n",
            );
        let rows = format!("{}\n{}", versioned_tool_row("example-tool.yml"), other);
        let error = match try_generate(&root, &config, Some(&rows)) {
            Ok(_) => panic!("a duplicate workflow name must fail closed, and did not"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("renders duplicate workflow name `example-tool`")
                && error.contains("give each publisher its own `name`"),
            "the duplicate name must name the problem: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Two publishers under one tag prefix fail closed: one tag stream
    /// cannot have two lanes.
    #[test]
    fn duplicate_versioned_tool_prefix_fails_closed() {
        let root = scanned_root("versioned-tool-duplicate-prefix");
        let config = config(&[], None);
        let rows = format!(
            "{}\n{}",
            versioned_tool_row("example-tool.yml"),
            versioned_tool_row("other-tool.yml")
        );
        let error = match try_generate(&root, &config, Some(&rows)) {
            Ok(_) => panic!("a duplicate tag prefix must fail closed, and did not"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("publishes tag prefix `example-tool-v`")
                && error.contains("already owned by file `example-tool.yml`")
                && error.contains("other-tool.yml"),
            "the duplicate prefix must name both rows: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// The pin stays for tag-triggered kinds: only main-branch-driven rows
    /// may render outside `release.yml`.
    #[test]
    fn tag_triggered_release_row_stays_pinned() {
        let root = scanned_root("versioned-tool-pin");
        let config = config(&[], None);
        let error = match try_generate(
            &root,
            &config,
            Some(
                "[[declare]]\nprimitive = \"release\"\nfile = \"other.yml\"\n\n\
                 [declare.args]\nkind = \"rust-binary\"\npackage = \"example\"\n\
                 binary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\n",
            ),
        ) {
            Ok(_) => {
                panic!("a tag-triggered row outside release.yml must fail closed, and did not")
            }
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("must declare `release.yml`, not `other.yml`"),
            "the pin must hold for tag-triggered kinds: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Declaring `release.yml` suppresses the default row: the declared row
    /// renders the canonical file alone, and a versioned-tool row alongside
    /// it renders its own file.
    #[test]
    fn declared_release_row_suppresses_the_default_row() {
        let root = scanned_root("versioned-tool-suppression");
        let config = config(&["release.yml"], Some(binary_spec()));
        let rows = format!(
            "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n{}",
            versioned_tool_row("example-tool.yml")
        );
        let surface = generate(&root, &config, Some(&rows));
        let release = rendered(&surface, "release.yml");
        assert!(
            release.contains("name: Release\n"),
            "the declared row renders the canonical publisher: {release}"
        );
        let workflow = rendered(&surface, "example-tool.yml");
        assert!(
            workflow.contains("name: example-tool\n"),
            "the versioned-tool row renders alongside: {workflow}"
        );
        assert_eq!(
            surface
                .files
                .keys()
                .filter(|path| path.ends_with("release.yml"))
                .count(),
            1,
            "exactly one release.yml renders: {:?}",
            surface.files.keys().collect::<Vec<_>>()
        );
        let _ = fs::remove_dir_all(root);
    }

    /// An incomplete versioned-tool contract names its row and every missing
    /// field at once.
    #[test]
    fn incomplete_versioned_tool_contract_names_every_missing_field() {
        let root = scanned_root("versioned-tool-incomplete");
        let config = config(&[], None);
        let error = match try_generate(
            &root,
            &config,
            Some(
                "[[declare]]\nprimitive = \"release\"\nfile = \"example-tool.yml\"\n\n\
                 [declare.args]\nkind = \"versioned-tool\"\npackage = \"example-tool\"\n",
            ),
        ) {
            Ok(_) => panic!("an incomplete versioned-tool contract must fail closed, and did not"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains(
                "`release` file `example-tool.yml` declares an incomplete versioned-tool contract"
            ) && error.contains("`binary`")
                && error.contains("`targets`")
                && error.contains("`version_manifest`")
                && error.contains("`version_prefix`")
                && error.contains("`publish_group`")
                && error.contains("`version_gate_tasks`")
                && error.contains("`build_tasks`")
                && error.contains("`push_paths`")
                && error.contains("`pull_request_paths`")
                && !error.contains("`package`"),
            "every missing field — and only the missing ones — must be named: {error}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Shape errors name the row: a bad prefix, a non-task ref, and empty
    /// task and path lists each fail with the file that declares them.
    #[test]
    fn versioned_tool_shape_errors_name_the_row() {
        let root = scanned_root("versioned-tool-shapes");
        let config = config(&[], None);
        for (name, from, to, expected) in [
            (
                "bad-prefix",
                "version_prefix = \"example-tool-v\"",
                "version_prefix = \"example-tool\"",
                "must be a tag prefix ending in `-v`",
            ),
            (
                "bad-gate-task",
                "version_gate_tasks = [\"check-example-tool-version\"]",
                "version_gate_tasks = [\"check example-tool\"]",
                "not a plain task reference",
            ),
            (
                "empty-gate-tasks",
                "version_gate_tasks = [\"check-example-tool-version\"]",
                "version_gate_tasks = []",
                "`version_gate_tasks`",
            ),
            (
                "empty-build-tasks",
                "build_tasks = [\"build-example-tool\"]",
                "build_tasks = []",
                "`build_tasks`",
            ),
            (
                "empty-push-paths",
                "push_paths = [\"crates/example-tool/**\"]",
                "push_paths = []",
                "`push_paths`",
            ),
        ] {
            let row = versioned_tool_row("example-tool.yml").replace(from, to);
            let error = match try_generate(&root, &config, Some(&row)) {
                Ok(_) => panic!("`{name}` must fail closed, and did not"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains("example-tool.yml") && error.contains(expected),
                "`{name}` must name the row and the problem: {error}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    /// Without assert tasks the assert job still renders its generic
    /// published-reuse check, and nothing product-shaped.
    #[test]
    fn versioned_tool_without_assert_tasks_keeps_the_generic_check() {
        let root = scanned_root("versioned-tool-no-assert");
        let config = config(&[], None);
        let row = versioned_tool_row("example-tool.yml")
            .replace("assert_tasks = [\"assert-example-tool-published\"]\n", "");
        let surface = generate(&root, &config, Some(&row));
        let workflow = rendered(&surface, "example-tool.yml");
        let assert_job = yaml_job(&workflow, "assert-version");
        assert!(
            assert_job.contains("gh release view \"$tag\"")
                && !assert_job.contains("mise run")
                && !assert_job.contains("Checkout"),
            "the assert job keeps only its generic check: {assert_job}"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// The workflow name generalizes per row: tag-driven publishers share
    /// `Release`, versioned-tool rows take their name or file stem.
    #[test]
    fn release_workflow_name_generalizes_per_row() {
        fn name_of(file: &str, pairs: &[(&str, &str)]) -> String {
            let args: BTreeMap<String, toml::Value> = pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), toml::Value::String((*value).to_owned())))
                .collect();
            must(
                release_workflow_name(file, &Args(&args)),
                "resolve the workflow name",
            )
        }
        assert_eq!(
            name_of("release.yml", &[("kind", "rust-binary")]),
            "Release"
        );
        assert_eq!(name_of("release.yml", &[]), "Release");
        assert_eq!(
            name_of("example-tool.yml", &[("kind", "versioned-tool")]),
            "example-tool"
        );
        assert_eq!(
            name_of("example-tool.yaml", &[("kind", "versioned-tool")]),
            "example-tool"
        );
        assert_eq!(
            name_of(
                "example-tool.yml",
                &[("kind", "versioned-tool"), ("name", "Example Tool")]
            ),
            "Example Tool"
        );
        assert!(is_main_branch_driven_release_kind("versioned-tool"));
        assert!(!is_main_branch_driven_release_kind("rust-binary"));
        assert!(!release_contract_complete(&ReleaseSpec {
            kind: "versioned-tool".to_owned(),
            ..ReleaseSpec::default()
        }));
    }

    /// The dispatch providers and runner-configs follow the repository
    /// providers: one provider renders one option and one config, several
    /// render each plus `all`.
    #[test]
    fn versioned_tool_providers_follow_the_repository_providers() {
        let all = config(&[], None);
        assert_eq!(
            versioned_tool_providers(&all),
            vec!["github-hosted", "github-self-hosted", "velnor", "all"]
        );
        let mut hosted = all.clone();
        hosted.providers = [crate::s2::provider::ProviderId::GithubHosted]
            .into_iter()
            .collect();
        assert_eq!(versioned_tool_providers(&hosted), vec!["github-hosted"]);
        let mut velnor = all.clone();
        velnor.providers = [crate::s2::provider::ProviderId::Velnor]
            .into_iter()
            .collect();
        assert_eq!(versioned_tool_providers(&velnor), vec!["velnor"]);
        assert!(
            versioned_tool_provider_configs(&all, "all").contains("\"provider\":\"github-hosted\"")
                && versioned_tool_provider_configs(&all, "all").contains("\"provider\":\"velnor\""),
            "all providers must fan out: {}",
            versioned_tool_provider_configs(&all, "all")
        );
        assert!(
            !versioned_tool_provider_configs(&all, "github-hosted")
                .contains("\"provider\":\"velnor\""),
            "one provider must not fan out: {}",
            versioned_tool_provider_configs(&all, "github-hosted")
        );
    }

    /// Exactly one provider uploads: in `all` mode the first local provider
    /// writes and the others do not, so two providers never publish the
    /// same per-target tarball name twice; a single provider always writes.
    #[test]
    fn versioned_tool_writer_is_single_and_local_preferred() {
        let all = config(&[], None);
        let configs = versioned_tool_provider_configs(&all, "all");
        assert_eq!(
            configs.matches("\"writer\":true").count(),
            1,
            "exactly one provider must write: {configs}"
        );
        assert_eq!(
            configs.matches("\"writer\":false").count(),
            2,
            "the other providers must not write: {configs}"
        );
        let (hosted, local) = must_some(
            configs.split_once("\"provider\":\"github-self-hosted\""),
            "all mode must configure the first local provider",
        );
        assert!(
            hosted.contains("\"writer\":false") && !hosted.contains("\"writer\":true"),
            "the hosted provider must not write: {configs}"
        );
        assert!(
            local.contains("\"writer\":true"),
            "the first local provider must write: {configs}"
        );
        for single in ["github-hosted", "velnor"] {
            let configs = versioned_tool_provider_configs(&all, single);
            assert_eq!(
                configs.matches("\"writer\":true").count(),
                1,
                "a single provider must write: {configs}"
            );
            assert!(
                !configs.contains("\"writer\":false"),
                "a single provider has no non-writer: {configs}"
            );
        }
    }

    /// A declared tag filter reaches every release trigger, whichever kind
    /// renders it: the base and docker renderers share one trigger block.
    #[test]
    fn declared_tag_pattern_flows_to_every_release_trigger() {
        for (name, mut spec) in [("rust-binary", binary_spec()), ("docker", docker_spec())] {
            spec.tag_pattern = "release-*".to_owned();
            let root = scanned_root(&format!("tag-pattern-{name}"));
            let config = config(&["release.yml"], Some(spec));
            let surface = generate(&root, &config, None);
            let release = rendered(&surface, "release.yml");
            assert!(
                release.contains("tags: [\"release-*\"]"),
                "the {name} trigger must carry the declared pattern"
            );
            assert!(
                !release.contains("tags: [\"v*\"]"),
                "the declared pattern must replace the default: {release}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    /// An undeclared tag filter keeps the `v*` default: the shared trigger
    /// block must render byte-identical output for existing contracts.
    #[test]
    fn absent_tag_pattern_keeps_default_v_star_trigger() {
        for (name, spec) in [("rust-binary", binary_spec()), ("docker", docker_spec())] {
            let root = scanned_root(&format!("tag-default-{name}"));
            let config = config(&["release.yml"], Some(spec));
            let surface = generate(&root, &config, None);
            let release = rendered(&surface, "release.yml");
            assert!(
                release.contains("tags: [\"v*\"]"),
                "the undeclared {name} trigger must keep the `v*` default"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    /// A declared pattern travels the declaration path end to end: the
    /// ingest validator accepts it and the rendered trigger carries it.
    #[test]
    fn declared_tag_pattern_travels_the_declaration_path() {
        let root = scanned_root("tag-declare");
        let config = config(&[], None);
        let surface = must(
            try_generate(
                &root,
                &config,
                Some(
                    "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                     [declare.args]\nkind = \"rust-binary\"\npackage = \"example\"\n\
                     binary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\n\
                     tag_pattern = \"release-*\"\n",
                ),
            ),
            "generate release surface with a declared tag pattern",
        );
        let release = rendered(&surface, "release.yml");
        assert!(
            release.contains("tags: [\"release-*\"]"),
            "the declared trigger must carry the declared pattern"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Every malformed tag filter fails closed at declaration time: an
    /// empty, blank, or multi-line pattern never reaches a trigger.
    #[test]
    fn malformed_tag_patterns_fail_closed() {
        let rows = |pattern: &str| {
            format!(
                "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                 [declare.args]\nkind = \"rust-binary\"\npackage = \"example\"\n\
                 binary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\n\
                 tag_pattern = \"{pattern}\"\n"
            )
        };
        for (name, pattern) in [
            ("empty", String::new()),
            ("whitespace", String::from("v 1")),
            ("newline", String::from("v\\n1")),
            ("leading space", String::from(" v*")),
        ] {
            let root = scanned_root("tag-invalid");
            let config = config(&[], None);
            let error = match try_generate(&root, &config, Some(&rows(&pattern))) {
                Ok(_) => panic!("a {name} tag pattern must fail closed, and did not"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains("tag_pattern"),
                "a {name} pattern must name the problem: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    /// A declared registry triple reaches every docker login step:
    /// admission, platform, and manifest all authenticate to the declared
    /// host with the declared credential secrets.
    #[test]
    fn declared_registry_auth_reaches_every_docker_login() {
        let mut spec = docker_spec();
        spec.image = "example/app".to_owned();
        spec.registry = "docker.io".to_owned();
        spec.registry_username_secret = "REGISTRY_USERNAME".to_owned();
        spec.registry_password_secret = "REGISTRY_PASSWORD".to_owned();
        let root = scanned_root("registry-auth");
        let config = config(&["release.yml"], Some(spec));
        let surface = generate(&root, &config, None);
        let release = rendered(&surface, "release.yml");
        assert_eq!(
            release.matches("Log in to docker.io").count(),
            3,
            "admission, platform, and manifest must log in to the declared registry"
        );
        assert!(
            !release.contains("Log in to GHCR"),
            "the declared registry must replace the default login"
        );
        assert!(
            release.contains("registry: docker.io")
                && release.contains("secrets.REGISTRY_USERNAME")
                && release.contains("secrets.REGISTRY_PASSWORD"),
            "the login steps must carry the declared host and credential references"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// An undeclared registry keeps the GHCR automatic-token login in all
    /// three docker jobs: the shared login step renders byte-identical
    /// output for existing contracts.
    #[test]
    fn absent_registry_auth_keeps_ghcr_login_bytes() {
        let root = scanned_root("registry-default");
        let config = config(&["release.yml"], Some(docker_spec()));
        let surface = generate(&root, &config, None);
        let release = rendered(&surface, "release.yml");
        assert_eq!(
            release.matches("Log in to GHCR").count(),
            3,
            "all three docker jobs must keep the GHCR login"
        );
        assert_eq!(
            release.matches("secrets.GITHUB_TOKEN").count(),
            3,
            "all three logins must keep the automatic token"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// A declared triple travels the declaration path end to end: the
    /// ingest validator accepts it and every login step carries it.
    #[test]
    fn declared_registry_auth_travels_the_declaration_path() {
        let root = scanned_root("registry-declare");
        let config = config(&[], None);
        let surface = must(
            try_generate(
                &root,
                &config,
                Some(
                    "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                     [declare.args]\nkind = \"docker\"\nimage = \"example/app\"\n\
                     platforms = [\"linux/amd64\", \"linux/arm64\"]\n\
                     registry = \"docker.io\"\n\
                     registry_username_secret = \"REGISTRY_USERNAME\"\n\
                     registry_password_secret = \"REGISTRY_PASSWORD\"\n",
                ),
            ),
            "generate release surface with declared registry auth",
        );
        let release = rendered(&surface, "release.yml");
        assert_eq!(
            release.matches("Log in to docker.io").count(),
            3,
            "every docker login must carry the declared registry"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// The registry host alphabet: lowercase DNS with an optional port —
    /// nothing that could smuggle credentials or break out of YAML.
    #[test]
    fn registry_host_validation_admits_hosts_and_rejects_injection() {
        for host in [
            "docker.io",
            "ghcr.io",
            "example-registry.local:5000",
            "localhost:5000",
        ] {
            assert!(
                crate::s2::runtime::valid_registry_host(host),
                "a usable host must validate: {host}"
            );
        }
        for host in [
            "",
            "Docker.io",
            "reg istry",
            "host:abc",
            "host:1:2",
            "host:999999",
            "..",
            ".host",
            "host-",
            "user@host",
            "host/path",
        ] {
            assert!(
                !crate::s2::runtime::valid_registry_host(host),
                "an unusable host must fail: {host}"
            );
        }
    }

    /// Every malformed registry declaration fails closed: partial triples,
    /// bad hosts, bad secret names, and triples on non-docker kinds.
    #[test]
    fn malformed_registry_auth_fails_closed() {
        let triple = "registry = \"docker.io\"\n\
             registry_username_secret = \"REGISTRY_USERNAME\"\n\
             registry_password_secret = \"REGISTRY_PASSWORD\"\n";
        let docker = |extra: &str| {
            format!(
                "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                 [declare.args]\nkind = \"docker\"\nimage = \"example/app\"\n\
                 platforms = [\"linux/amd64\", \"linux/arm64\"]\n{extra}"
            )
        };
        let binary = |extra: &str| {
            format!(
                "[[declare]]\nprimitive = \"release\"\nfile = \"release.yml\"\n\n\
                 [declare.args]\nkind = \"rust-binary\"\npackage = \"example\"\n\
                 binary = \"example\"\ntargets = [\"x86_64-unknown-linux-gnu\"]\n{extra}"
            )
        };
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "registry without secrets",
                docker("registry = \"docker.io\"\n"),
                "`registry_username_secret` is missing",
            ),
            (
                "secrets without registry",
                docker(
                    "registry_username_secret = \"REGISTRY_USERNAME\"\n\
                     registry_password_secret = \"REGISTRY_PASSWORD\"\n",
                ),
                "`registry` is missing",
            ),
            (
                "uppercase host",
                docker(&triple.replace("docker.io", "Docker.io")),
                "`registry` must be a lowercase host",
            ),
            (
                "lowercase secret",
                docker(&triple.replace("REGISTRY_USERNAME", "registry_username")),
                "must be an uppercase secret name",
            ),
            (
                "automatic token as password",
                docker(&triple.replace("REGISTRY_PASSWORD", "GITHUB_TOKEN")),
                "must name a dedicated secret, not GITHUB_TOKEN",
            ),
            (
                "triple on a binary publisher",
                binary(triple),
                "renders only for kind `docker`",
            ),
        ];
        for (name, rows, expected) in cases {
            let root = scanned_root("registry-invalid");
            let config = config(&[], None);
            let error = match try_generate(&root, &config, Some(&rows)) {
                Ok(_) => panic!("{name} must fail closed, and did not"),
                Err(error) => error.to_string(),
            };
            assert!(
                error.contains(expected),
                "{name} must name the problem: {error}"
            );
            let _ = fs::remove_dir_all(root);
        }
    }
}
