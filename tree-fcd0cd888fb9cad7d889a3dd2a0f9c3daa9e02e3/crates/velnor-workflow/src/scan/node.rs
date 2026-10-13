//! Node and Bun detector: package manifests and the workspace dependency graph.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::Value;

use super::file_walk::{files_named, join_repo_path, path_prefix, roots_for_manifests};
use super::{unit, RepositoryShape, ScanContext};
use crate::{
    identifier_suffix, parent_path, shell_change_dir, CacheSpec, GeneratorError, UnitKind,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PackageManager {
    Bun,
    Npm,
}

impl PackageManager {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "bun" => Some(Self::Bun),
            "npm" => Some(Self::Npm),
            _ => None,
        }
    }

    fn command(self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Npm => "npm",
        }
    }

    fn detected(self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Npm => "npm",
        }
    }

    fn lockfiles(self) -> &'static [&'static str] {
        match self {
            Self::Bun => &["bun.lock", "bun.lockb"],
            Self::Npm => &["package-lock.json"],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageJsonFacts {
    name: String,
    scripts: BTreeSet<String>,
    dependencies: BTreeSet<String>,
    manager: PackageManager,
    manager_version: Option<String>,
    workspace_root: bool,
}

fn parse_package_json(
    root: &Path,
    relative_root: &str,
    contents: &str,
    file_set: &BTreeSet<String>,
) -> Result<Option<PackageJsonFacts>, GeneratorError> {
    let value: Value = serde_json::from_str(contents).map_err(|error| {
        GeneratorError::usage(format!(
            "invalid package.json at {}: {error}",
            root.join(relative_root).display()
        ))
    })?;
    let object = value.as_object().ok_or_else(|| {
        GeneratorError::usage(format!(
            "package.json must contain an object at {}",
            root.join(relative_root).display()
        ))
    })?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map_or_else(|| relative_root.to_owned(), str::to_owned);
    if name.chars().any(char::is_control) {
        return Err(GeneratorError::usage(format!(
            "package name contains control characters at {}",
            root.join(relative_root).display()
        )));
    }
    let scripts = object
        .get("scripts")
        .and_then(Value::as_object)
        .map(|scripts| scripts.keys().cloned().collect())
        .unwrap_or_default();
    let mut dependencies = BTreeSet::new();
    for field in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        if let Some(entries) = object.get(field).and_then(Value::as_object) {
            dependencies.extend(entries.keys().cloned());
        }
    }
    let package_manager_spec = object
        .get("packageManager")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if package_manager_spec
        .as_deref()
        .is_some_and(|value| value.chars().any(char::is_control))
    {
        return Err(GeneratorError::usage(format!(
            "packageManager contains control characters at {}",
            root.join(relative_root).display()
        )));
    }
    let package_manager = package_manager_spec
        .as_deref()
        .map(|value| value.split_once('@').map_or(value, |(name, _)| name))
        .and_then(PackageManager::from_name);
    if package_manager_spec.is_some() && package_manager.is_none() {
        return Ok(None);
    }
    let manager =
        package_manager.or_else(|| package_manager_from_ancestors(root, relative_root, file_set));
    let Some(manager) = manager else {
        return Ok(None);
    };
    let manager_version = package_manager_spec
        .as_deref()
        .and_then(|value| value.split_once('@').map(|(_, version)| version.to_owned()))
        .or_else(|| {
            package_manager
                .is_none()
                .then(|| package_manager_version_from_ancestors(root, relative_root, manager))?
        });
    if manager_version
        .as_deref()
        .is_some_and(|value| value.chars().any(char::is_control))
    {
        return Err(GeneratorError::usage(format!(
            "packageManager version contains control characters at {}",
            root.join(relative_root).display()
        )));
    }
    let workspace_root = object.contains_key("workspaces");
    Ok(Some(PackageJsonFacts {
        name,
        scripts,
        dependencies,
        manager,
        manager_version,
        workspace_root,
    }))
}

fn package_manager_from_ancestors(
    root: &Path,
    relative_root: &str,
    file_set: &BTreeSet<String>,
) -> Option<PackageManager> {
    let mut current = relative_root.to_owned();
    loop {
        let manifest = root.join(join_repo_path(&current, "package.json"));
        if let Ok(contents) = fs::read_to_string(manifest)
            && let Ok(value) = serde_json::from_str::<Value>(&contents)
            && let Some(spec) = value
                .as_object()
                .and_then(|object| object.get("packageManager"))
                .and_then(Value::as_str)
            && let Some((name, _)) = spec.split_once('@')
            && let Some(manager) = PackageManager::from_name(name)
        {
            return Some(manager);
        }
        if let Some(manager) =
            [PackageManager::Bun, PackageManager::Npm]
                .into_iter()
                .find(|manager| {
                    manager
                        .lockfiles()
                        .iter()
                        .any(|lockfile| file_set.contains(&join_repo_path(&current, lockfile)))
                })
        {
            return Some(manager);
        }
        if current == "." {
            return None;
        }
        current = parent_path(&current);
    }
}

fn package_manager_version_from_ancestors(
    root: &Path,
    relative_root: &str,
    manager: PackageManager,
) -> Option<String> {
    let mut current = relative_root.to_owned();
    loop {
        let manifest = root.join(join_repo_path(&current, "package.json"));
        if let Ok(contents) = fs::read_to_string(manifest)
            && let Ok(value) = serde_json::from_str::<Value>(&contents)
            && let Some(spec) = value
                .as_object()
                .and_then(|object| object.get("packageManager"))
                .and_then(Value::as_str)
            && let Some((name, version)) = spec.split_once('@')
            && PackageManager::from_name(name) == Some(manager)
        {
            return Some(version.to_owned());
        }
        if current == "." {
            return None;
        }
        current = parent_path(&current);
    }
}

fn package_lockfile_path(
    relative_root: &str,
    manager: PackageManager,
    file_set: &BTreeSet<String>,
) -> Option<String> {
    let mut current = relative_root.to_owned();
    loop {
        if let Some(lockfile) = manager
            .lockfiles()
            .iter()
            .map(|lockfile| join_repo_path(&current, lockfile))
            .find(|lockfile| file_set.contains(lockfile))
        {
            return Some(lockfile);
        }
        if current == "." {
            return None;
        }
        current = parent_path(&current);
    }
}

fn package_commands(root: &str, facts: &PackageJsonFacts, locked: bool) -> Vec<String> {
    let prefix = shell_change_dir(root);
    let install = match facts.manager {
        PackageManager::Bun if locked => format!("{prefix}bun install --frozen-lockfile"),
        PackageManager::Bun => format!("{prefix}bun install"),
        PackageManager::Npm if locked => format!("{prefix}npm ci"),
        PackageManager::Npm => format!("{prefix}npm install"),
    };
    let command = facts.manager.command();
    let mut commands = vec![install];
    for script in ["lint", "typecheck", "build"] {
        if facts.scripts.contains(script) {
            commands.push(format!("{prefix}{command} run {script}"));
        }
    }
    if facts.scripts.contains("test") {
        commands.push(format!("{prefix}{command} run test"));
    }
    commands
}

pub(crate) fn detect(
    context: &ScanContext<'_>,
    shape: &mut RepositoryShape,
) -> Result<(), GeneratorError> {
    let package_manifests = files_named(context.files, "package.json");
    let mut package_units = Vec::<(String, String, BTreeSet<String>, String)>::new();
    let mut package_id_counts = BTreeMap::new();
    for package_root in roots_for_manifests(&package_manifests) {
        let prefix = path_prefix(&package_root);
        let manifest = join_repo_path(&package_root, "package.json");
        let contents = fs::read_to_string(context.root.join(&manifest)).map_err(|error| {
            GeneratorError::io(
                "read package manifest",
                &context.root.join(&manifest),
                &error,
            )
        })?;
        let Some(facts) =
            parse_package_json(context.root, &package_root, &contents, context.file_set)?
        else {
            shape.limitations.push(format!(
                "Skipped {manifest}: package manager is unsupported or unresolved; supported package managers are Bun and npm."
            ));
            continue;
        };
        let manager_lockfile =
            package_lockfile_path(&package_root, facts.manager, context.file_set);
        let kind = if facts.manager == PackageManager::Bun {
            UnitKind::Bun
        } else {
            UnitKind::Node
        };
        shape.detected.push(format!(
            "package-manager:{}:{}",
            facts.manager.detected(),
            package_root
        ));
        if facts.workspace_root {
            shape
                .detected
                .push(format!("package-workspace:{package_root}"));
        }
        for script in ["lint", "typecheck", "build", "test"] {
            if facts.scripts.contains(script) {
                shape
                    .detected
                    .push(format!("package-script:{package_root}:{script}"));
            }
        }
        if !facts.scripts.contains("test") {
            shape.limitations.push(format!(
                "No test script is declared in {manifest}; no package test command was invented."
            ));
        }
        if manager_lockfile.is_none() {
            shape.limitations.push(format!(
                "No {} lockfile is present for {manifest}; dependency installation is intentionally unlocked.",
                facts.manager.detected()
            ));
        }
        let lockfiles = ["bun.lock", "bun.lockb", "package-lock.json"]
            .into_iter()
            .filter(|lockfile| {
                context
                    .file_set
                    .contains(&join_repo_path(&package_root, lockfile))
            })
            .count();
        if lockfiles > 1 {
            shape.limitations.push(format!(
                "Multiple package lockfiles are present under {package_root}; the detected manager chooses one, review the package boundary."
            ));
        }
        let commands = package_commands(&package_root, &facts, manager_lockfile.is_some());
        let mut package_watch = vec![
            manifest.clone(),
            format!("{prefix}**/*.js"),
            format!("{prefix}**/*.jsx"),
            format!("{prefix}**/*.ts"),
            format!("{prefix}**/*.tsx"),
            format!("{prefix}**/*.json"),
            join_repo_path(&package_root, "bun.lock"),
            join_repo_path(&package_root, "bun.lockb"),
            join_repo_path(&package_root, "package-lock.json"),
        ];
        if let Some(lockfile) = &manager_lockfile {
            package_watch.push(lockfile.clone());
        }
        let package_cache = manager_lockfile.as_ref().map(|manager_lockfile| CacheSpec {
            key_files: vec![manifest.clone(), manager_lockfile.clone()],
            paths: vec![if facts.manager == PackageManager::Bun {
                "~/.bun/install/cache".to_owned()
            } else {
                "~/.npm".to_owned()
            }],
        });
        let mut package_unit = unit(kind, &package_root, package_watch, commands, package_cache);
        let base_id = format!("{}-{}", kind.id_prefix(), identifier_suffix(&facts.name));
        let count = package_id_counts.entry(base_id.clone()).or_insert(0_usize);
        *count += 1;
        package_unit.id = if *count == 1 {
            base_id
        } else {
            format!("{base_id}-{count}")
        };
        package_unit.label = format!("{} package ({})", kind.label(), facts.name);
        package_unit.tool_version.clone_from(&facts.manager_version);
        package_units.push((
            package_root,
            facts.name.clone(),
            facts.dependencies.clone(),
            package_unit.id.clone(),
        ));
        shape.units.push(package_unit);
    }
    let mut package_ids = BTreeMap::new();
    let mut duplicate_package_names = BTreeSet::new();
    for (_, name, _, id) in &package_units {
        if package_ids.insert(name.clone(), id.clone()).is_some() {
            duplicate_package_names.insert(name.clone());
        }
    }
    if !duplicate_package_names.is_empty() {
        shape.limitations.push(format!(
            "Duplicate package names ({}) prevent automatic internal dependency edges.",
            duplicate_package_names
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for unit in shape
        .units
        .iter_mut()
        .filter(|unit| matches!(unit.kind, UnitKind::Bun | UnitKind::Node))
    {
        let Some((_, _, dependencies, _)) = package_units
            .iter()
            .find(|(root, _, _, _)| root == &unit.root)
        else {
            continue;
        };
        unit.depends_on = dependencies
            .iter()
            .filter(|dependency| !duplicate_package_names.contains(*dependency))
            .filter_map(|dependency| package_ids.get(dependency).cloned())
            .filter(|dependency| dependency != &unit.id)
            .collect();
    }
    Ok(())
}
