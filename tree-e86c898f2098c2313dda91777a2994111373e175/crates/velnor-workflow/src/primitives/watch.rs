//! `watch-graph`: the affected-selection watch paths of every unit.
//!
//! Watch paths are derived, never name-matched: the kind of a unit and the
//! dependency closure of the units a repository declares decide what a unit
//! watches, and the config adds the repository-specific paths on top. A unit id
//! appears in the config only as the key of an explicit addition, never as a
//! branch inside this primitive.

use std::collections::BTreeSet;

use super::{Args, Primitive, RenderCtx, Rendered, WATCH_GRAPH};
use crate::scan::file_walk::{has_extension, is_test_support_path};
use crate::{GeneratorError, Unit, UnitKind};

/// Declare the repository's watch graph.
pub(crate) struct WatchGraph;

impl Primitive for WatchGraph {
    fn id(&self) -> &'static str {
        WATCH_GRAPH
    }

    fn schema(&self) -> &'static [&'static str] {
        &["runtime_inputs", "watch", "docker_closure"]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let runtime_inputs = args.strings("runtime_inputs")?.unwrap_or_default();
        let additions = args.unit_strings(ctx, "watch")?.unwrap_or_default();
        let closure = args.strings("docker_closure")?.unwrap_or_default();
        let docker_watch = docker_watch_paths(
            ctx.root,
            ctx.shape.files(),
            &ctx.config.units,
            &closure.iter().map(String::as_str).collect::<Vec<_>>(),
        )?;
        let mut units = Vec::new();
        for unit in ctx.units {
            let mut unit = (*unit).clone();
            // A root Docker image watches what its build consumes, derived from
            // the image's dependency closure. A package unit watches what its
            // own manifest and layout proved; a family the scan could not pin
            // down watches what the scan derived for it.
            let derived = matches!(
                unit.kind,
                UnitKind::Bun | UnitKind::Docs | UnitKind::OpenTofu
            ) || (unit.kind == UnitKind::Docker && unit.root == ".");
            let mut watch = BTreeSet::new();
            if !derived {
                watch.extend(unit.watch.iter().cloned());
            }
            if unit.kind == UnitKind::Docker && unit.root == "." {
                watch.extend(docker_watch.iter().cloned());
            }
            match unit.kind {
                // A root package watches its manifests, its sources, and every
                // root-level build configuration the walk observed.
                UnitKind::Bun => {
                    watch.extend([
                        "package.json".to_owned(),
                        "bun.lock".to_owned(),
                        "bun.lockb".to_owned(),
                        "package-lock.json".to_owned(),
                        "src/**".to_owned(),
                        "scripts/**".to_owned(),
                    ]);
                    watch.extend(
                        ctx.shape
                            .files()
                            .iter()
                            .filter(|file| {
                                crate::parent_path(file) == "."
                                    && (file.ends_with(".config.js")
                                        || file.ends_with(".config.ts")
                                        || file.as_str() == "tsconfig.json")
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
                    watch.extend(
                        ctx.shape
                            .files()
                            .iter()
                            .filter(|file| {
                                !is_test_support_path(file)
                                    && (has_extension(file, "tf")
                                        || has_extension(file, "tofu")
                                        || file.rsplit('/').next() == Some(".terraform.lock.hcl"))
                            })
                            .cloned(),
                    );
                }
                UnitKind::Rust | UnitKind::Docker => {
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
                UnitKind::Gradle | UnitKind::Node | UnitKind::Swift | UnitKind::Homebrew => {}
            }
            watch.extend(runtime_inputs.iter().cloned());
            if let Some(paths) = additions.get(&unit.id) {
                watch.extend(paths.iter().cloned());
            }
            unit.watch = watch.into_iter().collect();
            units.push(unit);
        }
        Ok(Rendered {
            units,
            ..Rendered::default()
        })
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
    let mut watch = BTreeSet::from([
        ".dockerignore".to_owned(),
        "Cargo.lock".to_owned(),
        "Cargo.toml".to_owned(),
        "Dockerfile".to_owned(),
        "docker/**".to_owned(),
        "mise.lock".to_owned(),
        "mise.toml".to_owned(),
        "rust-toolchain".to_owned(),
        "rust-toolchain.toml".to_owned(),
        ".cargo/**".to_owned(),
    ]);
    for dockerfile in files.iter().filter(|file| {
        !is_test_support_path(file)
            && crate::parent_path(file) == "."
            && file
                .rsplit('/')
                .next()
                .is_some_and(|name| name.starts_with("Dockerfile"))
    }) {
        let contents = std::fs::read_to_string(root.join(dockerfile)).map_err(|error| {
            GeneratorError::io("read Dockerfile", &root.join(dockerfile), &error)
        })?;
        for line in contents.lines() {
            let tokens = line
                .split_whitespace()
                .take_while(|token| !token.starts_with('#'))
                .collect::<Vec<_>>();
            let Some(command) = tokens.first().map(|token| token.to_ascii_uppercase()) else {
                continue;
            };
            if !matches!(command.as_str(), "COPY" | "ADD")
                || tokens.iter().any(|token| token.starts_with("--from="))
            {
                continue;
            }
            let sources = tokens
                .iter()
                .skip(1)
                .filter(|token| !token.starts_with("--"))
                .take(tokens.len().saturating_sub(2))
                .map(|token| token.trim_matches(['\'', '"']))
                .collect::<Vec<_>>();
            for source in sources {
                if source.starts_with('/') || source.contains('$') || source.contains("://") {
                    continue;
                }
                let source = source.trim_start_matches("./").trim_end_matches('/');
                if source.is_empty() || source == "." {
                    return Err(GeneratorError::usage(format!(
                        "Dockerfile source is too broad for affected CI: {dockerfile}"
                    )));
                }
                if files.iter().any(|file| file == source) {
                    watch.insert(source.to_owned());
                } else if files
                    .iter()
                    .any(|file| file.starts_with(&format!("{source}/")))
                {
                    watch.insert(format!("{source}/**"));
                } else {
                    return Err(GeneratorError::usage(format!(
                        "Dockerfile COPY/ADD source does not exist: {dockerfile}: {source}"
                    )));
                }
            }
        }
    }
    // The dependency closure of the declared seeds, expanded through the units'
    // own dependency edges.
    let seeds = closure_seeds
        .iter()
        .filter(|id| units.iter().any(|unit| &unit.id == *id))
        .map(|id| (*id).to_owned())
        .collect::<BTreeSet<_>>();
    let mut closure = seeds.clone();
    let mut pending = seeds.into_iter().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        if let Some(unit) = units.iter().find(|unit| unit.id == *id) {
            for dependency in &unit.depends_on {
                if closure.insert(dependency.clone()) {
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
    Ok(watch.into_iter().collect())
}
