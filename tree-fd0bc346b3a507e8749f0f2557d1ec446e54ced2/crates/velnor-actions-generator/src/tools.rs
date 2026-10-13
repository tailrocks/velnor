//! Generator-owned mise tool registry and consumer drift checks.
//!
//! The registry is the policy source. Consumer `mise.toml` files may use either
//! a mise alias (`actionlint`) or the registry's backend-qualified key; both
//! resolve to one canonical name, source, and pinned version before comparison.
//! Rust is deliberately not represented here: the root uses mise to install
//! the version declared by `rust-toolchain.toml`, while consumer rust pins are
//! rejected by this registry linter.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::Value;

const SCHEMA: u32 = 1;

const REQUIRED_MISE_VERBS: [&str; 6] = ["fmt", "fmt-fix", "lint", "test", "check", "ci"];

/// Tools required by the generator-owned fleet tasks.
pub const FLEET_TASK_TOOLS: [&str; 3] = ["repolint", "alint", "zizmor"];

const LEGACY_NEXTEST_KEY: &str = "github:nextest-rs/nextest";
const LEGACY_NEXTEST_VERSION: &str = "cargo-nextest-0.9.140";
const CANONICAL_NEXTEST_KEY: &str = "aqua:nextest-rs/nextest/cargo-nextest";
const CANONICAL_NEXTEST_VERSION: &str = "0.9.140";
const LEGACY_CARGO_AUDIT_KEY: &str = "github:rustsec/rustsec";
const LEGACY_CARGO_AUDIT_VERSION: &str = "cargo-audit/v0.22.2";
const CANONICAL_CARGO_AUDIT_KEY: &str = "aqua:rustsec/rustsec/cargo-audit";
const CANONICAL_CARGO_AUDIT_VERSION: &str = "0.22.2";

/// One generator-owned tool policy entry.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// Exact non-floating version consumed by mise.
    pub version: String,
    /// Canonical mise source. A bare value uses mise's native tool alias.
    pub source: String,
    /// Resolved backend recorded in mise.lock when it differs from `source`.
    #[serde(default)]
    pub backend: Option<String>,
    /// Human-readable rationale or migration note.
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    schema: u32,
    tools: BTreeMap<String, ToolSpec>,
}

#[derive(Debug, Deserialize)]
struct RustToolchainDocument {
    toolchain: RustToolchain,
}

#[derive(Debug, Deserialize)]
struct RustToolchain {
    channel: String,
}

/// One normalized tool declaration from a consumer mise file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveTool {
    /// Original key in the consumer file.
    pub key: String,
    /// Canonical registry name.
    pub name: String,
    /// Canonical registry source or native mise alias.
    pub source: String,
    /// Resolved backend recorded in mise.lock.
    pub backend: String,
    /// Exact pinned version.
    pub version: String,
}

/// Validated generator-owned registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRegistry {
    entries: BTreeMap<String, ToolSpec>,
}

impl ToolRegistry {
    /// Load and validate a registry file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = fs::read_to_string(path)
            .map_err(|error| format!("reading registry {}: {error}", path.display()))?;
        let document: RegistryDocument = toml::from_str(&bytes)
            .map_err(|error| format!("parsing registry {}: {error}", path.display()))?;
        if document.schema != SCHEMA {
            return Err(format!(
                "{}: unsupported schema {}, expected {SCHEMA}",
                path.display(),
                document.schema
            ));
        }
        if document.tools.is_empty() {
            return Err(format!("{}: registry has no tools", path.display()));
        }
        for (name, spec) in &document.tools {
            validate_name(name)?;
            if name == "rust" {
                return Err(
                    "rust must be selected by rust-toolchain.toml, not the registry".into(),
                );
            }
            validate_version(name, &spec.version)?;
            validate_source(name, &spec.source)?;
            if let Some(backend) = &spec.backend {
                validate_source(name, backend)?;
            }
        }
        Ok(Self {
            entries: document.tools,
        })
    }

    /// Return the canonical entries in deterministic order.
    #[must_use]
    pub fn entries(&self) -> &BTreeMap<String, ToolSpec> {
        &self.entries
    }

    /// Render a mise `[tools]` block for the selected canonical names.
    ///
    /// Backend-qualified sources become the mise key. Unqualified sources keep
    /// the canonical tool name because those sources are mise's built-in tool
    /// names (for example `python-build-standalone`).
    pub fn render_tools_block<'a, I>(&self, names: I) -> Result<String, String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let names: BTreeSet<&str> = names.into_iter().collect();
        if names.is_empty() {
            return Err("cannot render an empty [tools] block".into());
        }
        let mut output = String::from("[tools]\n");
        for name in names {
            let spec = self
                .entries
                .get(name)
                .ok_or_else(|| format!("tool {name:?} is not in the registry"))?;
            let key = if spec.source.contains(':') {
                spec.source.as_str()
            } else {
                name
            };
            writeln!(output, "{} = {}", toml_key(key), toml_string(&spec.version))
                .expect("writing a String cannot fail");
        }
        Ok(output)
    }

    /// Normalize the `[tools]` section of one consumer file while preserving
    /// all authored settings and tasks outside that section.
    pub fn normalize_mise_file(&self, body: &str) -> Result<String, String> {
        self.normalize_mise_file_with_tools(body, [])
    }

    /// Normalize a consumer file and add the generator-owned tools required by
    /// the supplied task projection. Existing authored tools remain intact.
    pub fn normalize_mise_file_with_tools<'a, I>(
        &self,
        body: &str,
        required: I,
    ) -> Result<String, String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let effective = self.parse_mise(body, Path::new("mise.toml"))?;
        let mut names: BTreeSet<&str> = effective.iter().map(|tool| tool.name.as_str()).collect();
        for name in required {
            if !self.entries.contains_key(name) {
                return Err(format!("tool {name:?} is not in the registry"));
            }
            names.insert(name);
        }
        let block = self.render_tools_block(names.iter().copied())?;
        let mut output = String::new();
        let mut in_tools = false;
        let mut replaced = false;
        for line in body.lines() {
            let trimmed = line.trim();
            if header_name(trimmed) == Some("[tools]") {
                if !replaced {
                    output.push_str(&block);
                    replaced = true;
                }
                in_tools = true;
                continue;
            }
            if in_tools && top_level_header(trimmed) {
                in_tools = false;
            }
            if !in_tools {
                output.push_str(line);
                output.push('\n');
            }
        }
        if !replaced {
            let mut prefixed = block;
            prefixed.push('\n');
            prefixed.push_str(body);
            if !body.ends_with('\n') {
                prefixed.push('\n');
            }
            return Ok(prefixed);
        }
        Ok(output)
    }

    /// Normalize the exact nextest declaration emitted by older consumer
    /// repositories before applying the strict registry check.
    ///
    /// This compatibility path is intentionally render-only: callers must
    /// still pass the returned body through `normalize_mise_file_with_tools`.
    /// No other legacy source or version is accepted.
    pub(crate) fn migrate_legacy_nextest(body: &str) -> Result<(String, bool), String> {
        migrate_legacy_tool(
            body,
            "nextest",
            LEGACY_NEXTEST_KEY,
            LEGACY_NEXTEST_VERSION,
            CANONICAL_NEXTEST_KEY,
            CANONICAL_NEXTEST_VERSION,
        )
    }

    /// Normalize the exact cargo-audit declaration emitted by older consumer
    /// repositories before applying the strict registry check.
    ///
    /// This compatibility path is intentionally render-only: callers must
    /// still pass the returned body through `normalize_mise_file_with_tools`.
    /// No other legacy source or version is accepted.
    pub(crate) fn migrate_legacy_cargo_audit(body: &str) -> Result<(String, bool), String> {
        migrate_legacy_tool(
            body,
            "cargo-audit",
            LEGACY_CARGO_AUDIT_KEY,
            LEGACY_CARGO_AUDIT_VERSION,
            CANONICAL_CARGO_AUDIT_KEY,
            CANONICAL_CARGO_AUDIT_VERSION,
        )
    }

    /// Project selected tool lock entries from a canonical source lock into a
    /// consumer lock. Existing non-selected entries are retained, while any
    /// selected aliases are replaced by the registry's canonical lock key and
    /// source record. TOML serialization gives the projection one deterministic
    /// representation independent of the consumer's authored ordering.
    pub fn project_lock_file<'a, I>(
        &self,
        existing: &str,
        source: &str,
        selected: I,
    ) -> Result<String, String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut document: Value = if existing.trim().is_empty() {
            let mut document = toml::map::Map::new();
            document.insert("lockfile_version".to_owned(), Value::Integer(1));
            Value::Table(document)
        } else {
            toml::from_str(existing)
                .map_err(|error| format!("parsing consumer mise.lock: {error}"))?
        };
        let source_document: Value = toml::from_str(source)
            .map_err(|error| format!("parsing generator mise.lock: {error}"))?;
        let source_tools = source_document
            .get("tools")
            .and_then(Value::as_table)
            .ok_or_else(|| "generator mise.lock is missing [tools] entries".to_owned())?;
        let tools = document
            .as_table_mut()
            .expect("TOML document is a table")
            .entry("tools".to_owned())
            .or_insert_with(|| Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| "consumer mise.lock tools is not a table".to_owned())?;

        let selected: BTreeSet<&str> = selected.into_iter().collect();
        for name in selected {
            let spec = self
                .entries
                .get(name)
                .ok_or_else(|| format!("tool {name:?} is not in the registry"))?;
            let canonical_key = if spec.source.contains(':') {
                spec.source.as_str()
            } else {
                name
            };
            let mut candidates = vec![
                name,
                spec.source.as_str(),
                spec.backend.as_deref().unwrap_or(""),
            ];
            if let Some(legacy_key) = legacy_key_for(name) {
                candidates.push(legacy_key);
            }
            let source_key = candidates
                .iter()
                .find(|candidate| source_tools.contains_key(**candidate))
                .copied()
                .ok_or_else(|| {
                    format!("generator mise.lock has no entry for tool {name:?} ({canonical_key})")
                })?;
            for candidate in candidates {
                if !candidate.is_empty() && candidate != canonical_key {
                    tools.remove(candidate);
                }
            }
            let value = source_tools
                .get(source_key)
                .expect("source lock key found above")
                .clone();
            tools.insert(canonical_key.to_owned(), value);
        }
        let serialized = toml::to_string_pretty(&document)
            .map_err(|error| format!("rendering projected mise.lock: {error}"))?;
        let mut output = String::from(
            "# @generated - this file is auto-generated by `mise lock` https://mise.jdx.dev/dev-tools/mise-lock.html\n\n",
        );
        output.push_str(&serialized);
        if !output.ends_with('\n') {
            output.push('\n');
        }
        Ok(output)
    }

    /// Check one mise file and its lockfile against the registry.
    pub fn check_files(&self, mise: &Path, lock: &Path) -> Result<usize, String> {
        self.check_files_inner(mise, lock)
    }

    /// Check the generator's own mise graph. Rust is derived from the sibling
    /// rust-toolchain.toml; the fleet registry governs the non-Rust tool graph.
    pub fn check_generator_files(&self, mise: &Path, lock: &Path) -> Result<usize, String> {
        check_mise_vocabulary(mise)?;
        self.check_files_inner(mise, lock)
    }

    fn check_files_inner(&self, mise: &Path, lock: &Path) -> Result<usize, String> {
        let mise_body = fs::read_to_string(mise)
            .map_err(|error| format!("reading mise file {}: {error}", mise.display()))?;
        let mut effective = self.parse_mise(&mise_body, mise)?;
        let rust_toolchain = mise
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("rust-toolchain.toml");
        let rust_version = rust_toolchain_version(&rust_toolchain)?;
        effective.push(EffectiveTool {
            key: "rust".to_owned(),
            name: "rust".to_owned(),
            source: "rust-toolchain.toml".to_owned(),
            backend: "core:rust".to_owned(),
            version: rust_version,
        });
        self.check_rendered_equality(&effective, mise)?;
        let lock_body = fs::read_to_string(lock)
            .map_err(|error| format!("reading lockfile {}: {error}", lock.display()))?;
        self.check_lock(&effective, &lock_body, lock, false)?;
        Ok(effective.len())
    }

    /// Check a single in-memory consumer pair. Used by the fixture harness.
    pub fn check_text(&self, mise: &str, lock: &str) -> Result<usize, String> {
        let effective = self.parse_mise(mise, Path::new("mise.toml"))?;
        self.check_rendered_equality(&effective, Path::new("mise.toml"))?;
        // `check_text` intentionally models only the registry-managed portion
        // of a consumer pair. A filesystem-backed check uses rust-toolchain.toml
        // and therefore validates the Rust lock entry as well.
        self.check_lock(&effective, lock, Path::new("mise.lock"), true)?;
        Ok(effective.len())
    }

    /// Check the canonical generator root and prove the supplied fleet manifest
    /// has the required 28-member shape. Consumer adoption is intentionally
    /// separate; this gate validates the generated policy source and generator
    /// own tool graph before rollout tasks change each repository.
    pub fn check_fleet(&self, root: &Path, fleet_path: &Path) -> Result<usize, String> {
        let manifest = crate::model::FleetManifest::load(root)?;
        let fleet_body = fs::read_to_string(fleet_path)
            .map_err(|error| format!("reading fleet manifest {}: {error}", fleet_path.display()))?;
        let fleet: Value = toml::from_str(&fleet_body)
            .map_err(|error| format!("parsing fleet manifest {}: {error}", fleet_path.display()))?;
        let repositories = fleet
            .get("repository")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{}: missing [[repository]] entries", fleet_path.display()))?;
        if repositories.len() < 28 {
            return Err(format!(
                "{}: expected at least 28 repositories, found {}",
                fleet_path.display(),
                repositories.len()
            ));
        }
        if manifest.repositories().len() != repositories.len() {
            return Err(format!(
                "{}: parsed repository count {} differs from manifest loader count {}",
                fleet_path.display(),
                repositories.len(),
                manifest.repositories().len()
            ));
        }
        let count = self.check_generator_files(&root.join("mise.toml"), &root.join("mise.lock"))?;
        let tools_template = root.join("templates").join("tools").join("mise.toml");
        let expected = self.render_tools_block(self.entries.keys().map(String::as_str))?;
        let committed = fs::read_to_string(&tools_template).map_err(|error| {
            format!(
                "reading generated tools template {}: {error}",
                tools_template.display()
            )
        })?;
        if committed != expected {
            return Err(format!(
                "{}: generated tools template diverges from registry",
                tools_template.display()
            ));
        }
        Ok(count)
    }

    fn parse_mise(&self, body: &str, path: &Path) -> Result<Vec<EffectiveTool>, String> {
        let document: Value = toml::from_str(body)
            .map_err(|error| format!("parsing mise file {}: {error}", path.display()))?;
        let tools = document
            .get("tools")
            .and_then(Value::as_table)
            .ok_or_else(|| format!("{}: missing [tools] table", path.display()))?;
        let mut effective = Vec::new();
        let mut seen = BTreeSet::new();
        for (key, value) in tools {
            let (version, explicit_source) = tool_value(value, key, path)?;
            if key == "rust" {
                return Err(format!(
                    "{}: rust pin is forbidden; use rust-toolchain.toml",
                    path.display()
                ));
            }
            validate_version(key, &version)?;
            let (name, spec) = self.resolve(key, explicit_source.as_deref(), path)?;
            let source = &spec.source;
            let backend = spec.backend.as_deref().unwrap_or(source).to_owned();
            if explicit_source
                .as_ref()
                .is_some_and(|value| value.as_str() != source.as_str() && value.as_str() != backend)
            {
                return Err(format!(
                    "{}: tool {key:?} source {:?} diverges from registry source {:?}",
                    path.display(),
                    explicit_source,
                    spec.source
                ));
            }
            if version != spec.version {
                return Err(format!(
                    "{}: tool {name:?} version {version:?} diverges from registry version {:?}",
                    path.display(),
                    spec.version
                ));
            }
            if !seen.insert(name.clone()) {
                return Err(format!(
                    "{}: tool {name:?} is declared more than once",
                    path.display()
                ));
            }
            effective.push(EffectiveTool {
                key: key.clone(),
                name,
                source: source.to_owned(),
                backend,
                version,
            });
        }
        if effective.is_empty() {
            return Err(format!("{}: [tools] table is empty", path.display()));
        }
        Ok(effective)
    }

    fn resolve<'a>(
        &'a self,
        key: &str,
        explicit_source: Option<&str>,
        path: &Path,
    ) -> Result<(String, &'a ToolSpec), String> {
        if let Some(name) = self.entries.get(key).map(|_| key.to_owned()) {
            let spec = &self.entries[&name];
            if explicit_source.is_none_or(|source| {
                source == spec.source || Some(source) == spec.backend.as_deref()
            }) {
                return Ok((name, spec));
            }
            return Err(format!(
                "{}: tool {key:?} source {explicit_source:?} diverges from registry source {:?}",
                path.display(),
                spec.source
            ));
        }
        let source = explicit_source.unwrap_or(key);
        self.entries
            .iter()
            .find(|(_, spec)| spec.source == source || spec.backend.as_deref() == Some(source))
            .map(|(name, spec)| (name.clone(), spec))
            .ok_or_else(|| {
                format!(
                    "{}: tool {key:?} with source {source:?} is not in the registry",
                    path.display()
                )
            })
    }

    fn check_rendered_equality(
        &self,
        effective: &[EffectiveTool],
        path: &Path,
    ) -> Result<(), String> {
        let names: Vec<&str> = effective
            .iter()
            .filter(|tool| tool.name != "rust")
            .map(|tool| tool.name.as_str())
            .collect();
        if names.is_empty() {
            return Err(format!(
                "{}: no registry-managed tools remain after excluding generator rust",
                path.display()
            ));
        }
        let rendered = self.render_tools_block(names.iter().copied())?;
        let rendered_effective = self.parse_mise(&rendered, path)?;
        let mut expected: Vec<_> = effective
            .iter()
            .filter(|tool| tool.name != "rust")
            .map(|tool| (&tool.name, &tool.source, &tool.backend, &tool.version))
            .collect();
        let mut actual: Vec<_> = rendered_effective
            .iter()
            .map(|tool| (&tool.name, &tool.source, &tool.backend, &tool.version))
            .collect();
        expected.sort_unstable();
        actual.sort_unstable();
        if expected != actual {
            return Err(format!(
                "{}: rendered [tools] block diverges from effective registry graph",
                path.display()
            ));
        }
        Ok(())
    }

    fn check_lock(
        &self,
        effective: &[EffectiveTool],
        body: &str,
        path: &Path,
        allow_unmodeled_rust: bool,
    ) -> Result<(), String> {
        let document: Value = toml::from_str(body)
            .map_err(|error| format!("parsing lockfile {}: {error}", path.display()))?;
        let tools = document
            .get("tools")
            .and_then(Value::as_table)
            .ok_or_else(|| format!("{}: missing [tools] lock entries", path.display()))?;
        let mut used_keys = BTreeSet::new();
        for tool in effective {
            let candidates = [&tool.key, &tool.name, &tool.source, &tool.backend];
            let (lock_key, value) = candidates
                .iter()
                .find_map(|candidate| {
                    tools
                        .get(candidate.as_str())
                        .map(|value| (*candidate, value))
                })
                .ok_or_else(|| {
                    format!(
                        "{}: no lock entry for tool {:?} ({})",
                        path.display(),
                        tool.name,
                        tool.backend
                    )
                })?;
            used_keys.insert(lock_key.to_owned());
            let records = value.as_array().ok_or_else(|| {
                format!(
                    "{}: lock entry {lock_key:?} is not an array",
                    path.display()
                )
            })?;
            let matching = records.len() == 1
                && records.iter().all(|record| {
                    let Some(record) = record.as_table() else {
                        return false;
                    };
                    record.get("version").and_then(Value::as_str) == Some(tool.version.as_str())
                        && record.get("backend").and_then(Value::as_str)
                            == Some(tool.backend.as_str())
                });
            if !matching {
                return Err(format!(
                    "{}: lock entry {lock_key:?} does not exclusively pin {}={} from {}",
                    path.display(),
                    tool.name,
                    tool.version,
                    tool.backend
                ));
            }
        }
        let extras: Vec<_> = tools
            .keys()
            .filter(|key| {
                !used_keys.contains(*key) && !(allow_unmodeled_rust && key.as_str() == "rust")
            })
            .cloned()
            .collect();
        if let Some(extra) = extras.first() {
            return Err(format!(
                "{}: lockfile has tool entry {extra:?} absent from effective [tools]",
                path.display()
            ));
        }
        Ok(())
    }
}

fn migrate_legacy_tool(
    body: &str,
    name: &str,
    legacy_key: &str,
    legacy_version: &str,
    canonical_key: &str,
    canonical_version: &str,
) -> Result<(String, bool), String> {
    let document: Value = toml::from_str(body)
        .map_err(|error| format!("parsing mise file for legacy migration: {error}"))?;
    let Some(tools) = document.get("tools").and_then(Value::as_table) else {
        return Ok((body.to_owned(), false));
    };
    let Some(value) = tools.get(legacy_key) else {
        return Ok((body.to_owned(), false));
    };
    if tools.contains_key(canonical_key) {
        return Err(format!(
            "mise declares both legacy {legacy_key:?} and canonical {canonical_key:?} {name} tools"
        ));
    }
    if value.as_str() != Some(legacy_version) {
        return Err(format!(
            "legacy {name} migration accepts only {legacy_key:?} = {legacy_version:?}, found {value:?}"
        ));
    }

    let mut output = String::new();
    let mut in_tools = false;
    let mut replaced = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if header_name(trimmed) == Some("[tools]") {
            in_tools = true;
        } else if in_tools && top_level_header(trimmed) {
            in_tools = false;
        }
        if in_tools && is_legacy_assignment(trimmed, legacy_key, legacy_version) {
            let indentation = &line[..line.len() - line.trim_start().len()];
            output.push_str(indentation);
            writeln!(
                output,
                "{} = {}",
                toml_key(canonical_key),
                toml_string(canonical_version)
            )
            .expect("writing a String cannot fail");
            replaced = true;
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    if !replaced {
        return Err(format!(
            "legacy {name} declaration {legacy_key:?} must be a simple [tools] assignment"
        ));
    }
    Ok((output, true))
}

fn tool_value(value: &Value, key: &str, path: &Path) -> Result<(String, Option<String>), String> {
    match value {
        Value::String(version) => Ok((version.clone(), None)),
        Value::Table(table) => {
            for field in table.keys() {
                if !matches!(field.as_str(), "version" | "source") {
                    return Err(format!(
                        "{}: tool {key:?} has unsupported field {field:?}",
                        path.display()
                    ));
                }
            }
            let version = table
                .get("version")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{}: tool {key:?} has no string version", path.display()))?;
            let source = table
                .get("source")
                .map(|value| {
                    value.as_str().map(str::to_owned).ok_or_else(|| {
                        format!("{}: tool {key:?} source is not a string", path.display())
                    })
                })
                .transpose()?;
            Ok((version.to_owned(), source))
        }
        _ => Err(format!(
            "{}: tool {key:?} must be a string or inline table",
            path.display()
        )),
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.chars().any(char::is_whitespace) || name.contains('=') {
        return Err(format!("invalid registry tool name {name:?}"));
    }
    Ok(())
}

fn validate_version(name: &str, version: &str) -> Result<(), String> {
    if (name == "repolint" || name.contains("tailrocks/repolint"))
        && version
            .strip_prefix("rev:")
            .is_some_and(crate::model::is_sha40)
    {
        return Ok(());
    }
    let exact_numeric_version = version.split('.').count() == 3
        && version.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        });
    if !exact_numeric_version || version.trim().is_empty() || version.eq_ignore_ascii_case("latest")
    {
        return Err(format!(
            "tool {name:?} has an unpinned or invalid version {version:?}"
        ));
    }
    Ok(())
}

fn validate_source(name: &str, source: &str) -> Result<(), String> {
    let valid = if let Some((prefix, target)) = source.split_once(':') {
        !target.is_empty() && matches!(prefix, "aqua" | "cargo" | "core" | "github")
    } else {
        source
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    };
    if !valid
        || source.trim().is_empty()
        || source.chars().any(char::is_whitespace)
        || source.starts_with(':')
        || source.ends_with(':')
    {
        return Err(format!("tool {name:?} has invalid source {source:?}"));
    }
    Ok(())
}

fn toml_string(value: &str) -> String {
    format!("{value:?}")
}

fn toml_key(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        value.to_owned()
    } else {
        toml_string(value)
    }
}

fn is_legacy_assignment(line: &str, key: &str, version: &str) -> bool {
    let declaration = line.split_once('#').map_or(line, |(prefix, _)| prefix);
    declaration.trim() == format!("{} = {}", toml_key(key), toml_string(version))
}

fn legacy_key_for(name: &str) -> Option<&'static str> {
    match name {
        "nextest" => Some(LEGACY_NEXTEST_KEY),
        "cargo-audit" => Some(LEGACY_CARGO_AUDIT_KEY),
        _ => None,
    }
}

fn top_level_header(line: &str) -> bool {
    let Some(header) = header_name(line) else {
        return false;
    };
    !header.starts_with("[tools.")
}

fn header_name(line: &str) -> Option<&str> {
    let without_comment = line.split_once('#').map_or(line, |(prefix, _)| prefix);
    let trimmed = without_comment.trim();
    (trimmed.starts_with('[') && trimmed.ends_with(']')).then_some(trimmed)
}

/// Resolve the registry path relative to a generator root.
#[must_use]
pub fn registry_path(root: &Path) -> PathBuf {
    root.join("fleet").join("fleet-tools.toml")
}

fn rust_toolchain_version(path: &Path) -> Result<String, String> {
    let bytes = fs::read_to_string(path)
        .map_err(|error| format!("reading Rust toolchain {}: {error}", path.display()))?;
    let document: RustToolchainDocument = toml::from_str(&bytes)
        .map_err(|error| format!("parsing Rust toolchain {}: {error}", path.display()))?;
    validate_version("rust", &document.toolchain.channel)?;
    Ok(document.toolchain.channel)
}

/// Validate a registry path without constructing a consumer.
pub fn check_registry(path: &Path) -> Result<ToolRegistry, String> {
    ToolRegistry::load(path)
}

/// Validate the repository-owned standard task vocabulary without inspecting
/// or rewriting task bodies. `release:check` is deliberately conditional on a
/// repository having a release unit.
pub fn check_mise_vocabulary(path: &Path) -> Result<(), String> {
    let body = fs::read_to_string(path)
        .map_err(|error| format!("reading mise file {}: {error}", path.display()))?;
    let document: Value = toml::from_str(&body)
        .map_err(|error| format!("parsing mise file {}: {error}", path.display()))?;
    let tasks = document
        .get("tasks")
        .and_then(Value::as_table)
        .ok_or_else(|| format!("{}: missing [tasks] table", path.display()))?;
    for verb in REQUIRED_MISE_VERBS {
        let task = tasks
            .get(verb)
            .ok_or_else(|| format!("{}: missing required mise task {verb:?}", path.display()))?;
        let table = task
            .as_table()
            .ok_or_else(|| format!("{}: task {verb:?} is not a table", path.display()))?;
        if table
            .get("description")
            .and_then(Value::as_str)
            .is_none_or(|description| description.trim().is_empty())
        {
            return Err(format!(
                "{}: mise task {verb:?} requires a non-empty description",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ToolRegistry {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        ToolRegistry::load(&root.join("fleet/fleet-tools.toml")).expect("load registry")
    }

    #[test]
    fn render_migrates_only_the_exact_legacy_nextest_declaration() {
        let legacy = "[tools]\n\"github:nextest-rs/nextest\" = \"cargo-nextest-0.9.140\"\n[tasks.fmt]\ndescription = \"fmt\"\n";
        let (migrated, changed) = ToolRegistry::migrate_legacy_nextest(legacy).unwrap();

        assert!(changed);
        assert!(migrated.contains("\"aqua:nextest-rs/nextest/cargo-nextest\" = \"0.9.140\""));
        assert!(!migrated.contains("github:nextest-rs/nextest"));
        assert!(migrated.contains("[tasks.fmt]\ndescription = \"fmt\""));

        let wrong = legacy.replace("cargo-nextest-0.9.140", "cargo-nextest-0.9.141");
        let error = ToolRegistry::migrate_legacy_nextest(&wrong).unwrap_err();
        assert!(error.contains("accepts only"), "unexpected error: {error}");
    }

    #[test]
    fn render_migrates_only_the_exact_legacy_cargo_audit_declaration() {
        let legacy = "[tools]\n\"github:rustsec/rustsec\" = \"cargo-audit/v0.22.2\"\n[tasks.fmt]\ndescription = \"fmt\"\n";
        let (migrated, changed) = ToolRegistry::migrate_legacy_cargo_audit(legacy).unwrap();

        assert!(changed);
        assert!(migrated.contains("\"aqua:rustsec/rustsec/cargo-audit\" = \"0.22.2\""));
        assert!(!migrated.contains("github:rustsec/rustsec"));
        assert!(migrated.contains("[tasks.fmt]\ndescription = \"fmt\""));

        let wrong = legacy.replace("cargo-audit/v0.22.2", "cargo-audit/v0.22.3");
        let error = ToolRegistry::migrate_legacy_cargo_audit(&wrong).unwrap_err();
        assert!(error.contains("accepts only"), "unexpected error: {error}");

        let both = legacy.replace(
            "[tools]\n",
            "[tools]\n\"aqua:rustsec/rustsec/cargo-audit\" = \"0.22.2\"\n",
        );
        let error = ToolRegistry::migrate_legacy_cargo_audit(&both).unwrap_err();
        assert!(error.contains("both legacy"), "unexpected error: {error}");

        let single_quoted = legacy.replace(
            "\"github:rustsec/rustsec\" = \"cargo-audit/v0.22.2\"",
            "'github:rustsec/rustsec' = 'cargo-audit/v0.22.2'",
        );
        let error = ToolRegistry::migrate_legacy_cargo_audit(&single_quoted).unwrap_err();
        assert!(
            error.contains("simple [tools] assignment"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn render_migration_projects_legacy_nextest_lock_to_canonical_source() {
        let registry = registry();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source_lock = fs::read_to_string(root.join("mise.lock")).unwrap();
        let legacy_lock = "# @generated\n\nlockfile_version = 1\n\n[[tools.\"github:nextest-rs/nextest\"]]\nversion = \"cargo-nextest-0.9.140\"\nbackend = \"github:nextest-rs/nextest\"\n";
        let mise = "[tools]\n\"aqua:nextest-rs/nextest/cargo-nextest\" = \"0.9.140\"\n";

        let projected = registry
            .project_lock_file(legacy_lock, &source_lock, ["nextest"])
            .unwrap();
        assert!(projected.contains("tools.\"aqua:nextest-rs/nextest/cargo-nextest\""));
        assert!(!projected.contains("github:nextest-rs/nextest"));
        assert_eq!(registry.check_text(mise, &projected).unwrap(), 1);
    }

    #[test]
    fn render_migration_projects_legacy_cargo_audit_lock_to_canonical_source() {
        let registry = registry();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source_lock = fs::read_to_string(root.join("mise.lock")).unwrap();
        let legacy_lock = "# @generated\n\nlockfile_version = 1\n\n[[tools.\"github:rustsec/rustsec\"]]\nversion = \"cargo-audit/v0.22.2\"\nbackend = \"github:rustsec/rustsec\"\n";
        let mise = "[tools]\n\"aqua:rustsec/rustsec/cargo-audit\" = \"0.22.2\"\n";

        let projected = registry
            .project_lock_file(legacy_lock, &source_lock, ["cargo-audit"])
            .unwrap();
        assert!(projected.contains("tools.\"aqua:rustsec/rustsec/cargo-audit\""));
        assert!(!projected.contains("github:rustsec/rustsec"));
        assert_eq!(registry.check_text(mise, &projected).unwrap(), 1);
    }
}
