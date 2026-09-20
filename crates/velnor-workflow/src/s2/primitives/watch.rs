//! `watch-graph`: the affected-selection watch paths of every unit.
//!
//! Watch paths are derived, never name-matched: the kind of a unit and the
//! dependency closure of the units a repository declares decide what a unit
//! watches, and the config adds the repository-specific paths on top. A unit id
//! appears in the config only as the key of an explicit addition, never as a
//! branch inside this primitive.

use std::collections::{BTreeMap, BTreeSet};

use super::{Args, Primitive, RenderCtx, Rendered, MUTABLE_MOUNT_SEED_CONTEXT, WATCH_GRAPH};
use crate::s2::scan::file_walk::{has_extension, is_test_support_path, join_repo_path};
use crate::s2::{DockerContext, GeneratorError, Unit, UnitKind};

/// Whole-tree crate globs duplicate per-crate units; `workspace_check` gates
/// validate topology on manifests, release infra, and explicit additions.
fn is_broad_per_crate_source_watch(path: &str) -> bool {
    matches!(path, "crates/**" | "tools/**")
}

/// Declare the repository's watch graph.
pub(crate) struct WatchGraph;

impl Primitive for WatchGraph {
    fn id(&self) -> &'static str {
        WATCH_GRAPH
    }

    fn schema(&self) -> &'static [&'static str] {
        &["watch", "docker_closure"]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let additions = args.unit_strings(ctx, "watch")?.unwrap_or_default();
        let closure = args.strings("docker_closure")?.unwrap_or_default();
        let units = render_watch_units(
            ctx.root,
            ctx.shape.files(),
            &ctx.config.units,
            ctx.units,
            &additions,
            &closure,
        )?;
        Ok(Rendered {
            units,
            ..Rendered::default()
        })
    }
}

fn render_watch_units(
    root: &std::path::Path,
    files: &[String],
    config_units: &[Unit],
    source_units: &[&Unit],
    additions: &BTreeMap<String, Vec<String>>,
    closure: &[String],
) -> Result<Vec<Unit>, GeneratorError> {
    let docker_watch = docker_watch_paths(
        root,
        files,
        config_units,
        &closure.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    let mut units = Vec::new();
    for unit in source_units {
        let mut unit = (*unit).clone();
        // A root Docker image watches what its build consumes, derived from
        // the image's dependency closure. A package unit watches what its
        // own manifest and layout proved; a family the scan could not pin
        // down watches what the scan derived for it.
        let derived = matches!(unit.kind, UnitKind::Docs | UnitKind::OpenTofu)
            || (unit.kind == UnitKind::Docker && unit.root == ".");
        let mut watch = BTreeSet::new();
        if !derived {
            watch.extend(
                unit.watch
                    .iter()
                    .filter(|path| !(unit.workspace_check && is_broad_per_crate_source_watch(path)))
                    .cloned(),
            );
        }
        if unit.kind == UnitKind::Docker && unit.root == "." {
            watch.extend(docker_watch.iter().cloned());
        }
        if unit.kind == UnitKind::Docker {
            watch.extend(docker_context_watch_paths(&unit.docker_contexts));
        }
        match unit.kind {
            // The scanner owns the package's source closure. Add only the
            // manager metadata that every package command reads, rooted at
            // the package instead of assuming the repository root.
            UnitKind::Bun => {
                watch.extend(package_manager_watch_paths(&unit));
                if unit.root.is_empty() || unit.root == "." {
                    // Keep the root package's explicit source families in
                    // addition to the scanner-owned files. Nested packages
                    // must remain scoped to their own discovered closure.
                    watch.extend(["src/**".to_owned(), "scripts/**".to_owned()]);
                }
                watch.extend(
                    files
                        .iter()
                        .filter(|file| {
                            crate::s2::parent_path(file) == unit.root
                                && (file.ends_with(".config.js")
                                    || file.ends_with(".config.ts")
                                    || file.rsplit('/').next() == Some("tsconfig.json"))
                        })
                        .cloned(),
                );
            }
            UnitKind::Docs => {
                watch.extend(
                    ["content/docs/**/*.mdx".to_owned(), "*.md".to_owned()].map(String::from),
                );
            }
            UnitKind::OpenTofu => {
                watch.extend(opentofu_watch_paths(files));
            }
            UnitKind::Rust => {
                watch.extend([
                    ".cargo/**".to_owned(),
                    "Cargo.toml".to_owned(),
                    "Cargo.lock".to_owned(),
                    "rust-toolchain.toml".to_owned(),
                    "rust-toolchain".to_owned(),
                    "mise.toml".to_owned(),
                    "mise.lock".to_owned(),
                ]);
            }
            UnitKind::Docker
            | UnitKind::Gradle
            | UnitKind::Node
            | UnitKind::Swift
            | UnitKind::Homebrew => {}
        }
        if let Some(paths) = additions.get(&unit.id) {
            watch.extend(
                paths
                    .iter()
                    .filter(|path| !(unit.workspace_check && is_broad_per_crate_source_watch(path)))
                    .cloned(),
            );
        }
        unit.watch = watch.into_iter().collect();
        units.push(unit);
    }
    Ok(units)
}

fn package_manager_watch_paths(unit: &Unit) -> impl Iterator<Item = String> {
    ["package.json", "bun.lock", "bun.lockb", "package-lock.json"]
        .into_iter()
        .map(|path| scoped_repo_path(unit.root.as_str(), path))
}

fn scoped_repo_path(root: &str, child: &str) -> String {
    if root.is_empty() || root == "." {
        child.to_owned()
    } else {
        join_repo_path(root, child)
    }
}

/// A declared named Docker context is a source input of the image. The
/// reserved cache-seed context is injected by the cache transport and is
/// performance state, not repository source. A repository-root context is
/// intentionally broad until a complete Dockerfile/context closure proves a
/// narrower set.
fn docker_context_watch_paths(contexts: &[DockerContext]) -> impl Iterator<Item = String> + '_ {
    contexts.iter().map(|context| {
        let path = context.path.trim_start_matches("./").trim_end_matches('/');
        if path.is_empty() || path == "." {
            "**".to_owned()
        } else {
            format!("{path}/**")
        }
    })
}

fn opentofu_watch_paths(files: &[String]) -> Vec<String> {
    let terraform_files = files
        .iter()
        .filter(|file| {
            !is_test_support_path(file)
                && (has_extension(file, "tf")
                    || has_extension(file, "tofu")
                    || file.rsplit('/').next() == Some(".terraform.lock.hcl"))
        })
        .cloned()
        .collect::<Vec<_>>();
    if terraform_files.is_empty() {
        // The pinned Planning runtime requires every unit to carry at least
        // one watch. Keep an explicit OpenTofu declaration affected-only
        // without borrowing an unrelated source such as workflow runtime
        // code: these globs match a future IaC file, but no current diff.
        vec![
            "**/*.tf".to_owned(),
            "**/*.tofu".to_owned(),
            "**/.terraform.lock.hcl".to_owned(),
        ]
    } else {
        terraform_files
    }
}

/// The watch paths a root Docker image consumes.
///
/// The image's own build inputs come from its `Dockerfile` `COPY` and `ADD`
/// sources; the closure adds the roots of every unit the image depends on,
/// which is what makes an image rebuild when a unit it packages changes.
pub(crate) fn docker_watch_paths(
    root: &std::path::Path,
    files: &[String],
    units: &[Unit],
    closure_seeds: &[&str],
) -> Result<Vec<String>, GeneratorError> {
    let mut watch = BTreeSet::from([".dockerignore".to_owned()]);
    let mut conservative_context = false;
    let dockerfiles = files
        .iter()
        .filter(|file| {
            !is_test_support_path(file)
                && crate::s2::parent_path(file) == "."
                && file
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.starts_with("Dockerfile"))
        })
        .collect::<Vec<_>>();
    for dockerfile in &dockerfiles {
        watch.insert((*dockerfile).clone());
    }
    for unit in units
        .iter()
        .filter(|unit| unit.kind == UnitKind::Docker && unit.root == ".")
    {
        watch.extend(docker_context_watch_paths(&unit.docker_contexts));
    }
    let declared_context_names = units
        .iter()
        .filter(|unit| unit.kind == UnitKind::Docker && unit.root == ".")
        .flat_map(|unit| unit.docker_contexts.iter())
        .map(|context| context.name.as_str())
        .collect::<BTreeSet<_>>();
    for dockerfile in dockerfiles {
        inspect_dockerfile(
            root,
            dockerfile,
            files,
            &declared_context_names,
            &mut watch,
            &mut conservative_context,
        )?;
    }
    // The dependency closure of the declared seeds, expanded through the units'
    // own dependency edges.
    let seeds = closure_seeds
        .iter()
        .filter(|id| {
            let known = units.iter().any(|unit| &unit.id == *id);
            if !known {
                conservative_context = true;
            }
            known
        })
        .map(|id| (*id).to_owned())
        .collect::<BTreeSet<_>>();
    let mut closure = seeds.clone();
    let mut pending = seeds.into_iter().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        if let Some(unit) = units.iter().find(|unit| unit.id == *id) {
            for dependency in &unit.depends_on {
                if !units.iter().any(|candidate| candidate.id == *dependency) {
                    conservative_context = true;
                } else if closure.insert(dependency.clone()) {
                    pending.push(dependency.clone());
                }
            }
        }
    }
    for unit in units.iter().filter(|unit| closure.contains(&unit.id)) {
        if unit.root != "." {
            watch.insert(format!("{}/**", unit.root));
        }
    }
    if conservative_context {
        eprintln!(
            "watch-graph: Docker source closure is incomplete or dynamic; selecting the full build context"
        );
        watch.insert("**".to_owned());
    }
    Ok(watch.into_iter().collect())
}

fn inspect_dockerfile(
    root: &std::path::Path,
    dockerfile: &str,
    files: &[String],
    declared_context_names: &BTreeSet<&str>,
    watch: &mut BTreeSet<String>,
    conservative_context: &mut bool,
) -> Result<(), GeneratorError> {
    let path = root.join(dockerfile);
    let contents = std::fs::read_to_string(&path)
        .map_err(|error| GeneratorError::io("read Dockerfile", &path, &error))?;
    validate_canonical_release_products(dockerfile, &contents)?;
    let stages = dockerfile_stages(&contents);
    let (instructions, unsupported_directive) = dockerfile_instructions(&contents);
    if unsupported_directive {
        // A parser directive can change how every subsequent instruction is
        // tokenized. The supported grammar below is insufficient to prove a
        // narrow closure when that directive is unknown or malformed.
        *conservative_context = true;
    }
    for instruction in instructions {
        // Docker comments are full-line comments. A `#` token inside an
        // instruction is an ordinary argument and can be a source filename;
        // truncating here silently drops later COPY inputs.
        let tokens = instruction.split_whitespace().collect::<Vec<_>>();
        let Some(command) = tokens.first().map(|token| token.to_ascii_uppercase()) else {
            continue;
        };
        match command.as_str() {
            "COPY" | "ADD" => inspect_docker_copy(
                tokens.as_slice(),
                files,
                declared_context_names,
                &stages,
                watch,
                conservative_context,
            ),
            "RUN" => inspect_docker_run_mounts(
                tokens.as_slice(),
                files,
                declared_context_names,
                &stages,
                watch,
                conservative_context,
            ),
            "ONBUILD" => {
                let Some(nested) = tokens.get(1..) else {
                    *conservative_context = true;
                    continue;
                };
                let Some(nested_command) = nested.first().map(|token| token.to_ascii_uppercase())
                else {
                    *conservative_context = true;
                    continue;
                };
                match nested_command.as_str() {
                    "COPY" | "ADD" => inspect_docker_copy(
                        nested,
                        files,
                        declared_context_names,
                        &stages,
                        watch,
                        conservative_context,
                    ),
                    "RUN" => inspect_docker_run_mounts(
                        nested,
                        files,
                        declared_context_names,
                        &stages,
                        watch,
                        conservative_context,
                    ),
                    // ONBUILD may carry any builder instruction. Unknown or
                    // future source-consuming forms must not be skipped.
                    _ => *conservative_context = true,
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn inspect_docker_copy(
    tokens: &[&str],
    files: &[String],
    declared_context_names: &BTreeSet<&str>,
    stages: &BTreeSet<String>,
    watch: &mut BTreeSet<String>,
    conservative_context: &mut bool,
) {
    let (from, sources) = docker_copy_sources(tokens, conservative_context);
    if let Some(from) = from {
        if from == MUTABLE_MOUNT_SEED_CONTEXT {
            return;
        }
        if from.starts_with('$')
            || (!stages.contains(&from)
                && !declared_context_names.contains(from.as_str())
                && !looks_like_external_image(&from))
        {
            *conservative_context = true;
        }
        return;
    }
    for source in sources {
        add_docker_source_watch(watch, files, &source, conservative_context);
    }
}

fn dockerfile_instructions(contents: &str) -> (Vec<String>, bool) {
    let (escape, unsupported_directive) = dockerfile_escape_char(contents);
    let mut instructions = Vec::new();
    let mut current = String::new();
    for raw_line in contents.lines() {
        let mut line = raw_line.trim();
        // A comment-only line between continued instruction lines does not
        // terminate the instruction. Docker comments are recognized only at
        // the beginning of a logical line, not after an instruction token.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let continued = line.ends_with(escape);
        if continued {
            line = line[..line.len() - escape.len_utf8()].trim_end();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(line);
        if !continued {
            instructions.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        instructions.push(current);
    }
    (instructions, unsupported_directive)
}

fn dockerfile_escape_char(contents: &str) -> (char, bool) {
    let mut escape = '\\';
    let mut directives_open = true;
    let mut seen = BTreeSet::new();
    let mut unsupported = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            directives_open = false;
            continue;
        }
        if !line.starts_with('#') {
            break;
        }
        if !directives_open {
            continue;
        }
        let body = line[1..].trim_start();
        let Some((raw_key, raw_value)) = body.split_once('=') else {
            // An ordinary leading comment ends the parser-directive region.
            directives_open = false;
            continue;
        };
        let key = raw_key.trim().to_ascii_lowercase();
        let value = raw_value.trim();
        if key.is_empty() || !seen.insert(key.clone()) {
            unsupported = true;
            directives_open = false;
            continue;
        }
        match key.as_str() {
            "escape" => match value {
                "\\" | "`" => escape = value.chars().next().unwrap_or('\\'),
                _ => unsupported = true,
            },
            "syntax" => {
                // The bundled Dockerfile frontend is the grammar this parser
                // understands. Custom frontends may add source-consuming
                // instructions, so select conservatively for them.
                if !value.starts_with("docker/dockerfile:")
                    && !value.starts_with("docker/dockerfile-upstream:")
                {
                    unsupported = true;
                }
            }
            "check" => {}
            _ => unsupported = true,
        }
        if unsupported {
            directives_open = false;
        }
    }
    (escape, unsupported)
}

fn docker_copy_sources(
    tokens: &[&str],
    conservative_context: &mut bool,
) -> (Option<String>, Vec<String>) {
    let mut from = None;
    let mut values = Vec::new();
    for token in tokens.iter().skip(1) {
        let token = token.trim_matches(['\'', '"']);
        if let Some(value) = token.strip_prefix("--from=") {
            from = Some(value.to_owned());
        } else if !token.starts_with("--") {
            values.push(token.to_owned());
        }
    }
    if from.is_none() && (values.len() < 2 || values.first().is_some_and(|value| value == "[")) {
        *conservative_context = true;
        return (None, Vec::new());
    }
    if from.is_some() {
        return (from, Vec::new());
    }
    values.pop();
    (None, values)
}

fn add_docker_source_watch(
    watch: &mut BTreeSet<String>,
    files: &[String],
    source: &str,
    conservative_context: &mut bool,
) {
    if source.contains("://") {
        return;
    }
    if source.contains('$') || source.contains('*') || source.contains('?') {
        *conservative_context = true;
        return;
    }
    let source = source
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/');
    if source.is_empty() || source == "." {
        *conservative_context = true;
    } else if files.iter().any(|file| file == source) {
        watch.insert(source.to_owned());
    } else if files
        .iter()
        .any(|file| file.starts_with(&format!("{source}/")))
    {
        watch.insert(format!("{source}/**"));
    } else {
        *conservative_context = true;
    }
}

fn inspect_docker_run_mounts(
    tokens: &[&str],
    files: &[String],
    declared_context_names: &BTreeSet<&str>,
    stages: &BTreeSet<String>,
    watch: &mut BTreeSet<String>,
    conservative_context: &mut bool,
) {
    for token in tokens.iter().skip(1) {
        let Some(token) = token.strip_prefix("--mount=") else {
            continue;
        };
        let Some(mount) = parse_supported_mount(token) else {
            // Quoted, interpolated, malformed, and unknown mount syntax
            // is outside this parser's grammar. Keep required coverage by
            // selecting the complete context instead of guessing.
            *conservative_context = true;
            continue;
        };
        let SupportedMount::Bind { from, source } = mount else {
            continue;
        };
        if let Some(from) = from {
            if from == MUTABLE_MOUNT_SEED_CONTEXT
                || stages.contains(from)
                || declared_context_names.contains(from)
                || looks_like_external_image(from)
            {
                continue;
            }
            *conservative_context = true;
            continue;
        }
        if let Some(source) = source {
            add_docker_source_watch(watch, files, source, conservative_context);
        } else {
            *conservative_context = true;
        }
    }
}

/// The deliberately small, static `RUN --mount` grammar used for watch
/// derivation. A bind mount carries source inputs; a known non-source mount
/// (`cache`, `tmpfs`, `secret`, or `ssh`) carries none. Anything quoted,
/// interpolated, duplicated, or outside the known option set returns `None`
/// so the caller selects the full context.
enum SupportedMount<'a> {
    Bind {
        from: Option<&'a str>,
        source: Option<&'a str>,
    },
    NonSource,
}

fn parse_supported_mount(spec: &str) -> Option<SupportedMount<'_>> {
    if spec.is_empty()
        || spec
            .chars()
            .any(|character| matches!(character, '\'' | '"' | '$' | '\\'))
    {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut kind = None;
    let mut from = None;
    let mut source = None;
    let mut source_seen = false;
    for field in spec.split(',') {
        if field.is_empty() {
            return None;
        }
        if let Some((key, value)) = field.split_once('=') {
            if key.is_empty() || value.is_empty() || !seen.insert(key) {
                return None;
            }
            match key {
                "type" => kind = Some(value),
                "from" => from = Some(value),
                "source" | "src" => {
                    if source_seen {
                        return None;
                    }
                    source_seen = true;
                    source = Some(value);
                }
                "target" | "id" | "sharing" | "mode" | "uid" | "gid" | "size" | "env"
                | "consistency" | "bind-recursive" => {}
                _ => return None,
            }
        } else if !seen.insert(field)
            || !matches!(
                field,
                "ro" | "rw"
                    | "readonly"
                    | "required"
                    | "bind-nonrecursive"
                    | "private"
                    | "rprivate"
                    | "shared"
                    | "rshared"
                    | "slave"
                    | "rslave"
            )
        {
            return None;
        }
    }
    match kind.unwrap_or("bind") {
        "bind" => Some(SupportedMount::Bind { from, source }),
        "cache" | "tmpfs" | "secret" | "ssh" => Some(SupportedMount::NonSource),
        _ => None,
    }
}

fn dockerfile_stages(contents: &str) -> BTreeSet<String> {
    contents
        .lines()
        .filter_map(|line| {
            let tokens = line.split_whitespace().collect::<Vec<_>>();
            let from = tokens
                .first()
                .is_some_and(|token| token.eq_ignore_ascii_case("FROM"));
            from.then(|| {
                tokens
                    .windows(2)
                    .find(|pair| pair[0].eq_ignore_ascii_case("AS"))
                    .map(|pair| pair[1].to_owned())
            })?
        })
        .collect()
}

fn looks_like_external_image(value: &str) -> bool {
    value.contains(':') || value.contains('/') || value.contains('@')
}

/// Release images must publish immutable canonical products, not expose the
/// mutable Cargo target directory to a runtime stage. When a Dockerfile has a
/// `release` stage, that stage must stage `/out` artifacts and checksums; a
/// later stage may consume only `/out`.
pub(crate) fn validate_canonical_release_products(
    dockerfile: &str,
    contents: &str,
) -> Result<(), GeneratorError> {
    let has_release_stage = contents.lines().any(|line| {
        let line = line.trim();
        line.starts_with("FROM ")
            && (line.ends_with(" AS release") || line.ends_with(" as release"))
    });
    if !has_release_stage {
        return Ok(());
    }
    if !contents.contains("/out/")
        || !contents.contains(".sha256")
        || !contents.contains("rm -rf /out")
    {
        return Err(GeneratorError::usage(format!(
            "Dockerfile release stage must stage checksummed canonical products in /out: {dockerfile}"
        )));
    }
    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("COPY ")
            && (trimmed.contains("/src/target/") || trimmed.contains("${CARGO_TARGET_DIR}"))
        {
            return Err(GeneratorError::usage(format!(
                "Dockerfile runtime stage consumes mutable target products: {dockerfile}"
            )));
        }
        if trimmed.starts_with("COPY --from=release ") && !trimmed.contains("/out/") {
            return Err(GeneratorError::usage(format!(
                "Dockerfile runtime stage must copy canonical /out products: {dockerfile}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        docker_context_watch_paths, docker_watch_paths, dockerfile_stages,
        is_broad_per_crate_source_watch, opentofu_watch_paths, render_watch_units,
        scoped_repo_path, WatchGraph,
    };
    use crate::s2::primitives::{Args, Primitive, RenderCtx};
    use crate::s2::reuse::{select_affected, ChangeKind, ChangedPath, WatchedUnit};
    use crate::s2::{DockerContext, UnitKind};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    #[test]
    fn watch_graph_render_keeps_scanner_bun_inputs_for_root_and_nested(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("velnor-watch-bun-{}", crate::unique_suffix()));
        fs::create_dir_all(root.join("ui"))?;
        fs::create_dir_all(root.join("src"))?;
        fs::create_dir_all(root.join("scripts"))?;
        fs::write(
            root.join("package.json"),
            r#"{"name":"root","packageManager":"bun@1.3.14","scripts":{"build":"bun build"}}"#,
        )?;
        fs::write(root.join("bun.lock"), "lock")?;
        fs::write(root.join("src/index.ts"), "export {};\n")?;
        fs::write(root.join("scripts/check.ts"), "export {};\n")?;
        fs::write(
            root.join("ui/package.json"),
            r#"{"name":"ui","packageManager":"bun@1.3.14","scripts":{"build":"bun build"}}"#,
        )?;
        fs::write(root.join("ui/bun.lock"), "lock")?;
        fs::write(root.join("ui/codegen.ts"), "export {};\n")?;
        fs::write(root.join("ui/tsconfig.json"), "{}\n")?;
        fs::write(root.join("ui/vite.config.ts"), "export {};\n")?;

        let scan_providers: crate::s2::provider::ProviderSet =
            crate::s2::provider::ProviderId::ALL.into_iter().collect();
        let shape = crate::s2::scan::scan_shape(&root, &scan_providers, "main", &[])?;
        let config = crate::s2::ProjectConfig::from(shape.clone());
        let units = config.units.iter().collect::<Vec<_>>();
        let pins = super::super::Pins::resolved();
        let providers = super::super::providers::resolve(&config, &[])?;
        let cache = super::super::cache::resolve(&[])?;
        let nodes = Vec::new();
        let contracts = BTreeMap::new();
        let ctx = RenderCtx {
            root: &root,
            shape: &shape,
            config: &config,
            unit: None,
            units: &units,
            file: None,
            family: super::super::WATCH_GRAPH,
            pins: &pins,
            providers: &providers,
            cache: &cache,
            nodes: &nodes,
            contracts: &contracts,
        };
        let args = BTreeMap::new();
        let rendered = Primitive::render(&WatchGraph, &ctx, &Args(&args))?;
        let root_unit = rendered
            .units
            .iter()
            .find(|unit| unit.root == ".")
            .ok_or_else(|| std::io::Error::other("root Bun unit missing"))?;
        let nested_unit = rendered
            .units
            .iter()
            .find(|unit| unit.root == "ui")
            .ok_or_else(|| std::io::Error::other("nested Bun unit missing"))?;

        assert!(root_unit.watch.contains(&"**/*.ts".to_owned()));
        assert!(root_unit.watch.contains(&"src/**".to_owned()));
        assert!(root_unit.watch.contains(&"scripts/**".to_owned()));
        assert!(nested_unit.watch.contains(&"ui/**/*.ts".to_owned()));
        assert!(nested_unit.watch.contains(&"ui/package.json".to_owned()));
        assert!(nested_unit.watch.contains(&"ui/tsconfig.json".to_owned()));
        assert!(nested_unit.watch.contains(&"ui/vite.config.ts".to_owned()));
        assert!(!nested_unit.watch.contains(&"package.json".to_owned()));

        fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn workspace_topology_gate_ignores_whole_crate_trees() {
        assert!(is_broad_per_crate_source_watch("crates/**"));
        assert!(is_broad_per_crate_source_watch("tools/**"));
        assert!(!is_broad_per_crate_source_watch("crates/velnor-runner/**"));
        assert!(!is_broad_per_crate_source_watch("docker/**"));
    }

    #[test]
    fn opentofu_without_current_files_keeps_future_affected_selection() {
        assert_eq!(
            opentofu_watch_paths(&[]),
            vec![
                "**/*.tf".to_owned(),
                "**/*.tofu".to_owned(),
                "**/.terraform.lock.hcl".to_owned(),
            ]
        );
        assert_eq!(
            opentofu_watch_paths(&[
                "tests/fixtures/infra.tf".to_owned(),
                "infra/main.tf".to_owned(),
                "infra/.terraform.lock.hcl".to_owned(),
            ]),
            vec![
                "infra/main.tf".to_owned(),
                "infra/.terraform.lock.hcl".to_owned(),
            ]
        );
    }

    #[test]
    fn package_paths_remain_repository_relative_and_rooted() {
        assert_eq!(scoped_repo_path(".", "package.json"), "package.json");
        assert_eq!(scoped_repo_path("", "package.json"), "package.json");
        assert_eq!(scoped_repo_path("ui", "package.json"), "ui/package.json");
        assert_eq!(scoped_repo_path("ui", "bun.lock"), "ui/bun.lock");
    }

    #[test]
    fn merged_bun_watch_keeps_nested_scanner_inputs() {
        let source = crate::s2::scan::unit(
            crate::s2::UnitKind::Bun,
            "ui",
            vec!["ui/src/**".to_owned(), "ui/codegen.ts".to_owned()],
            vec!["bun run build".to_owned()],
            None,
        );
        let files = [
            "ui/package.json".to_owned(),
            "ui/tsconfig.json".to_owned(),
            "ui/codegen.ts".to_owned(),
        ];
        let rendered = render_watch_units(
            Path::new("."),
            &files,
            std::slice::from_ref(&source),
            &[&source],
            &BTreeMap::new(),
            &[],
        )
        .unwrap_or_default();
        let watch = rendered.first().map(|unit| &unit.watch);
        assert!(watch.is_some_and(|paths| paths.contains(&"ui/src/**".to_owned())));
        assert!(watch.is_some_and(|paths| paths.contains(&"ui/tsconfig.json".to_owned())));
        assert!(watch.is_some_and(|paths| paths.contains(&"ui/codegen.ts".to_owned())));
        assert!(watch.is_none_or(|paths| !paths.contains(&"src/**".to_owned())));
        assert!(watch.is_none_or(|paths| !paths.contains(&"scripts/**".to_owned())));
    }

    #[test]
    fn merged_root_bun_watch_keeps_scanner_assets_and_source_families() {
        let source = crate::s2::scan::unit(
            crate::s2::UnitKind::Bun,
            ".",
            vec!["assets/**".to_owned(), "src/styles.css".to_owned()],
            vec!["bun run build".to_owned()],
            None,
        );
        let files = ["package.json".to_owned()];
        let rendered = render_watch_units(
            Path::new("."),
            &files,
            std::slice::from_ref(&source),
            &[&source],
            &BTreeMap::new(),
            &[],
        )
        .unwrap_or_default();
        let watch = rendered.first().map(|unit| &unit.watch);
        assert!(watch.is_some_and(|paths| paths.contains(&"assets/**".to_owned())));
        assert!(watch.is_some_and(|paths| paths.contains(&"src/styles.css".to_owned())));
        assert!(watch.is_some_and(|paths| paths.contains(&"src/**".to_owned())));
        assert!(watch.is_some_and(|paths| paths.contains(&"scripts/**".to_owned())));
    }

    #[test]
    fn declared_docker_contexts_cover_their_actual_source_roots() {
        let contexts = [
            DockerContext {
                name: "checkout".to_owned(),
                path: ".".to_owned(),
            },
            DockerContext {
                name: "ui".to_owned(),
                path: "./ui/".to_owned(),
            },
        ];
        let paths = docker_context_watch_paths(&contexts).collect::<Vec<_>>();
        assert_eq!(paths, ["**", "ui/**"]);
    }

    #[test]
    fn dockerfile_stage_parser_separates_local_stages_from_images() {
        let stages = dockerfile_stages(
            "FROM ubuntu:26.04 AS build\nCOPY --from=build /out /out\nFROM scratch\n",
        );
        assert!(stages.contains("build"));
        assert!(!stages.contains("ubuntu:26.04"));
    }

    #[test]
    fn broad_docker_copy_falls_back_to_the_full_context() {
        let root = std::env::temp_dir().join(format!(
            "velnor-watch-docker-{}",
            crate::s2::unique_suffix()
        ));
        assert!(std::fs::create_dir_all(&root).is_ok());
        assert!(std::fs::write(root.join("Dockerfile"), "FROM scratch\nCOPY . /app\n").is_ok());
        let paths =
            docker_watch_paths(&root, &["Dockerfile".to_owned()], &[], &[]).unwrap_or_default();
        assert!(paths.iter().any(|path| path == "**"));
        assert!(std::fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn static_docker_copy_keeps_a_narrow_source_watch() {
        let root = std::env::temp_dir().join(format!(
            "velnor-watch-docker-{}",
            crate::s2::unique_suffix()
        ));
        assert!(std::fs::create_dir_all(&root).is_ok());
        assert!(std::fs::write(
            root.join("Dockerfile"),
            "FROM scratch\nCOPY docker/app /app\nCOPY --from=velnor-cache-seed /seed /seed\n"
        )
        .is_ok());
        let files = ["Dockerfile".to_owned(), "docker/app/main".to_owned()];
        let paths = docker_watch_paths(&root, &files, &[], &[]).unwrap_or_default();
        assert!(paths.iter().any(|path| path == "docker/app/**"));
        assert!(!paths.iter().any(|path| path == "**"));
        assert!(std::fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn docker_bind_mounts_and_multiline_copy_cover_sources() {
        let root = std::env::temp_dir().join(format!(
            "velnor-watch-docker-{}",
            crate::s2::unique_suffix()
        ));
        assert!(std::fs::create_dir_all(&root).is_ok());
        assert!(std::fs::write(
            root.join("Dockerfile"),
            "FROM scratch\nRUN --mount=type=bind,source=/docker/config,target=/cfg cat /cfg\nCOPY \\\n  /docker \\\n  /app\n"
        )
        .is_ok());
        let files = [
            "Dockerfile".to_owned(),
            "docker/config".to_owned(),
            "docker/app/main".to_owned(),
        ];
        let paths = docker_watch_paths(&root, &files, &[], &[]).unwrap_or_default();
        assert!(paths.iter().any(|path| path == "docker/**"));
        assert!(!paths.iter().any(|path| path == "**"));
        assert!(std::fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn docker_copy_keeps_hash_sources_and_comment_continuations() {
        let root = std::env::temp_dir().join(format!(
            "velnor-watch-docker-{}",
            crate::s2::unique_suffix()
        ));
        assert!(std::fs::create_dir_all(&root).is_ok());
        assert!(std::fs::write(
            root.join("Dockerfile"),
            "FROM scratch\nCOPY app-a app-b #named-source /app\nCOPY \\\n  app-c \\\n  # this line is a Docker comment\n  app-d \\\n  /app2\n"
        )
        .is_ok());
        let files = [
            "Dockerfile".to_owned(),
            "app-a".to_owned(),
            "app-b".to_owned(),
            "#named-source".to_owned(),
            "app-c".to_owned(),
            "app-d".to_owned(),
        ];
        let paths = docker_watch_paths(&root, &files, &[], &[]).unwrap_or_default();
        for source in ["app-a", "app-b", "#named-source", "app-c", "app-d"] {
            assert!(
                paths.iter().any(|path| path == source),
                "{source}: {paths:?}"
            );
        }
        assert!(!paths.iter().any(|path| path == "**"), "{paths:?}");
        assert!(std::fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn docker_escape_directives_accept_case_and_spacing_but_unknowns_fallback() {
        let root = std::env::temp_dir().join(format!(
            "velnor-watch-docker-{}",
            crate::s2::unique_suffix()
        ));
        assert!(std::fs::create_dir_all(&root).is_ok());
        let files = [
            "Dockerfile".to_owned(),
            "app-a".to_owned(),
            "app-b".to_owned(),
        ];
        assert!(std::fs::write(
            root.join("Dockerfile"),
            "# EsCaPe = `\nFROM scratch\nCOPY app-a `\n  app-b /app\n"
        )
        .is_ok());
        let paths = docker_watch_paths(&root, &files, &[], &[]).unwrap_or_default();
        assert!(paths.iter().any(|path| path == "app-a"), "{paths:?}");
        assert!(paths.iter().any(|path| path == "app-b"), "{paths:?}");
        assert!(!paths.iter().any(|path| path == "**"), "{paths:?}");

        for directive in ["# unknown = value", "# escape = x"] {
            assert!(std::fs::write(
                root.join("Dockerfile"),
                format!("{directive}\nFROM scratch\nCOPY app-a `\n  app-b /app\n")
            )
            .is_ok());
            let paths = docker_watch_paths(&root, &files, &[], &[]).unwrap_or_default();
            assert!(
                paths.iter().any(|path| path == "**"),
                "{directive}: {paths:?}"
            );
        }
        assert!(std::fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn onbuild_copy_in_a_local_stage_keeps_context_source() {
        let root = std::env::temp_dir().join(format!(
            "velnor-watch-docker-{}",
            crate::s2::unique_suffix()
        ));
        assert!(std::fs::create_dir_all(&root).is_ok());
        assert!(std::fs::write(
            root.join("Dockerfile"),
            "FROM scratch AS base\nONBUILD COPY inherited /app\nFROM base AS child\n"
        )
        .is_ok());
        let files = ["Dockerfile".to_owned(), "inherited".to_owned()];
        let paths = docker_watch_paths(&root, &files, &[], &[]).unwrap_or_default();
        assert!(paths.iter().any(|path| path == "inherited"), "{paths:?}");
        assert!(!paths.iter().any(|path| path == "**"), "{paths:?}");
        assert!(std::fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn docker_hash_source_selects_image_when_peer_unit_matches_same_input() {
        let root = std::env::temp_dir().join(format!(
            "velnor-watch-docker-{}",
            crate::s2::unique_suffix()
        ));
        assert!(std::fs::create_dir_all(&root).is_ok());
        assert!(std::fs::write(
            root.join("Dockerfile"),
            "FROM scratch\nCOPY docker-input shared-config #named-source /app\n"
        )
        .is_ok());
        let files = [
            "Dockerfile".to_owned(),
            "docker-input".to_owned(),
            "shared-config".to_owned(),
            "#named-source".to_owned(),
        ];
        let docker = crate::s2::scan::unit(
            UnitKind::Docker,
            ".",
            Vec::new(),
            vec!["docker build".to_owned()],
            None,
        );
        let mut peer = crate::s2::scan::unit(
            UnitKind::Rust,
            "peer",
            vec!["shared-config".to_owned()],
            vec!["cargo check".to_owned()],
            None,
        );
        peer.id = "peer".to_owned();
        let config_units = [docker.clone(), peer.clone()];
        let source_units = [&docker, &peer];
        let rendered = render_watch_units(
            &root,
            &files,
            &config_units,
            &source_units,
            &BTreeMap::new(),
            &[],
        )
        .expect("watch graph renders");
        let watched = rendered
            .iter()
            .map(|unit| WatchedUnit {
                id: unit.id.clone(),
                watch: unit.watch.clone(),
                depends_on: unit.depends_on.clone(),
            })
            .collect::<Vec<_>>();
        let selection = select_affected(
            &watched,
            &[ChangedPath {
                path: "shared-config".to_owned(),
                previous: None,
                status: ChangeKind::Modified,
            }],
            &[],
        )
        .expect("affected selection succeeds");
        assert!(selection.required.contains("docker"), "{selection:?}");
        assert!(selection.required.contains("peer"), "{selection:?}");
        assert!(
            selection
                .explanations
                .get("docker")
                .is_some_and(|reason| reason.contains("shared-config")),
            "{selection:?}"
        );
        assert!(std::fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn unscoped_bind_mount_falls_back_to_the_full_context() {
        let root = std::env::temp_dir().join(format!(
            "velnor-watch-docker-{}",
            crate::s2::unique_suffix()
        ));
        assert!(std::fs::create_dir_all(&root).is_ok());
        assert!(std::fs::write(
            root.join("Dockerfile"),
            "FROM scratch\nRUN --mount=type=bind,target=/src cat /src\n"
        )
        .is_ok());
        let paths =
            docker_watch_paths(&root, &["Dockerfile".to_owned()], &[], &[]).unwrap_or_default();
        assert!(paths.iter().any(|path| path == "**"));
        assert!(std::fs::remove_dir_all(root).is_ok());
    }

    #[test]
    fn quoted_and_dynamic_mounts_fall_back_to_the_full_context() {
        for mount in [
            "--mount=\"type=bind,source=app,target=/app\"",
            "--mount=type=${MOUNT},source=app,target=/app",
        ] {
            let root = std::env::temp_dir().join(format!(
                "velnor-watch-docker-{}",
                crate::s2::unique_suffix()
            ));
            assert!(std::fs::create_dir_all(&root).is_ok());
            assert!(std::fs::write(
                root.join("Dockerfile"),
                format!("FROM scratch\nRUN {mount} cat /app\n")
            )
            .is_ok());
            let paths = docker_watch_paths(
                &root,
                &["Dockerfile".to_owned(), "app".to_owned()],
                &[],
                &[],
            )
            .unwrap_or_default();
            assert!(paths.iter().any(|path| path == "**"), "{mount}: {paths:?}");
            assert!(std::fs::remove_dir_all(root).is_ok());
        }
    }

    #[test]
    fn incomplete_multiline_copy_falls_back_to_the_full_context() {
        let root = std::env::temp_dir().join(format!(
            "velnor-watch-docker-{}",
            crate::s2::unique_suffix()
        ));
        assert!(std::fs::create_dir_all(&root).is_ok());
        assert!(std::fs::write(root.join("Dockerfile"), "FROM scratch\nCOPY \\\n").is_ok());
        let paths =
            docker_watch_paths(&root, &["Dockerfile".to_owned()], &[], &[]).unwrap_or_default();
        assert!(paths.iter().any(|path| path == "**"));
        assert!(std::fs::remove_dir_all(root).is_ok());
    }
}
