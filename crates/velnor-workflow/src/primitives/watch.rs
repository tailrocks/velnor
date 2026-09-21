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
        &["watch", "docker_closure", "reads"]
    }

    fn render(&self, ctx: &RenderCtx<'_>, args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        let additions = args.unit_strings(ctx, "watch")?.unwrap_or_default();
        let reads = args.unit_reads(ctx, "reads")?.unwrap_or_default();
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
                watch.extend(
                    unit.watch
                        .iter()
                        .filter(|path| {
                            !(unit.workspace_check && is_broad_per_crate_source_watch(path))
                        })
                        .cloned(),
                );
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
                    watch.extend(opentofu_watch_paths(ctx.shape.files()));
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
            if let Some(paths) = additions.get(&unit.id) {
                watch.extend(
                    paths
                        .iter()
                        .filter(|path| {
                            !(unit.workspace_check && is_broad_per_crate_source_watch(path))
                        })
                        .cloned(),
                );
            }
            // Declared reads compile into the watch set: an idempotent
            // union, so overlapping an already-watched path changes nothing.
            if let Some(entries) = reads.get(&unit.id) {
                watch.extend(entries.iter().flat_map(|entry| entry.paths.iter().cloned()));
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
        validate_canonical_release_products(dockerfile, &contents)?;
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
    use super::{is_broad_per_crate_source_watch, opentofu_watch_paths, WatchGraph};
    use crate::primitives::{Args, Primitive, RenderCtx};
    use std::{collections::BTreeMap, fs};

    #[test]
    fn watch_graph_render_unions_declared_reads_into_unit_watch(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("velnor-watch-reads-{}", crate::unique_suffix()));
        fs::create_dir_all(root.join("src"))?;
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"example\"\nversion = \"0.1.0\"\n",
        )?;
        fs::write(
            root.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.91.1\"\n",
        )?;
        fs::write(root.join("src/lib.rs"), "pub fn f() {}\n")?;

        let shape = crate::scan::scan_shape(&root, crate::RunnerMode::Both, "main", &[])?;
        let config = crate::ProjectConfig::from(shape.clone());
        let unit_id = config
            .units
            .iter()
            .find(|unit| unit.kind == crate::UnitKind::Rust)
            .map(|unit| unit.id.clone())
            .ok_or_else(|| std::io::Error::other("a rust unit must scan"))?;
        let units = config.units.iter().collect::<Vec<_>>();
        let pins = super::super::Pins::resolved();
        let lanes = super::super::lanes::resolve(&config, &[])?;
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
            lanes: &lanes,
            cache: &cache,
            nodes: &nodes,
            contracts: &contracts,
        };
        // One declared input no scan watch owns, plus one overlapping an
        // already-watched path: the union is idempotent.
        let reads: toml::Value = toml::from_str(&format!(
            r#""{unit_id}" = [{{ paths = ["schemas/**", "Cargo.lock"], reason = "helper script reads undeclared inputs" }}]"#
        ))?;
        let mut args_map = BTreeMap::new();
        args_map.insert("reads".to_owned(), reads);
        let rendered = Primitive::render(&WatchGraph, &ctx, &Args(&args_map))?;
        let rendered_unit = rendered
            .units
            .iter()
            .find(|unit| unit.id == unit_id)
            .ok_or_else(|| std::io::Error::other("the rust unit must render"))?;
        assert!(
            rendered_unit.watch.contains(&"schemas/**".to_owned()),
            "declared reads join the rendered unit watch: {:?}",
            rendered_unit.watch
        );
        assert_eq!(
            rendered_unit
                .watch
                .iter()
                .filter(|path| path.as_str() == "Cargo.lock")
                .count(),
            1,
            "overlapping an already-watched path changes nothing: {:?}",
            rendered_unit.watch
        );
        for unit in rendered.units.iter().filter(|unit| unit.id != unit_id) {
            assert!(
                !unit.watch.contains(&"schemas/**".to_owned()),
                "declared reads stay scoped to their consumer: {}: {:?}",
                unit.id,
                unit.watch
            );
        }

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
}
