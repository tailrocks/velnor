//! Rust detector: Cargo manifests, workspace graph, and Rust source facts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::file_walk::{
    files_named, has_extension, join_repo_path, path_prefix, resolve_repo_path,
};
use super::{RepositoryShape, ScanContext};
use crate::rust_include::{parse_include_paths, resolve_include_path, IncludePathError};
use crate::s2::{
    identifier_suffix, parent_path, shell_change_dir, shell_quote, CachePurpose, CacheSpec,
    GeneratorError, RustToolchain, Unit, UnitKind,
};

#[cfg(test)]
pub(crate) use crate::rust_include::parse_include_str_literals;

/// The scanner's Rust dependency-policy unit. Its commands resolve the
/// advisory databases themselves, so the Cargo-network restrictions applied to
/// ordinary Rust units deliberately do not apply to it.
const POLICY_UNIT_ID: &str = "rust-policy";

/// Installation profiles `rustup toolchain install --profile` accepts. Any
/// other value fails the install, so the scan rejects it up front.
const RUSTUP_PROFILES: [&str; 3] = ["minimal", "default", "complete"];

/// Read the repository's pinned toolchain from a repository directory,
/// statting the two candidate files. For callers that already hold the walked
/// file set, prefer [`parse_rust_toolchain`].
pub(crate) fn parse_rust_toolchain_from_dir(
    root: &Path,
) -> Result<Option<RustToolchain>, GeneratorError> {
    if root.join("rust-toolchain.toml").is_file() || root.join("rust-toolchain").is_file() {
        let files = super::file_walk::repository_files(root, &[])?;
        let file_set: BTreeSet<String> = files.iter().cloned().collect();
        return parse_rust_toolchain(root, &file_set);
    }
    Ok(None)
}

/// Parse the repository's pinned Rust toolchain, if it declares one.
///
/// `rust-toolchain.toml` is the canonical form and carries the contract under
/// its `[toolchain]` table; the legacy bare `rust-toolchain` file carries only
/// a channel. Everything the renderer later feeds to `rustup` is validated
/// here, so an unparsable pin fails the scan instead of failing a workflow
/// step on a runner.
pub(crate) fn parse_rust_toolchain(
    root: &Path,
    file_set: &BTreeSet<String>,
) -> Result<Option<RustToolchain>, GeneratorError> {
    if file_set.contains("rust-toolchain.toml") {
        let path = root.join("rust-toolchain.toml");
        let contents = fs::read_to_string(&path)
            .map_err(|error| GeneratorError::io("read rust-toolchain.toml", &path, &error))?;
        return parse_toolchain_table(&contents, &path).map(Some);
    }
    if file_set.contains("rust-toolchain") {
        let path = root.join("rust-toolchain");
        let contents = fs::read_to_string(&path)
            .map_err(|error| GeneratorError::io("read rust-toolchain", &path, &error))?;
        return Ok(Some(RustToolchain {
            channel: validate_toolchain_value("channel", contents.trim(), &path)?,
            components: Vec::new(),
            targets: Vec::new(),
            profile: None,
        }));
    }
    Ok(None)
}

#[expect(
    clippy::too_many_lines,
    reason = "one pass over the toolchain table keeps validation next to parsing"
)]
fn parse_toolchain_table(contents: &str, path: &Path) -> Result<RustToolchain, GeneratorError> {
    let mut channel = None;
    let mut profile = None;
    let mut components = Vec::new();
    let mut targets = Vec::new();
    let lines: Vec<&str> = contents.lines().collect();
    let mut section = String::new();
    let mut index = 0_usize;
    while index < lines.len() {
        let trimmed = strip_toml_comment(lines[index]).trim().to_owned();
        index += 1;
        if trimmed.is_empty() {
            continue;
        }
        if let Some(header) = trimmed.strip_prefix('[') {
            let Some(header) = header.strip_suffix(']') else {
                return Err(GeneratorError::usage(format!(
                    "malformed table header in {}: {trimmed}",
                    path.display()
                )));
            };
            header.trim().clone_into(&mut section);
            continue;
        }
        if section != "toolchain" {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(GeneratorError::usage(format!(
                "expected `key = value` under [toolchain] in {}: {trimmed}",
                path.display()
            )));
        };
        let key = key.trim();
        let mut value = value.trim().to_owned();
        while toml_array_is_open(&value) {
            let Some(next) = lines.get(index) else {
                break;
            };
            index += 1;
            value.push(' ');
            value.push_str(strip_toml_comment(next).trim());
        }
        match key {
            "channel" => {
                channel = Some(toml_string_value(&value).ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "[toolchain].channel must be a quoted string in {}",
                        path.display()
                    ))
                })?);
            }
            "profile" => {
                let parsed = toml_string_value(&value).ok_or_else(|| {
                    GeneratorError::usage(format!(
                        "[toolchain].profile must be a quoted string in {}",
                        path.display()
                    ))
                })?;
                if !RUSTUP_PROFILES.contains(&parsed.as_str()) {
                    return Err(GeneratorError::usage(format!(
                        "[toolchain].profile is `{parsed}` in {}, but rustup only accepts {}",
                        path.display(),
                        RUSTUP_PROFILES
                            .map(|profile| format!("`{profile}`"))
                            .join(", ")
                    )));
                }
                profile = Some(parsed);
            }
            // `toml_array_values` already unwraps each quoted entry.
            "components" | "targets" => {
                let parsed = toml_array_values(&value);
                if key == "components" {
                    components = parsed;
                } else {
                    targets = parsed;
                }
            }
            other => {
                return Err(GeneratorError::usage(format!(
                    "unknown [toolchain] key `{other}` in {}",
                    path.display()
                )));
            }
        }
    }
    let Some(channel) = channel else {
        return Err(GeneratorError::usage(format!(
            "[toolchain] declares no channel in {}",
            path.display()
        )));
    };
    for (field, values) in [
        ("channel", std::slice::from_ref(&channel)),
        ("components", &components),
        ("targets", &targets),
    ] {
        for value in values {
            validate_toolchain_value(field, value, path)?;
        }
    }
    Ok(RustToolchain {
        channel,
        components,
        targets,
        profile,
    })
}

/// Reject anything that would not survive being rendered into a quoted shell
/// word on a runner: whitespace, control characters, and shell metacharacters.
fn validate_toolchain_value(
    field: &str,
    value: &str,
    path: &Path,
) -> Result<String, GeneratorError> {
    let valid = !value.is_empty()
        && !value.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || !matches!(character,
                    'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '-' | '_' | '/' | ':' | '+')
        });
    if valid {
        Ok(value.to_owned())
    } else {
        Err(GeneratorError::usage(format!(
            "[toolchain].{field} value `{value}` is not a safe toolchain identifier in {}",
            path.display()
        )))
    }
}

fn cargo_deny_has_license_policy(root: &Path, file_set: &BTreeSet<String>) -> bool {
    ["deny.toml", ".cargo/deny.toml"].into_iter().any(|path| {
        file_set.contains(path)
            && fs::read_to_string(root.join(path))
                .is_ok_and(|contents| contents.lines().any(|line| line.trim() == "[licenses]"))
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CargoDependency {
    pub(crate) name: String,
    pub(crate) package_name: Option<String>,
    pub(crate) path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CargoManifestFacts {
    pub(crate) root: String,
    pub(crate) has_package: bool,
    pub(crate) package_name: Option<String>,
    pub(crate) has_workspace: bool,
    pub(crate) workspace_members: Vec<String>,
    pub(crate) workspace_excludes: Vec<String>,
    pub(crate) dependencies: Vec<CargoDependency>,
    pub(crate) build_script: Option<String>,
    pub(crate) binary_targets: Vec<String>,
    pub(crate) test_targets: Vec<String>,
    pub(crate) features: Vec<String>,
    pub(crate) crate_types: Vec<String>,
}

#[derive(Default)]
struct RustAnalysis {
    units: Vec<Unit>,
    detected: Vec<String>,
    limitations: Vec<String>,
}

/// Whether a workspace `members`/`exclude` pattern selects a package. Both
/// the pattern (workspace-relative) and the package root (repository-relative)
/// are `/`-separated paths; `*` matches one segment and `**` crosses
/// segments, cargo's manifest glob contract.
fn workspace_pattern_matches(pattern: &str, workspace_root: &str, package_root: &str) -> bool {
    fn segments(value: &str) -> Vec<&str> {
        value
            .split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .collect()
    }
    fn matches(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((head, tail)) if *head == "**" => {
                (0..=path.len()).any(|skip| matches(tail, &path[skip..]))
            }
            Some((head, tail)) => match path.split_first() {
                Some((name, rest)) if *head == "*" || *head == *name => matches(tail, rest),
                _ => false,
            },
        }
    }
    let mut absolute = segments(workspace_root);
    absolute.extend(segments(pattern));
    matches(&absolute, &segments(package_root))
}

#[expect(
    clippy::too_many_lines,
    reason = "Rust evidence normalization keeps graph construction together"
)]
fn analyze_rust_manifests(
    root: &Path,
    files: &[String],
    file_set: &BTreeSet<String>,
    manifests: &[String],
) -> Result<RustAnalysis, GeneratorError> {
    let mut facts = Vec::new();
    for manifest in manifests {
        let manifest_root = parent_path(manifest);
        let path = root.join(manifest);
        let contents = fs::read_to_string(&path)
            .map_err(|error| GeneratorError::io("read Cargo manifest", &path, &error))?;
        let parsed = parse_cargo_manifest(&manifest_root, &contents);
        for (field, value) in parsed
            .package_name
            .iter()
            .map(|value| ("package name", value))
            .chain(
                parsed
                    .build_script
                    .iter()
                    .map(|value| ("build script", value)),
            )
            .chain(
                parsed
                    .binary_targets
                    .iter()
                    .map(|value| ("binary target", value)),
            )
            .chain(
                parsed
                    .test_targets
                    .iter()
                    .map(|value| ("test target", value)),
            )
        {
            if value.chars().any(char::is_control) {
                return Err(GeneratorError::usage(format!(
                    "Cargo {field} contains control characters at {}",
                    path.display()
                )));
            }
        }
        facts.push(parsed);
    }

    let workspace_root = facts
        .iter()
        .find(|manifest| manifest.has_workspace)
        .map(|manifest| manifest.root.clone());
    let has_nextest =
        file_set.contains(".config/nextest.toml") || file_set.contains("nextest.toml");
    let has_cargo_deny = file_set.contains("deny.toml") || file_set.contains(".cargo/deny.toml");
    let has_cargo_audit = file_set.contains("audit.toml") || file_set.contains(".cargo/audit.toml");
    let toolchain = match parse_rust_toolchain(root, file_set)? {
        Some(toolchain) => toolchain,
        // A Rust repository without a pin has no toolchain contract to
        // render: provisioning would silently fall back to whatever mise or
        // the runner image resolves, which is exactly the drift the pin
        // exists to prevent. Fail the scan where the fix belongs.
        None if facts.iter().any(|manifest| manifest.has_package) => {
            return Err(GeneratorError::usage(
                "the repository contains Rust packages but pins no Rust toolchain; add \
                 rust-toolchain.toml with a [toolchain] channel (plus any components and \
                 targets) so every workflow provisions exactly that toolchain",
            ));
        }
        None => RustToolchain::default(),
    };
    let mut result = RustAnalysis::default();

    let workspaces = facts
        .iter()
        .filter(|manifest| manifest.has_workspace)
        .collect::<Vec<_>>();
    // Units verify crates cargo can resolve from their own directory: without
    // any workspace the scan keeps its single-crate behavior, but where
    // workspaces exist, explicitly excluded crates and orphan crates no
    // workspace claims (vendored sources without their own workspace table)
    // resolve nowhere, so no unit may target them. Repositories that verify
    // such crates with root-relative commands re-add them through [[units]].
    let package_facts = facts
        .iter()
        .filter(|manifest| manifest.has_package)
        .filter(|manifest| {
            // Explicit exclusion wins even over a self-governing crate: the
            // repository took it out of the workspace contract.
            workspaces.is_empty()
                || (!workspaces.iter().any(|workspace| {
                    workspace.workspace_excludes.iter().any(|pattern| {
                        workspace_pattern_matches(pattern, &workspace.root, &manifest.root)
                    })
                }) && (manifest.has_workspace
                    || workspaces.iter().any(|workspace| {
                        workspace.workspace_members.iter().any(|pattern| {
                            workspace_pattern_matches(pattern, &workspace.root, &manifest.root)
                        })
                    })))
        })
        .collect::<Vec<_>>();

    if workspace_root.is_some() {
        result
            .detected
            .push(format!("rust-workspace-members:{}", package_facts.len()));
    } else if facts.iter().any(|manifest| manifest.has_package) {
        result.detected.push("rust-package".to_owned());
    }
    if has_nextest {
        result.detected.push("cargo-nextest-policy".to_owned());
    }
    if has_cargo_deny {
        result.detected.push("cargo-deny-policy".to_owned());
    }
    if has_cargo_audit {
        result.detected.push("cargo-audit-policy".to_owned());
    }
    // Detected, never "mise provides rust": the Rust toolchain is rustup's
    // job and mise only ever contributes task-runner tools here.
    if files
        .iter()
        .any(|file| matches!(file.rsplit('/').next(), Some("mise.toml" | "mise.lock")))
    {
        result.detected.push("mise-present".to_owned());
    }

    let mut package_ids = BTreeMap::new();
    let mut roots_by_manifest = BTreeMap::new();
    let mut rust_id_counts = BTreeMap::new();
    for manifest in &package_facts {
        let suffix = manifest
            .package_name
            .as_deref()
            .map(identifier_suffix)
            .filter(|suffix| !suffix.is_empty())
            .unwrap_or_else(|| identifier_suffix(&manifest.root));
        let base_id = format!("rust-{suffix}");
        let count = rust_id_counts.entry(base_id.clone()).or_insert(0_usize);
        *count += 1;
        let id = if *count == 1 {
            base_id
        } else {
            format!("{base_id}-{count}")
        };
        roots_by_manifest.insert(join_repo_path(&manifest.root, "Cargo.toml"), id.clone());
        if let Some(package_name) = &manifest.package_name {
            package_ids.insert(package_name.clone(), id);
        }
    }

    let package_roots = package_facts
        .iter()
        .map(|manifest| manifest.root.clone())
        .collect::<Vec<_>>();
    let workspace_manifest = workspace_root
        .as_deref()
        .map(|workspace| join_repo_path(workspace, "Cargo.toml"));
    for manifest in &package_facts {
        let manifest_path = join_repo_path(&manifest.root, "Cargo.toml");
        let prefix = path_prefix(&manifest.root);
        let command_prefix = shell_change_dir(&manifest.root);
        let default_build_script = join_repo_path(&manifest.root, "build.rs");
        let build_script = manifest.build_script.as_deref().or_else(|| {
            file_set
                .contains(&default_build_script)
                .then_some("build.rs")
        });
        let has_test_sources = file_set.iter().any(|file| {
            file.starts_with(&format!("{prefix}tests/"))
                || file.starts_with(&format!("{prefix}benches/"))
        });
        let package_selector = manifest.package_name.as_deref().map_or_else(
            || format!("--manifest-path {}", shell_quote("Cargo.toml")),
            |name| format!("--package {}", shell_quote(name)),
        );
        let cargo_lock_flag = if file_set.contains("Cargo.lock") {
            "--locked"
        } else {
            ""
        };
        let test_command = if has_nextest {
            // Crates without any test target must still verify green: without
            // the flag nextest exits nonzero on an empty collection, failing
            // test-less crates that fmt and clippy accept.
            format!(
                "{command_prefix}cargo nextest run {cargo_lock_flag} --all-features {package_selector} --no-tests pass"
            )
        } else {
            format!(
                "{command_prefix}cargo test {cargo_lock_flag} --all-features {package_selector}"
            )
        };
        let clippy_command = format!(
            "{command_prefix}cargo clippy {cargo_lock_flag} --profile test --no-deps --all-targets --all-features {package_selector} -- -D warnings"
        );
        // Nextest before clippy: rustc test-profile artifacts are reused by
        // clippy, avoiding a separate dev-profile compile plus a clippy-driver
        // rebuild that nextest would not share anyway.
        let commands = vec![
            format!(
                "{command_prefix}cargo fmt --manifest-path {} -- --check",
                shell_quote("Cargo.toml")
            ),
            test_command,
            clippy_command,
        ];
        let mut watch = vec![
            manifest_path.clone(),
            "Cargo.lock".to_owned(),
            "rust-toolchain.toml".to_owned(),
            "rust-toolchain".to_owned(),
            "mise.toml".to_owned(),
            "mise.lock".to_owned(),
            ".cargo/**".to_owned(),
            format!("{prefix}**/*.rs"),
            format!("{prefix}src/**"),
            format!("{prefix}tests/**"),
            format!("{prefix}examples/**"),
            format!("{prefix}benches/**"),
        ];
        if let Some(workspace_manifest) = &workspace_manifest
            && workspace_manifest != &manifest_path
        {
            watch.push(workspace_manifest.clone());
        }
        if let Some(build_script) = build_script {
            if let Some(build_path) = resolve_repo_path(&manifest.root, build_script) {
                watch.push(build_path);
            }
            result.detected.push(format!(
                "rust-build-script:{}",
                manifest.package_name.as_deref().unwrap_or(&manifest.root)
            ));
        }
        if !manifest.binary_targets.is_empty() {
            let package = manifest.package_name.as_deref().unwrap_or(&manifest.root);
            result.detected.push(format!(
                "rust-binaries:{package}:{}",
                manifest.binary_targets.len()
            ));
            for bin in &manifest.binary_targets {
                if bin.ends_with("-guest-agent") {
                    result.detected.push(format!("guest-agent:{package}:{bin}"));
                }
                if bin.ends_with("-guest-image") {
                    result.detected.push(format!("guest-image:{package}:{bin}"));
                }
            }
        }
        if !manifest.test_targets.is_empty() || has_test_sources {
            result.detected.push(format!(
                "rust-test-targets:{}:{}",
                manifest.package_name.as_deref().unwrap_or(&manifest.root),
                manifest
                    .test_targets
                    .len()
                    .max(usize::from(has_test_sources))
            ));
        }
        if !manifest.features.is_empty() {
            result.detected.push(format!(
                "rust-features:{}:{}",
                manifest.package_name.as_deref().unwrap_or(&manifest.root),
                manifest.features.len()
            ));
            if manifest
                .features
                .iter()
                .any(|feature| feature == "release-build")
            {
                result.detected.push(format!(
                    "release-build:{}",
                    manifest.package_name.as_deref().unwrap_or(&manifest.root)
                ));
            }
        }
        watch.extend(include_str_paths_for_package(
            root,
            files,
            file_set,
            &package_roots,
            &manifest.root,
        )?);
        let local_lock = join_repo_path(&manifest.root, "Cargo.lock");
        if file_set.contains(&local_lock) {
            watch.push(local_lock.clone());
        }
        watch.sort();
        watch.dedup();
        let lockfile_key = if file_set.contains(&local_lock) {
            local_lock
        } else {
            "Cargo.lock".to_owned()
        };
        let cache_key_files = vec![
            ".cargo/**".to_owned(),
            lockfile_key,
            "rust-toolchain.toml".to_owned(),
            "rust-toolchain".to_owned(),
        ];
        // FFI shape is scan evidence (`rust-ffi:<name>`), never a placement
        // need: the crate still verifies on any executor, and prerequisite
        // wiring comes from the repository's own product declarations.
        if crate::s2::platform::is_ffi_crate_type(&manifest.crate_types) {
            result.detected.push(format!(
                "rust-ffi:{}",
                manifest.package_name.as_deref().unwrap_or(&manifest.root)
            ));
        }
        result.units.push(Unit {
            id: roots_by_manifest
                .get(&manifest_path)
                .cloned()
                .unwrap_or_else(|| format!("rust-{}", identifier_suffix(&manifest.root))),
            label: format!(
                "Rust crate ({})",
                manifest.package_name.as_deref().unwrap_or(&manifest.root)
            ),
            kind: UnitKind::Rust,
            root: manifest.root.clone(),
            watch,
            pr_commands: commands.clone(),
            full_commands: commands,
            depends_on: Vec::new(),
            pinned_lockfile: file_set.contains("Cargo.lock"),
            cache: Some(CacheSpec {
                key_files: cache_key_files,
                paths: vec!["~/.cargo/registry".to_owned(), "~/.cargo/git".to_owned()],
                purpose: CachePurpose::CargoSources,
                mbx_output_cache_justification: None,
                mutable_mount_seed: false,
            }),
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: Some(toolchain.clone()),
            services: Vec::new(),
            trust: crate::s2::provider::TrustReq::UntrustedOk,
            platform: crate::s2::provider::Platform::LinuxX64,
            capabilities: crate::s2::provider::Capabilities {
                docker: true,
                testcontainers: true,
                ..crate::s2::provider::Capabilities::default()
            },
            workspace_check: false,
            products: Vec::new(),
            prerequisites: Vec::new(),
            docker_contexts: Vec::new(),
            env: std::collections::BTreeMap::new(),
            mbx: None,
            prepared_tools: Vec::new(),
        });
    }

    if has_cargo_deny || has_cargo_audit {
        let mut commands = Vec::new();
        if has_cargo_deny {
            let command = if cargo_deny_has_license_policy(root, file_set) {
                "cargo deny check"
            } else {
                result.limitations.push(
                    "Cargo deny license checks are skipped because deny.toml declares no [licenses] policy.".to_owned(),
                );
                "cargo deny check advisories bans sources"
            };
            commands.push(command.to_owned());
        }
        if has_cargo_audit {
            commands.push("cargo audit".to_owned());
        }
        result.units.push(Unit {
            id: POLICY_UNIT_ID.to_owned(),
            label: "Rust dependency policy".to_owned(),
            kind: UnitKind::Rust,
            root: ".".to_owned(),
            watch: vec![
                "Cargo.toml".to_owned(),
                "Cargo.lock".to_owned(),
                "deny.toml".to_owned(),
                ".cargo/deny.toml".to_owned(),
                "audit.toml".to_owned(),
                ".cargo/audit.toml".to_owned(),
            ],
            pr_commands: commands.clone(),
            full_commands: commands,
            depends_on: Vec::new(),
            pinned_lockfile: file_set.contains("Cargo.lock"),
            cache: Some(CacheSpec {
                key_files: vec![
                    ".cargo/**".to_owned(),
                    "Cargo.lock".to_owned(),
                    "deny.toml".to_owned(),
                ],
                paths: vec!["~/.cargo/registry".to_owned(), "~/.cargo/git".to_owned()],
                purpose: CachePurpose::CargoSources,
                mbx_output_cache_justification: None,
                mutable_mount_seed: false,
            }),
            tool_version: None,
            mise_tools: Vec::new(),
            toolchain: Some(toolchain.clone()),
            services: Vec::new(),
            trust: crate::s2::provider::TrustReq::UntrustedOk,
            platform: crate::s2::provider::Platform::LinuxX64,
            capabilities: crate::s2::provider::Capabilities {
                docker: true,
                testcontainers: true,
                ..crate::s2::provider::Capabilities::default()
            },
            workspace_check: false,
            products: Vec::new(),
            prerequisites: Vec::new(),
            docker_contexts: Vec::new(),
            env: std::collections::BTreeMap::new(),
            mbx: None,
            prepared_tools: Vec::new(),
        });
    }

    for manifest in &package_facts {
        let owner = roots_by_manifest
            .get(&join_repo_path(&manifest.root, "Cargo.toml"))
            .cloned()
            .unwrap_or_default();
        let Some(unit) = result.units.iter_mut().find(|unit| unit.id == owner) else {
            continue;
        };
        let mut dependencies = BTreeSet::new();
        for dependency in &manifest.dependencies {
            let dependency_root = dependency
                .path
                .as_deref()
                .and_then(|path| resolve_repo_path(&manifest.root, path))
                .map(|path| join_repo_path(&path, "Cargo.toml"));
            let dependency_id = dependency_root
                .and_then(|path| roots_by_manifest.get(&path).cloned())
                .or_else(|| {
                    dependency
                        .package_name
                        .as_ref()
                        .and_then(|name| package_ids.get(name).cloned())
                })
                .or_else(|| package_ids.get(&dependency.name).cloned());
            if let Some(dependency_id) = dependency_id.filter(|id| id != &owner) {
                dependencies.insert(dependency_id);
            }
        }
        unit.depends_on = dependencies.into_iter().collect();
    }

    if facts.iter().any(|manifest| {
        manifest.build_script.is_some()
            || file_set.contains(&join_repo_path(&manifest.root, "build.rs"))
    }) {
        result.limitations.push(
            "Rust build scripts are watched but their system dependencies and generated outputs are not inferred.".to_owned(),
        );
    }
    if files.iter().any(|file| file == "Cargo.lock") {
        result.detected.push("cargo-lockfile".to_owned());
    }
    result.detected.sort();
    result.detected.dedup();
    result.limitations.sort();
    result.limitations.dedup();
    Ok(result)
}

#[cfg(test)]
fn include_str_paths(
    root: &Path,
    files: &[String],
    file_set: &BTreeSet<String>,
    package_root: &str,
) -> Result<Vec<String>, GeneratorError> {
    include_str_paths_for_package(root, files, file_set, &[], package_root)
}

fn include_str_paths_for_package(
    root: &Path,
    files: &[String],
    file_set: &BTreeSet<String>,
    package_roots: &[String],
    package_root: &str,
) -> Result<Vec<String>, GeneratorError> {
    let mut targets = BTreeSet::new();
    for source in files.iter().filter(|file| {
        has_extension(file, "rs") && source_belongs_to_package(file, package_roots, package_root)
    }) {
        let source_contents = fs::read_to_string(root.join(source))
            .map_err(|error| GeneratorError::io("read Rust source", &root.join(source), &error))?;
        let included_paths = parse_include_paths(&source_contents).map_err(|error| {
            GeneratorError::usage(format!("{error} in {}", root.join(source).display()))
        })?;
        for included in included_paths {
            let target = resolve_include_path(
                root,
                &parent_path(source),
                package_root,
                &included,
                file_set,
            )
            .map_err(|error| match error {
                IncludePathError::Escapes => GeneratorError::usage(format!(
                    "include_str! escapes the repository from {}: {}",
                    root.join(source).display(),
                    included.display()
                )),
                IncludePathError::Missing => GeneratorError::usage(format!(
                    "include_str! target does not exist: {} -> {}",
                    root.join(source).display(),
                    included.display()
                )),
                IncludePathError::Io {
                    operation,
                    path,
                    source,
                } => GeneratorError::io(operation, &path, &source),
            })?;
            targets.insert(target);
        }
    }
    Ok(targets.into_iter().collect())
}

fn source_belongs_to_package(source: &str, package_roots: &[String], package_root: &str) -> bool {
    // The test-only helper has no manifest census. Preserve its historical
    // package-prefix behavior in that mode; production always supplies all
    // Cargo package roots and can therefore assign every source to its
    // nearest (longest) owning root.
    if package_roots.is_empty() {
        return package_root == "." || source.starts_with(&path_prefix(package_root));
    }
    package_roots
        .iter()
        .filter(|candidate| *candidate == "." || source.starts_with(&path_prefix(candidate)))
        .max_by_key(|candidate| candidate.len())
        .is_some_and(|owner| owner == package_root)
}

pub(crate) fn cargo_dependency_name(key: &str) -> &str {
    key.strip_suffix(".workspace").unwrap_or(key)
}

pub(crate) fn parse_cargo_manifest(root: &str, contents: &str) -> CargoManifestFacts {
    let mut facts = CargoManifestFacts {
        root: root.to_owned(),
        has_package: false,
        package_name: None,
        has_workspace: false,
        workspace_members: Vec::new(),
        workspace_excludes: Vec::new(),
        dependencies: Vec::new(),
        build_script: None,
        binary_targets: Vec::new(),
        test_targets: Vec::new(),
        features: Vec::new(),
        crate_types: Vec::new(),
    };
    let mut section = String::new();
    let lines = contents.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        let line = strip_toml_comment(lines[index]).trim().to_owned();
        index += 1;
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let normalized_section = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim_start_matches('[')
                .trim_end_matches(']');
            normalized_section.clone_into(&mut section);
            if section == "package" {
                facts.has_package = true;
            }
            if section == "workspace" {
                facts.has_workspace = true;
            }
            continue;
        }
        let Some((key, first_value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().trim_matches('"').to_owned();
        let mut value = first_value.trim().to_owned();
        while toml_array_is_open(&value) && index < lines.len() {
            value.push(' ');
            value.push_str(strip_toml_comment(lines[index]).trim());
            index += 1;
        }
        match section.as_str() {
            "package" if key == "name" => facts.package_name = toml_string_value(&value),
            "package" if key == "build" => facts.build_script = toml_string_value(&value),
            "workspace" if key == "members" => {
                facts.workspace_members = toml_array_values(&value);
            }
            "workspace" if key == "exclude" => {
                facts.workspace_excludes = toml_array_values(&value);
            }
            "bin" if key == "name" => {
                if let Some(name) = toml_string_value(&value) {
                    facts.binary_targets.push(name);
                }
            }
            "test" if key == "name" => {
                if let Some(name) = toml_string_value(&value) {
                    facts.test_targets.push(name);
                }
            }
            "lib" if key == "crate-type" => {
                facts.crate_types = toml_string_value(&value)
                    .map_or_else(|| toml_array_values(&value), |single| vec![single]);
            }
            "features" => facts.features.push(key),
            section if is_cargo_dependency_section(section) => {
                facts.dependencies.push(CargoDependency {
                    name: cargo_dependency_name(&key).to_owned(),
                    package_name: toml_inline_string(&value, "package"),
                    path: toml_inline_string(&value, "path"),
                });
            }
            _ => {}
        }
    }
    facts
}

fn is_cargo_dependency_section(section: &str) -> bool {
    matches!(
        section,
        "dependencies" | "dev-dependencies" | "build-dependencies"
    ) || (section != "workspace.dependencies" && section.ends_with(".dependencies"))
}

fn strip_toml_comment(line: &str) -> String {
    let mut output = String::new();
    let mut basic = false;
    let mut literal = false;
    let mut escaped = false;
    for character in line.chars() {
        if basic {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                basic = false;
            }
        } else if literal {
            if character == '\'' {
                literal = false;
            }
        } else if character == '"' {
            basic = true;
        } else if character == '\'' {
            literal = true;
        } else if character == '#' {
            break;
        }
        output.push(character);
    }
    output
}

fn toml_array_is_open(value: &str) -> bool {
    let mut depth = 0_i32;
    let mut quote = None;
    let mut escaped = false;
    for character in value.chars() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if active == '"' && character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        } else if character == '[' {
            depth += 1;
        } else if character == ']' {
            depth -= 1;
        }
    }
    depth > 0
}

fn toml_string_value(value: &str) -> Option<String> {
    let value = value.trim();
    let quote = value.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut result = String::new();
    let mut escaped = false;
    for character in value.chars().skip(1) {
        if quote == '"' && escaped {
            result.push(match character {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if quote == '"' && character == '\\' {
            escaped = true;
        } else if character == quote {
            return Some(result);
        } else {
            result.push(character);
        }
    }
    None
}

fn toml_array_values(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut quote = None;
    let mut current = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if let Some(active) = quote {
            if active == '"' && escaped {
                current.push(match character {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    other => other,
                });
                escaped = false;
            } else if active == '"' && character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
                values.push(std::mem::take(&mut current));
            } else {
                current.push(character);
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        }
    }
    values
}

fn toml_inline_string(value: &str, wanted_key: &str) -> Option<String> {
    let value = value.trim().strip_prefix('{')?.strip_suffix('}')?;
    value.split(',').find_map(|entry| {
        let (key, value) = entry.split_once('=')?;
        (key.trim().trim_matches('"') == wanted_key).then(|| toml_string_value(value.trim()))?
    })
}

pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<(), GeneratorError> {
    let cargo_manifests = files_named(context.files, "Cargo.toml");
    if cargo_manifests.is_empty() {
        return Ok(());
    }
    let rust = analyze_rust_manifests(
        context.root,
        context.files,
        context.file_set,
        &cargo_manifests,
    )?;
    shape.units.extend(rust.units);
    shape.detected.extend(rust.detected);
    shape.limitations.extend(rust.limitations);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::{include_str_paths, include_str_paths_for_package, parse_cargo_manifest};
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    #[test]
    fn lib_crate_types_mark_ffi_evidence() {
        let facts = parse_cargo_manifest(
            "crates/ffi",
            "[package]\nname = \"ffi\"\nversion = \"0.1.0\"\n\n[lib]\ncrate-type = [\"staticlib\", \"rlib\"]\n",
        );
        assert_eq!(facts.crate_types, vec!["staticlib", "rlib"]);
        assert!(crate::s2::platform::is_ffi_crate_type(&facts.crate_types));
        let single = parse_cargo_manifest(
            "crates/ffi",
            "[package]\nname = \"ffi\"\nversion = \"0.1.0\"\n\n[lib]\ncrate-type = \"cdylib\"\n",
        );
        assert_eq!(single.crate_types, vec!["cdylib"]);
        assert!(crate::s2::platform::is_ffi_crate_type(&single.crate_types));
        let plain = parse_cargo_manifest(
            "crates/plain",
            "[package]\nname = \"plain\"\nversion = \"0.1.0\"\n",
        );
        assert!(plain.crate_types.is_empty());
        assert!(!crate::s2::platform::is_ffi_crate_type(&plain.crate_types));
    }

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-rust-scan-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default()
        ));
        must(fs::create_dir_all(&root), "create scratch directory");
        root
    }

    #[test]
    fn include_str_through_symlinked_directory_watches_resolved_tracked_file() {
        let root = scratch("symlinked-include");
        must(
            fs::create_dir_all(root.join("src")),
            "create source directory",
        );
        must(
            fs::create_dir_all(root.join("assets")),
            "create asset directory",
        );
        must(
            fs::write(
                root.join("src/lib.rs"),
                "const MANIFEST: &str = include_str!(\"linked/manifest.yml\");\n",
            ),
            "write Rust source",
        );
        must(
            fs::write(root.join("assets/manifest.yml"), "manifest\n"),
            "write include target",
        );
        must(
            symlink("../assets", root.join("src/linked")),
            "create tracked symlink directory",
        );

        let files = vec!["assets/manifest.yml".to_owned(), "src/lib.rs".to_owned()];
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        let targets = must(
            include_str_paths(&root, &files, &file_set, "."),
            "resolve symlinked include target",
        );

        assert_eq!(targets, vec!["assets/manifest.yml"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn include_str_manifest_dir_concat_tracks_package_source_and_ignores_docs_template() {
        let root = scratch("manifest-dir-concat");
        must(
            fs::create_dir_all(root.join("crates/app/src")),
            "create package source directory",
        );
        must(
            fs::create_dir_all(root.join("crates/app/assets")),
            "create package asset directory",
        );
        must(
            fs::create_dir_all(root.join("docs/templates")),
            "create docs template directory",
        );
        must(
            fs::write(
                root.join("crates/app/src/lib.rs"),
                "const SOURCE: &str = include_str!(concat!(\n    env!(\"CARGO_MANIFEST_DIR\"),\n    \"/src/lib.rs\",\n));\nconst FONT: &[u8] = include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/assets/font.ttf\"));\n",
            ),
            "write package source",
        );
        must(
            fs::write(root.join("crates/app/assets/font.ttf"), b"font\n"),
            "write package asset",
        );
        must(
            fs::write(
                root.join("docs/templates/example.rs"),
                "const DOC: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/missing.rs\"));\n",
            ),
            "write docs template",
        );

        let files = vec![
            "crates/app/assets/font.ttf".to_owned(),
            "crates/app/src/lib.rs".to_owned(),
            "docs/templates/example.rs".to_owned(),
        ];
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        let targets = must(
            include_str_paths(&root, &files, &file_set, "crates/app"),
            "resolve manifest-dir include",
        );

        assert_eq!(
            targets,
            vec!["crates/app/assets/font.ttf", "crates/app/src/lib.rs"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn root_package_include_scan_excludes_nested_member_sources() {
        let root = scratch("manifest-dir-workspace-member");
        must(
            fs::create_dir_all(root.join("src")),
            "create root source directory",
        );
        must(
            fs::create_dir_all(root.join("crates/member/src")),
            "create member source directory",
        );
        must(
            fs::create_dir_all(root.join("crates/member/assets")),
            "create member asset directory",
        );
        must(
            fs::write(
                root.join("src/lib.rs"),
                "const ROOT: &str = include_str!(\"../root.txt\");\n",
            ),
            "write root source",
        );
        must(
            fs::write(root.join("root.txt"), "root\n"),
            "write root asset",
        );
        must(
            fs::write(
                root.join("crates/member/src/lib.rs"),
                "const MEMBER: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/assets/member.txt\"));\n",
            ),
            "write member source",
        );
        must(
            fs::write(root.join("crates/member/assets/member.txt"), "member\n"),
            "write member asset",
        );

        let files = vec![
            "crates/member/assets/member.txt".to_owned(),
            "crates/member/src/lib.rs".to_owned(),
            "root.txt".to_owned(),
            "src/lib.rs".to_owned(),
        ];
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        let package_roots = vec![".".to_owned(), "crates/member".to_owned()];
        let root_targets = must(
            include_str_paths_for_package(&root, &files, &file_set, &package_roots, "."),
            "resolve root package includes",
        );
        assert_eq!(root_targets, vec!["root.txt"]);
        let member_targets = must(
            include_str_paths_for_package(
                &root,
                &files,
                &file_set,
                &package_roots,
                "crates/member",
            ),
            "resolve member package includes",
        );
        assert_eq!(member_targets, vec!["crates/member/assets/member.txt"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nested_package_include_scan_uses_nearest_package_owner() {
        let root = scratch("manifest-dir-nested-package-ownership");
        for directory in [
            "crates/outer/src",
            "crates/outer/assets",
            "crates/outer/tools/inner/src",
            "crates/outer/tools/inner/assets",
        ] {
            must(
                fs::create_dir_all(root.join(directory)),
                "create package directory",
            );
        }
        must(
            fs::write(
                root.join("crates/outer/src/lib.rs"),
                "const OUTER: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/assets/outer.txt\"));\n",
            ),
            "write outer source",
        );
        must(
            fs::write(
                root.join("crates/outer/tools/inner/src/lib.rs"),
                "const INNER: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/assets/inner.txt\"));\n",
            ),
            "write inner source",
        );
        must(
            fs::write(root.join("crates/outer/assets/outer.txt"), "outer\n"),
            "write outer asset",
        );
        must(
            fs::write(
                root.join("crates/outer/tools/inner/assets/inner.txt"),
                "inner\n",
            ),
            "write inner asset",
        );

        let files = vec![
            "crates/outer/assets/outer.txt".to_owned(),
            "crates/outer/src/lib.rs".to_owned(),
            "crates/outer/tools/inner/assets/inner.txt".to_owned(),
            "crates/outer/tools/inner/src/lib.rs".to_owned(),
        ];
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        let package_roots = vec![
            "crates/outer".to_owned(),
            "crates/outer/tools/inner".to_owned(),
        ];
        let outer_targets = must(
            include_str_paths_for_package(&root, &files, &file_set, &package_roots, "crates/outer"),
            "resolve outer package includes",
        );
        assert_eq!(outer_targets, vec!["crates/outer/assets/outer.txt"]);
        let inner_targets = must(
            include_str_paths_for_package(
                &root,
                &files,
                &file_set,
                &package_roots,
                "crates/outer/tools/inner",
            ),
            "resolve inner package includes",
        );
        assert_eq!(
            inner_targets,
            vec!["crates/outer/tools/inner/assets/inner.txt"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn include_str_manifest_dir_concat_preserves_exact_boundary() {
        let root = scratch("manifest-dir-exact-boundary");
        must(
            fs::create_dir_all(root.join("crates/app/src")),
            "create package source directory",
        );
        must(
            fs::create_dir_all(root.join("crates/appsrc")),
            "create exact-concat target directory",
        );
        must(
            fs::write(
                root.join("crates/app/src/lib.rs"),
                "const SOURCE: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"src/lib.rs\"));\n",
            ),
            "write package source",
        );
        must(
            fs::write(root.join("crates/appsrc/lib.rs"), "exact\n"),
            "write exact-concat target",
        );

        let files = vec![
            "crates/app/src/lib.rs".to_owned(),
            "crates/appsrc/lib.rs".to_owned(),
        ];
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        let targets = must(
            include_str_paths(&root, &files, &file_set, "crates/app"),
            "resolve exact concat include",
        );

        assert_eq!(targets, vec!["crates/appsrc/lib.rs"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn include_str_manifest_dir_traversal_is_rejected() {
        let root = scratch("manifest-dir-traversal");
        must(
            fs::create_dir_all(root.join("crates/app/src")),
            "create package source directory",
        );
        must(
            fs::write(
                root.join("crates/app/src/lib.rs"),
                "const SOURCE: &str = include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../../outside.txt\"));\n",
            ),
            "write package source",
        );
        let files = vec!["crates/app/src/lib.rs".to_owned()];
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        match include_str_paths(&root, &files, &file_set, "crates/app") {
            Err(error) => assert!(
                error.to_string().contains("escapes the repository"),
                "{error}"
            ),
            Ok(targets) => assert!(!targets.is_empty(), "manifest-dir traversal was ignored"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn include_str_dynamic_expression_is_rejected() {
        let root = scratch("dynamic-include");
        must(
            fs::create_dir_all(root.join("crates/app/src")),
            "create package source directory",
        );
        must(
            fs::write(
                root.join("crates/app/src/lib.rs"),
                "const PATH: &str = \"src/lib.rs\"; const SOURCE: &str = include_str!(PATH);\n",
            ),
            "write package source",
        );
        let files = vec!["crates/app/src/lib.rs".to_owned()];
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        match include_str_paths(&root, &files, &file_set, "crates/app") {
            Err(error) => assert!(
                error.to_string().contains("static string expression"),
                "{error}"
            ),
            Ok(targets) => assert!(!targets.is_empty(), "dynamic include was ignored"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn release_build_feature_is_detected_per_package() {
        let root = scratch("release-build");
        must(
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"widget\"\nversion = \"0.1.0\"\n\n[features]\nrelease-build = []\nplain = []\n",
            ),
            "write widget manifest",
        );
        must(
            fs::create_dir_all(root.join("tool")),
            "create tool directory",
        );
        must(
            fs::write(
                root.join("tool/Cargo.toml"),
                "[package]\nname = \"tool\"\nversion = \"0.1.0\"\n\n[features]\nplain = []\n",
            ),
            "write tool manifest",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.91.1\"\n",
            ),
            "write toolchain pin",
        );
        let files = vec![
            "Cargo.toml".to_owned(),
            "tool/Cargo.toml".to_owned(),
            "rust-toolchain.toml".to_owned(),
        ];
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        let analysis = must(
            super::analyze_rust_manifests(&root, &files, &file_set, &files[..2]),
            "analyze manifests",
        );
        assert!(
            analysis
                .detected
                .contains(&"release-build:widget".to_owned()),
            "the release-build feature must be detected: {:?}",
            analysis.detected
        );
        assert!(
            !analysis
                .detected
                .iter()
                .any(|item| item.starts_with("release-build:tool")),
            "a package without the feature must stay undetected: {:?}",
            analysis.detected
        );
        let _ = fs::remove_dir_all(root);
    }

    #[expect(
        clippy::panic,
        reason = "the test must distinguish an accepted external target from the expected error"
    )]
    #[test]
    fn include_str_through_external_github_parent_is_rejected() {
        let root = scratch("external-github-parent");
        let outside = scratch("external-github-parent-target");
        must(
            fs::create_dir_all(root.join("src")),
            "create source directory",
        );
        must(
            fs::write(outside.join("manifest.yml"), "external\n"),
            "write external target",
        );
        must(
            fs::write(
                root.join("src/lib.rs"),
                "const MANIFEST: &str = include_str!(\"../.github/manifest.yml\");\n",
            ),
            "write Rust source",
        );
        must(
            symlink(&outside, root.join(".github")),
            "create external .github symlink directory",
        );

        let files = vec!["src/lib.rs".to_owned()];
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        let error = match include_str_paths(&root, &files, &file_set, ".") {
            Ok(targets) => panic!("external .github include target was accepted: {targets:?}"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("escapes the repository"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[expect(
        clippy::panic,
        reason = "the test must distinguish an accepted external target from the expected error"
    )]
    #[test]
    fn include_str_through_external_symlink_is_rejected() {
        let root = scratch("external-symlink");
        let outside = scratch("external-symlink-target");
        must(
            fs::create_dir_all(root.join("src")),
            "create source directory",
        );
        must(
            fs::write(outside.join("manifest.yml"), "external\n"),
            "write external target",
        );
        must(
            fs::write(
                root.join("src/lib.rs"),
                "const MANIFEST: &str = include_str!(\"linked/manifest.yml\");\n",
            ),
            "write Rust source",
        );
        must(
            symlink(&outside, root.join("src/linked")),
            "create external symlink directory",
        );

        let files = vec!["src/lib.rs".to_owned()];
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        let error = match include_str_paths(&root, &files, &file_set, ".") {
            Ok(targets) => panic!("external include target was accepted: {targets:?}"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("escapes the repository"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }
}
