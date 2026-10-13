//! Static Mise task/tool closure resolution.
//!
//! A generated job may disable Mise's implicit installation. That is safe only
//! when the generator knows the tools used by every selected task and every
//! typed task dependency. This module reads declarative TOML only. It never
//! interprets shell commands as task or tool edges.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use toml::{Table, Value};

use crate::GeneratorError;

type ToolIdentity = (
    ToolScope,
    String,
    String,
    Option<String>,
    BTreeMap<String, String>,
    String,
);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ToolScope {
    Root,
    Task(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToolRequirement {
    pub(crate) key: String,
    pub(crate) selector: String,
    pub(crate) locked_version: String,
    pub(crate) backend: Option<String>,
    pub(crate) options: BTreeMap<String, String>,
    pub(crate) scope: ToolScope,
    pub(crate) platform: String,
    pub(crate) url: Option<String>,
    pub(crate) checksum: Option<String>,
}

/// Rust is a typed toolchain boundary, not a bare Mise executable. The two
/// renderers own equivalent `RustToolchain` types, so the shared closure
/// borrows their common fields through this generation-only contract instead
/// of copying a third schema.
pub(crate) trait RustToolchainLike {
    fn channel(&self) -> &str;
    fn components(&self) -> &[String];
    fn targets(&self) -> &[String];
    fn profile(&self) -> Option<&str> {
        None
    }

    fn mise_options(&self) -> BTreeMap<String, String> {
        let mut options = BTreeMap::new();
        if let Some(profile) = self.profile() {
            options.insert("profile".to_owned(), profile.to_owned());
        }
        if !self.components().is_empty() {
            options.insert("components".to_owned(), self.components().join(","));
        }
        if !self.targets().is_empty() {
            options.insert("targets".to_owned(), self.targets().join(","));
        }
        options
    }
}

fn validate_rust_tool(
    toolchain: &dyn RustToolchainLike,
    key: &str,
    scope: &ToolScope,
    spec: &ToolSpec,
) -> Result<BTreeMap<String, String>, GeneratorError> {
    if spec.selector != toolchain.channel() {
        return Err(GeneratorError::usage(format!(
            "Mise Rust tool {key} in {scope:?} selects {}, but rust-toolchain pins {}",
            spec.selector,
            toolchain.channel()
        )));
    }
    let expected = toolchain.mise_options();
    let supplied = spec.options.clone();
    let effective = if supplied.is_empty() {
        expected.clone()
    } else {
        supplied
    };
    if effective != expected {
        return Err(GeneratorError::usage(format!(
            "Mise Rust tool {key} in {scope:?} options {effective:?} differ from rust-toolchain options {expected:?}"
        )));
    }
    Ok(expected)
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum TaskPhase {
    Before,
    Main,
    Run,
    After,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskInvocation {
    pub(crate) task: String,
    pub(crate) phase: TaskPhase,
    pub(crate) parallel_group: Option<usize>,
    pub(crate) args: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TaskWait {
    pub(crate) task: String,
    pub(crate) waits_for: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TaskClosure {
    pub(crate) tasks: Vec<String>,
    pub(crate) tools: Vec<ToolRequirement>,
    pub(crate) invocations: Vec<TaskInvocation>,
    pub(crate) waits: Vec<TaskWait>,
    /// Present only when a selected task actually uses the typed Rust
    /// boundary. This keeps Rust provisioning stage-scoped.
    pub(crate) uses_rust_toolchain: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ToolInstallPlan {
    pub(crate) requirements: Vec<ToolRequirement>,
    /// Whether the typed Rustup boundary, rather than Mise, owns the Rust
    /// executable for this profile.
    pub(crate) rustup_owns_rust: bool,
}

impl TaskClosure {
    /// Resolve the typed requirements into the exact products a profile may
    /// install. The renderer still owns the explicit profile contract; this
    /// plan only adds discovered task-local products and preserves their
    /// locked identities.
    pub(crate) fn installation_plan(
        &self,
        profile_id: &str,
    ) -> Result<ToolInstallPlan, GeneratorError> {
        let mut root_by_key = BTreeMap::<&str, &ToolRequirement>::new();
        for requirement in &self.tools {
            if !matches!(&requirement.scope, ToolScope::Root) {
                continue;
            }
            if root_by_key
                .insert(&requirement.key, requirement)
                .is_some_and(|previous| {
                    previous.selector != requirement.selector
                        || previous.backend != requirement.backend
                        || previous.options != requirement.options
                })
            {
                return Err(GeneratorError::usage(format!(
                    "check profile {profile_id} resolves multiple root Mise identities for tool {}; declare one effective root selector",
                    requirement.key
                )));
            }
        }
        let requirements = self.tools.clone();
        for requirement in &self.tools {
            if let ToolScope::Root = &requirement.scope {
                debug_assert!(root_by_key.contains_key(requirement.key.as_str()));
            }
        }
        Ok(ToolInstallPlan {
            requirements,
            rustup_owns_rust: self.uses_rust_toolchain,
        })
    }

    /// Validate that explicit profile tools are lock-backed. Task-local
    /// requirements may be omitted from the explicit list because the typed
    /// installation plan carries their exact version and scope.
    ///
    pub(crate) fn validate_profile_tools(
        &self,
        profile_id: &str,
        explicit_tools: &[String],
    ) -> Result<(), GeneratorError> {
        let _plan = self.installation_plan(profile_id)?;
        let explicit = explicit_tools.iter().collect::<BTreeSet<_>>();
        for key in explicit {
            if *key == "rust" && self.uses_rust_toolchain {
                continue;
            }
            if !self.tools.iter().any(|requirement| {
                requirement.key == *key && matches!(requirement.scope, ToolScope::Root)
            }) {
                return Err(GeneratorError::usage(format!(
                    "check profile {profile_id} names Mise tool {key}, but no root lock-backed requirement was resolved"
                )));
            }
        }
        Ok(())
    }
}

impl ToolInstallPlan {
    /// Render the narrow Mise argument form supported by the current profile
    /// renderer. The current runner contract accepts only bare keys that occur
    /// in the adjacent lock. A task-local identity that differs from the root
    /// identity therefore fails until the coordinated exact-selection
    /// installer migration adds a trusted representation for it.
    pub(crate) fn mise_args(&self, profile_id: &str) -> Result<Vec<String>, GeneratorError> {
        let mut root_by_key = BTreeMap::<&str, &ToolRequirement>::new();
        let mut args = Vec::new();
        let mut seen = BTreeSet::new();
        for requirement in &self.requirements {
            if let ToolScope::Root = &requirement.scope {
                if requirement.key == "rust" && self.rustup_owns_rust {
                    continue;
                }
                root_by_key.entry(&requirement.key).or_insert(requirement);
            }
        }
        for requirement in &self.requirements {
            if matches!(&requirement.scope, ToolScope::Root)
                && (requirement.key != "rust" || !self.rustup_owns_rust)
                && seen.insert(requirement.key.clone())
            {
                args.push(requirement.key.clone());
            }
        }
        for requirement in &self.requirements {
            if let ToolScope::Task(task) = &requirement.scope {
                let same_root = root_by_key
                    .get(requirement.key.as_str())
                    .is_some_and(|root| {
                        root.selector == requirement.selector
                            && root.locked_version == requirement.locked_version
                            && root.backend == requirement.backend
                            && root.options == requirement.options
                    });
                if same_root {
                    continue;
                }
                return Err(GeneratorError::usage(format!(
                    "check profile {profile_id} task {task} requires task-local Mise identity {}@{} (selector {:?}, backend {:?}, options {:?}), but the current installer accepts only bare root lock keys; declare a matching root identity or complete the exact locked tool selection migration",
                    requirement.key,
                    requirement.locked_version,
                    requirement.selector,
                    requirement.backend,
                    requirement.options
                )));
            }
        }
        Ok(args)
    }
}

#[cfg(test)]
fn resolve_profile(
    root: &Path,
    tasks: &[String],
    explicit_tools: &[String],
) -> Result<TaskClosure, GeneratorError> {
    // Tests choose their target explicitly.  The generator must never bind a
    // lock artifact to the machine compiling it.
    resolve_profile_for_platform(root, tasks, explicit_tools, "linux-x64")
}

#[cfg(test)]
pub(crate) fn resolve_profile_for_platform(
    root: &Path,
    tasks: &[String],
    explicit_tools: &[String],
    platform: &str,
) -> Result<TaskClosure, GeneratorError> {
    resolve_profile_for_platform_with_rust(root, tasks, explicit_tools, platform, None)
}

/// Resolve a profile with the repository's already-parsed typed Rust pin.
/// `None` preserves generic Mise-only behavior for callers that do not have a
/// Rust toolchain file; a manifest that requests Rust options then fails
/// closed instead of silently dropping them.
pub(crate) fn resolve_profile_for_platform_with_rust(
    root: &Path,
    tasks: &[String],
    explicit_tools: &[String],
    platform: &str,
    rust_toolchain: Option<&dyn RustToolchainLike>,
) -> Result<TaskClosure, GeneratorError> {
    validate_platform(platform)?;
    let manifest = Manifest::load(root)?;
    let mut resolver = Resolver {
        manifest,
        closure: TaskClosure::default(),
        task_state: BTreeMap::new(),
        task_stack: Vec::new(),
        tool_seen: BTreeSet::new(),
        platform: platform.to_owned(),
        rust_toolchain,
    };
    for task in tasks {
        resolver.visit_task(task, TaskPhase::Main, &TaskRef::plain(task))?;
    }
    for key in explicit_tools {
        if resolver.manifest.tools.contains_key(key) {
            resolver.add_root_tool(key)?;
        } else if !resolver.closure.tools.iter().any(|tool| tool.key == *key) {
            return Err(GeneratorError::usage(format!(
                "check profile names Mise tool {key}, which mise.toml does not declare"
            )));
        }
    }
    Ok(resolver.closure)
}

/// The lockfile platform vocabulary used by the strict installer.  A runner
/// label is routing data, not artifact identity; labels that do not carry a
/// typed declaration must fail instead of being guessed.
pub(crate) fn validate_platform(platform: &str) -> Result<(), GeneratorError> {
    match platform {
        "linux-x64"
        | "linux-arm64"
        | "macos-x64"
        | "macos-arm64"
        | "windows-x64"
        | "windows-arm64" => Ok(()),
        _ => Err(GeneratorError::usage(format!(
            "unknown Mise execution platform `{platform}`; expected one of: linux-x64, linux-arm64, macos-x64, macos-arm64, windows-x64, windows-arm64"
        ))),
    }
}

/// Resolve a profile's typed target platform.  A provider label alone cannot
/// prove its OS or architecture, so callers must provide the target platform
/// explicitly.  The S2 renderer supplies its fixed Apple lane's typed value
/// at the call site; configurable legacy runners cannot infer one.
pub(crate) fn platform_for_runner<'a>(
    runner: &str,
    declared_platform: Option<&'a str>,
) -> Result<&'a str, GeneratorError> {
    if let Some(platform) = declared_platform {
        validate_platform(platform)?;
        return Ok(platform);
    }
    Err(GeneratorError::usage(format!(
        "profile runner `{runner}` has no typed Mise platform; declare platform = \"linux-x64\", \"linux-arm64\", \"macos-x64\", \"macos-arm64\", \"windows-x64\", or \"windows-arm64\""
    )))
}

/// Resolve a profile platform against the labels that the renderer will put
/// in `runs-on`. A lock platform is only useful when it describes the machine
/// that actually executes the job. Known GitHub labels are an immutable table
/// here; arbitrary self-hosted labels require the profile's explicit typed
/// platform declaration and are never guessed from their spelling.
pub(crate) fn platform_for_runner_labels<'platform, 'label, I>(
    runner: &str,
    declared_platform: Option<&'platform str>,
    labels: I,
) -> Result<String, GeneratorError>
where
    I: IntoIterator<Item = &'label str>,
{
    let mut observed = None;
    for label in labels {
        let Some(platform) = known_runner_label_platform(label) else {
            continue;
        };
        if let Some(previous) = observed
            && previous != platform
        {
            return Err(GeneratorError::usage(format!(
                "profile runner `{runner}` labels imply both {previous} and {platform}; declare one coherent typed runner"
            )));
        }
        observed = Some(platform);
    }
    let platform = declared_platform.or(observed).ok_or_else(|| {
        GeneratorError::usage(format!(
            "profile runner `{runner}` has no typed Mise platform; declare `platform` or use a known runner label"
        ))
    })?;
    validate_platform(platform)?;
    if let Some(observed) = observed
        && observed != platform
    {
        return Err(GeneratorError::usage(format!(
            "profile runner `{runner}` label platform {observed} conflicts with declared Mise platform {platform}"
        )));
    }
    let compatible = match runner {
        // The legacy and S2 `github` check-profile lane is the hosted Linux
        // lane. Apple has its own named lane, and Windows is not a supported
        // check-profile executor in either renderer.
        "github" | "velnor" => platform.starts_with("linux-"),
        "macos" => platform.starts_with("macos-"),
        // Velnor's provider contract currently exposes Linux only. Its custom
        // labels carry no architecture fact, so an explicit declaration is
        // required when no known hosted label supplied one above.
        _ => false,
    };
    if !compatible {
        return Err(GeneratorError::usage(format!(
            "profile runner `{runner}` cannot execute Mise platform {platform}; use a compatible typed runner or change `platform`"
        )));
    }
    Ok(platform.to_owned())
}

fn known_runner_label_platform(label: &str) -> Option<&'static str> {
    // Keep this table exact. A user-defined label that happens to contain one
    // of these fragments must remain unknown and therefore needs an explicit
    // profile platform declaration.
    const LINUX_X64: &[&str] = &[
        "ubuntu-latest",
        "ubuntu-slim",
        "ubuntu-22.04",
        "ubuntu-24.04",
        "ubuntu-26.04",
        "ubuntu-22.04-x64",
        "ubuntu-24.04-x64",
        "ubuntu-26.04-x64",
    ];
    const LINUX_ARM64: &[&str] = &["ubuntu-22.04-arm", "ubuntu-24.04-arm", "ubuntu-26.04-arm"];
    const MACOS_X64: &[&str] = &[
        "macos-14-large",
        "macos-15-intel",
        "macos-15-large",
        "macos-26-intel",
        "macos-26-large",
        "macos-latest-large",
    ];
    const MACOS_ARM64: &[&str] = &[
        "macos-latest",
        "macos-14",
        "macos-14-xlarge",
        "macos-15",
        "macos-15-xlarge",
        "macos-26",
        "macos-26-xlarge",
        "macos-latest-xlarge",
        "xcode-27",
        "xcode-27-xlarge",
    ];
    const WINDOWS_X64: &[&str] = &[
        "windows-latest",
        "windows-2022",
        "windows-2025",
        "windows-2025-vs2026",
    ];
    const WINDOWS_ARM64: &[&str] = &["windows-11-arm", "windows-11-vs2026-arm"];
    if LINUX_X64.contains(&label) {
        Some("linux-x64")
    } else if LINUX_ARM64.contains(&label) {
        Some("linux-arm64")
    } else if MACOS_X64.contains(&label) {
        Some("macos-x64")
    } else if MACOS_ARM64.contains(&label) {
        Some("macos-arm64")
    } else if WINDOWS_X64.contains(&label) {
        Some("windows-x64")
    } else if WINDOWS_ARM64.contains(&label) {
        Some("windows-arm64")
    } else {
        None
    }
}

/// Resolve the S2 renderer's fixed Apple lane.  Its `macos-26` routing label
/// is generator-owned and typed as Apple ARM64; an explicit contradiction is
/// rejected instead of changing the lock platform behind the renderer.
pub(crate) fn platform_for_s2_runner<'a>(
    runner: &str,
    declared_platform: Option<&'a str>,
) -> Result<&'a str, GeneratorError> {
    let declared_platform = match (runner, declared_platform) {
        ("mac" | "macos", Some(platform)) if platform != "macos-arm64" => {
            return Err(GeneratorError::usage(format!(
                "profile runner `{runner}` is fixed to macos-arm64, not `{platform}`"
            )));
        }
        ("mac" | "macos", None) => Some("macos-arm64"),
        (_, declared_platform) => declared_platform,
    };
    platform_for_runner(runner, declared_platform)
}

#[derive(Clone, Debug)]
struct ToolSpec {
    selector: String,
    backend: Option<String>,
    depends: Vec<ToolRef>,
    options: BTreeMap<String, String>,
    os: Vec<String>,
}

#[derive(Clone, Debug)]
struct ToolRef {
    key: String,
    selector: Option<String>,
    backend: Option<String>,
    options: BTreeMap<String, String>,
    os: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct TaskSpec {
    tools: BTreeMap<String, ToolSpec>,
    depends: Vec<TaskRef>,
    depends_post: Vec<TaskRef>,
    wait_for: Vec<TaskRef>,
    run_tasks: Vec<TaskRef>,
    aliases: Vec<String>,
}

#[derive(Clone, Debug)]
struct TaskRef {
    name: String,
    optional: bool,
    parallel_group: Option<usize>,
    args: Vec<String>,
    env: BTreeMap<String, String>,
}

impl TaskRef {
    fn plain(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            optional: false,
            parallel_group: None,
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Manifest {
    tools: BTreeMap<String, ToolSpec>,
    tasks: BTreeMap<String, TaskSpec>,
    aliases: BTreeMap<String, String>,
    lock: Lockfile,
}

impl Manifest {
    fn load(root: &Path) -> Result<Self, GeneratorError> {
        let path = root.join("mise.toml");
        let text = fs::read_to_string(&path)
            .map_err(|error| GeneratorError::io("read mise.toml", &path, &error))?;
        let document = toml::from_str::<Value>(&text).map_err(|error| {
            GeneratorError::usage(format!("parse mise.toml {}: {error}", path.display()))
        })?;
        let table = document.as_table().ok_or_else(|| {
            GeneratorError::usage(format!(
                "parse mise.toml {}: document must be a table",
                path.display()
            ))
        })?;
        reject_unsupported_manifest_shape(table, &path)?;
        let mut manifest = Self {
            tools: parse_tools(table.get("tools"), "root")?,
            tasks: parse_tasks(table.get("tasks"), &path)?,
            aliases: BTreeMap::new(),
            lock: Lockfile::load(root)?,
        };
        for (name, task) in &manifest.tasks {
            for alias in &task.aliases {
                if manifest.tasks.contains_key(alias) {
                    // Mise gives a concrete task name precedence over an
                    // alias with the same spelling.
                    continue;
                }
                if manifest.aliases.contains_key(alias) {
                    return Err(GeneratorError::usage(format!(
                        "mise.toml task {name} declares duplicate alias {alias}"
                    )));
                }
                manifest.aliases.insert(alias.clone(), name.clone());
            }
        }
        add_idiomatic_rust_tool(root, table, &mut manifest.tools)?;
        Ok(manifest)
    }

    fn canonical_task(&self, requested: &str) -> Result<String, GeneratorError> {
        if self.tasks.contains_key(requested) {
            return Ok(requested.to_owned());
        }
        let mut current = requested.to_owned();
        let mut seen = BTreeSet::new();
        while let Some(next) = self.aliases.get(&current) {
            if !seen.insert(current.clone()) {
                return Err(GeneratorError::usage(format!(
                    "mise.toml task alias cycle contains {requested}"
                )));
            }
            current.clone_from(next);
            if self.tasks.contains_key(&current) {
                return Ok(current);
            }
        }
        Err(GeneratorError::usage(format!(
            "check profile names Mise task {requested}, which mise.toml does not declare"
        )))
    }

    fn resolve_task_names(&self, requested: &str) -> Result<Vec<String>, GeneratorError> {
        if requested.contains('*') || requested.contains('?') {
            return Ok(self
                .tasks
                .keys()
                .filter(|name| glob_matches(requested, name))
                .cloned()
                .collect());
        }
        if self.tasks.contains_key(requested) || self.aliases.contains_key(requested) {
            self.canonical_task(requested).map(|task| vec![task])
        } else {
            Ok(Vec::new())
        }
    }
}

fn glob_matches(pattern: &str, value: &str) -> bool {
    fn inner(pattern: &[u8], value: &[u8]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some((b'*', rest)) => {
                inner(rest, value) || value.first().is_some_and(|_| inner(pattern, &value[1..]))
            }
            Some((b'?', rest)) => value
                .split_first()
                .is_some_and(|(_, value)| inner(rest, value)),
            Some((head, rest)) => value
                .split_first()
                .is_some_and(|(value_head, value)| head == value_head && inner(rest, value)),
        }
    }
    inner(pattern.as_bytes(), value.as_bytes())
}

#[derive(Clone, Debug, Default)]
struct Lockfile {
    tools: BTreeMap<String, Vec<LockEntry>>,
}

#[derive(Clone, Debug)]
struct LockedPlatform {
    url: String,
    checksum: String,
}

#[derive(Clone, Debug)]
struct LockEntry {
    version: String,
    requested: Option<String>,
    specifiers: Vec<String>,
    backend: Option<String>,
    options: BTreeMap<String, String>,
    platforms: BTreeMap<String, LockedPlatform>,
}

#[derive(Clone, Debug)]
struct LockedArtifact {
    version: String,
    url: Option<String>,
    checksum: Option<String>,
}

impl Lockfile {
    fn load(root: &Path) -> Result<Self, GeneratorError> {
        let path = root.join("mise.lock");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(GeneratorError::io("read mise.lock", &path, &error)),
        };
        let document = toml::from_str::<Value>(&text).map_err(|error| {
            GeneratorError::usage(format!("parse mise.lock {}: {error}", path.display()))
        })?;
        let Some(tools) = document.get("tools").and_then(Value::as_table) else {
            return Ok(Self::default());
        };
        let mut locked = BTreeMap::new();
        for (key, value) in tools {
            let rows = match value {
                Value::Array(rows) => rows.iter().collect::<Vec<_>>(),
                _ => vec![value],
            };
            let mut entries = Vec::new();
            for row in rows {
                let Some(row) = row.as_table() else {
                    return Err(GeneratorError::usage(format!(
                        "mise.lock tool {key} must contain lock tables"
                    )));
                };
                let version = row.get("version").and_then(Value::as_str).ok_or_else(|| {
                    GeneratorError::usage(format!("mise.lock tool {key} has no locked version"))
                })?;
                if version.is_empty() {
                    return Err(GeneratorError::usage(format!(
                        "mise.lock tool {key} has an empty locked version"
                    )));
                }
                let requested = match row.get("requested") {
                    None => None,
                    Some(value) => {
                        let requested = value.as_str().ok_or_else(|| {
                            GeneratorError::usage(format!(
                                "mise.lock tool {key} requested selector must be a string"
                            ))
                        })?;
                        if requested.is_empty() {
                            return Err(GeneratorError::usage(format!(
                                "mise.lock tool {key} has an empty requested selector"
                            )));
                        }
                        Some(requested.to_owned())
                    }
                };
                let backend = match row.get("backend") {
                    None => None,
                    Some(value) => {
                        let backend = value.as_str().ok_or_else(|| {
                            GeneratorError::usage(format!(
                                "mise.lock tool {key} backend must be a string"
                            ))
                        })?;
                        if backend.is_empty() {
                            return Err(GeneratorError::usage(format!(
                                "mise.lock tool {key} has an empty backend"
                            )));
                        }
                        Some(backend.to_owned())
                    }
                };
                entries.push(LockEntry {
                    version: version.to_owned(),
                    requested,
                    specifiers: string_list(row.get("specifiers"), "mise.lock specifiers")?,
                    backend,
                    options: row
                        .get("options")
                        .map(|value| string_map(value, "mise.lock tool options"))
                        .transpose()?
                        .unwrap_or_default(),
                    platforms: parse_lock_platforms(row, key)?,
                });
            }
            locked.insert(key.clone(), entries);
        }
        Ok(Self { tools: locked })
    }

    fn resolve(
        &self,
        key: &str,
        selector: &str,
        backend: Option<&str>,
        options: &BTreeMap<String, String>,
        platform: &str,
    ) -> Result<LockedArtifact, GeneratorError> {
        validate_platform(platform)?;
        let Some(entries) = self.tools.get(key) else {
            return Err(GeneratorError::usage(format!(
                "Mise tool {key} is absent from mise.lock; typed profile closure cannot install it with auto-install disabled"
            )));
        };
        let matched = entries
            .iter()
            .filter(|entry| {
                let selector_match = entry.version == selector
                    || entry.requested.as_deref() == Some(selector)
                    || entry.specifiers.iter().any(|value| value == selector);
                let backend_match = match (backend, entry.backend.as_deref()) {
                    (Some(expected), Some(actual)) => expected == actual,
                    (Some(_), None) => false,
                    (None, _) => true,
                };
                selector_match && backend_match && entry.options == *options
            })
            .collect::<Vec<_>>();
        if matched.is_empty() {
            return Err(GeneratorError::usage(format!(
                "Mise tool {key} selector {selector} has no matching locked version/backend/options"
            )));
        }
        if matched.len() > 1 {
            return Err(GeneratorError::usage(format!(
                "Mise tool {key} selector {selector} matches multiple locked artifacts"
            )));
        }
        let entry = matched[0];
        let artifact = if entry.platforms.is_empty() {
            if requires_platform_record(entry.backend.as_deref()) {
                return Err(GeneratorError::usage(format!(
                    "Mise tool {key}@{} has no platform lock record for {platform}",
                    entry.version
                )));
            }
            LockedArtifact {
                version: entry.version.clone(),
                url: None,
                checksum: None,
            }
        } else {
            let Some(platform) = entry.platforms.get(platform) else {
                return Err(GeneratorError::usage(format!(
                    "Mise tool {key}@{} has no platform lock record for {platform}",
                    entry.version
                )));
            };
            LockedArtifact {
                version: entry.version.clone(),
                url: Some(platform.url.clone()),
                checksum: Some(platform.checksum.clone()),
            }
        };
        Ok(artifact)
    }
}

fn requires_platform_record(backend: Option<&str>) -> bool {
    backend.is_some_and(|backend| {
        !backend.starts_with("cargo:")
            && !backend.starts_with("pipx:")
            && !backend.starts_with("core:")
    })
}

fn parse_lock_platforms(
    row: &Table,
    key: &str,
) -> Result<BTreeMap<String, LockedPlatform>, GeneratorError> {
    let mut platforms = BTreeMap::new();
    for (nested_key, value) in row {
        let Some(platform) = nested_key.strip_prefix("platforms.") else {
            continue;
        };
        if platform.is_empty() {
            return Err(GeneratorError::usage(format!(
                "mise.lock tool {key} has an empty platform lock key"
            )));
        }
        let table = value.as_table().ok_or_else(|| {
            GeneratorError::usage(format!(
                "mise.lock tool {key} platform {platform} must be a table"
            ))
        })?;
        let url = table.get("url").and_then(Value::as_str).ok_or_else(|| {
            GeneratorError::usage(format!(
                "mise.lock tool {key} platform {platform} has no URL"
            ))
        })?;
        if url.is_empty() {
            return Err(GeneratorError::usage(format!(
                "mise.lock tool {key} platform {platform} has an empty URL"
            )));
        }
        let checksum = table
            .get("checksum")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                GeneratorError::usage(format!(
                    "mise.lock tool {key} platform {platform} has no checksum"
                ))
            })?;
        if checksum.is_empty() {
            return Err(GeneratorError::usage(format!(
                "mise.lock tool {key} platform {platform} has an empty checksum"
            )));
        }
        platforms.insert(
            platform.to_owned(),
            LockedPlatform {
                url: url.to_owned(),
                checksum: checksum.to_owned(),
            },
        );
    }
    Ok(platforms)
}

fn string_map(value: &Value, context: &str) -> Result<BTreeMap<String, String>, GeneratorError> {
    let table = value
        .as_table()
        .ok_or_else(|| GeneratorError::usage(format!("{context} must be a table")))?;
    table
        .iter()
        .map(|(key, value)| Ok((key.clone(), option_value(value))))
        .collect()
}

fn option_value(value: &Value) -> String {
    if let Some(value) = value.as_str() {
        return value.to_owned();
    }
    if let Some(values) = value.as_array()
        && values.iter().all(Value::is_str)
    {
        return values
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(",");
    }
    value.to_string()
}

struct Resolver<'a> {
    manifest: Manifest,
    closure: TaskClosure,
    task_state: BTreeMap<(String, TaskPhase), VisitState>,
    task_stack: Vec<(String, TaskPhase)>,
    tool_seen: BTreeSet<ToolIdentity>,
    platform: String,
    rust_toolchain: Option<&'a dyn RustToolchainLike>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VisitState {
    Visiting,
    Complete,
}

impl Resolver<'_> {
    fn add_root_tool(&mut self, key: &str) -> Result<(), GeneratorError> {
        let spec = self.manifest.tools.get(key).cloned().ok_or_else(|| {
            GeneratorError::usage(format!(
                "check profile names Mise tool {key}, which mise.toml does not declare"
            ))
        })?;
        self.add_tool(&ToolScope::Root, key, spec)
    }

    fn add_tool(
        &mut self,
        scope: &ToolScope,
        key: &str,
        mut spec: ToolSpec,
    ) -> Result<(), GeneratorError> {
        if let ToolScope::Task(_) = scope {
            let Some(root) = self.manifest.tools.get(key) else {
                return self.add_tool_unbound(scope, key, spec);
            };
            if spec.backend.is_none() {
                spec.backend.clone_from(&root.backend);
            }
            if spec.options.is_empty() {
                spec.options.clone_from(&root.options);
            }
            if spec.os.is_empty() {
                spec.os.clone_from(&root.os);
            }
        }
        self.add_tool_unbound(scope, key, spec)
    }

    fn add_tool_unbound(
        &mut self,
        scope: &ToolScope,
        key: &str,
        spec: ToolSpec,
    ) -> Result<(), GeneratorError> {
        validate_tool_platform(key, &spec.os, &self.platform)?;
        let typed_rust =
            key == "rust" && (self.rust_toolchain.is_some() || !spec.options.is_empty());
        let rust_options = if typed_rust {
            let Some(boundary) = self.rust_toolchain else {
                return Err(rustup_boundary_error(
                    "Mise Rust options",
                    spec.options.keys().map(String::as_str),
                ));
            };
            Some(validate_rust_tool(boundary, key, scope, &spec)?)
        } else {
            None
        };
        if typed_rust
            && spec
                .backend
                .as_deref()
                .is_some_and(|backend| backend != "core:rust")
        {
            return Err(GeneratorError::usage(format!(
                "Mise Rust tool {key} in {scope:?} must use backend core:rust for the typed Rustup boundary"
            )));
        }
        let lock_options = rust_options.as_ref().unwrap_or(&spec.options);
        let lock_backend = typed_rust
            .then_some("core:rust")
            .or(spec.backend.as_deref());
        let locked = self.manifest.lock.resolve(
            key,
            &spec.selector,
            lock_backend,
            lock_options,
            &self.platform,
        )?;
        if typed_rust && locked.version != spec.selector {
            return Err(GeneratorError::usage(format!(
                "Mise Rust tool {key} selector {} resolves locked version {}, which is not the exact rustup channel; pin an exact Rust channel and matching lock entry",
                spec.selector, locked.version
            )));
        }
        let identity = (
            scope.clone(),
            key.to_owned(),
            spec.selector.clone(),
            spec.backend.clone(),
            lock_options.clone(),
            self.platform.clone(),
        );
        if self.tool_seen.insert(identity) {
            if typed_rust {
                self.closure.uses_rust_toolchain = true;
            } else {
                self.closure.tools.push(ToolRequirement {
                    key: key.to_owned(),
                    selector: spec.selector.clone(),
                    locked_version: locked.version,
                    backend: spec.backend.clone(),
                    options: lock_options.clone(),
                    scope: scope.clone(),
                    platform: self.platform.clone(),
                    url: locked.url,
                    checksum: locked.checksum,
                });
            }
            for dependency in spec.depends {
                self.add_tool_ref(scope, dependency)?;
            }
        }
        Ok(())
    }

    fn add_tool_ref(
        &mut self,
        scope: &ToolScope,
        reference: ToolRef,
    ) -> Result<(), GeneratorError> {
        let spec = self
            .manifest
            .tools
            .get(&reference.key)
            .cloned()
            .ok_or_else(|| {
                GeneratorError::usage(format!(
                    "Mise tool dependency {} is not declared in root tools",
                    reference.key
                ))
            })?;
        let options = if reference.options.is_empty() {
            spec.options.clone()
        } else {
            reference.options
        };
        self.add_tool(
            scope,
            &reference.key,
            ToolSpec {
                selector: reference.selector.unwrap_or(spec.selector),
                backend: reference.backend.or(spec.backend),
                depends: spec.depends,
                options,
                os: if reference.os.is_empty() {
                    spec.os
                } else {
                    reference.os
                },
            },
        )
    }

    fn visit_task(
        &mut self,
        requested: &str,
        phase: TaskPhase,
        reference: &TaskRef,
    ) -> Result<(), GeneratorError> {
        let task = self.manifest.canonical_task(requested)?;
        let state_key = (task.clone(), phase);
        match self.task_state.get(&state_key) {
            Some(VisitState::Complete) => return Ok(()),
            Some(VisitState::Visiting) => {
                let cycle = self
                    .task_stack
                    .iter()
                    .chain(std::iter::once(&state_key))
                    .map(|(task, phase)| format!("{task} ({phase:?})"))
                    .collect::<Vec<_>>()
                    .join(" -> ");
                return Err(GeneratorError::usage(format!(
                    "Mise task dependency cycle: {cycle}"
                )));
            }
            None => {}
        }
        self.task_state
            .insert(state_key.clone(), VisitState::Visiting);
        self.task_stack.push(state_key.clone());
        let result = self.visit_task_body(&task, phase, reference);
        self.task_stack.pop();
        if result.is_ok() {
            self.task_state.insert(state_key, VisitState::Complete);
        }
        result
    }

    fn visit_task_body(
        &mut self,
        task: &str,
        phase: TaskPhase,
        reference: &TaskRef,
    ) -> Result<(), GeneratorError> {
        let spec = self
            .manifest
            .tasks
            .get(task)
            .cloned()
            .ok_or_else(|| GeneratorError::usage(format!("Mise task {task} is missing")))?;
        for dependency in spec.depends {
            self.visit_ref(&dependency, "depends", TaskPhase::Before)?;
        }
        if !self.closure.tasks.iter().any(|name| name == task) {
            self.closure.tasks.push(task.to_owned());
        }
        self.closure.invocations.push(TaskInvocation {
            task: task.to_owned(),
            phase,
            parallel_group: reference.parallel_group,
            args: reference.args.clone(),
            env: reference.env.clone(),
        });
        let scope = ToolScope::Task(task.to_owned());
        for (key, tool) in spec.tools {
            self.add_tool(&scope, &key, tool)?;
        }
        for run in spec.run_tasks {
            self.visit_ref(&run, "run", TaskPhase::Run)?;
        }
        for dependency in spec.depends_post {
            self.visit_ref(&dependency, "depends_post", TaskPhase::After)?;
        }
        for dependency in spec.wait_for {
            let names = self.manifest.resolve_task_names(&dependency.name)?;
            if names.is_empty() && !dependency.optional {
                return Err(GeneratorError::usage(format!(
                    "Mise task {task} wait_for references missing task {}",
                    dependency.name
                )));
            }
            for name in names {
                if self
                    .closure
                    .invocations
                    .iter()
                    .any(|invocation| invocation.task == name)
                    && !self
                        .closure
                        .waits
                        .iter()
                        .any(|wait| wait.task == task && wait.waits_for == name)
                {
                    self.closure.waits.push(TaskWait {
                        task: task.to_owned(),
                        waits_for: name,
                    });
                }
            }
        }
        Ok(())
    }

    fn visit_ref(
        &mut self,
        reference: &TaskRef,
        relation: &str,
        phase: TaskPhase,
    ) -> Result<(), GeneratorError> {
        let names = self.manifest.resolve_task_names(&reference.name)?;
        if names.is_empty() {
            if reference.optional {
                return Ok(());
            }
            let task = self
                .task_stack
                .last()
                .map_or("profile", |(task, _)| task.as_str());
            return Err(GeneratorError::usage(format!(
                "Mise task {task} {relation} references missing task {}",
                reference.name
            )));
        }
        for name in names {
            self.visit_task(&name, phase, reference)?;
        }
        Ok(())
    }
}

fn validate_tool_platform(key: &str, os: &[String], platform: &str) -> Result<(), GeneratorError> {
    if os.is_empty() {
        return Ok(());
    }
    let expected = if platform.starts_with("macos") {
        "macos"
    } else if platform.starts_with("linux") {
        "linux"
    } else if platform.starts_with("windows") {
        "windows"
    } else {
        platform
    };
    if os
        .iter()
        .any(|value| value == expected || value == platform)
    {
        return Ok(());
    }
    Err(GeneratorError::usage(format!(
        "Mise tool {key} is restricted to OS {os:?}, incompatible with runner platform {platform}"
    )))
}

fn parse_tools(
    value: Option<&Value>,
    context: &str,
) -> Result<BTreeMap<String, ToolSpec>, GeneratorError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let table = value.as_table().ok_or_else(|| {
        GeneratorError::usage(format!("mise.toml [{context}] tools must be a table"))
    })?;
    let mut tools = BTreeMap::new();
    for (key, value) in table {
        if key.trim().is_empty() || key.chars().any(char::is_whitespace) {
            return Err(GeneratorError::usage(format!(
                "mise.toml [{context}] declares invalid tool key {key}"
            )));
        }
        let spec = parse_tool_spec(value, &format!("{context} tool {key}"))?;
        tools.insert(key.clone(), spec);
    }
    Ok(tools)
}

fn parse_tool_spec(value: &Value, context: &str) -> Result<ToolSpec, GeneratorError> {
    match value {
        Value::String(selector) => Ok(ToolSpec {
            selector: selector.clone(),
            backend: None,
            depends: Vec::new(),
            options: BTreeMap::new(),
            os: Vec::new(),
        }),
        Value::Table(table) => {
            let selector = table
                .get("version")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    GeneratorError::usage(format!("{context} must declare a string version"))
                })?;
            Ok(ToolSpec {
                selector: selector.to_owned(),
                backend: table
                    .get("backend")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                depends: parse_tool_refs(table.get("depends"), context)?,
                options: tool_options(table, context)?,
                os: parse_os(table.get("os"), context)?,
            })
        }
        _ => Err(GeneratorError::usage(format!(
            "{context} must be a version string or table"
        ))),
    }
}

fn tool_options(table: &Table, context: &str) -> Result<BTreeMap<String, String>, GeneratorError> {
    let mut options = table
        .iter()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "version" | "backend" | "depends" | "tool" | "key" | "os" | "options"
            )
        })
        .map(|(key, value)| (key.clone(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    if let Some(value) = table.get("options") {
        options.extend(string_map(value, &format!("{context} options"))?);
    }
    Ok(options)
}

fn parse_os(value: Option<&Value>, context: &str) -> Result<Vec<String>, GeneratorError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    string_list(Some(value), &format!("{context} os"))
}

fn parse_tool_refs(value: Option<&Value>, context: &str) -> Result<Vec<ToolRef>, GeneratorError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match value {
        Value::String(name) => Ok(vec![parse_tool_ref_name(name, context)?]),
        Value::Array(values) => values
            .iter()
            .map(|value| parse_tool_ref_value(value, context))
            .collect(),
        _ => Ok(vec![parse_tool_ref_value(value, context)?]),
    }
}

fn parse_tool_ref_value(value: &Value, context: &str) -> Result<ToolRef, GeneratorError> {
    match value {
        Value::String(name) => parse_tool_ref_name(name, context),
        Value::Table(table) => {
            reject_unknown_keys(
                table,
                &["tool", "key", "version", "backend", "os", "options"],
                context,
            )?;
            let key = table
                .get("tool")
                .or_else(|| table.get("key"))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    GeneratorError::usage(format!("{context} dependency table needs string tool"))
                })?;
            Ok(ToolRef {
                key: valid_name(key, context)?,
                selector: table
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                backend: table
                    .get("backend")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                options: tool_options(table, context)?,
                os: parse_os(table.get("os"), context)?,
            })
        }
        _ => Err(GeneratorError::usage(format!(
            "{context} dependency must be a string or table"
        ))),
    }
}

fn parse_tool_ref_name(name: &str, context: &str) -> Result<ToolRef, GeneratorError> {
    if name.is_empty() || name.chars().any(char::is_whitespace) {
        return Err(GeneratorError::usage(format!(
            "{context} dependency {name} is not a typed tool reference"
        )));
    }
    Ok(ToolRef {
        key: name
            .rsplit_once('@')
            .filter(|(key, selector)| !key.is_empty() && !selector.is_empty())
            .map_or_else(|| name.to_owned(), |(key, _)| key.to_owned()),
        selector: name
            .rsplit_once('@')
            .filter(|(key, selector)| !key.is_empty() && !selector.is_empty())
            .map(|(_, selector)| selector.to_owned()),
        backend: None,
        options: BTreeMap::new(),
        os: Vec::new(),
    })
}

fn parse_tasks(
    value: Option<&Value>,
    path: &Path,
) -> Result<BTreeMap<String, TaskSpec>, GeneratorError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let table = value.as_table().ok_or_else(|| {
        GeneratorError::usage(format!(
            "parse mise.toml {}: tasks must be a table",
            path.display()
        ))
    })?;
    let mut tasks = BTreeMap::new();
    for (name, value) in table {
        let spec = match value {
            Value::Table(table) => parse_task_spec(table, name)?,
            Value::String(_) => TaskSpec::default(),
            _ => {
                return Err(GeneratorError::usage(format!(
                    "parse mise.toml {}: task {name} must be a table or command string",
                    path.display()
                )));
            }
        };
        tasks.insert(name.clone(), spec);
    }
    Ok(tasks)
}

fn parse_task_spec(table: &Table, name: &str) -> Result<TaskSpec, GeneratorError> {
    let context = format!("mise.toml task {name}");
    for key in ["extends", "include", "file", "template"] {
        if table.contains_key(key) {
            return Err(GeneratorError::usage(format!(
                "{context} uses unsupported inherited task shape `{key}`; declare typed edges in this mise.toml"
            )));
        }
    }
    Ok(TaskSpec {
        tools: parse_tools(table.get("tools"), &context)?,
        depends: parse_task_refs(table.get("depends"), &context)?,
        depends_post: parse_task_refs(table.get("depends_post"), &context)?,
        wait_for: parse_task_refs(table.get("wait_for"), &context)?,
        run_tasks: parse_run_tasks(table.get("run"), &context)?,
        aliases: parse_aliases(table.get("alias"), &context)?,
    })
}

fn reject_unsupported_manifest_shape(table: &Table, path: &Path) -> Result<(), GeneratorError> {
    for key in ["task_config", "task_templates", "includes", "extends"] {
        if table.contains_key(key) {
            return Err(GeneratorError::usage(format!(
                "parse mise.toml {}: unsupported config overlay `{key}`; use a paired flat mise.toml/mise.lock scope",
                path.display()
            )));
        }
    }
    Ok(())
}

fn parse_task_refs(value: Option<&Value>, context: &str) -> Result<Vec<TaskRef>, GeneratorError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match value {
        Value::String(name) => Ok(vec![parse_task_ref_name(name, context)?]),
        Value::Array(values) => values
            .iter()
            .map(|value| parse_task_ref_value(value, context))
            .collect(),
        Value::Table(_) => Ok(vec![parse_task_ref_value(value, context)?]),
        _ => Err(GeneratorError::usage(format!(
            "{context} task references must be strings, arrays, or tables"
        ))),
    }
}

fn parse_task_ref_value(value: &Value, context: &str) -> Result<TaskRef, GeneratorError> {
    match value {
        Value::String(name) => parse_task_ref_name(name, context),
        Value::Table(table) => {
            reject_unknown_keys(table, &["task", "optional", "args", "env"], context)?;
            let name = table.get("task").and_then(Value::as_str).ok_or_else(|| {
                GeneratorError::usage(format!(
                    "{context} structured task reference needs string task"
                ))
            })?;
            Ok(TaskRef {
                name: valid_name(name, context)?,
                optional: table
                    .get("optional")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                parallel_group: None,
                args: parse_task_args(table.get("args"), context)?,
                env: parse_task_env(table.get("env"), context)?,
            })
        }
        _ => Err(GeneratorError::usage(format!(
            "{context} task reference must be a name or table"
        ))),
    }
}

fn parse_task_ref_name(name: &str, context: &str) -> Result<TaskRef, GeneratorError> {
    Ok(TaskRef {
        name: valid_name(name, context)?,
        optional: false,
        parallel_group: None,
        args: Vec::new(),
        env: BTreeMap::new(),
    })
}

fn parse_run_tasks(value: Option<&Value>, context: &str) -> Result<Vec<TaskRef>, GeneratorError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values: Vec<&Value> = match value {
        Value::Array(values) => values.iter().collect(),
        // Mise accepts one structured task object without wrapping it in an
        // array. Treating that shape as an opaque shell task would silently
        // drop a typed edge and its task-local tools.
        Value::Table(_) => vec![value],
        Value::String(_) => return Ok(Vec::new()),
        _ => {
            return Err(GeneratorError::usage(format!(
                "{context} structured run must be a table, array, or opaque string"
            )))
        }
    };
    let mut refs = Vec::new();
    for (group_index, value) in values.into_iter().enumerate() {
        let Some(table) = value.as_table() else {
            if !value.is_str() {
                return Err(GeneratorError::usage(format!(
                    "{context} structured run entries must be tables or opaque strings"
                )));
            }
            continue;
        };
        reject_unknown_keys(table, &["task", "tasks", "args", "env"], context)?;
        if !table.contains_key("task") && !table.contains_key("tasks") {
            return Err(GeneratorError::usage(format!(
                "{context} structured run entry needs task or tasks"
            )));
        }
        if table.contains_key("task") && table.contains_key("tasks") {
            return Err(GeneratorError::usage(format!(
                "{context} structured run entry cannot contain both task and tasks"
            )));
        }
        let args = parse_task_args(table.get("args"), context)?;
        let env = parse_task_env(table.get("env"), context)?;
        if let Some(task) = table.get("task") {
            let mut reference = parse_task_ref_value(task, context)?;
            if !args.is_empty() {
                if !reference.args.is_empty() {
                    return Err(GeneratorError::usage(format!(
                        "{context} run task reference declares args twice"
                    )));
                }
                reference.args.clone_from(&args);
            }
            if !env.is_empty() {
                if !reference.env.is_empty() {
                    return Err(GeneratorError::usage(format!(
                        "{context} run task reference declares env twice"
                    )));
                }
                reference.env = env.clone();
            }
            refs.push(reference);
        }
        if let Some(tasks) = table.get("tasks") {
            let mut nested = parse_task_refs(Some(tasks), context)?;
            for reference in &mut nested {
                reference.parallel_group = Some(group_index);
                if !args.is_empty() {
                    if !reference.args.is_empty() {
                        return Err(GeneratorError::usage(format!(
                            "{context} run task reference declares args twice"
                        )));
                    }
                    reference.args.clone_from(&args);
                }
                if !env.is_empty() {
                    if !reference.env.is_empty() {
                        return Err(GeneratorError::usage(format!(
                            "{context} run task reference declares env twice"
                        )));
                    }
                    reference.env.clone_from(&env);
                }
            }
            refs.extend(nested);
        }
    }
    Ok(refs)
}

fn parse_task_args(value: Option<&Value>, context: &str) -> Result<Vec<String>, GeneratorError> {
    string_list(value, &format!("{context} task args"))
}

fn parse_task_env(
    value: Option<&Value>,
    context: &str,
) -> Result<BTreeMap<String, String>, GeneratorError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    string_map(value, &format!("{context} task env"))
}

fn reject_unknown_keys(
    table: &Table,
    allowed: &[&str],
    context: &str,
) -> Result<(), GeneratorError> {
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(GeneratorError::usage(format!(
            "{context} structured declaration contains unsupported key {key}"
        )));
    }
    Ok(())
}

fn parse_aliases(value: Option<&Value>, context: &str) -> Result<Vec<String>, GeneratorError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = match value {
        Value::String(_) => vec![value],
        Value::Array(values) => values.iter().collect(),
        _ => {
            return Err(GeneratorError::usage(format!(
                "{context} alias must be a string or array"
            )));
        }
    };
    values
        .iter()
        .map(|value| {
            let alias = value.as_str().ok_or_else(|| {
                GeneratorError::usage(format!("{context} aliases must be strings"))
            })?;
            valid_name(alias, context)
        })
        .collect()
}

fn valid_name(value: &str, context: &str) -> Result<String, GeneratorError> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(GeneratorError::usage(format!(
            "{context} reference {value} contains whitespace; use a structured declaration"
        )));
    }
    Ok(value.to_owned())
}

fn string_list(value: Option<&Value>, context: &str) -> Result<Vec<String>, GeneratorError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    match value {
        Value::String(value) => Ok(vec![value.clone()]),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    GeneratorError::usage(format!("{context} must contain only strings"))
                })
            })
            .collect(),
        _ => Err(GeneratorError::usage(format!(
            "{context} must be a string or array of strings"
        ))),
    }
}

fn add_idiomatic_rust_tool(
    root: &Path,
    document: &Table,
    tools: &mut BTreeMap<String, ToolSpec>,
) -> Result<(), GeneratorError> {
    if tools.contains_key("rust") {
        return Ok(());
    }
    let enabled = document
        .get("settings")
        .and_then(Value::as_table)
        .and_then(|settings| settings.get("idiomatic_version_file_enable_tools"))
        .and_then(Value::as_array)
        .is_some_and(|values| values.iter().any(|value| value.as_str() == Some("rust")));
    if !enabled {
        return Ok(());
    }
    let toml_path = root.join("rust-toolchain.toml");
    let (selector, options) = match fs::read_to_string(&toml_path) {
        Ok(text) => {
            let value = toml::from_str::<Value>(&text).map_err(|error| {
                GeneratorError::usage(format!(
                    "parse rust-toolchain.toml {}: {error}",
                    toml_path.display()
                ))
            })?;
            let Some(toolchain) = value.get("toolchain") else {
                return Ok(());
            };
            if let Some(selector) = toolchain.as_str() {
                (selector.to_owned(), BTreeMap::new())
            } else if let Some(table) = toolchain.as_table() {
                let selector = table
                    .get("channel")
                    .and_then(Value::as_str)
                    .filter(|selector| !selector.is_empty())
                    .ok_or_else(|| {
                        GeneratorError::usage(format!(
                            "rust-toolchain.toml {} toolchain.channel must be a nonempty string",
                            toml_path.display()
                        ))
                    })?;
                let options = table
                    .iter()
                    .filter(|(key, _)| key.as_str() != "channel")
                    .map(|(key, value)| (key.clone(), option_value(value)))
                    .collect();
                (selector.to_owned(), options)
            } else {
                return Err(GeneratorError::usage(format!(
                    "rust-toolchain.toml {} toolchain must be a string or table",
                    toml_path.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let path = root.join("rust-toolchain");
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(GeneratorError::io("read rust-toolchain", &path, &error)),
            };
            let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
            let selector = lines.next().ok_or_else(|| {
                GeneratorError::usage(format!(
                    "rust-toolchain {} must contain one nonempty channel",
                    path.display()
                ))
            })?;
            if lines.next().is_some() || selector.chars().any(char::is_whitespace) {
                return Err(GeneratorError::usage(format!(
                    "rust-toolchain {} must contain exactly one channel",
                    path.display()
                )));
            }
            (selector.to_owned(), BTreeMap::new())
        }
        Err(error) => {
            return Err(GeneratorError::io(
                "read rust-toolchain.toml",
                &toml_path,
                &error,
            ))
        }
    };
    tools.insert(
        "rust".to_owned(),
        ToolSpec {
            selector,
            backend: None,
            depends: Vec::new(),
            options,
            os: Vec::new(),
        },
    );
    Ok(())
}

fn rustup_boundary_error<'a, I>(source: &str, options: I) -> GeneratorError
where
    I: IntoIterator<Item = &'a str>,
{
    let options = options.into_iter().collect::<Vec<_>>().join(", ");
    GeneratorError::usage(format!(
        "{source} resolves Rust options ({options}), but check-profile Mise closure cannot provision Rust components, targets, or profile; use the typed RustToolchain/rustup provisioning path before enabling this requirement"
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[expect(clippy::panic, reason = "fixture setup failures name their cause")]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    fn fixture(mise: &str, lock: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("velnor-mise-closure-{}", crate::unique_suffix()));
        must(fs::create_dir_all(&root), "create fixture");
        must(fs::write(root.join("mise.toml"), mise), "write mise");
        must(fs::write(root.join("mise.lock"), lock), "write lock");
        root
    }

    fn remove_fixture(root: &Path) {
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_typed_dependencies_and_scoped_tools_without_shell_parsing() {
        let root = fixture(
            r#"
[tools]
node = "22.14.0"
rust = "1.87.0"

[tasks.leaf]
tools = { "cargo:boltffi_cli" = "0.30.1" }

[tasks."desktop-test"]
depends = [{ task = "leaf" }]
depends_post = ["cleanup"]
wait_for = [{ task = "external", optional = true }]
run = [{ task = "leaf" }]

[tasks.cleanup]
tools = { rust = "1.87.0" }
"#,
            r#"
[tools]
node = [{ version = "22.14.0" }]
rust = [{ version = "1.87.0" }]
"cargo:boltffi_cli" = [{ version = "0.30.1" }]
"#,
        );
        let closure = must(
            resolve_profile(
                &root,
                &["desktop-test".to_owned()],
                &[
                    "node".to_owned(),
                    "rust".to_owned(),
                    "cargo:boltffi_cli".to_owned(),
                ],
            ),
            "resolve fixture",
        );
        assert_eq!(
            closure.tasks,
            vec![
                "leaf".to_owned(),
                "desktop-test".to_owned(),
                "cleanup".to_owned()
            ]
        );
        assert!(closure.tools.iter().any(|tool| {
            tool.key == "cargo:boltffi_cli"
                && tool.selector == "0.30.1"
                && tool.locked_version == "0.30.1"
                && tool.scope == ToolScope::Task("leaf".to_owned())
        }));
        assert!(closure.tools.iter().any(|tool| {
            tool.key == "rust" && tool.scope == ToolScope::Task("cleanup".to_owned())
        }));
        remove_fixture(&root);
    }

    #[test]
    fn rejects_unlocked_transitive_tool() {
        let root = fixture(
            "[tools]\nnode = \"22\"\n\n[tasks.check]\ntools = { bun = \"1\" }\n",
            "[tools]\nnode = [{ version = \"22\" }]\n",
        );
        let error = must_fail(
            resolve_profile(&root, &["check".to_owned()], &["node".to_owned()]),
            "missing lock must fail",
        );
        assert!(error.to_string().contains("bun"));
        assert!(error.to_string().contains("mise.lock"));
        remove_fixture(&root);
    }

    #[expect(clippy::panic, reason = "test must fail when no error is returned")]
    fn must_fail<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> E {
        match result {
            Ok(_) => panic!("{context}"),
            Err(error) => error,
        }
    }

    #[test]
    fn rejects_task_local_selector_until_exact_installer_contract_exists() {
        let root = fixture(
            "[tools]\nnode = \"22\"\n\n[tasks.check]\ntools = { node = \"20\" }\n",
            "[tools]\nnode = [{ version = \"22\" }, { version = \"20\" }]\n",
        );
        let closure = must(
            resolve_profile(&root, &["check".to_owned()], &["node".to_owned()]),
            "resolve scoped selector",
        );
        must(
            closure.validate_profile_tools("check", &["node".to_owned()]),
            "root selector remains valid",
        );
        let error = must_fail(
            closure
                .installation_plan("check")
                .and_then(|plan| plan.mise_args("check")),
            "task-local selector must not silently use root install args",
        );
        assert!(error
            .to_string()
            .contains("current installer accepts only bare root lock keys"));
        remove_fixture(&root);
    }

    #[test]
    fn emitted_install_args_are_bare_committed_lock_keys() {
        let root = fixture(
            "[tools]\nnode = \"22\"\n\n[tasks.check]\ntools = { node = \"22\" }\n",
            "[tools]\nnode = [{ version = \"22\" }]\n",
        );
        let closure = must(
            resolve_profile(&root, &["check".to_owned()], &["node".to_owned()]),
            "resolve matching root identity",
        );
        let args = must(
            closure
                .installation_plan("check")
                .and_then(|plan| plan.mise_args("check")),
            "render bare install args",
        );
        assert_eq!(args, vec!["node".to_owned()]);
        must(
            velnor_runner::validate_install_args_against_lock(
                &args.join(" "),
                "[tools]\nnode = [{ version = \"22\" }]\n",
            ),
            "runner accepts emitted bare lock key",
        );
        remove_fixture(&root);
    }

    #[test]
    fn preserves_wait_for_as_validation_only() {
        let root = fixture(
            "[tools]\nnode = \"22\"\n\n[tasks.check]\nwait_for = [\"other\"]\n\n[tasks.other]\nrun = \"echo other\"\n",
            "[tools]\nnode = [{ version = \"22\" }]\n",
        );
        let closure = must(
            resolve_profile(&root, &["check".to_owned()], &["node".to_owned()]),
            "resolve wait_for",
        );
        assert_eq!(closure.tasks, vec!["check".to_owned()]);
        remove_fixture(&root);
    }

    #[test]
    fn resolves_versioned_tool_dependency_without_shell_splitting() {
        let root = fixture(
            "[tools]\nnode = { version = \"22\", depends = [\"python@3.14\"] }\npython = \"3.14\"\n\n[tasks.check]\nrun = \"node --version\"\n",
            "[tools]\nnode = [{ version = \"22\" }]\npython = [{ version = \"3.14\" }]\n",
        );
        let closure = must(
            resolve_profile(&root, &["check".to_owned()], &["node".to_owned()]),
            "resolve versioned dependency",
        );
        assert!(closure.tools.iter().any(|tool| {
            tool.key == "python" && tool.selector == "3.14" && tool.scope == ToolScope::Root
        }));
        remove_fixture(&root);
    }

    #[test]
    fn preserves_structured_task_arguments_and_environment() {
        let root = fixture(
            "[tasks.check]\ndepends = [{ task = \"leaf\", args = [\"release\"], env = { MODE = \"ci\" } }]\n\n[tasks.leaf]\nrun = \"echo leaf\"\n",
            "[tools]\n",
        );
        let closure = must(
            resolve_profile(&root, &["check".to_owned()], &[]),
            "structured task references",
        );
        assert!(closure.invocations.iter().any(|invocation| {
            invocation.task == "leaf"
                && invocation.phase == TaskPhase::Before
                && invocation.args == ["release"]
                && invocation.env.get("MODE").map(String::as_str) == Some("ci")
        }));
        remove_fixture(&root);
    }

    #[test]
    fn resolves_single_structured_run_task_and_its_tools() {
        let root = fixture(
            "[tools]\nnode = \"22\"\n\n[tasks.parent]\nrun = { task = \"leaf\", args = [\"ci\"], env = { MODE = \"test\" } }\n\n[tasks.leaf]\ntools = { bun = \"1.2\" }\n",
            "[tools]\nnode = [{ version = \"22\" }]\nbun = [{ version = \"1.2\" }]\n",
        );
        let closure = must(
            resolve_profile(&root, &["parent".to_owned()], &["node".to_owned()]),
            "single structured run task",
        );
        assert!(closure.invocations.iter().any(|invocation| {
            invocation.task == "leaf"
                && invocation.phase == TaskPhase::Run
                && invocation.args == ["ci"]
                && invocation.env.get("MODE").map(String::as_str) == Some("test")
        }));
        assert!(closure
            .tools
            .iter()
            .any(|tool| { tool.key == "bun" && tool.scope == ToolScope::Task("leaf".to_owned()) }));
        remove_fixture(&root);
    }

    #[test]
    fn leaves_opaque_shell_tools_out_of_the_typed_closure() {
        let root = fixture(
            "[tools]\nnode = \"22\"\n\n[tasks.check]\nrun = \"node --version\"\n",
            "[tools]\nnode = [{ version = \"22\" }]\n",
        );
        let closure = must(
            resolve_profile(&root, &["check".to_owned()], &[]),
            "opaque shell command must not be parsed",
        );
        assert!(closure.tools.is_empty());
        must(
            closure.validate_profile_tools("check", &[]),
            "opaque shell command has no inferred tools",
        );
        remove_fixture(&root);
    }

    #[test]
    fn keeps_before_and_after_invocations_distinct() {
        let root = fixture(
            "[tasks.parent]\ndepends = [\"leaf\"]\ndepends_post = [\"leaf\"]\n\n[tasks.leaf]\nrun = \"echo leaf\"\n",
            "[tools]\n",
        );
        let closure = must(
            resolve_profile(&root, &["parent".to_owned()], &[]),
            "resolve before and after",
        );
        assert_eq!(
            closure
                .invocations
                .iter()
                .filter(|invocation| invocation.task == "leaf")
                .map(|invocation| invocation.phase)
                .collect::<Vec<_>>(),
            vec![TaskPhase::Before, TaskPhase::After]
        );
        assert_eq!(closure.tasks, vec!["leaf".to_owned(), "parent".to_owned()]);
        remove_fixture(&root);
    }

    #[test]
    fn expands_optional_wildcard_task_references() {
        let root = fixture(
            "[tasks.parent]\ndepends = [{ task = \"group:*\", optional = true }]\n\n[tasks.\"group:a\"]\nrun = \"echo a\"\n\n[tasks.\"group:b\"]\nrun = \"echo b\"\n",
            "[tools]\n",
        );
        let closure = must(
            resolve_profile(&root, &["parent".to_owned()], &[]),
            "resolve wildcard",
        );
        assert_eq!(
            closure.tasks,
            vec![
                "group:a".to_owned(),
                "group:b".to_owned(),
                "parent".to_owned()
            ]
        );
        remove_fixture(&root);
    }

    #[test]
    fn concrete_task_name_wins_alias_collision() {
        let root = fixture(
            "[tasks.alias-source]\nalias = [\"shared\"]\n\n[tasks.shared]\nrun = \"echo shared\"\n",
            "[tools]\n",
        );
        let closure = must(
            resolve_profile(&root, &["shared".to_owned()], &[]),
            "resolve concrete task",
        );
        assert_eq!(closure.tasks, vec!["shared".to_owned()]);
        remove_fixture(&root);
    }

    #[test]
    fn binds_lock_backend_options_and_platform_artifact() {
        let root = fixture(
            "[tools]\nfoo = { version = \"1.0.0\", backend = \"aqua:example/foo\", options = { flavor = \"full\" } }\n",
            "[[tools.foo]]\nversion = \"1.0.0\"\nbackend = \"aqua:example/foo\"\n[tools.foo.options]\nflavor = \"full\"\n[tools.foo.\"platforms.macos-arm64\"]\nurl = \"https://example.invalid/foo\"\nchecksum = \"sha256:abc\"\n",
        );
        let closure = must(
            resolve_profile_for_platform(&root, &[], &["foo".to_owned()], "macos-arm64"),
            "resolve locked artifact",
        );
        assert_eq!(
            closure.tools[0].url.as_deref(),
            Some("https://example.invalid/foo")
        );
        assert_eq!(closure.tools[0].checksum.as_deref(), Some("sha256:abc"));
        remove_fixture(&root);
    }

    #[test]
    fn rejects_lock_platform_or_backend_mismatch() {
        let root = fixture(
            "[tools]\nfoo = { version = \"1.0.0\", backend = \"aqua:example/foo\" }\n",
            "[[tools.foo]]\nversion = \"1.0.0\"\n[tools.foo.\"platforms.macos-arm64\"]\nurl = \"https://example.invalid/foo\"\nchecksum = \"sha256:abc\"\n",
        );
        let error = must_fail(
            resolve_profile_for_platform(&root, &[], &["foo".to_owned()], "macos-arm64"),
            "missing backend must fail",
        );
        assert!(error.to_string().contains("backend/options"));
        remove_fixture(&root);

        let root = fixture(
            "[tools]\nfoo = { version = \"1.0.0\", backend = \"aqua:example/foo\" }\n",
            "[[tools.foo]]\nversion = \"1.0.0\"\nbackend = \"aqua:example/foo\"\n",
        );
        let error = must_fail(
            resolve_profile_for_platform(&root, &[], &["foo".to_owned()], "macos-arm64"),
            "missing platform must fail",
        );
        assert!(error.to_string().contains("platform lock record"));
        remove_fixture(&root);

        let root = fixture(
            "[tools]\nfoo = { version = \"1.0.0\", backend = \"aqua:example/foo\", options = { flavor = \"full\" } }\n",
            "[[tools.foo]]\nversion = \"1.0.0\"\nbackend = \"aqua:example/foo\"\n[tools.foo.options]\nflavor = \"minimal\"\n[tools.foo.\"platforms.macos-arm64\"]\nurl = \"https://example.invalid/foo\"\nchecksum = \"sha256:abc\"\n",
        );
        let error = must_fail(
            resolve_profile_for_platform(&root, &[], &["foo".to_owned()], "macos-arm64"),
            "option mismatch must fail",
        );
        assert!(error.to_string().contains("backend/options"));
        remove_fixture(&root);
    }

    #[test]
    fn rejects_task_local_backend_without_root_installer_identity() {
        let root = fixture(
            "[tasks.check]\ntools = { foo = { version = \"1.0.0\", backend = \"core:foo\" } }\n",
            "[[tools.foo]]\nversion = \"1.0.0\"\nbackend = \"core:foo\"\n",
        );
        let closure = must(
            resolve_profile(&root, &["check".to_owned()], &[]),
            "resolve task-local backend",
        );
        let error = must_fail(
            closure
                .installation_plan("check")
                .and_then(|plan| plan.mise_args("check")),
            "task-local backend must not bypass root installer identity",
        );
        assert!(error
            .to_string()
            .contains("current installer accepts only bare root lock keys"));
        remove_fixture(&root);
    }

    #[test]
    fn rejects_malformed_lock_identity_fields() {
        let cases = [
            (
                "[[tools.foo]]\nversion = \"\"\n",
                "empty locked version",
            ),
            (
                "[[tools.foo]]\nversion = \"1.0.0\"\nrequested = 1\n",
                "requested selector must be a string",
            ),
            (
                "[[tools.foo]]\nversion = \"1.0.0\"\nbackend = 1\n",
                "backend must be a string",
            ),
            (
                "[[tools.foo]]\nversion = \"1.0.0\"\nbackend = \"aqua:foo\"\n[tools.foo.\"platforms.linux-x64\"]\nurl = \"\"\nchecksum = \"sha256:abc\"\n",
                "empty URL",
            ),
            (
                "[[tools.foo]]\nversion = \"1.0.0\"\nbackend = \"aqua:foo\"\n[tools.foo.\"platforms.linux-x64\"]\nurl = \"https://example.invalid/foo\"\nchecksum = \"\"\n",
                "empty checksum",
            ),
        ];
        for (lock, expected) in cases {
            let root = fixture("[tools]\nfoo = \"1.0.0\"\n", lock);
            let error = must_fail(
                resolve_profile_for_platform(&root, &[], &["foo".to_owned()], "linux-x64"),
                "malformed lock must fail closed",
            );
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?} in {error}"
            );
            remove_fixture(&root);
        }
    }

    #[test]
    fn rejects_unknown_direct_platform() {
        let root = fixture(
            "[tools]\nnode = \"22\"\n",
            "[tools]\nnode = [{ version = \"22\" }]\n",
        );
        let error = must_fail(
            resolve_profile_for_platform(&root, &[], &["node".to_owned()], "linux-riscv64"),
            "unknown platform must fail",
        );
        assert!(error
            .to_string()
            .contains("unknown Mise execution platform"));
        remove_fixture(&root);
    }

    #[test]
    fn runner_platform_mapping_is_host_independent() {
        assert!(platform_for_runner("macos", None).is_err());
        assert_eq!(
            must(
                platform_for_runner("custom-macos-large", Some("macos-x64")),
                "explicit Intel platform"
            ),
            "macos-x64"
        );
        assert_eq!(
            must(
                platform_for_runner("macos-large", Some("macos-arm64")),
                "explicit Apple ARM platform for Intel-looking label"
            ),
            "macos-arm64"
        );
        assert_eq!(
            must(
                platform_for_runner("custom-macos-large", Some("macos-arm64")),
                "explicit Apple ARM platform"
            ),
            "macos-arm64"
        );
        assert_eq!(
            must(
                platform_for_runner("custom-windows-arm", Some("windows-arm64")),
                "explicit Windows ARM platform"
            ),
            "windows-arm64"
        );
        assert!(platform_for_runner("custom-self-hosted", None).is_err());
        assert!(platform_for_runner("macos-large", None).is_err());
        assert!(platform_for_runner("windows-2025-arm", None).is_err());
        assert!(platform_for_runner("github", Some("windows-arm64")).is_ok());
        assert_eq!(
            must(platform_for_s2_runner("macos", None), "fixed S2 Apple lane"),
            "macos-arm64"
        );
        assert!(platform_for_s2_runner("macos", Some("windows-arm64")).is_err());
    }

    #[test]
    fn known_runner_labels_bind_lock_platform_and_unknown_labels_need_facts() {
        assert_eq!(
            must(
                platform_for_runner_labels("github", None, ["ubuntu-24.04"].into_iter(),),
                "hosted Linux platform"
            ),
            "linux-x64"
        );
        assert_eq!(
            must(
                platform_for_runner_labels("macos", None, ["macos-15"].into_iter()),
                "hosted Apple platform"
            ),
            "macos-arm64"
        );
        assert_eq!(
            must(
                platform_for_runner_labels(
                    "macos",
                    Some("macos-x64"),
                    ["macos-15-intel"].into_iter(),
                ),
                "explicit Intel platform"
            ),
            "macos-x64"
        );
        assert!(platform_for_runner_labels("velnor", None, ["custom-host"].into_iter()).is_err());
        assert!(
            platform_for_runner_labels("macos", Some("macos-x64"), ["macos-15"].into_iter())
                .is_err()
        );
        assert!(platform_for_runner_labels(
            "github",
            Some("windows-arm64"),
            ["ubuntu-24.04"].into_iter()
        )
        .is_err());
    }

    #[test]
    fn rejects_rust_toolchain_options_until_typed_rustup_provisioning() {
        let root = fixture(
            "[settings]\nidiomatic_version_file_enable_tools = [\"rust\"]\n",
            "[[tools.rust]]\nversion = \"1.90.0\"\nbackend = \"core:rust\"\n[tools.rust.options]\ncomponents = [\"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n",
        );
        must(fs::write(
            root.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.90.0\"\ncomponents = [\"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n",
        ), "write rust toolchain toml");
        let error = must_fail(
            resolve_profile_for_platform(&root, &[], &["rust".to_owned()], "linux-x64"),
            "reject Mise Rust options",
        );
        assert!(error.to_string().contains("typed RustToolchain/rustup"));
        remove_fixture(&root);
    }

    #[test]
    fn typed_rust_boundary_preserves_pin_and_excludes_rust_from_mise_install() {
        let root = fixture(
            "[settings]\nidiomatic_version_file_enable_tools = [\"rust\"]\n\n[tasks.check]\ntools = { rust = \"1.90.0\" }\n",
            "[[tools.rust]]\nversion = \"1.90.0\"\nbackend = \"core:rust\"\n[tools.rust.options]\nprofile = \"minimal\"\ncomponents = [\"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.90.0\"\nprofile = \"minimal\"\ncomponents = [\"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n",
            ),
            "write rust toolchain toml",
        );
        let boundary = crate::RustToolchain {
            channel: "1.90.0".to_owned(),
            components: vec!["rustfmt".to_owned()],
            targets: vec!["wasm32-unknown-unknown".to_owned()],
            profile: Some("minimal".to_owned()),
        };
        let closure = must(
            resolve_profile_for_platform_with_rust(
                &root,
                &["check".to_owned()],
                &["rust".to_owned()],
                "linux-x64",
                Some(&boundary),
            ),
            "resolve typed Rust boundary",
        );
        assert!(closure.uses_rust_toolchain);
        assert!(closure.tools.iter().all(|tool| tool.key != "rust"));
        let args = must(
            closure
                .installation_plan("check")
                .and_then(|plan| plan.mise_args("check")),
            "render non-Rust Mise args",
        );
        assert!(args.is_empty());
        must(
            closure.validate_profile_tools("check", &["rust".to_owned()]),
            "typed Rust remains a valid explicit profile tool",
        );
        remove_fixture(&root);
    }

    #[test]
    fn typed_rust_boundary_rejects_task_selector_drift() {
        let root = fixture(
            "[settings]\nidiomatic_version_file_enable_tools = [\"rust\"]\n\n[tasks.check]\ntools = { rust = \"1.89.0\" }\n",
            "[[tools.rust]]\nversion = \"1.90.0\"\nbackend = \"core:rust\"\n[tools.rust.options]\ncomponents = [\"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n[[tools.rust]]\nversion = \"1.89.0\"\nbackend = \"core:rust\"\n[tools.rust.options]\ncomponents = [\"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.90.0\"\ncomponents = [\"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n",
            ),
            "write rust toolchain toml",
        );
        let boundary = crate::RustToolchain {
            channel: "1.90.0".to_owned(),
            components: vec!["rustfmt".to_owned()],
            targets: vec!["wasm32-unknown-unknown".to_owned()],
            profile: None,
        };
        let error = must_fail(
            resolve_profile_for_platform_with_rust(
                &root,
                &["check".to_owned()],
                &["rust".to_owned()],
                "linux-x64",
                Some(&boundary),
            ),
            "task-local Rust selector drift must fail",
        );
        assert!(error.to_string().contains("rust-toolchain pins 1.90.0"));
        remove_fixture(&root);
    }

    #[test]
    fn typed_rust_boundary_rejects_lock_version_drift() {
        let root = fixture(
            "[settings]\nidiomatic_version_file_enable_tools = [\"rust\"]\n\n[tasks.check]\ntools = { rust = \"1.90.0\" }\n",
            "[[tools.rust]]\nversion = \"1.90.1\"\nrequested = \"1.90.0\"\nbackend = \"core:rust\"\n",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.90.0\"\n",
            ),
            "write rust toolchain toml",
        );
        let boundary = crate::RustToolchain {
            channel: "1.90.0".to_owned(),
            components: Vec::new(),
            targets: Vec::new(),
            profile: None,
        };
        let error = must_fail(
            resolve_profile_for_platform_with_rust(
                &root,
                &["check".to_owned()],
                &[],
                "linux-x64",
                Some(&boundary),
            ),
            "typed Rust lock version drift must fail",
        );
        assert!(error.to_string().contains("locked version 1.90.1"));
        remove_fixture(&root);
    }

    #[test]
    fn typed_rust_boundary_requires_core_rust_lock_backend() {
        let root = fixture(
            "[settings]\nidiomatic_version_file_enable_tools = [\"rust\"]\n",
            "[[tools.rust]]\nversion = \"1.90.0\"\nbackend = \"aqua:rust\"\n",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.90.0\"\n",
            ),
            "write rust toolchain toml",
        );
        let boundary = crate::RustToolchain {
            channel: "1.90.0".to_owned(),
            components: Vec::new(),
            targets: Vec::new(),
            profile: None,
        };
        let error = must_fail(
            resolve_profile_for_platform_with_rust(
                &root,
                &[],
                &["rust".to_owned()],
                "linux-x64",
                Some(&boundary),
            ),
            "typed Rust must reject a non-core lock backend",
        );
        assert!(error.to_string().contains("backend/options"), "{error}");
        remove_fixture(&root);
    }

    #[test]
    fn typed_rust_boundary_is_stage_scoped() {
        let root = fixture(
            "[settings]\nidiomatic_version_file_enable_tools = [\"rust\"]\n[tools]\nnode = \"22.0.0\"\n\n[tasks.docs]\nrun = \"node --version\"\n",
            "[tools]\nnode = [{ version = \"22.0.0\" }]\n[[tools.rust]]\nversion = \"1.90.0\"\nbackend = \"core:rust\"\n[tools.rust.options]\ncomponents = [\"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n",
        );
        must(
            fs::write(
                root.join("rust-toolchain.toml"),
                "[toolchain]\nchannel = \"1.90.0\"\ncomponents = [\"rustfmt\"]\ntargets = [\"wasm32-unknown-unknown\"]\n",
            ),
            "write rust toolchain toml",
        );
        let boundary = crate::RustToolchain {
            channel: "1.90.0".to_owned(),
            components: vec!["rustfmt".to_owned()],
            targets: vec!["wasm32-unknown-unknown".to_owned()],
            profile: None,
        };
        let closure = must(
            resolve_profile_for_platform_with_rust(
                &root,
                &["docs".to_owned()],
                &["node".to_owned()],
                "linux-x64",
                Some(&boundary),
            ),
            "resolve non-Rust stage",
        );
        assert!(!closure.uses_rust_toolchain);
        let args = must(
            closure
                .installation_plan("docs")
                .and_then(|plan| plan.mise_args("docs")),
            "render non-Rust stage Mise args",
        );
        assert_eq!(args, ["node"]);
        remove_fixture(&root);
    }

    #[test]
    fn reads_plain_rust_toolchain_channel() {
        let root = fixture(
            "[settings]\nidiomatic_version_file_enable_tools = [\"rust\"]\n",
            "[[tools.rust]]\nversion = \"1.90.0\"\nbackend = \"core:rust\"\n",
        );
        must(
            fs::write(root.join("rust-toolchain"), "1.90.0\n"),
            "write plain rust toolchain",
        );
        let closure = must(
            resolve_profile_for_platform(&root, &[], &["rust".to_owned()], "linux-x64"),
            "resolve plain rust toolchain",
        );
        assert_eq!(closure.tools[0].locked_version, "1.90.0");
        let args = must(
            closure
                .installation_plan("profile")
                .and_then(|plan| plan.mise_args("profile")),
            "plain Mise Rust remains an install argument",
        );
        assert_eq!(args, ["rust"]);
        remove_fixture(&root);
    }
}
