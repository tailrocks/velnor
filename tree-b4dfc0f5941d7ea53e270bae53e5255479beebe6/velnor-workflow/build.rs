//! Source identity embedding for `velnor-workflow`.
//!
//! Four values are stamped into every binary:
//!
//! * `VELNOR_WORKFLOW_SOURCE_SHA`: the exact 40-hex `HEAD` of the crate's
//!   checkout (`velnor-workflow --revision`). Provenance metadata: which
//!   commit the binary was built from.
//! * `VELNOR_WORKFLOW_CLOSURE_DIGEST`: the source-closure digest of the
//!   checkout's `HEAD` tree (`velnor-workflow --closure`, see
//!   `src/closure.rs`). Product identity: binaries built from different
//!   commits with the same closure are interchangeable renderers.
//! * `VELNOR_WORKFLOW_FEATURES`: the sorted enabled-feature list the closure
//!   footer hashed (see `cargo_features`).
//! * `VELNOR_WORKFLOW_PROFILE`: the Cargo profile the closure footer hashed.
//!   Together with the feature list it lets `promote` recompute the stamped
//!   pin's closure under exactly the running binary's own build identity, so
//!   the render-with-X-stamp-X binding holds for release products and
//!   development builds alike.
//!
//! The closure duplicates the canonicalization in `src/closure.rs` (a build
//! script cannot import the crate it builds): `git ls-tree -r HEAD` over the
//! closure paths, re-sorted in byte order, plus the footer, hashed with
//! SHA-256. The footer version and path list are pinned by unit tests against
//! `src/closure.rs`, so the two implementations cannot drift silently: any
//! drift fails closed (digests mismatch, no product is accepted).
//!
//! A tree without git (or a git failure) stamps `unknown` for both values,
//! which no pinned-revision or closure probe can ever match.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const BASE_CLOSURE_PATHS: &[&str] = &[
    "crates/velnor-workflow",
    "crates/velnor-model",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rust-toolchain",
    ".cargo",
];

/// Closure algorithm version, mirroring `closure::CLOSURE_VERSION`.
const CLOSURE_VERSION: u8 = 1;

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_else(|| ".".into()));
    // Re-run on every build. Cargo only re-runs a build script when a file it
    // named last time changed, and the fingerprint (and therefore that list of
    // files) is keyed by package, not by checkout: a target directory shared by
    // two checkouts would otherwise keep the first checkout's `HEAD` paths and
    // hand the second checkout a binary stamped with the first one's commit —
    // exactly the false proof the D19 guard must never be able to produce.
    // Naming a path that never exists marks the script stale unconditionally;
    // the cost is one `git rev-parse`, and the crate itself only recompiles
    // when the stamped value actually changes.
    if let Some(out_dir) = std::env::var_os("OUT_DIR") {
        let sentinel = PathBuf::from(out_dir).join("velnor-workflow-source-sha.always-rerun");
        println!("cargo:rerun-if-changed={}", sentinel.display());
    }
    let sha = git(&manifest_dir, &["rev-parse", "HEAD"])
        .filter(|value| is_full_sha(value))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=VELNOR_WORKFLOW_SOURCE_SHA={sha}");
    println!(
        "cargo:rustc-env=VELNOR_WORKFLOW_CLOSURE_DIGEST={}",
        self_closure(&manifest_dir).unwrap_or_else(|| "unknown".to_owned())
    );
    println!(
        "cargo:rustc-env=VELNOR_WORKFLOW_FEATURES={}",
        cargo_features()
    );
    println!(
        "cargo:rustc-env=VELNOR_WORKFLOW_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned())
    );
}

/// Closure digest of the checkout's `HEAD` tree, or `None` when it cannot be
/// proven (no git, unknown `HEAD`, or no closure inputs tracked).
fn self_closure(manifest_dir: &Path) -> Option<String> {
    let root = git(manifest_dir, &["rev-parse", "--show-toplevel"])?;
    let root = PathBuf::from(root);
    let workflow_manifest = git(&root, &["show", "HEAD:crates/velnor-workflow/Cargo.toml"])?;
    let workspace_manifest = git(&root, &["show", "HEAD:Cargo.toml"])?;
    let paths = build_closure_paths(&workflow_manifest, &workspace_manifest)?;
    let mut arguments = vec![
        "ls-tree".to_owned(),
        "-r".to_owned(),
        "HEAD".to_owned(),
        "--".to_owned(),
    ];
    arguments.extend(closure_pathspecs(&paths));
    let output = Command::new("git")
        .current_dir(&root)
        .args(&arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8(output.stdout).ok()?;
    let mut lines: Vec<&str> = listing.lines().collect();
    if lines.is_empty() || has_unsafe_closure_symlink(&lines) {
        return None;
    }
    for dependency_root in &paths[BASE_CLOSURE_PATHS.len()..] {
        if !has_dependency_root(&lines, dependency_root) {
            return None;
        }
        for line in &lines {
            let Some((_, path)) = line.split_once('\t') else {
                continue;
            };
            if !(path == dependency_root || path.starts_with(&format!("{dependency_root}/")))
                || !path.ends_with("Cargo.toml")
            {
                continue;
            }
            let manifest = git(&root, &[&format!("show HEAD:{path}")])?;
            if manifest_has_local_dependency(&manifest, &workspace_manifest, dependency_root, path)?
            {
                return None;
            }
        }
    }
    lines.sort_unstable();
    lines.dedup();
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(
        format!(
            "closure-version:{CLOSURE_VERSION}\nfeatures:{}\nprofile:{}\n",
            cargo_features(),
            std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned())
        )
        .as_bytes(),
    );
    let digest = Sha256::digest(&bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    Some(output)
}

fn has_unsafe_closure_symlink(lines: &[&str]) -> bool {
    lines.iter().any(|line| {
        line.starts_with("120000 ")
            && line
                .split_once('\t')
                .is_some_and(|(_, path)| path != "crates/velnor-workflow/CLAUDE.md")
    })
}

fn has_dependency_root(lines: &[&str], dependency_root: &str) -> bool {
    lines.iter().any(|line| {
        line.split_once('\t').is_some_and(|(_, path)| {
            path == dependency_root || path.starts_with(&format!("{dependency_root}/"))
        })
    })
}

fn manifest_has_local_dependency(
    manifest: &str,
    workspace: &str,
    dependency_root: &str,
    manifest_path: &str,
) -> Option<bool> {
    let manifest = toml::from_str::<toml::Value>(manifest).ok()?;
    let workspace = toml::from_str::<toml::Value>(workspace).ok()?;
    let workspace_dependencies = match workspace.get("workspace") {
        Some(value) => match value.as_table()?.get("dependencies") {
            Some(value) => Some(value.as_table()?),
            None => None,
        },
        None => None,
    };
    let workspace_table = workspace.get("workspace")?.as_table()?;
    let members = match workspace_table.get("members") {
        Some(members) => Some(
            members
                .as_array()?
                .iter()
                .map(toml::Value::as_str)
                .collect::<Option<Vec<_>>>()?,
        ),
        None => None,
    };
    for (_, dependencies) in dependency_sections(&manifest)? {
        for (alias, declaration) in dependencies.as_table()? {
            if declaration
                .get("workspace")
                .is_some_and(|value| !value.is_bool())
                || (declaration.get("workspace").and_then(toml::Value::as_bool) == Some(true)
                    && declaration.get("path").is_some())
            {
                return None;
            }
            if declaration.get("workspace").and_then(toml::Value::as_bool) == Some(true)
                && (manifest_path != format!("{dependency_root}/Cargo.toml")
                    || !members
                        .as_ref()
                        .is_some_and(|members| members.contains(&dependency_root)))
            {
                return None;
            }
            let mut declaration = declaration;
            if !declaration.is_str() && !declaration.is_table() {
                return None;
            }
            if declaration
                .get("package")
                .is_some_and(|package| !package.is_str())
            {
                return None;
            }
            if declaration.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                declaration = workspace_dependencies?.get(alias)?;
                if !declaration.is_str() && !declaration.is_table() {
                    return None;
                }
            }
            if let Some(path) = declaration.get("path") {
                path.as_str()?;
                return Some(true);
            }
        }
    }
    Some(false)
}

fn closure_pathspecs(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            if index < BASE_CLOSURE_PATHS.len() {
                path.clone()
            } else {
                format!(":(literal){path}")
            }
        })
        .collect()
}

/// Dependency detector for build.rs. Cargo has already parsed these manifests
/// before invoking the build script. This narrow reader only accepts the
/// dependency table forms that can select the optional local source path; any
/// ambiguous model declaration returns `None`, stamping `unknown` instead of
/// minting a digest that could omit compiled source.
fn build_closure_paths(workflow: &str, workspace: &str) -> Option<Vec<String>> {
    let workflow = toml::from_str::<toml::Value>(workflow).ok()?;
    let workspace = toml::from_str::<toml::Value>(workspace).ok()?;
    let workspace_dependencies = match workspace.get("workspace") {
        Some(value) => {
            let table = value.as_table()?;
            match table.get("dependencies") {
                Some(value) => Some(value.as_table()?),
                None => None,
            }
        }
        None => None,
    };
    let mut paths = BASE_CLOSURE_PATHS
        .iter()
        .map(|path| (*path).to_owned())
        .collect::<Vec<_>>();
    for (section, deps) in dependency_sections(&workflow)? {
        if section == "dev-dependencies" || section.ends_with(".dev-dependencies") {
            continue;
        }
        for (alias, declaration) in deps.as_table()? {
            let mut decl = declaration;
            if !decl.is_str() && !decl.is_table() {
                return None;
            }
            if decl.get("workspace").is_some_and(|value| !value.is_bool())
                || decl.get("package").is_some_and(|package| !package.is_str())
            {
                return None;
            }
            let mut inherited = false;
            if decl.get("workspace").and_then(toml::Value::as_bool) == Some(true) {
                if decl.get("path").is_some() {
                    return None;
                }
                decl = workspace_dependencies?.get(alias)?;
                if !decl.is_str() && !decl.is_table() {
                    return None;
                }
                inherited = true;
            }
            let Some(path) = decl.get("path") else {
                continue;
            };
            let path = path.as_str()?;
            let base = if inherited {
                ""
            } else {
                "crates/velnor-workflow"
            };
            let path = normalize_repo_path(base, path)?;
            if path.is_empty() {
                return None;
            }
            if BASE_CLOSURE_PATHS
                .iter()
                .any(|base| base.starts_with(&format!("{path}/")))
            {
                return None;
            }
            paths.push(path);
        }
    }
    let (base, extras) = paths.split_at(BASE_CLOSURE_PATHS.len());
    let mut extras = extras.to_vec();
    extras.sort();
    extras.dedup();
    let mut paths = base.to_vec();
    for path in extras {
        let covered = BASE_CLOSURE_PATHS.iter().any(|base| {
            path == *base
                || path
                    .strip_prefix(base)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        });
        let covered_by_dependency = paths[BASE_CLOSURE_PATHS.len()..].iter().any(|root| {
            path == *root
                || path
                    .strip_prefix(root)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        });
        if !covered && !covered_by_dependency && !paths.contains(&path) {
            paths.push(path);
        }
    }
    Some(paths)
}

fn dependency_sections(manifest: &toml::Value) -> Option<Vec<(&'static str, &toml::Value)>> {
    let root = manifest.as_table()?;
    let mut sections = Vec::new();
    for name in ["dependencies", "build-dependencies", "dev-dependencies"] {
        if let Some(value) = root.get(name) {
            sections.push((name, value));
        }
    }
    if let Some(targets) = root.get("target") {
        for value in targets.as_table()?.values() {
            let target = value.as_table()?;
            for name in ["dependencies", "build-dependencies", "dev-dependencies"] {
                if let Some(value) = target.get(name) {
                    sections.push((name, value));
                }
            }
        }
    }
    Some(sections)
}

fn normalize_repo_path(base: &str, path: &str) -> Option<String> {
    let path = Path::new(path);
    if path.is_absolute() {
        return None;
    }
    let mut components = Path::new(base)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            Component::CurDir => None,
            _ => Some(String::new()),
        })
        .collect::<Vec<_>>();
    if components.iter().any(String::is_empty) {
        return None;
    }
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => components.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                components.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(components.join("/"))
}

/// Enabled Cargo features as a sorted comma list (`CARGO_FEATURE_*` is set
/// per enabled feature, uppercased with `-` mapped to `_`). The synthetic
/// `default` marker is filtered out: it names no code, and the canonical
/// closure form (`closure::DEV_FEATURES`) spells the default set without it,
/// so keeping it would make no default build ever match its own closure.
fn cargo_features() -> String {
    let mut features: Vec<String> = std::env::vars()
        .filter_map(|(name, _)| {
            name.strip_prefix("CARGO_FEATURE_")
                .map(str::to_ascii_lowercase)
        })
        .filter(|feature| feature != "default")
        .collect();
    features.sort();
    features.join(",")
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::{
        build_closure_paths, closure_pathspecs, has_dependency_root, has_unsafe_closure_symlink,
        manifest_has_local_dependency, BASE_CLOSURE_PATHS,
    };

    #[test]
    fn build_stamp_resolves_renamed_workspace_model_dependency() {
        let workflow = "[dependencies]\nmodel = { workspace = true }\n";
        let workspace = "[workspace]\n[workspace.dependencies]\nmodel = { package = \"velnor-model\", path = \"crates/velnor-model\" }\n";
        let mut expected = BASE_CLOSURE_PATHS
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>();
        expected.push("crates/velnor-model".to_owned());
        assert_eq!(build_closure_paths(workflow, workspace), Some(expected));
    }

    #[test]
    fn build_stamp_fails_closed_on_unresolved_model_dependency() {
        let workflow = "[dependencies]\nmodel = { workspace = true }\n";
        assert_eq!(build_closure_paths(workflow, "[workspace]\n"), None);
    }

    #[test]
    fn build_stamp_fails_closed_on_base_ancestor_and_malformed_declarations() {
        assert_eq!(
            build_closure_paths(
                "[dependencies]\nhelper = { path = \"..\" }\n",
                "[workspace]\n"
            ),
            None
        );
        assert_eq!(
            build_closure_paths("[dependencies]\nhelper = 42\n", "[workspace]\n"),
            None
        );
        assert_eq!(
            build_closure_paths(
                "[dependencies]\nhelper = { workspace = true }\n",
                "[workspace]\ndependencies = 42\n"
            ),
            None
        );
        assert_eq!(
            build_closure_paths(
                "[dependencies]\nhelper = { workspace = true }\n",
                "[workspace]\n[workspace.dependencies]\nhelper = 42\n"
            ),
            None
        );
    }

    #[test]
    fn build_stamp_fails_closed_on_dependency_array_of_tables() {
        let workflow = "[[dependencies]]\npackage = \"velnor-model\"\npath = \"../velnor-model\"\n";
        assert_eq!(build_closure_paths(workflow, "[workspace]\n"), None);
    }

    #[test]
    fn build_stamp_parses_multiline_inline_dependency() {
        let workflow = "[dependencies]\nvelnor-model = {\npath = \"../velnor-model\"\n}\n";
        assert!(build_closure_paths(workflow, "[workspace]\n")
            .expect("valid multiline TOML")
            .contains(&"crates/velnor-model".to_owned()));
    }

    #[test]
    fn build_stamp_handles_comments_and_triple_quoted_paths() {
        let workflow = "[dependencies] # Cargo dependency table\nmodel = { package = \"velnor-model\", path = \"../velnor-model\" }\n";
        let mut expected = BASE_CLOSURE_PATHS
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>();
        expected.push("crates/velnor-model".to_owned());
        assert_eq!(
            build_closure_paths(workflow, "[workspace] # root workspace\n"),
            Some(expected)
        );

        let triple_quoted = "[dependencies] # Cargo dependency table\nmodel = { package = \"velnor-model\", path = \"\"\"../velnor-model\"\"\" }\n";
        assert!(build_closure_paths(triple_quoted, "[workspace]\n")
            .expect("valid TOML multiline string")
            .contains(&"crates/velnor-model".to_owned()));
    }

    #[test]
    fn build_stamp_checks_all_local_model_aliases() {
        let workflow = "[dependencies]\nmodel = { package = \"velnor-model\", path = \"../velnor-model\" }\nother = { package = \"velnor-model\", path = \"../../../outside\" }\n";
        assert_eq!(build_closure_paths(workflow, "[workspace]\n"), None);
    }

    #[test]
    fn build_stamp_uses_toml_dotted_keys_and_all_dependency_kinds() {
        let workflow = r#"
[build-dependencies."build.helper"]
path = "../../tools/helper"

[target.'cfg(unix)'.dependencies.'target.helper']
path = "../native-helper"

[dependencies]
shared = { workspace = true }
"#;
        let workspace = r#"
[workspace.dependencies.shared]
path = "tools/shared"
"#;
        let paths = build_closure_paths(workflow, workspace).expect("valid TOML");
        assert!(paths.contains(&"tools/helper".to_owned()));
        assert!(paths.contains(&"crates/native-helper".to_owned()));
        assert!(paths.contains(&"tools/shared".to_owned()));
    }

    #[test]
    fn build_stamp_rejects_dependency_symlinks_but_keeps_legacy_links() {
        assert!(has_unsafe_closure_symlink(&[
            "120000 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tcrates/helper/src/linked.rs"
        ]));
        assert!(!has_unsafe_closure_symlink(
            &["120000 blob bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\tcrates/velnor-workflow/CLAUDE.md"]
        ));
        assert!(has_unsafe_closure_symlink(
            &["120000 blob cccccccccccccccccccccccccccccccccccccccc\tcrates/velnor-workflow/src/linked.rs"]
        ));
    }

    #[test]
    fn build_stamp_requires_every_selected_dependency_tree() {
        let lines = [
            "100644 blob aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\tcrates/velnor-model/Cargo.toml",
        ];
        assert!(has_dependency_root(&lines, "crates/velnor-model"));
        assert!(!has_dependency_root(&lines, "crates/missing-model"));
    }

    #[test]
    fn build_stamp_fails_closed_on_transitive_path_dependencies() {
        let manifest =
            "[package]\nname = \"helper\"\n[dependencies]\nnested = { path = \"../nested\" }\n";
        assert_eq!(
            manifest_has_local_dependency(
                manifest,
                "[workspace]\n",
                "crates/velnor-model",
                "crates/velnor-model/Cargo.toml"
            ),
            Some(true)
        );
        let inherited =
            "[package]\nname = \"helper\"\n[dependencies]\nnested = { workspace = true }\n";
        let workspace = "[workspace]\nmembers = [\"crates/velnor-model\"]\n[workspace.dependencies]\nnested = { path = \"crates/nested\" }\n";
        assert_eq!(
            manifest_has_local_dependency(
                inherited,
                workspace,
                "crates/velnor-model",
                "crates/velnor-model/Cargo.toml"
            ),
            Some(true)
        );
        assert_eq!(
            manifest_has_local_dependency(
                inherited,
                "[workspace]\nmembers = [\"crates/velnor-model\"]\n[workspace.dependencies]\nnested = \"1\"\n",
                "crates/velnor-model/nested",
                "crates/velnor-model/nested/Cargo.toml"
            ),
            None
        );
        assert_eq!(
            manifest_has_local_dependency(
                "[dependencies]\nnested = { workspace = true, path = \"../shadow\" }\n",
                "[workspace]\nmembers = [\"crates/velnor-model\"]\n[workspace.dependencies]\nnested = \"1\"\n",
                "crates/velnor-model",
                "crates/velnor-model/Cargo.toml"
            ),
            None
        );
        assert_eq!(
            manifest_has_local_dependency(
                "[dependencies]\nnested = { package = 42, path = \"../nested\" }\n",
                "[workspace]\n",
                "crates/velnor-model",
                "crates/velnor-model/Cargo.toml"
            ),
            None
        );
    }

    #[test]
    fn build_stamp_collapses_nested_local_dependency_roots() {
        let workflow = "[dependencies]\nouter = { path = \"../../vendor\" }\ninner = { path = \"../../vendor/inner\" }\n";
        let paths = build_closure_paths(workflow, "[workspace]\n").expect("valid paths");
        assert!(paths.contains(&"vendor".to_owned()));
        assert!(!paths.contains(&"vendor/inner".to_owned()));
    }

    #[test]
    fn build_stamp_uses_literal_pathspecs_for_glob_characters() {
        let mut paths = BASE_CLOSURE_PATHS
            .iter()
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>();
        paths.push("vendor/[literal]*".to_owned());
        let pathspecs = closure_pathspecs(&paths);
        assert_eq!(
            pathspecs.last().map(String::as_str),
            Some(":(literal)vendor/[literal]*")
        );
    }
}
