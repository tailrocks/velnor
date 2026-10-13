//! Rust detector: Cargo manifests, workspace graph, and Rust source facts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use globset::{Glob, GlobSetBuilder};

use super::file_walk::{
    files_named, has_extension, join_repo_path, path_prefix, resolve_repo_path,
};
use super::{RepositoryShape, ScanContext};
use crate::s2::{
    identifier_suffix, parent_path, shell_change_dir, shell_quote, CachePurpose, CacheSpec,
    GeneratorError, RustToolchain, Unit, UnitKind,
};

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
        // Clippy before tests: report lint failures before compiling and
        // running the test targets. Keep every command and its exact flags;
        // the test command still covers test-less crates through `--no-tests`.
        let commands = vec![
            format!(
                "{command_prefix}cargo fmt --manifest-path {} -- --check",
                shell_quote("Cargo.toml")
            ),
            clippy_command,
            test_command,
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
        watch.extend(include_str_paths(root, files, file_set, &manifest.root)?);
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
            phases: Vec::new(),
            check_commands: Vec::new(),
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
            xcode: None,
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
            phases: Vec::new(),
            check_commands: Vec::new(),
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
            xcode: None,
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

fn include_str_paths(
    root: &Path,
    files: &[String],
    file_set: &BTreeSet<String>,
    package_root: &str,
) -> Result<Vec<String>, GeneratorError> {
    let prefix = path_prefix(package_root);
    let mut targets = BTreeSet::new();
    for source in files.iter().filter(|file| {
        has_extension(file, "rs") && (package_root == "." || file.starts_with(&prefix))
    }) {
        let source_contents = fs::read_to_string(root.join(source))
            .map_err(|error| GeneratorError::io("read Rust source", &root.join(source), &error))?;
        let included_paths = parse_include_str_literals(&source_contents).map_err(|error| {
            GeneratorError::usage(format!("{error} in {}", root.join(source).display()))
        })?;
        for included in included_paths {
            let target = resolve_repo_path(&parent_path(source), &included).ok_or_else(|| {
                GeneratorError::usage(format!(
                    "include_str! escapes the repository from {}: {included}",
                    root.join(source).display()
                ))
            })?;
            let target = if file_set.contains(&target) || static_github_input_exists(root, &target)?
            {
                target
            } else if let Some(resolved) = resolve_tracked_include_path(root, &target, file_set)? {
                resolved
            } else {
                return Err(GeneratorError::usage(format!(
                    "include_str! target does not exist: {} -> {}",
                    root.join(source).display(),
                    target
                )));
            };
            targets.insert(target);
        }
    }
    Ok(targets.into_iter().collect())
}

/// Resolve an include path through a symlink to the tracked path that owns its
/// bytes. The final canonical path must stay under the repository root; an
/// existing but untracked or external target is not valid scan input.
fn resolve_tracked_include_path(
    root: &Path,
    target: &str,
    file_set: &BTreeSet<String>,
) -> Result<Option<String>, GeneratorError> {
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| GeneratorError::io("canonicalize repository root", root, &error))?;
    let path = root.join(target);
    let canonical_target = match fs::canonicalize(&path) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(GeneratorError::io(
                "resolve include_str target",
                &path,
                &error,
            ));
        }
    };
    let Ok(relative) = canonical_target.strip_prefix(&canonical_root) else {
        return Ok(None);
    };
    let relative = relative
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    if file_set.contains(&relative) {
        Ok(Some(relative))
    } else {
        Ok(None)
    }
}

pub(crate) fn parse_include_str_literals(source: &str) -> Result<Vec<String>, String> {
    const MACRO: &str = "include_str!";
    let bytes = source.as_bytes();
    let mut cursor = 0;
    let mut included = Vec::new();
    while cursor < bytes.len() {
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'/') {
            cursor = skip_line_comment(bytes, cursor + 2);
            continue;
        }
        if bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            cursor = skip_block_comment(bytes, cursor + 2);
            continue;
        }
        if let Some(end) = skip_raw_string(bytes, cursor) {
            cursor = end;
            continue;
        }
        if bytes[cursor] == b'"' {
            cursor = skip_quoted_literal(bytes, cursor, b'"')
                .map_err(|error| format!("{error} at byte {cursor}"))?;
            continue;
        }
        if bytes[cursor] == b'\'' && is_char_literal_start(bytes, cursor) {
            cursor = skip_quoted_literal(bytes, cursor, b'\'')
                .map_err(|error| format!("{error} at byte {cursor}"))?;
            continue;
        }

        if bytes[cursor..].starts_with(MACRO.as_bytes())
            && (cursor == 0 || !is_rust_identifier_byte(bytes[cursor - 1]))
        {
            let mut argument = cursor + MACRO.len();
            while bytes.get(argument).is_some_and(u8::is_ascii_whitespace) {
                argument += 1;
            }
            if bytes.get(argument) != Some(&b'(') {
                cursor += MACRO.len();
                continue;
            }
            argument += 1;
            while bytes.get(argument).is_some_and(u8::is_ascii_whitespace) {
                argument += 1;
            }
            if bytes.get(argument) != Some(&b'"') {
                return Err("include_str! must use a plain string literal".to_owned());
            }
            let literal_end = skip_quoted_literal(bytes, argument, b'"')
                .map_err(|error| format!("{error} at byte {argument}"))?;
            let literal = &source[argument..literal_end];
            let value = serde_json::from_str::<String>(literal).map_err(|error| {
                format!("include_str! must use a plain string literal: {error}")
            })?;
            let mut close = literal_end;
            while bytes.get(close).is_some_and(u8::is_ascii_whitespace) {
                close += 1;
            }
            if bytes.get(close) != Some(&b')') {
                return Err("unterminated include_str!".to_owned());
            }
            included.push(value);
            cursor = close + 1;
            continue;
        }
        cursor += 1;
    }
    Ok(included)
}

fn is_rust_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn skip_line_comment(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len() && bytes[cursor] != b'\n' {
        cursor += 1;
    }
    cursor
}

fn skip_block_comment(bytes: &[u8], mut cursor: usize) -> usize {
    let mut depth = 1;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'/' && bytes[cursor + 1] == b'*' {
            depth += 1;
            cursor += 2;
        } else if bytes[cursor] == b'*' && bytes[cursor + 1] == b'/' {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return cursor;
            }
        } else {
            cursor += 1;
        }
    }
    bytes.len()
}

fn skip_raw_string(bytes: &[u8], cursor: usize) -> Option<usize> {
    let (hash_start, content_start) = match bytes.get(cursor..) {
        Some([b'r', rest @ ..]) => (
            cursor + 1,
            cursor + 1 + rest.iter().take_while(|byte| **byte == b'#').count(),
        ),
        Some([b'b', b'r', rest @ ..]) => (
            cursor + 2,
            cursor + 2 + rest.iter().take_while(|byte| **byte == b'#').count(),
        ),
        _ => return None,
    };
    if bytes.get(content_start) != Some(&b'"') {
        return None;
    }
    let hashes = content_start - hash_start;
    let mut end = content_start + 1;
    while end < bytes.len() {
        if bytes[end] == b'"'
            && bytes
                .get(end + 1..end + 1 + hashes)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            return Some(end + 1 + hashes);
        }
        end += 1;
    }
    Some(bytes.len())
}

fn is_char_literal_start(bytes: &[u8], cursor: usize) -> bool {
    match bytes.get(cursor + 1) {
        Some(b'\\') => true,
        Some(byte) if *byte != b'\'' && *byte != b'\n' => bytes.get(cursor + 2) == Some(&b'\''),
        _ => false,
    }
}

fn skip_quoted_literal(bytes: &[u8], mut cursor: usize, delimiter: u8) -> Result<usize, String> {
    cursor += 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = cursor.saturating_add(2),
            character if character == delimiter => return Ok(cursor + 1),
            b'\n' if delimiter == b'\'' => {
                return Err(format!("unterminated quoted literal at byte {cursor}"));
            }
            _ => cursor += 1,
        }
    }
    Err(format!("unterminated quoted literal at byte {cursor}"))
}

pub(crate) fn static_github_input_exists(
    root: &Path,
    target: &str,
) -> Result<bool, GeneratorError> {
    if !target.starts_with(".github/")
        || target == ".github/UNIFIED-ACTIONS.md"
        || target.starts_with(".github/ci/")
        || target.starts_with(".github/workflows/")
    {
        return Ok(false);
    }
    let path = root.join(target);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(GeneratorError::io(
            "inspect include_str target",
            &path,
            &error,
        )),
    }
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

/// One `boltffi.toml` Apple producer: a Rust crate whose `pack apple` run
/// emits `{framework}.xcframework` under a manifest-relative output
/// directory. Field semantics mirror `boltffi_cli` 0.30.1
/// (`Config::xcframework_name`, `apple_xcframework_output`,
/// `AppleNames::ffi_module_name`): the framework name falls back from
/// `targets.apple.xcframework.name` through
/// `targets.apple.swift.module_name` to `PascalCase`(`package.name`), the
/// output parent falls back from `targets.apple.xcframework.output` through
/// `targets.apple.output` to `dist/apple`, and the FFI module falls back
/// from `targets.apple.swift.ffi_module_name` to `{framework}FFI`. The
/// bindings directory falls back from `targets.apple.swift.output` through
/// `{apple.output}/Sources`, plus `BoltFFI` under the split SPM layout, and
/// the binding file is `{PascalCase(crate)}BoltFFI.swift` inside it. Paths
/// resolve against the manifest directory, the working directory `BoltFFI`
/// itself assumes. The recipe carries the pack policy (profile, lock
/// enforcement, verbosity) and renders the producer commands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoltffiProducer {
    pub(crate) manifest: String,
    pub(crate) root: String,
    pub(crate) package: String,
    pub(crate) crate_name: String,
    pub(crate) framework: String,
    pub(crate) ffi_module: String,
    /// Normalized repo-relative `{parent}/{framework}.xcframework`.
    pub(crate) output: String,
    /// Normalized repo-relative `generate swift` output directory: the
    /// resolved Swift output, plus `BoltFFI` under the split SPM layout.
    pub(crate) bindings_dir: String,
    /// The expected generated binding file under `bindings_dir`:
    /// `{PascalCase(crate)}BoltFFI.swift`.
    pub(crate) bindings_file: String,
    /// The Apple deployment target that shapes generated manifests.
    pub(crate) deployment_target: String,
    /// The generated `Package.swift` the drift check snapshots, if the
    /// manifest does not skip it. Render-local: the recipe consumes it at
    /// join time, so it stays off the product until transport needs it.
    pub(crate) package_swift: Option<String>,
    /// The typed pack recipe: profile, lock enforcement, verbosity.
    pub(crate) recipe: BoltffiRecipe,
    /// The Rust unit owning the producing crate, resolved at detect time.
    pub(crate) unit: Option<String>,
    /// The expected structural output files: the framework manifest plus,
    /// per slice, the static library and header modulemap. Sorted.
    pub(crate) output_files: Vec<String>,
    /// The transitive input closure: sorted patterns whose bytes feed the
    /// pack, plus the binding manifest itself.
    pub(crate) inputs: Vec<String>,
    /// Closure gaps the scan could not resolve; empty means complete.
    pub(crate) inputs_unknown: Vec<String>,
    /// The exact inputs digest over the expanded closure bytes, or `None`
    /// when `inputs_unknown` is nonempty: exact reuse without a complete
    /// contract would be a false identity.
    pub(crate) inputs_digest: Option<String>,
}

/// The `boltffi.toml` fields the producer join reads. SPM layout and debug
/// symbols belong to the execution adapter, but architectures are static
/// matching facts: they fix the expected slice directories of the pack.
#[derive(Default)]
struct BoltffiManifest {
    enabled: bool,
    package_name: Option<String>,
    package_crate: Option<String>,
    apple_output: Option<String>,
    deployment_target: Option<String>,
    xcframework_name: Option<String>,
    xcframework_output: Option<String>,
    swift_module_name: Option<String>,
    swift_output: Option<String>,
    swift_ffi_module_name: Option<String>,
    spm_layout: Option<String>,
    spm_output: Option<String>,
    spm_wrapper_sources: Option<String>,
    skip_package_swift: bool,
    include_macos: bool,
    /// `None` means the key is absent and `BoltFFI` defaults apply; `Some`
    /// (possibly empty) is an explicit list, where empty disables the slice.
    ios_architectures: Option<Vec<String>>,
    simulator_architectures: Option<Vec<String>>,
    macos_architectures: Option<Vec<String>>,
}

fn parse_boltffi_manifest(contents: &str) -> BoltffiManifest {
    let mut manifest = BoltffiManifest {
        enabled: true,
        ..BoltffiManifest::default()
    };
    let mut section = String::new();
    let lines: Vec<&str> = contents.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = strip_toml_comment(lines[index]).trim().to_owned();
        index += 1;
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            if let Some(header) = header.strip_suffix(']') {
                header.trim().clone_into(&mut section);
            }
            continue;
        }
        let Some((key, first_value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let mut value = first_value.trim().to_owned();
        while toml_array_is_open(&value) && index < lines.len() {
            value.push(' ');
            value.push_str(strip_toml_comment(lines[index]).trim());
            index += 1;
        }
        match (section.as_str(), key) {
            ("package", "name") => manifest.package_name = toml_string_value(&value),
            ("package", "crate") => manifest.package_crate = toml_string_value(&value),
            ("targets.apple", "enabled") => {
                if value == "true" {
                    manifest.enabled = true;
                } else if value == "false" {
                    manifest.enabled = false;
                }
            }
            ("targets.apple", "output") => manifest.apple_output = toml_string_value(&value),
            ("targets.apple", "deployment_target") => {
                manifest.deployment_target = toml_string_value(&value);
            }
            ("targets.apple", "include_macos") => {
                manifest.include_macos = value == "true";
            }
            ("targets.apple", "ios_architectures") => {
                manifest.ios_architectures = Some(toml_array_values(&value));
            }
            ("targets.apple", "simulator_architectures") => {
                manifest.simulator_architectures = Some(toml_array_values(&value));
            }
            ("targets.apple", "macos_architectures") => {
                manifest.macos_architectures = Some(toml_array_values(&value));
            }
            ("targets.apple.xcframework", "name") => {
                manifest.xcframework_name = toml_string_value(&value);
            }
            ("targets.apple.xcframework", "output") => {
                manifest.xcframework_output = toml_string_value(&value);
            }
            ("targets.apple.swift", "module_name") => {
                manifest.swift_module_name = toml_string_value(&value);
            }
            ("targets.apple.swift", "output") => {
                manifest.swift_output = toml_string_value(&value);
            }
            ("targets.apple.swift", "ffi_module_name") => {
                manifest.swift_ffi_module_name = toml_string_value(&value);
            }
            ("targets.apple.spm", "layout") => {
                manifest.spm_layout = toml_string_value(&value);
            }
            ("targets.apple.spm", "output") => {
                manifest.spm_output = toml_string_value(&value);
            }
            ("targets.apple.spm", "wrapper_sources") => {
                manifest.spm_wrapper_sources = toml_string_value(&value);
            }
            ("targets.apple.spm", "skip_package_swift") => {
                manifest.skip_package_swift = value == "true";
            }
            _ => {}
        }
    }
    manifest
}

/// `BoltFFI`'s `to_pascal_case`: split on `_` and `-`, uppercase each word's
/// first character, concatenate the rest unchanged.
fn boltffi_pascal_case(name: &str) -> String {
    name.split(['_', '-'])
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

/// Whether `segment` can name the framework directory inside a normalized
/// product output: no separators, control characters, or blank/path
/// navigation segments.
fn valid_framework_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment.bytes().any(|byte| byte == b'/' || byte == b'\\')
        && !segment.chars().any(char::is_control)
        && segment != "."
        && segment != ".."
}

/// `BoltFFI` 0.30.1 default slice architectures when the corresponding
/// `targets.apple.*_architectures` key is absent
/// (`boltffi_cli/src/target.rs`: `Architecture::IOS` and
/// `APPLE_MULTI_ARCH`; `boltffi_cli/src/config/mod.rs`
/// `apple_*_architectures`). An explicit empty list disables the slice.
const BOLTFFI_DEFAULT_IOS_ARCHITECTURES: [&str; 1] = ["arm64"];
const BOLTFFI_DEFAULT_MULTI_ARCHITECTURES: [&str; 2] = ["arm64", "x86_64"];

/// `BoltFFI` 0.30.1 `targets.apple` output and deployment defaults
/// (`default_apple_output`, `default_apple_deployment_target`).
const BOLTFFI_DEFAULT_APPLE_OUTPUT: &str = "dist/apple";
const BOLTFFI_DEFAULT_DEPLOYMENT_TARGET: &str = "16.0";

/// Whether `arch` is an architecture `BoltFFI` can place in an Apple slice
/// (`boltffi_cli/src/target.rs` `Architecture`, Apple members only).
fn valid_boltffi_apple_arch(arch: &str) -> bool {
    matches!(arch, "arm64" | "x86_64" | "armv7" | "x86")
}

/// Resolve the expected `XCFramework` slice directories in `BoltFFI` target
/// order (iOS device, simulator, macOS). Directory names follow the
/// `xcodebuild -create-xcframework` convention
/// (`{macos,ios}-{archs joined by _}[-simulator]`), with architectures
/// sorted so multi-arch slices are deterministic. An unknown architecture
/// or an empty slice set (which `BoltFFI` itself rejects at build time)
/// is an `Err` naming the problem for a scan diagnostic.
/// An explicit architecture list as declared, or the `BoltFFI` default when
/// the key is absent. An explicit empty list stays empty: it disables the
/// slice.
fn boltffi_arch_list(explicit: Option<&Vec<String>>, default: &[&str]) -> Vec<String> {
    explicit.map_or_else(
        || default.iter().map(ToString::to_string).collect(),
        Clone::clone,
    )
}

fn boltffi_slice_dirs(manifest: &BoltffiManifest) -> Result<Vec<String>, String> {
    let ios = boltffi_arch_list(
        manifest.ios_architectures.as_ref(),
        &BOLTFFI_DEFAULT_IOS_ARCHITECTURES,
    );
    let simulator = boltffi_arch_list(
        manifest.simulator_architectures.as_ref(),
        &BOLTFFI_DEFAULT_MULTI_ARCHITECTURES,
    );
    let macos = boltffi_arch_list(
        manifest.macos_architectures.as_ref(),
        &BOLTFFI_DEFAULT_MULTI_ARCHITECTURES,
    );
    for arch in ios.iter().chain(simulator.iter()).chain(macos.iter()) {
        if !valid_boltffi_apple_arch(arch) {
            return Err(format!(
                "architecture `{arch}` is not a supported Apple slice architecture"
            ));
        }
    }
    let mut slices = Vec::new();
    if !ios.is_empty() {
        slices.push(format!("ios-{}", join_sorted_arches(&ios)));
    }
    if !simulator.is_empty() {
        slices.push(format!("ios-{}-simulator", join_sorted_arches(&simulator)));
    }
    if manifest.include_macos && !macos.is_empty() {
        slices.push(format!("macos-{}", join_sorted_arches(&macos)));
    }
    if slices.is_empty() {
        return Err(
            "no Apple slice is enabled; at least one architecture list must be nonempty".to_owned(),
        );
    }
    Ok(slices)
}

/// Join slice architectures with `_` in sorted order for a deterministic
/// directory name.
fn join_sorted_arches(arches: &[String]) -> String {
    let mut sorted: Vec<&str> = arches.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.join("_")
}

/// The expected structural files of the pack: the framework manifest plus,
/// per slice, the static library and header modulemap. Cargo names a
/// staticlib `lib{crate}.a` with `-` folded to `_`; these are the paths
/// a consumer's framework verification asserts. Generated headers
/// beyond the modulemap and per-file digests are build-time facts the
/// producer manifest records, not plan facts.
fn boltffi_expected_files(output: &str, crate_name: &str, slices: &[String]) -> Vec<String> {
    let mut files = vec![format!("{output}/Info.plist")];
    let library = format!("lib{}.a", crate_name.replace('-', "_"));
    for slice in slices {
        files.push(format!("{output}/{slice}/{library}"));
        files.push(format!("{output}/{slice}/Headers/module.modulemap"));
    }
    files.sort();
    files
}

/// Resolve the `pack apple` bindings directory and expected binding file
/// for a manifest rooted at `root`. Mirrors
/// `boltffi_cli/src/pack/apple/mod.rs` `generate_apple_bindings`: every SPM
/// layout nests Swift sources under a `BoltFFI` segment. Bundled and
/// ffi-only (the default) anchor at the SPM package root
/// (`Config::apple_spm_output`, falling back through
/// `[targets.apple].output` to `dist/apple`); bundled inserts
/// `wrapper_sources` when configured, else `Sources`. Split anchors at the
/// Swift output (`Config::apple_swift_output`: `targets.apple.swift.output`
/// through `{apple.output}/Sources`). The file stem mirrors
/// `Config::swift_bindings_file_stem`: an empty `PascalCase` library name
/// yields bare `BoltFFI`. An unknown layout or a resolved path outside the
/// repository is an `Err` for a scan diagnostic.
/// The `pack apple` binding facts: output directory, expected binding
/// file, deployment target, and FFI module name.
#[derive(Debug)]
struct BoltffiBindings {
    dir: String,
    file: String,
    deployment_target: String,
    ffi_module: String,
    /// The generated `Package.swift` `pack apple` emits at
    /// `<spm output>/Package.swift` (`SpmPackageGenerator::generate`),
    /// unless `skip_package_swift` opts out. The SPM output falls back
    /// through `[targets.apple].output` to `dist/apple`
    /// (`Config::apple_spm_output`).
    package_swift: Option<String>,
}

/// Resolve the binding facts for a manifest rooted at `root`. The directory
/// mirrors `boltffi_cli/src/pack/apple/mod.rs` `generate_apple_bindings`:
/// every layout nests under `BoltFFI`, with bundled/ffi-only anchored at
/// the SPM package root and split at the Swift output (see
/// `BoltffiBindings`). The file stem mirrors
/// `Config::swift_bindings_file_stem`: an empty `PascalCase` library name
/// yields bare `BoltFFI`. The FFI module falls back from
/// `targets.apple.swift.ffi_module_name` to `{framework}FFI`. An unknown
/// layout or a resolved path outside the repository is an `Err` for a scan
/// diagnostic.
fn boltffi_binding_facts(
    parsed: &BoltffiManifest,
    root: &str,
    crate_name: &str,
    framework: &str,
) -> Result<BoltffiBindings, String> {
    let layout = parsed.spm_layout.as_deref().unwrap_or("ffi-only");
    if !matches!(layout, "bundled" | "split" | "ffi-only") {
        return Err(format!(
            "declares SPM layout `{layout}`, which is not bundled, split, or ffi-only"
        ));
    }
    let package_root = parsed
        .spm_output
        .as_deref()
        .unwrap_or(BOLTFFI_DEFAULT_APPLE_OUTPUT);
    let base = match layout {
        "bundled" => format!(
            "{package_root}/{}/BoltFFI",
            parsed.spm_wrapper_sources.as_deref().unwrap_or("Sources")
        ),
        "ffi-only" => format!("{package_root}/Sources/BoltFFI"),
        _ => {
            let swift_base = parsed.swift_output.as_deref().map_or_else(
                || {
                    format!(
                        "{}/Sources",
                        parsed
                            .apple_output
                            .as_deref()
                            .unwrap_or(BOLTFFI_DEFAULT_APPLE_OUTPUT)
                    )
                },
                str::to_owned,
            );
            format!("{swift_base}/BoltFFI")
        }
    };
    let Some(dir) = resolve_repo_path(root, &base) else {
        return Err(format!(
            "declares Swift bindings output `{base}`, which is absolute or escapes the repository"
        ));
    };
    let pascal = boltffi_pascal_case(crate_name);
    let stem = if pascal.is_empty() {
        "BoltFFI".to_owned()
    } else {
        format!("{pascal}BoltFFI")
    };
    let file = join_repo_path(&dir, &format!("{stem}.swift"));
    let package_swift = if parsed.skip_package_swift {
        None
    } else {
        let base = parsed.spm_output.as_deref().map_or_else(
            || {
                parsed
                    .apple_output
                    .as_deref()
                    .unwrap_or(BOLTFFI_DEFAULT_APPLE_OUTPUT)
                    .to_owned()
            },
            str::to_owned,
        );
        let Some(resolved) = resolve_repo_path(root, &base) else {
            return Err(format!(
                "declares SPM output `{base}`, which is absolute or escapes the repository"
            ));
        };
        Some(join_repo_path(&resolved, "Package.swift"))
    };
    Ok(BoltffiBindings {
        dir,
        file,
        deployment_target: parsed
            .deployment_target
            .clone()
            .unwrap_or_else(|| BOLTFFI_DEFAULT_DEPLOYMENT_TARGET.to_owned()),
        ffi_module: parsed
            .swift_ffi_module_name
            .clone()
            .unwrap_or_else(|| format!("{framework}FFI")),
        package_swift,
    })
}

/// Discover every usable `BoltFFI` Apple producer plus one diagnostic per
/// manifest that cannot join a consumer. Semantic failures stay diagnostics:
/// a malformed binding manifest must not fail the whole scan when `BoltFFI`
/// itself will report the error at build time.
/// The transitive input closure of the native-producing crate at
/// `crate_root`: per-crate manifest, source, build-script, and `include_str!`
/// patterns for the crate and every transitive path dependency, plus the
/// binding `seed` files, shared locks and toolchain pins, and Cargo
/// configuration. Returns sorted, deduplicated patterns with the closure
/// gaps the scan could not resolve. Registry and git dependencies resolve
/// through the lockfiles; a path dependency without a tracked manifest, or
/// one pointing outside the repository, is a gap, never a silent omission:
/// exact reuse must stay disabled while any gap remains.
///
/// # Errors
/// Returns an IO error when a tracked manifest cannot be read, and a usage
/// error when an `include_str!` target is missing, like the Rust unit scan.
pub(crate) fn native_input_closure(
    root: &Path,
    files: &[String],
    file_set: &BTreeSet<String>,
    crate_root: &str,
    seed: &[String],
) -> Result<(Vec<String>, Vec<String>), GeneratorError> {
    let mut inputs: Vec<String> = seed.to_vec();
    for pinned in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "rust-toolchain",
        "mise.toml",
        "mise.lock",
    ] {
        if file_set.contains(pinned) {
            inputs.push(pinned.to_owned());
        }
    }
    inputs.push(".cargo/**".to_owned());
    let local_lock = join_repo_path(crate_root, "Cargo.lock");
    if local_lock != "Cargo.lock" && file_set.contains(&local_lock) {
        inputs.push(local_lock);
    }
    let mut gaps = Vec::new();
    let mut visited = BTreeSet::new();
    let mut stack = vec![crate_root.to_owned()];
    while let Some(current) = stack.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }
        let manifest_path = join_repo_path(&current, "Cargo.toml");
        let path = root.join(&manifest_path);
        let contents = fs::read_to_string(&path)
            .map_err(|error| GeneratorError::io("read Cargo manifest", &path, &error))?;
        let facts = parse_cargo_manifest(&current, &contents);
        let prefix = path_prefix(&current);
        inputs.extend([
            manifest_path,
            format!("{prefix}**/*.rs"),
            format!("{prefix}src/**"),
            format!("{prefix}tests/**"),
            format!("{prefix}examples/**"),
            format!("{prefix}benches/**"),
        ]);
        let build_script = facts.build_script.as_deref().or_else(|| {
            file_set
                .contains(&join_repo_path(&current, "build.rs"))
                .then_some("build.rs")
        });
        if let Some(script) = build_script {
            match resolve_repo_path(&current, script) {
                Some(script_path) => inputs.push(script_path),
                None => gaps.push(format!(
                    "build script `{script}` of crate `{current}` is absolute or escapes the repository"
                )),
            }
        }
        inputs.extend(include_str_paths(root, files, file_set, &current)?);
        for dependency in &facts.dependencies {
            let Some(path) = dependency.path.as_deref() else {
                continue;
            };
            let Some(dep_root) = resolve_repo_path(&current, path) else {
                gaps.push(format!(
                    "path dependency `{}` of crate `{current}` is absolute or escapes the repository",
                    dependency.name
                ));
                continue;
            };
            if !file_set.contains(&join_repo_path(&dep_root, "Cargo.toml")) {
                gaps.push(format!(
                    "path dependency `{}` of crate `{current}` has no tracked Cargo.toml",
                    dependency.name
                ));
                continue;
            }
            stack.push(dep_root);
        }
    }
    inputs.sort();
    inputs.dedup();
    gaps.sort();
    gaps.dedup();
    Ok((inputs, gaps))
}

/// The typed `BoltFFI` Apple pack execution recipe: the policy half of a
/// native producer. `profile` selects the Cargo profile through
/// `--cargo-arg` (`None` keeps `BoltFFI`'s own default, today Debug);
/// `locked` passes `--cargo-arg=--locked` so nested Cargo invocations
/// enforce the committed resolution instead of drifting; `verbose`
/// streams the target build's compiler output (`-v`) so a long native
/// link leaves liveness evidence instead of silence.
/// The in-repo surface `pack apple` rewrites and the drift check guards:
/// the Swift bindings directory, always, plus the generated `Package.swift`
/// unless the manifest skips it. Headers land in `BoltFFI` scratch, never
/// the repo, so they stay outside this surface. `framework` scopes the
/// staging directory when one crate produces several frameworks.
pub(crate) struct BoltffiDriftSurface<'a> {
    pub(crate) bindings_dir: &'a str,
    pub(crate) package_swift: Option<&'a str>,
    pub(crate) framework: &'a str,
}

/// Render the pre-pack snapshot, one command per surface member so each
/// failure keeps its own precise error: refuse a missing member (a fresh
/// checkout without committed bindings is drift, not a silent skip),
/// wipe any stale staging, then copy the surface.
fn boltffi_snapshot_commands(
    surface: &BoltffiDriftSurface<'_>,
    staging: &str,
    snapshot: &str,
) -> Vec<String> {
    let mut commands = vec![format!(
        "test -d {} || {{ echo {} >&2; exit 1; }} && rm -rf {} && mkdir -p {} && cp -R {} {}",
        shell_quote(surface.bindings_dir),
        shell_quote(&format!(
            "error: committed Swift bindings `{}` are missing from the checkout; the native pack needs them to compare against",
            surface.bindings_dir
        )),
        shell_quote(staging),
        shell_quote(staging),
        shell_quote(surface.bindings_dir),
        shell_quote(snapshot),
    )];
    if let Some(package_swift) = surface.package_swift {
        commands.push(format!(
            "test -f {} || {{ echo {} >&2; exit 1; }} && cp {} {}",
            shell_quote(package_swift),
            shell_quote(&format!(
                "error: committed `{package_swift}` is missing from the checkout; the native pack needs it to compare against"
            )),
            shell_quote(package_swift),
            shell_quote(&join_repo_path(staging, "Package.swift")),
        ));
    }
    commands
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BoltffiRecipe {
    pub(crate) profile: Option<String>,
    pub(crate) locked: bool,
    pub(crate) verbose: bool,
}

impl BoltffiRecipe {
    /// The recipe half of the exact inputs digest: profile and lock
    /// enforcement change the produced artifacts, so they participate in
    /// the identity. Verbosity only changes streamed diagnostics and is
    /// deliberately absent: a quiet and a verbose run over the same
    /// closure produce the same bytes.
    pub(crate) fn digest_identity(&self) -> String {
        format!(
            "boltffi:pack:apple:profile={}:locked={}",
            self.profile.as_deref().unwrap_or("default"),
            self.locked,
        )
    }

    /// Render the producer commands: wipe the previous framework bundle
    /// first (`BoltFFI` merges into an existing output directory, which
    /// would keep stale slices), snapshot the committed bindings, pack
    /// from the manifest root (the working directory `BoltFFI` itself
    /// assumes), then diff the snapshot against the rewritten tree.
    /// `pack apple` offers no output redirect, so the snapshot is the
    /// staging: a pack that rewrites tracked bindings fails the unit
    /// instead of silently testing only the rewritten tree.
    pub(crate) fn commands(
        &self,
        manifest_root: &str,
        output: &str,
        surface: &BoltffiDriftSurface<'_>,
    ) -> Vec<String> {
        let staging = join_repo_path(
            &join_repo_path(manifest_root, "target/velnor-boltffi-staging"),
            surface.framework,
        );
        let snapshot = join_repo_path(&staging, "bindings");
        let mut commands = vec![format!("rm -rf {}", shell_quote(output))];
        commands.extend(boltffi_snapshot_commands(surface, &staging, &snapshot));
        let mut pack = String::from("boltffi");
        if self.verbose {
            pack.push_str(" -v");
        }
        if self.locked {
            pack.push_str(" --cargo-arg=--locked");
        }
        if let Some(profile) = &self.profile {
            pack.push_str(" --cargo-arg=--profile --cargo-arg=");
            pack.push_str(&shell_quote(profile));
        }
        pack.push_str(" pack apple");
        commands.push(format!("{}{pack}", shell_change_dir(manifest_root)));
        commands.push(format!(
            "diff -r {} {} || {{ echo {} >&2; exit 1; }}",
            shell_quote(&snapshot),
            shell_quote(surface.bindings_dir),
            shell_quote(&format!(
                "error: committed Swift bindings in `{}` drifted from regeneration; rerun the native pack locally and commit the result",
                surface.bindings_dir
            )),
        ));
        if let Some(package_swift) = surface.package_swift {
            commands.push(format!(
                "diff {} {} || {{ echo {} >&2; exit 1; }}",
                shell_quote(&join_repo_path(&staging, "Package.swift")),
                shell_quote(package_swift),
                shell_quote(&format!(
                    "error: committed `{package_swift}` drifted from regeneration; rerun the native pack locally and commit the result"
                )),
            ));
        }
        commands
    }
}

/// Locked resolution applies exactly when a `Cargo.lock` governs the
/// producer closure: `--locked` without a lockfile fails every build,
/// while omitting it with a committed lock lets nested Cargo drift.
fn boltffi_recipe_locked(inputs: &[String]) -> bool {
    inputs
        .iter()
        .any(|input| input == "Cargo.lock" || input.ends_with("/Cargo.lock"))
}

/// Expand validated closure patterns against tracked files and read the
/// matched bytes. A pattern matching nothing contributes nothing (like
/// `hashFiles`); deleting the last match still changes the digest because the
/// file's `source` line disappears. Only tracked files participate: the
/// digest identifies the merge candidate CI checks out, not local untracked
/// state. An unreadable match fails the scan — a digest over unknown bytes
/// would be a false identity.
///
/// # Errors
/// Returns a usage error for a pattern that does not compile, or an I/O
/// error naming the matched file that cannot be read.
fn closure_sources(
    root: &Path,
    files: &[String],
    inputs: &[String],
    manifest_path: &str,
) -> Result<Vec<(String, Vec<u8>)>, GeneratorError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in inputs {
        let glob = Glob::new(pattern).map_err(|error| {
            GeneratorError::usage(format!(
                "BoltFFI manifest {manifest_path} produced input `{pattern}`, which is not a valid glob: {error}"
            ))
        })?;
        builder.add(glob);
    }
    let set = builder.build().map_err(|error| {
        GeneratorError::usage(format!(
            "BoltFFI manifest {manifest_path} inputs could not be compiled: {error}"
        ))
    })?;
    let mut sources = Vec::new();
    for file in files {
        if !set.is_match(file) {
            continue;
        }
        let path = root.join(file);
        let bytes = std::fs::read(&path)
            .map_err(|error| GeneratorError::io("read closure input", &path, &error))?;
        sources.push((file.clone(), bytes));
    }
    Ok(sources)
}

/// Compute the exact inputs digest over the expanded closure, or `None`
/// with a diagnostic when closure gaps make the contract incomplete.
/// I/O errors propagate because they may hide a producer input.
fn boltffi_inputs_digest(
    context: &ScanContext<'_>,
    manifest_path: &str,
    inputs: &[String],
    inputs_unknown: &[String],
    recipe: &BoltffiRecipe,
    diagnostics: &mut Vec<String>,
) -> Result<Option<String>, GeneratorError> {
    if !inputs_unknown.is_empty() {
        diagnostics.push(format!(
            "BoltFFI manifest {manifest_path} has an incomplete input closure ({}); no exact inputs digest was computed and the product cannot be reused exactly.",
            inputs_unknown.join(", "),
        ));
        return Ok(None);
    }
    let sources = closure_sources(context.root, context.files, inputs, manifest_path)?;
    Ok(Some(crate::s2::primitives::prepared_tools::inputs_digest(
        &crate::s2::primitives::prepared_tools::InputsFacts {
            // Lockfiles and toolchain pins ride `sources` as raw bytes,
            // which subsumes the parsed lock/toolchain facts.
            locks: Vec::new(),
            sources,
            recipe: vec![recipe.digest_identity()],
            toolchain: Vec::new(),
        },
    )))
}

/// Identify the producing crate: the manifest's sibling `Cargo.toml` must
/// exist and declare the `[package] crate` name (or `name` when `crate` is
/// absent). Returns `Ok(None)` with a diagnostic when the crate cannot be
/// identified; I/O errors propagate because they may hide a real producer.
fn boltffi_producer_crate(
    context: &ScanContext<'_>,
    root: &str,
    manifest_path: &str,
    package: &str,
    package_crate: Option<String>,
    diagnostics: &mut Vec<String>,
) -> Result<Option<String>, GeneratorError> {
    let cargo_manifest = join_repo_path(root, "Cargo.toml");
    if !context.file_set.contains(&cargo_manifest) {
        diagnostics.push(format!(
            "BoltFFI manifest {manifest_path} has no sibling Cargo.toml; the producing crate cannot be identified."
        ));
        return Ok(None);
    }
    let cargo_path = context.root.join(&cargo_manifest);
    let cargo_contents = fs::read_to_string(&cargo_path)
        .map_err(|error| GeneratorError::io("read Cargo manifest", &cargo_path, &error))?;
    let cargo = parse_cargo_manifest(root, &cargo_contents);
    let crate_name = package_crate.unwrap_or_else(|| package.to_owned());
    if cargo.package_name.as_deref() != Some(crate_name.as_str()) {
        diagnostics.push(format!(
            "BoltFFI manifest {manifest_path} names crate `{crate_name}`, but the sibling Cargo.toml declares package `{}`; no producer edge was constructed.",
            cargo.package_name.as_deref().unwrap_or("<unnamed>"),
        ));
        return Ok(None);
    }
    Ok(Some(crate_name))
}

/// Parse one `boltffi.toml` into a producer, pushing a diagnostic and
/// returning `Ok(None)` when the manifest cannot yield a joinable edge.
/// I/O and closure errors propagate because they may hide a real producer.
/// Resolve the framework name for a `BoltFFI` manifest: the explicit
/// `XCFramework` name wins, then the Swift module name, then `PascalCase`
/// over the package name. An unusable segment is a diagnostic, never a guess.
fn boltffi_framework_name(
    parsed: &BoltffiManifest,
    manifest_path: &str,
    package: &str,
    diagnostics: &mut Vec<String>,
) -> Option<String> {
    let framework = parsed
        .xcframework_name
        .clone()
        .or(parsed.swift_module_name.clone())
        .unwrap_or_else(|| boltffi_pascal_case(package));
    if !valid_framework_segment(&framework) {
        diagnostics.push(format!(
            "BoltFFI manifest {manifest_path} resolves framework name `{framework}`, which is not a valid path segment; no producer edge was constructed."
        ));
        return None;
    }
    Some(framework)
}

fn boltffi_producer_from_manifest(
    context: &ScanContext<'_>,
    manifest_path: &String,
    diagnostics: &mut Vec<String>,
) -> Result<Option<BoltffiProducer>, GeneratorError> {
    let root = parent_path(manifest_path);
    let path = context.root.join(manifest_path);
    let contents = fs::read_to_string(&path)
        .map_err(|error| GeneratorError::io("read BoltFFI manifest", &path, &error))?;
    let parsed = parse_boltffi_manifest(&contents);
    if !parsed.enabled {
        return Ok(None);
    }
    let slices = match boltffi_slice_dirs(&parsed) {
        Ok(slices) => slices,
        Err(problem) => {
            diagnostics.push(format!(
                "BoltFFI manifest {manifest_path} {problem}; no producer edge was constructed."
            ));
            return Ok(None);
        }
    };
    let Some(package) = parsed.package_name.clone() else {
        diagnostics.push(format!(
            "BoltFFI manifest {manifest_path} declares no [package] name; the framework name cannot be derived."
        ));
        return Ok(None);
    };
    let Some(framework) = boltffi_framework_name(&parsed, manifest_path, &package, diagnostics)
    else {
        return Ok(None);
    };
    let declared_parent = parsed
        .xcframework_output
        .as_deref()
        .or(parsed.apple_output.as_deref())
        .unwrap_or(BOLTFFI_DEFAULT_APPLE_OUTPUT);
    let Some(parent) = resolve_repo_path(&root, declared_parent) else {
        diagnostics.push(format!(
            "BoltFFI manifest {manifest_path} declares output `{declared_parent}`, which is absolute or escapes the repository; no producer edge was constructed."
        ));
        return Ok(None);
    };
    let Some(crate_name) = boltffi_producer_crate(
        context,
        &root,
        manifest_path,
        &package,
        parsed.package_crate.clone(),
        diagnostics,
    )?
    else {
        return Ok(None);
    };
    let bindings = match boltffi_binding_facts(&parsed, &root, &crate_name, &framework) {
        Ok(bindings) => bindings,
        Err(problem) => {
            diagnostics.push(format!(
                "BoltFFI manifest {manifest_path} {problem}; no producer edge was constructed."
            ));
            return Ok(None);
        }
    };
    let output = join_repo_path(&parent, &format!("{framework}.xcframework"));
    let output_files = boltffi_expected_files(&output, &crate_name, &slices);
    // The committed bindings are drift-checked inputs: an edit to the
    // generated Swift or `Package.swift` must select the producer so `pack`
    // re-runs and the drift check fails instead of testing a stale tree.
    let mut seed = vec![manifest_path.to_owned(), format!("{}/**", bindings.dir)];
    if let Some(package_swift) = bindings.package_swift.as_deref() {
        seed.push(package_swift.to_owned());
    }
    let (inputs, inputs_unknown) =
        native_input_closure(context.root, context.files, context.file_set, &root, &seed)?;
    // The scanner never invents a Cargo profile: `None` keeps `BoltFFI`'s
    // own default. A typed profile policy arrives with declared Apple
    // facts; until then the recipe records exactly what the scan knows.
    let recipe = BoltffiRecipe {
        profile: None,
        locked: boltffi_recipe_locked(&inputs),
        verbose: true,
    };
    let inputs_digest = boltffi_inputs_digest(
        context,
        manifest_path,
        &inputs,
        &inputs_unknown,
        &recipe,
        diagnostics,
    )?;
    Ok(Some(BoltffiProducer {
        manifest: manifest_path.to_owned(),
        root,
        package,
        crate_name,
        ffi_module: bindings.ffi_module,
        output,
        output_files,
        bindings_dir: bindings.dir,
        bindings_file: bindings.file,
        deployment_target: bindings.deployment_target,
        framework,
        package_swift: bindings.package_swift,
        recipe,
        unit: None,
        inputs,
        inputs_unknown,
        inputs_digest,
    }))
}

pub(crate) fn boltffi_producers(
    context: &ScanContext<'_>,
) -> Result<(Vec<BoltffiProducer>, Vec<String>), GeneratorError> {
    let mut manifests = files_named(context.files, "boltffi.toml");
    manifests.sort();
    let mut producers = Vec::new();
    let mut diagnostics = Vec::new();
    for manifest_path in &manifests {
        if let Some(producer) =
            boltffi_producer_from_manifest(context, manifest_path, &mut diagnostics)?
        {
            producers.push(producer);
        }
    }
    let mut by_output: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for producer in &producers {
        by_output
            .entry(producer.output.as_str())
            .or_default()
            .push(producer.manifest.as_str());
    }
    let mut conflicted: BTreeSet<String> = BTreeSet::new();
    for (output, claimants) in &by_output {
        if claimants.len() > 1 {
            diagnostics.push(format!(
                "BoltFFI manifests {} claim the same XCFramework output `{output}`; none of them is joined to a consumer.",
                claimants.join(", "),
            ));
            conflicted.insert((*output).to_owned());
        }
    }
    producers.retain(|producer| !conflicted.contains(&producer.output));
    Ok((producers, diagnostics))
}

pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<(), GeneratorError> {
    let (mut producers, diagnostics) = boltffi_producers(context)?;
    let cargo_manifests = files_named(context.files, "Cargo.toml");
    if cargo_manifests.is_empty() {
        shape.limitations.extend(diagnostics);
        return Ok(());
    }
    let rust = analyze_rust_manifests(
        context.root,
        context.files,
        context.file_set,
        &cargo_manifests,
    )?;
    for producer in &mut producers {
        producer.unit = rust
            .units
            .iter()
            .find(|unit| {
                unit.kind == UnitKind::Rust
                    && unit.root == producer.root
                    && unit.id != POLICY_UNIT_ID
            })
            .map(|unit| unit.id.clone());
        shape
            .detected
            .push(format!("boltffi-producer:{}", producer.root));
    }
    shape.boltffi_producers.extend(producers);
    shape.units.extend(rust.units);
    shape.detected.extend(rust.detected);
    shape.limitations.extend(diagnostics);
    shape.limitations.extend(rust.limitations);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::{include_str_paths, parse_cargo_manifest};
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

    #[expect(
        clippy::panic,
        reason = "tests need setup failures to name their root cause"
    )]
    fn must_err<T: std::fmt::Debug, E>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(value) => panic!("{context}: unexpectedly succeeded with {value:?}"),
            Err(error) => error,
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

    #[test]
    fn validation_commands_put_clippy_first_without_changing_the_command_set() {
        for use_nextest in [true, false] {
            let root = scratch(if use_nextest {
                "validation-order-nextest"
            } else {
                "validation-order-cargo-test"
            });
            must(
                fs::write(
                    root.join("Cargo.toml"),
                    "[package]\nname = \"widget\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                ),
                "write validation manifest",
            );
            must(
                fs::write(root.join("Cargo.lock"), ""),
                "write validation lockfile",
            );
            must(
                fs::create_dir_all(root.join("tests")),
                "create validation test directory",
            );
            must(
                fs::write(
                    root.join("tests/integration.rs"),
                    "#[test]\nfn smoke() {}\n",
                ),
                "write validation test",
            );
            must(
                fs::write(
                    root.join("rust-toolchain.toml"),
                    "[toolchain]\nchannel = \"1.91.1\"\n",
                ),
                "write validation toolchain",
            );
            let mut files = vec![
                "Cargo.lock".to_owned(),
                "Cargo.toml".to_owned(),
                "rust-toolchain.toml".to_owned(),
                "tests/integration.rs".to_owned(),
            ];
            if use_nextest {
                must(
                    fs::create_dir_all(root.join(".config")),
                    "create nextest configuration directory",
                );
                must(
                    fs::write(root.join(".config/nextest.toml"), ""),
                    "write nextest configuration",
                );
                files.push(".config/nextest.toml".to_owned());
            }
            let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
            let analysis = must(
                super::analyze_rust_manifests(&root, &files, &file_set, &["Cargo.toml".to_owned()]),
                "analyze validation manifest",
            );
            let commands = &analysis.units[0].pr_commands;
            let format = "cargo fmt --manifest-path 'Cargo.toml' -- --check".to_owned();
            let test = if use_nextest {
                "cargo nextest run --locked --all-features --package 'widget' --no-tests pass"
                    .to_owned()
            } else {
                "cargo test --locked --all-features --package 'widget'".to_owned()
            };
            let clippy = "cargo clippy --locked --profile test --no-deps --all-targets --all-features --package 'widget' -- -D warnings".to_owned();
            let expected_order = vec![format, clippy, test];
            let mut expected_multiset = expected_order.clone();
            expected_multiset.sort();
            let mut observed_multiset = commands.clone();
            observed_multiset.sort();
            assert_eq!(
                observed_multiset, expected_multiset,
                "validation command flags or selector changed"
            );
            assert_eq!(
                commands, &expected_order,
                "validation command order changed"
            );
            assert_eq!(analysis.units[0].full_commands, expected_order);
            let _ = fs::remove_dir_all(root);
        }
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

        assert!(error
            .to_string()
            .contains("include_str! target does not exist"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(outside);
    }

    #[test]
    fn boltffi_manifest_parses_apple_fields() {
        use super::parse_boltffi_manifest;
        let parsed = parse_boltffi_manifest(
            "[package]\nname = \"bridge-core\"\ncrate = \"bridge-core-ffi\"\n\n\
             [targets.apple]\noutput = \"shared\"\n\n\
             [targets.apple.xcframework]\nname = \"BridgeCore\"\noutput = \"../../target/xcframework\"\n\n\
             [targets.apple.swift]\nmodule_name = \"BridgeSwift\"\n",
        );
        assert!(parsed.enabled);
        assert_eq!(parsed.package_name.as_deref(), Some("bridge-core"));
        assert_eq!(parsed.package_crate.as_deref(), Some("bridge-core-ffi"));
        assert_eq!(parsed.apple_output.as_deref(), Some("shared"));
        assert_eq!(parsed.xcframework_name.as_deref(), Some("BridgeCore"));
        assert_eq!(
            parsed.xcframework_output.as_deref(),
            Some("../../target/xcframework")
        );
        assert_eq!(parsed.swift_module_name.as_deref(), Some("BridgeSwift"));
        let disabled =
            parse_boltffi_manifest("[package]\nname = \"x\"\n\n[targets.apple]\nenabled = false\n");
        assert!(!disabled.enabled);
    }

    #[test]
    fn boltffi_manifest_parses_spm_output_and_skip() {
        use super::{boltffi_binding_facts, parse_boltffi_manifest};
        let parsed = parse_boltffi_manifest("[package]\nname = \"x\"\n");
        assert_eq!(parsed.spm_output, None);
        assert!(!parsed.skip_package_swift);
        // Default: generated at `<apple output>/Package.swift`.
        let facts = must(
            boltffi_binding_facts(&parsed, "libs/bridge-ffi", "bridge-core-ffi", "BridgeCore"),
            "default package facts",
        );
        assert_eq!(
            facts.package_swift.as_deref(),
            Some("libs/bridge-ffi/dist/apple/Package.swift")
        );
        let explicit = parse_boltffi_manifest(
            "[package]\nname = \"x\"\n\n[targets.apple.spm]\noutput = \"../pkg\"\nskip_package_swift = true\n",
        );
        assert_eq!(explicit.spm_output.as_deref(), Some("../pkg"));
        assert!(explicit.skip_package_swift);
        let skipped = must(
            boltffi_binding_facts(
                &explicit,
                "libs/bridge-ffi",
                "bridge-core-ffi",
                "BridgeCore",
            ),
            "skipped package facts",
        );
        assert_eq!(skipped.package_swift, None);
        // Skip off but output explicit: the manifest path wins.
        let emitted = parse_boltffi_manifest(
            "[package]\nname = \"x\"\n\n[targets.apple.spm]\noutput = \"../pkg\"\n",
        );
        let kept = must(
            boltffi_binding_facts(&emitted, "libs/bridge-ffi", "bridge-core-ffi", "BridgeCore"),
            "explicit package facts",
        );
        assert_eq!(
            kept.package_swift.as_deref(),
            Some("libs/pkg/Package.swift")
        );
        let escaping = parse_boltffi_manifest(
            "[package]\nname = \"x\"\n\n[targets.apple.spm]\noutput = \"../../../elsewhere\"\n",
        );
        let error = must_err(
            boltffi_binding_facts(
                &escaping,
                "libs/bridge-ffi",
                "bridge-core-ffi",
                "BridgeCore",
            ),
            "escaping spm output must fail",
        );
        assert!(error.contains("escapes the repository"), "{error}");
    }

    #[test]
    fn boltffi_slice_dirs_applies_verified_defaults() {
        use super::{boltffi_slice_dirs, parse_boltffi_manifest};
        // No [targets.apple] keys: iOS [arm64], simulator [arm64, x86_64],
        // macOS off. Matches BoltFFI 0.30.1 platform defaults.
        let parsed = parse_boltffi_manifest("[package]\nname = \"x\"\n");
        assert_eq!(
            boltffi_slice_dirs(&parsed),
            Ok(vec![
                "ios-arm64".to_owned(),
                "ios-arm64_x86_64-simulator".to_owned(),
            ])
        );
    }

    #[test]
    fn boltffi_slice_dirs_honors_explicit_lists() {
        use super::{boltffi_slice_dirs, parse_boltffi_manifest};
        // macOS-only arm64 shape: iOS and simulator disabled.
        let parsed = parse_boltffi_manifest(
            "[package]\nname = \"x\"\n\n\
             [targets.apple]\ninclude_macos = true\nios_architectures = []\n\
             simulator_architectures = []\nmacos_architectures = [\"arm64\"]\n",
        );
        assert_eq!(
            boltffi_slice_dirs(&parsed),
            Ok(vec!["macos-arm64".to_owned()])
        );
        // Multi-arch lists join sorted for deterministic directory names.
        let parsed = parse_boltffi_manifest(
            "[package]\nname = \"x\"\n\n\
             [targets.apple]\ninclude_macos = true\n\
             macos_architectures = [\"x86_64\", \"arm64\"]\n\
             ios_architectures = []\nsimulator_architectures = []\n",
        );
        assert_eq!(
            boltffi_slice_dirs(&parsed),
            Ok(vec!["macos-arm64_x86_64".to_owned()])
        );
    }

    #[test]
    fn boltffi_slice_dirs_fails_closed_on_bad_config() {
        use super::{boltffi_slice_dirs, parse_boltffi_manifest};
        let parsed = parse_boltffi_manifest(
            "[package]\nname = \"x\"\n\n[targets.apple]\nmacos_architectures = [\"riscv64\"]\n",
        );
        let error = must_err(boltffi_slice_dirs(&parsed), "unknown arch must fail");
        assert!(error.contains("riscv64"), "unexpected error: {error}");
        // Everything disabled: BoltFFI itself rejects the empty slice set.
        let parsed = parse_boltffi_manifest(
            "[package]\nname = \"x\"\n\n\
             [targets.apple]\nios_architectures = []\nsimulator_architectures = []\n",
        );
        let error = must_err(boltffi_slice_dirs(&parsed), "zero slices must fail");
        assert!(
            error.contains("no Apple slice"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn boltffi_expected_files_lists_plist_lib_modulemap() {
        use super::boltffi_expected_files;
        let files = boltffi_expected_files(
            "target/xcframework/BridgeCore.xcframework",
            "bridge-core-ffi",
            &["macos-arm64".to_owned()],
        );
        assert_eq!(
            files,
            vec![
                "target/xcframework/BridgeCore.xcframework/Info.plist".to_owned(),
                "target/xcframework/BridgeCore.xcframework/macos-arm64/Headers/module.modulemap"
                    .to_owned(),
                "target/xcframework/BridgeCore.xcframework/macos-arm64/libbridge_core_ffi.a"
                    .to_owned(),
            ]
        );
    }

    #[test]
    fn boltffi_pascal_case_matches_upstream() {
        use super::boltffi_pascal_case;
        assert_eq!(boltffi_pascal_case("bridge-core"), "BridgeCore");
        assert_eq!(boltffi_pascal_case("foo_bar"), "FooBar");
        assert_eq!(boltffi_pascal_case("Already"), "Already");
        assert_eq!(boltffi_pascal_case("a--b"), "AB");
        assert_eq!(boltffi_pascal_case("x"), "X");
    }

    fn boltffi_fixture(entries: &[(&str, &str)]) -> (PathBuf, Vec<String>, BTreeSet<String>) {
        let root = std::env::temp_dir().join(format!(
            "velnor-workflow-boltffi-scan-{}",
            crate::unique_suffix()
        ));
        must(fs::create_dir_all(&root), "create scratch directory");
        let mut files = Vec::new();
        for (path, contents) in entries {
            let target = root.join(path);
            if let Some(parent) = target.parent() {
                must(fs::create_dir_all(parent), "create fixture directory");
            }
            must(fs::write(&target, contents), "write fixture file");
            files.push((*path).to_owned());
        }
        files.sort();
        let file_set = files.iter().cloned().collect::<BTreeSet<_>>();
        (root, files, file_set)
    }

    fn boltffi_minimal_crate(package: &str) -> String {
        format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\n")
    }

    #[test]
    fn boltffi_producers_resolves_happy_path() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "libs/bridge-ffi/boltffi.toml",
                "[package]\nname = \"bridge-core\"\ncrate = \"bridge-core-ffi\"\n\n\
                 [targets.apple.xcframework]\nname = \"BridgeCore\"\noutput = \"../../target/xcframework\"\n",
            ),
            (
                "libs/bridge-ffi/Cargo.toml",
                &boltffi_minimal_crate("bridge-core-ffi"),
            ),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(producers.len(), 1);
        let producer = &producers[0];
        assert_eq!(producer.manifest, "libs/bridge-ffi/boltffi.toml");
        assert_eq!(producer.root, "libs/bridge-ffi");
        assert_eq!(producer.package, "bridge-core");
        assert_eq!(producer.crate_name, "bridge-core-ffi");
        assert_eq!(producer.framework, "BridgeCore");
        assert_eq!(producer.ffi_module, "BridgeCoreFFI");
        assert_eq!(producer.output, "target/xcframework/BridgeCore.xcframework");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_applies_name_and_output_defaults() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "crates/plain/boltffi.toml",
                "[package]\nname = \"plain-core\"\n",
            ),
            (
                "crates/plain/Cargo.toml",
                &boltffi_minimal_crate("plain-core"),
            ),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(producers.len(), 1);
        assert_eq!(producers[0].framework, "PlainCore");
        assert_eq!(producers[0].ffi_module, "PlainCoreFFI");
        assert_eq!(
            producers[0].output,
            "crates/plain/dist/apple/PlainCore.xcframework"
        );
        assert_eq!(producers[0].crate_name, "plain-core");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_prefers_module_name_over_pascal_case() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "crates/modular/boltffi.toml",
                "[package]\nname = \"modular-core\"\n\n\
                 [targets.apple.swift]\nmodule_name = \"CustomModule\"\n",
            ),
            (
                "crates/modular/Cargo.toml",
                &boltffi_minimal_crate("modular-core"),
            ),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(producers.len(), 1);
        assert_eq!(producers[0].framework, "CustomModule");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_resolves_split_bindings() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "libs/bridge-ffi/boltffi.toml",
                "[package]\nname = \"bridge-core\"\ncrate = \"bridge-core-ffi\"\n\n\
                 [targets.apple]\ndeployment_target = \"15.0\"\n\n\
                 [targets.apple.xcframework]\nname = \"BridgeCore\"\noutput = \"../../target/xcframework\"\n\n\
                 [targets.apple.spm]\nlayout = \"split\"\n\n\
                 [targets.apple.swift]\noutput = \"../../app/Sources/BridgeBindings\"\n",
            ),
            (
                "libs/bridge-ffi/Cargo.toml",
                &boltffi_minimal_crate("bridge-core-ffi"),
            ),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(producers.len(), 1);
        let producer = &producers[0];
        assert_eq!(producer.bindings_dir, "app/Sources/BridgeBindings/BoltFFI");
        assert_eq!(
            producer.bindings_file,
            "app/Sources/BridgeBindings/BoltFFI/BridgeCoreFfiBoltFFI.swift"
        );
        assert_eq!(producer.deployment_target, "15.0");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_resolves_bundled_bindings() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "libs/bridge-ffi/boltffi.toml",
                "[package]\nname = \"bridge-core\"\ncrate = \"bridge-core-ffi\"\n\n\
                 [targets.apple.spm]\nlayout = \"bundled\"\noutput = \"../pkg\"\nwrapper_sources = \"Wrapped\"\n",
            ),
            (
                "libs/bridge-ffi/Cargo.toml",
                &boltffi_minimal_crate("bridge-core-ffi"),
            ),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(producers.len(), 1);
        let producer = &producers[0];
        assert_eq!(producer.bindings_dir, "libs/pkg/Wrapped/BoltFFI");
        assert_eq!(
            producer.bindings_file,
            "libs/pkg/Wrapped/BoltFFI/BridgeCoreFfiBoltFFI.swift"
        );
        assert_eq!(
            producer.package_swift.as_deref(),
            Some("libs/pkg/Package.swift")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_applies_bindings_defaults() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "crates/plain/boltffi.toml",
                "[package]\nname = \"plain-core\"\n",
            ),
            (
                "crates/plain/Cargo.toml",
                &boltffi_minimal_crate("plain-core"),
            ),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(producers.len(), 1);
        let producer = &producers[0];
        assert_eq!(
            producer.bindings_dir,
            "crates/plain/dist/apple/Sources/BoltFFI"
        );
        assert_eq!(
            producer.bindings_file,
            "crates/plain/dist/apple/Sources/BoltFFI/PlainCoreBoltFFI.swift"
        );
        assert_eq!(producer.deployment_target, "16.0");
        assert!(
            producer
                .inputs
                .contains(&"crates/plain/dist/apple/Sources/BoltFFI/**".to_owned()),
            "committed bindings select the producer: {:?}",
            producer.inputs
        );
        assert!(
            producer
                .inputs
                .contains(&"crates/plain/dist/apple/Package.swift".to_owned()),
            "the generated manifest selects the producer: {:?}",
            producer.inputs
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_honors_ffi_module_name() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "crates/custom/boltffi.toml",
                "[package]\nname = \"custom-core\"\n\n\
                 [targets.apple.swift]\nffi_module_name = \"CustomShim\"\n",
            ),
            (
                "crates/custom/Cargo.toml",
                &boltffi_minimal_crate("custom-core"),
            ),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(producers.len(), 1);
        assert_eq!(producers[0].framework, "CustomCore");
        assert_eq!(producers[0].ffi_module, "CustomShim");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_rejects_unknown_spm_layout() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "crates/woven/boltffi.toml",
                "[package]\nname = \"woven-core\"\n\n\
                 [targets.apple.spm]\nlayout = \"woven\"\n",
            ),
            (
                "crates/woven/Cargo.toml",
                &boltffi_minimal_crate("woven-core"),
            ),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(producers.is_empty(), "{producers:?}");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("SPM layout `woven`")),
            "{diagnostics:?}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_rejects_escaping_swift_output() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "crates/leaky/boltffi.toml",
                "[package]\nname = \"leaky-core\"\n\n\
                 [targets.apple.swift]\noutput = \"../../../outside\"\n\n\
                 [targets.apple.spm]\nlayout = \"split\"\n",
            ),
            (
                "crates/leaky/Cargo.toml",
                &boltffi_minimal_crate("leaky-core"),
            ),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(producers.is_empty(), "{producers:?}");
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic.contains("Swift bindings output `../../../outside/BoltFFI`")
            }),
            "{diagnostics:?}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_skips_disabled_apple() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "crates/off/boltffi.toml",
                "[package]\nname = \"off\"\n\n[targets.apple]\nenabled = false\n",
            ),
            ("crates/off/Cargo.toml", &boltffi_minimal_crate("off")),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(producers.is_empty());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_reports_unusable_manifests() {
        use super::{boltffi_producers, ScanContext};
        let (root, files, file_set) = boltffi_fixture(&[
            ("crates/nameless/boltffi.toml", "[targets.apple]\n"),
            (
                "crates/nameless/Cargo.toml",
                &boltffi_minimal_crate("nameless"),
            ),
            (
                "crates/orphan/boltffi.toml",
                "[package]\nname = \"orphan\"\n",
            ),
            (
                "crates/mismatched/boltffi.toml",
                "[package]\nname = \"mismatched\"\ncrate = \"other-crate\"\n",
            ),
            (
                "crates/mismatched/Cargo.toml",
                &boltffi_minimal_crate("mismatched"),
            ),
            (
                "crates/escaping/boltffi.toml",
                "[package]\nname = \"escaping\"\n\n\
                 [targets.apple.xcframework]\nname = \"Escaping\"\noutput = \"../../../../tmp\"\n",
            ),
            (
                "crates/escaping/Cargo.toml",
                &boltffi_minimal_crate("escaping"),
            ),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(producers.is_empty());
        assert_eq!(diagnostics.len(), 4);
        assert!(diagnostics.iter().any(|diagnostic| diagnostic
            .contains("crates/nameless/boltffi.toml declares no [package] name")));
        assert!(diagnostics.iter().any(|diagnostic| diagnostic
            .contains("crates/orphan/boltffi.toml has no sibling Cargo.toml")));
        assert!(diagnostics.iter().any(|diagnostic| diagnostic
            .contains("names crate `other-crate`")
            && diagnostic.contains("declares package `mismatched`")));
        assert!(diagnostics.iter().any(|diagnostic| diagnostic
            .contains("crates/escaping/boltffi.toml")
            && diagnostic.contains("escapes the repository")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_producers_rejects_conflicting_outputs() {
        use super::{boltffi_producers, ScanContext};
        let manifest = "[package]\nname = \"shared-core\"\ncrate = \"shared-core\"\n\n\
             [targets.apple.xcframework]\nname = \"Shared\"\noutput = \"../../target/xcframework\"\n";
        let (root, files, file_set) = boltffi_fixture(&[
            ("libs/one/boltffi.toml", manifest),
            ("libs/one/Cargo.toml", &boltffi_minimal_crate("shared-core")),
            ("libs/two/boltffi.toml", manifest),
            ("libs/two/Cargo.toml", &boltffi_minimal_crate("shared-core")),
        ]);
        let context = ScanContext {
            root: &root,
            files: &files,
            file_set: &file_set,
        };
        let (producers, diagnostics) = must(boltffi_producers(&context), "discover producers");
        assert!(producers.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("claim the same XCFramework output"));
        assert!(diagnostics[0].contains("libs/one/boltffi.toml"));
        assert!(diagnostics[0].contains("libs/two/boltffi.toml"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_closure_collects_direct_crate_inputs() {
        use super::native_input_closure;
        let (root, files, file_set) = boltffi_fixture(&[
            ("Cargo.lock", "lock\n"),
            ("rust-toolchain.toml", "[toolchain]\n"),
            (
                "libs/bridge-ffi/Cargo.toml",
                "[package]\nname = \"bridge-core-ffi\"\nversion = \"0.1.0\"\n",
            ),
            ("libs/bridge-ffi/build.rs", "fn main() {}\n"),
            (
                "libs/bridge-ffi/src/lib.rs",
                "const SCHEMA: &str = include_str!(\"schema.json\");\n",
            ),
            ("libs/bridge-ffi/src/schema.json", "{}\n"),
        ]);
        let seed = vec!["libs/bridge-ffi/boltffi.toml".to_owned()];
        let (inputs, gaps) = must(
            native_input_closure(&root, &files, &file_set, "libs/bridge-ffi", &seed),
            "collect direct closure",
        );
        assert!(gaps.is_empty(), "{gaps:?}");
        for expected in [
            "libs/bridge-ffi/boltffi.toml",
            "libs/bridge-ffi/Cargo.toml",
            "libs/bridge-ffi/**/*.rs",
            "libs/bridge-ffi/src/**",
            "libs/bridge-ffi/build.rs",
            "libs/bridge-ffi/src/schema.json",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".cargo/**",
        ] {
            assert!(
                inputs.iter().any(|input| input == expected),
                "closure contains {expected}: {inputs:?}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_closure_follows_transitive_path_dependencies() {
        use super::native_input_closure;
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "libs/bridge-ffi/Cargo.toml",
                "[package]\nname = \"bridge-core-ffi\"\nversion = \"0.1.0\"\n\n[dependencies]\nsibling = { path = \"../sibling\" }\n",
            ),
            (
                "libs/sibling/Cargo.toml",
                "[package]\nname = \"sibling\"\nversion = \"0.1.0\"\n\n[dependencies]\nleaf = { path = \"../leaf\" }\n",
            ),
            (
                "libs/leaf/Cargo.toml",
                "[package]\nname = \"leaf\"\nversion = \"0.1.0\"\n",
            ),
        ]);
        let (inputs, gaps) = must(
            native_input_closure(&root, &files, &file_set, "libs/bridge-ffi", &[]),
            "collect transitive closure",
        );
        assert!(gaps.is_empty(), "{gaps:?}");
        for expected in [
            "libs/bridge-ffi/Cargo.toml",
            "libs/sibling/Cargo.toml",
            "libs/sibling/**/*.rs",
            "libs/leaf/Cargo.toml",
            "libs/leaf/src/**",
        ] {
            assert!(
                inputs.iter().any(|input| input == expected),
                "closure contains {expected}: {inputs:?}"
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_closure_reports_missing_path_manifest_as_gap() {
        use super::native_input_closure;
        let (root, files, file_set) = boltffi_fixture(&[(
            "libs/bridge-ffi/Cargo.toml",
            "[package]\nname = \"bridge-core-ffi\"\nversion = \"0.1.0\"\n\n[dependencies]\nmissing = { path = \"../missing\" }\n",
        )]);
        let (inputs, gaps) = must(
            native_input_closure(&root, &files, &file_set, "libs/bridge-ffi", &[]),
            "collect closure with a missing dep",
        );
        assert!(
            inputs
                .iter()
                .any(|input| input == "libs/bridge-ffi/Cargo.toml"),
            "{inputs:?}"
        );
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(gaps[0].contains("missing"), "{gaps:?}");
        assert!(gaps[0].contains("no tracked Cargo.toml"), "{gaps:?}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn native_closure_terminates_on_dependency_cycle() {
        use super::native_input_closure;
        let (root, files, file_set) = boltffi_fixture(&[
            (
                "libs/one/Cargo.toml",
                "[package]\nname = \"one\"\nversion = \"0.1.0\"\n\n[dependencies]\ntwo = { path = \"../two\" }\n",
            ),
            (
                "libs/two/Cargo.toml",
                "[package]\nname = \"two\"\nversion = \"0.1.0\"\n\n[dependencies]\none = { path = \"../one\" }\n",
            ),
        ]);
        let (inputs, gaps) = must(
            native_input_closure(&root, &files, &file_set, "libs/one", &[]),
            "collect cyclic closure",
        );
        assert!(gaps.is_empty(), "{gaps:?}");
        assert!(
            inputs.iter().any(|input| input == "libs/one/**/*.rs"),
            "{inputs:?}"
        );
        assert!(
            inputs.iter().any(|input| input == "libs/two/**/*.rs"),
            "{inputs:?}"
        );
        let mut deduped = inputs.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(inputs, deduped, "closure patterns are sorted and unique");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn boltffi_recipe_digest_separates_policy_from_verbosity() {
        use super::BoltffiRecipe;
        let quiet = BoltffiRecipe {
            profile: None,
            locked: false,
            verbose: false,
        };
        let loud = BoltffiRecipe {
            profile: None,
            locked: false,
            verbose: true,
        };
        assert_eq!(
            quiet.digest_identity(),
            "boltffi:pack:apple:profile=default:locked=false"
        );
        assert_eq!(quiet.digest_identity(), loud.digest_identity());
        let profiled = BoltffiRecipe {
            profile: Some("desktop-release".to_owned()),
            locked: false,
            verbose: false,
        };
        assert_eq!(
            profiled.digest_identity(),
            "boltffi:pack:apple:profile=desktop-release:locked=false"
        );
        assert_ne!(quiet.digest_identity(), profiled.digest_identity());
        let locked = BoltffiRecipe {
            profile: None,
            locked: true,
            verbose: false,
        };
        assert_eq!(
            locked.digest_identity(),
            "boltffi:pack:apple:profile=default:locked=true"
        );
        assert_ne!(quiet.digest_identity(), locked.digest_identity());
    }

    fn drift_recipe() -> (super::BoltffiRecipe, super::BoltffiDriftSurface<'static>) {
        (
            super::BoltffiRecipe {
                profile: None,
                locked: false,
                verbose: true,
            },
            super::BoltffiDriftSurface {
                bindings_dir: "native/Sources/BridgeCore/BoltFFI",
                package_swift: Some("dist/apple/Package.swift"),
                framework: "BridgeCore",
            },
        )
    }

    #[test]
    fn boltffi_recipe_renders_snapshot_pack_diff() {
        let (recipe, surface) = drift_recipe();
        let commands = recipe.commands(
            "libs/bridge-ffi",
            "target/xcframework/BridgeCore.xcframework",
            &surface,
        );
        assert_eq!(commands.len(), 6, "{commands:?}");
        assert_eq!(
            commands[0],
            "rm -rf 'target/xcframework/BridgeCore.xcframework'"
        );
        assert!(
            commands[1].contains("test -d 'native/Sources/BridgeCore/BoltFFI'")
                && commands[1]
                    .contains("rm -rf 'libs/bridge-ffi/target/velnor-boltffi-staging/BridgeCore'")
                && commands[1].contains("cp -R 'native/Sources/BridgeCore/BoltFFI'"),
            "bindings snapshot: {}",
            commands[1]
        );
        assert!(
            commands[2].contains("test -f 'dist/apple/Package.swift'")
                && commands[2].contains("cp 'dist/apple/Package.swift'"),
            "package snapshot: {}",
            commands[2]
        );
        assert_eq!(
            commands[3],
            "cd -- 'libs/bridge-ffi' && boltffi -v pack apple"
        );
        assert!(
            commands[4].starts_with("diff -r ")
                && commands[4].contains("'native/Sources/BridgeCore/BoltFFI'"),
            "bindings drift check: {}",
            commands[4]
        );
        assert!(
            commands[5].starts_with("diff ") && commands[5].contains("'dist/apple/Package.swift'"),
            "package drift check: {}",
            commands[5]
        );
    }

    #[test]
    fn boltffi_recipe_omits_package_swift_when_skipped() {
        let recipe = super::BoltffiRecipe {
            profile: Some("desktop-release".to_owned()),
            locked: true,
            verbose: false,
        };
        let surface = super::BoltffiDriftSurface {
            bindings_dir: "Sources/BoltFFI",
            package_swift: None,
            framework: "BridgeCore",
        };
        let commands = recipe.commands(".", "dist/apple/BridgeCore.xcframework", &surface);
        assert_eq!(commands.len(), 4, "{commands:?}");
        assert!(
            commands
                .iter()
                .all(|command| !command.contains("Package.swift")),
            "{commands:?}"
        );
        assert!(
            commands[1].contains("rm -rf 'target/velnor-boltffi-staging/BridgeCore'"),
            "staging stays repo-relative at the root: {}",
            commands[1]
        );
        assert_eq!(
            commands[2],
            "boltffi --cargo-arg=--locked --cargo-arg=--profile --cargo-arg='desktop-release' pack apple"
        );
    }

    /// Execute the rendered snapshot/diff pair against a fixture tree,
    /// simulating the pack by rewriting the file in between: the check
    /// must pass on an untouched tree and fail on a rewrite, an
    /// addition, or a missing checkout member.
    #[test]
    fn boltffi_drift_shell_detects_rewrite() {
        use std::process::Command;
        let (recipe, surface) = drift_recipe();
        let commands = recipe.commands(
            "libs/bridge-ffi",
            "target/xcframework/BridgeCore.xcframework",
            &surface,
        );
        let root = scratch("boltffi-drift");
        let live = root.join("native/Sources/BridgeCore/BoltFFI");
        must(
            std::fs::create_dir_all(&live),
            "create live bindings fixture",
        );
        must(
            std::fs::write(live.join("BridgeCoreBoltFFI.swift"), "committed\n"),
            "write live bindings fixture",
        );
        must(
            std::fs::create_dir_all(root.join("dist/apple")),
            "create spm fixture",
        );
        must(
            std::fs::write(root.join("dist/apple/Package.swift"), "committed\n"),
            "write package fixture",
        );
        let run = |command: &str| {
            must(
                Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .current_dir(&root)
                    .output(),
                "run drift shell",
            )
        };
        assert!(run(&commands[1]).status.success());
        assert!(run(&commands[2]).status.success());
        assert!(run(&commands[4]).status.success());
        assert!(run(&commands[5]).status.success());
        must(
            std::fs::write(live.join("BridgeCoreBoltFFI.swift"), "rewritten\n"),
            "simulate pack rewrite",
        );
        let drifted = run(&commands[4]);
        assert!(!drifted.status.success());
        let stderr = String::from_utf8_lossy(&drifted.stderr);
        assert!(stderr.contains("drifted from regeneration"), "{stderr}");
        must(
            std::fs::write(live.join("BridgeCoreBoltFFI.swift"), "committed\n"),
            "restore live bindings fixture",
        );
        must(
            std::fs::write(live.join("Extra.swift"), "added\n"),
            "simulate pack addition",
        );
        assert!(!run(&commands[4]).status.success());
        let _ = std::fs::remove_dir_all(&live);
        let missing = run(&commands[1]);
        assert!(!missing.status.success());
        let stderr = String::from_utf8_lossy(&missing.stderr);
        assert!(stderr.contains("missing from the checkout"), "{stderr}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
