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

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const BASE_CLOSURE_PATHS: &[&str] = &[
    "crates/velnor-workflow",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "rust-toolchain",
    ".cargo",
];
const VELNOR_MODEL_PATH: &str = "crates/velnor-model";

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
    let mut arguments = vec!["ls-tree", "-r", "HEAD", "--"];
    arguments.extend_from_slice(&paths);
    let listing = git(&root, &arguments)?;
    let mut lines: Vec<&str> = listing.lines().collect();
    if lines.is_empty() {
        return None;
    }
    lines.sort_unstable();
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

/// Dependency detector for build.rs. Cargo has already parsed these manifests
/// before invoking the build script. This narrow reader only accepts the
/// dependency table forms that can select the optional local source path; any
/// ambiguous model declaration returns `None`, stamping `unknown` instead of
/// minting a digest that could omit compiled source.
fn build_closure_paths(workflow: &str, workspace: &str) -> Option<Vec<&'static str>> {
    let dependencies = dependency_declarations(workflow, false)?;
    let workspace_dependencies = dependency_declarations(workspace, true)?;
    let mut includes_model = false;
    for (alias, declaration) in dependencies {
        let effective = if declaration.workspace {
            workspace_dependencies.get(&alias)?
        } else {
            &declaration
        };
        if effective.package.as_deref().unwrap_or(alias.as_str()) != "velnor-model" {
            continue;
        }
        let Some(path) = effective.path.as_deref() else {
            continue;
        };
        let base = if declaration.workspace {
            ""
        } else {
            "crates/velnor-workflow"
        };
        if normalize_repo_path(base, path)? != VELNOR_MODEL_PATH {
            return None;
        }
        includes_model = true;
    }
    if includes_model {
        let mut paths = BASE_CLOSURE_PATHS.to_vec();
        paths.push(VELNOR_MODEL_PATH);
        Some(paths)
    } else {
        Some(BASE_CLOSURE_PATHS.to_vec())
    }
}

#[derive(Default)]
struct DependencyDecl {
    package: Option<String>,
    path: Option<String>,
    workspace: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ManifestSection {
    Ignore,
    Dependencies,
    WorkspaceDependencies,
    DependencyTable,
}

fn dependency_declarations(
    manifest: &str,
    workspace_manifest: bool,
) -> Option<BTreeMap<String, DependencyDecl>> {
    let mut declarations = BTreeMap::new();
    let mut section = ManifestSection::Ignore;
    let mut table_alias: Option<String> = None;
    let mut table_decl = DependencyDecl::default();

    for raw_line in manifest.lines() {
        let raw_trimmed = strip_comment(raw_line)?.trim();
        if raw_trimmed.is_empty() || raw_trimmed.starts_with('#') {
            continue;
        }
        if let Some(array_header) = raw_trimmed
            .strip_prefix("[[")
            .and_then(|header| header.strip_suffix("]]"))
        {
            let array_header = array_header.trim();
            if array_header == "dependencies"
                || array_header == "build-dependencies"
                || (array_header.starts_with("target.")
                    && (array_header.ends_with(".dependencies")
                        || array_header.ends_with(".build-dependencies")))
            {
                return None;
            }
            section = ManifestSection::Ignore;
            continue;
        }
        if raw_trimmed.starts_with('[') && raw_trimmed.ends_with(']') {
            if let Some(alias) = table_alias.take() {
                declarations.insert(alias, table_decl);
                table_decl = DependencyDecl::default();
            }
            let header = raw_trimmed
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim();
            section = if workspace_manifest && header == "workspace.dependencies" {
                ManifestSection::WorkspaceDependencies
            } else if !workspace_manifest
                && (header == "dependencies"
                    || header == "build-dependencies"
                    || (header.starts_with("target.")
                        && (header.ends_with(".dependencies")
                            || header.ends_with(".build-dependencies"))))
            {
                ManifestSection::Dependencies
            } else {
                ManifestSection::Ignore
            };
            if let Some(alias) = dependency_table_alias(header, workspace_manifest) {
                section = ManifestSection::DependencyTable;
                table_alias = Some(alias);
            }
            continue;
        }
        if section == ManifestSection::Ignore {
            continue;
        }
        let line = strip_comment(raw_line)?.trim();
        if line.is_empty() {
            continue;
        }
        let (raw_key, value) = line.split_once('=')?;
        let key = parse_key(raw_key.trim())?;
        let value = value.trim();
        match section {
            ManifestSection::Dependencies | ManifestSection::WorkspaceDependencies => {
                declarations.insert(key.clone(), parse_dependency_decl(&key, value)?);
            }
            ManifestSection::DependencyTable => match key.as_str() {
                "package" => table_decl.package = Some(parse_toml_string(value)?),
                "path" => table_decl.path = Some(parse_toml_string(value)?),
                "workspace" => table_decl.workspace = value == "true",
                _ => {}
            },
            ManifestSection::Ignore => {}
        }
    }
    if let Some(alias) = table_alias {
        declarations.insert(alias, table_decl);
    }
    Some(declarations)
}

fn dependency_table_alias(header: &str, workspace_manifest: bool) -> Option<String> {
    if workspace_manifest {
        return header
            .strip_prefix("workspace.dependencies.")
            .and_then(parse_key);
    }
    for marker in ["dependencies.", "build-dependencies."] {
        if let Some(alias) = header.strip_prefix(marker) {
            return parse_key(alias);
        }
    }
    for marker in [".dependencies.", ".build-dependencies."] {
        if let Some((prefix, alias)) = header.rsplit_once(marker)
            && (prefix.is_empty() || prefix == "target" || prefix.starts_with("target."))
        {
            return parse_key(alias);
        }
    }
    None
}

fn parse_dependency_decl(alias: &str, value: &str) -> Option<DependencyDecl> {
    if value.starts_with('{') && value.ends_with('}') {
        let mut declaration = DependencyDecl::default();
        for field in value[1..value.len() - 1].split(',') {
            let Some((raw_key, field_value)) = field.split_once('=') else {
                continue;
            };
            let key = parse_key(raw_key.trim())?;
            let field_value = field_value.trim();
            match key.as_str() {
                "package" => declaration.package = Some(parse_toml_string(field_value)?),
                "path" => declaration.path = Some(parse_toml_string(field_value)?),
                "workspace" => declaration.workspace = field_value == "true",
                _ => {}
            }
        }
        Some(declaration)
    } else if value.starts_with('"') || value.starts_with('\'') {
        Some(DependencyDecl {
            package: Some(alias.to_owned()),
            ..DependencyDecl::default()
        })
    } else {
        None
    }
}

fn parse_key(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with("\"\"\"") || value.starts_with("'''") {
        return None;
    }
    if value.starts_with('"') || value.starts_with('\'') {
        let quote = value.chars().next()?;
        if value.chars().last()? != quote || value.len() < 2 {
            return None;
        }
        return Some(value[1..value.len() - 1].to_owned());
    }
    (!value.is_empty()).then(|| value.to_owned())
}

fn parse_toml_string(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with("\"\"\"") || value.starts_with("'''") {
        return None;
    }
    let quote = value.chars().next()?;
    if !matches!(quote, '"' | '\'') || value.chars().last()? != quote || value.len() < 2 {
        return None;
    }
    Some(value[1..value.len() - 1].to_owned())
}

fn strip_comment(line: &str) -> Option<&str> {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote == Some('"') {
            escaped = true;
            continue;
        }
        if matches!(character, '"' | '\'') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
        } else if character == '#' && quote.is_none() {
            return Some(&line[..index]);
        }
    }
    quote.is_none().then_some(line)
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
    use super::{build_closure_paths, BASE_CLOSURE_PATHS, VELNOR_MODEL_PATH};

    #[test]
    fn build_stamp_resolves_renamed_workspace_model_dependency() {
        let workflow = "[dependencies]\nmodel = { workspace = true }\n";
        let workspace = "[workspace]\n[workspace.dependencies]\nmodel = { package = \"velnor-model\", path = \"crates/velnor-model\" }\n";
        let mut expected = BASE_CLOSURE_PATHS.to_vec();
        expected.push(VELNOR_MODEL_PATH);
        assert_eq!(build_closure_paths(workflow, workspace), Some(expected));
    }

    #[test]
    fn build_stamp_fails_closed_on_unresolved_model_dependency() {
        let workflow = "[dependencies]\nmodel = { workspace = true }\n";
        assert_eq!(build_closure_paths(workflow, "[workspace]\n"), None);
    }

    #[test]
    fn build_stamp_fails_closed_on_dependency_array_of_tables() {
        let workflow = "[[dependencies]]\npackage = \"velnor-model\"\npath = \"../velnor-model\"\n";
        assert_eq!(build_closure_paths(workflow, "[workspace]\n"), None);
    }

    #[test]
    fn build_stamp_fails_closed_on_multiline_inline_dependency() {
        let workflow = "[dependencies]\nvelnor-model = {\npath = \"../velnor-model\"\n}\n";
        assert_eq!(build_closure_paths(workflow, "[workspace]\n"), None);
    }

    #[test]
    fn build_stamp_handles_commented_headers_and_rejects_triple_quoted_paths() {
        let workflow = "[dependencies] # Cargo dependency table\nmodel = { package = \"velnor-model\", path = \"../velnor-model\" }\n";
        let mut expected = BASE_CLOSURE_PATHS.to_vec();
        expected.push(VELNOR_MODEL_PATH);
        assert_eq!(
            build_closure_paths(workflow, "[workspace] # root workspace\n"),
            Some(expected)
        );

        let triple_quoted = "[dependencies] # Cargo dependency table\nmodel = { package = \"velnor-model\", path = \"\"\"../velnor-model\"\"\" }\n";
        assert_eq!(build_closure_paths(triple_quoted, "[workspace]\n"), None);
    }

    #[test]
    fn build_stamp_checks_all_local_model_aliases() {
        let workflow = "[dependencies]\nmodel = { package = \"velnor-model\", path = \"../velnor-model\" }\nother = { package = \"velnor-model\", path = \"../../outside\" }\n";
        assert_eq!(build_closure_paths(workflow, "[workspace]\n"), None);
    }
}
