//! Trusted, lane-neutral cache declaration and key contract.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{ALL_CLASSES, RepositoryClass};

/// Version of the generated cache declaration and runtime handshake.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

/// A correctness-safe cache artifact class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactClass {
    /// Downloaded tools and installers.
    ToolDownload,
    /// Dependency downloads or resolved dependency state.
    Dependency,
    /// Reusable compiler/build output.
    BuildOutput,
}

/// One validated canonical cache declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheDeclaration {
    /// Owning class.
    pub class: RepositoryClass,
    /// Stable, low-cardinality cache identifier.
    pub id: String,
    /// Artifact category.
    pub artifact_class: ArtifactClass,
    /// Fixed cache destinations/globs. Never contributor-selected.
    pub paths: Vec<String>,
    /// Lock/tool input globs hashed for correctness.
    pub lock_globs: Vec<String>,
    /// Stable execution phase.
    pub phase: String,
    /// Whether a same-lock earlier phase may restore partially.
    pub compatible_phase_prefix: bool,
    /// Whether a prior compatible lock may restore partially.
    pub compatible_lock_prefix: bool,
}

/// Complete validated cache contract plus raw declaration identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheContract {
    declarations: Vec<CacheDeclaration>,
    declaration_sha256: String,
}

impl CacheContract {
    /// Load canonical `fleet/caches.toml` and validate every field fail-closed.
    ///
    /// # Errors
    ///
    /// Rejects malformed TOML, unknown fields/classes, unsafe paths, duplicate
    /// identities, high-cardinality/dynamic data, or a class with no cache.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        let raw: CacheFile =
            toml::from_slice(&bytes).map_err(|e| format!("parsing {}: {e}", path.display()))?;
        if raw.schema_version != CACHE_SCHEMA_VERSION {
            return Err(format!(
                "cache schema_version {} != {CACHE_SCHEMA_VERSION}",
                raw.schema_version
            ));
        }
        if raw.cache.is_empty() {
            return Err("cache declaration is empty".into());
        }

        let mut seen = BTreeSet::new();
        let mut class_counts = BTreeMap::new();
        let mut declarations = Vec::with_capacity(raw.cache.len());
        for item in raw.cache {
            let class = parse_class(&item.class)?;
            validate_token("cache id", &item.id)?;
            validate_token("phase", &item.phase)?;
            if !seen.insert((class.code(), item.id.clone())) {
                return Err(format!(
                    "duplicate cache identity {}/{}",
                    class.code(),
                    item.id
                ));
            }
            validate_globs("paths", &item.paths)?;
            validate_globs("lock_globs", &item.lock_globs)?;
            validate_correctness_inputs(class, &item.id, &item.lock_globs)?;
            *class_counts.entry(class.code()).or_insert(0_usize) += 1;
            declarations.push(CacheDeclaration {
                class,
                id: item.id,
                artifact_class: item.artifact_class,
                paths: item.paths,
                lock_globs: item.lock_globs,
                phase: item.phase,
                compatible_phase_prefix: item.compatible_phase_prefix,
                compatible_lock_prefix: item.compatible_lock_prefix,
            });
        }
        for class in ALL_CLASSES {
            if class_counts.get(class.code()).copied().unwrap_or_default() == 0 {
                return Err(format!("class {} has no cache declaration", class.code()));
            }
        }
        let declaration_sha256 = hex::encode(Sha256::digest(&bytes));
        Ok(Self {
            declarations,
            declaration_sha256,
        })
    }

    /// Canonical declarations in file order.
    #[must_use]
    pub fn declarations(&self) -> &[CacheDeclaration] {
        &self.declarations
    }

    /// SHA-256 of the exact trusted declaration bytes.
    #[must_use]
    pub fn declaration_sha256(&self) -> &str {
        &self.declaration_sha256
    }

    /// Find one class/cache identity.
    #[must_use]
    pub fn find(&self, class: RepositoryClass, id: &str) -> Option<&CacheDeclaration> {
        self.declarations
            .iter()
            .find(|item| item.class == class && item.id == id)
    }
}

/// Immutable key inputs. Digests are lowercase SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKeyInputs<'a> {
    /// Repository class.
    pub class: RepositoryClass,
    /// Declared cache ID.
    pub cache_id: &'a str,
    /// Runner operating system token.
    pub os: &'a str,
    /// Runner architecture token.
    pub arch: &'a str,
    /// Digest of all toolchain-affecting inputs.
    pub toolchain_digest: &'a str,
    /// Digest of protected/current lock inputs as appropriate.
    pub lock_digest: &'a str,
    /// Digest of phase-affecting inputs.
    pub phase_digest: &'a str,
}

/// Exact key and ordered correctness-safe restore prefixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKeyPlan {
    /// Exact cache key.
    pub exact: String,
    /// Ordered restore prefixes, most specific first.
    pub restore_prefixes: Vec<String>,
}

/// Build a bounded, content-derived key plan.
///
/// # Errors
///
/// Rejects unknown declarations, unsafe tokens, and non-SHA-256 digests.
pub fn build_key_plan(
    contract: &CacheContract,
    inputs: &CacheKeyInputs<'_>,
) -> Result<CacheKeyPlan, String> {
    validate_token("cache id", inputs.cache_id)?;
    validate_token("os", inputs.os)?;
    validate_token("arch", inputs.arch)?;
    validate_digest("toolchain digest", inputs.toolchain_digest)?;
    validate_digest("lock digest", inputs.lock_digest)?;
    validate_digest("phase digest", inputs.phase_digest)?;
    let declaration = contract
        .find(inputs.class, inputs.cache_id)
        .ok_or_else(|| {
            format!(
                "undeclared cache {}/{}",
                inputs.class.code(),
                inputs.cache_id
            )
        })?;
    let root = format!(
        "ci-v{CACHE_SCHEMA_VERSION}/{}/{}/{}-{}/{}/",
        inputs.class.code(),
        inputs.cache_id,
        inputs.os,
        inputs.arch,
        inputs.toolchain_digest
    );
    let lock_root = format!("{root}{}/", inputs.lock_digest);
    let exact = format!("{lock_root}{}", inputs.phase_digest);
    let mut restore_prefixes = Vec::new();
    if declaration.compatible_phase_prefix {
        restore_prefixes.push(lock_root);
    }
    if declaration.compatible_lock_prefix {
        restore_prefixes.push(root);
    }
    Ok(CacheKeyPlan {
        exact,
        restore_prefixes,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheFile {
    schema_version: u32,
    #[serde(default)]
    cache: Vec<CacheEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheEntry {
    class: String,
    id: String,
    artifact_class: ArtifactClass,
    paths: Vec<String>,
    lock_globs: Vec<String>,
    phase: String,
    compatible_phase_prefix: bool,
    compatible_lock_prefix: bool,
}

fn parse_class(value: &str) -> Result<RepositoryClass, String> {
    ALL_CLASSES
        .into_iter()
        .find(|class| class.code() == value)
        .ok_or_else(|| format!("unknown cache class {value:?}"))
}

fn validate_token(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!("invalid {label} {value:?}"));
    }
    const FORBIDDEN_CARDINALITY: [&str; 8] = [
        "commit",
        "branch",
        "pull",
        "run",
        "attempt",
        "date",
        "wave",
        "repository",
    ];
    if FORBIDDEN_CARDINALITY
        .iter()
        .any(|word| value.contains(word))
    {
        return Err(format!("high-cardinality {label} {value:?}"));
    }
    Ok(())
}

fn validate_digest(label: &str, value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{label} is not 64 lowercase hex"));
    }
    Ok(())
}

fn validate_globs(label: &str, values: &[String]) -> Result<(), String> {
    if values.is_empty() {
        return Err(format!("{label} is empty"));
    }
    let mut seen = BTreeSet::new();
    const ALLOWED_CACHE_PATHS: [&str; 7] = [
        ".cache/mise",
        ".local/share/mise/installs",
        ".cargo/git",
        ".cargo/registry",
        ".gradle/caches",
        "**/node_modules",
        "target",
    ];
    const ALLOWED_RECURSIVE_TARGET: &str = "**/target";
    for value in values {
        let unsafe_component = value.is_empty()
            || value == "."
            || value == "*"
            || value == "**"
            || value.starts_with('/')
            || value.starts_with('~')
            || value.contains('\\')
            || value.contains("${{")
            || value.split('/').any(|part| part == ".." || part.is_empty());
        let unsupported = !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b'*')
        });
        if unsafe_component || unsupported {
            return Err(format!("unsafe {label} entry {value:?}"));
        }
        if label == "paths"
            && !ALLOWED_CACHE_PATHS.contains(&value.as_str())
            && value != ALLOWED_RECURSIVE_TARGET
        {
            return Err(format!("undeclared {label} entry {value:?}"));
        }
        if !seen.insert(value) {
            return Err(format!("duplicate {label} entry {value:?}"));
        }
    }
    Ok(())
}

fn validate_correctness_inputs(
    class: RepositoryClass,
    id: &str,
    lock_globs: &[String],
) -> Result<(), String> {
    const TOOL_INPUTS: [&str; 2] = ["mise.lock", "mise.toml"];
    const DEPENDENCY_INPUTS: [&str; 22] = [
        "**/.cargo/config.toml",
        "**/.npmrc",
        "**/.yarnrc.yml",
        "**/Cargo.lock",
        "**/Cargo.toml",
        "**/build.gradle",
        "**/build.gradle.kts",
        "**/bun.lock",
        "**/bun.lockb",
        "**/bunfig.toml",
        "**/gradle.lockfile",
        "**/gradle.properties",
        "**/gradle/libs.versions.toml",
        "**/gradle-wrapper.properties",
        "**/package-lock.json",
        "**/package.json",
        "**/pnpm-lock.yaml",
        "**/pnpm-workspace.yaml",
        "**/settings.gradle",
        "**/settings.gradle.kts",
        "**/yarn.lock",
        "mise.toml",
    ];
    const BUILD_INPUTS: [&str; 4] = [
        "**/.cargo/config.toml",
        "**/Cargo.lock",
        "**/Cargo.toml",
        "rust-toolchain.toml",
    ];
    let required: &[&str] = match (class, id) {
        (_, "tools") => &TOOL_INPUTS,
        (RepositoryClass::Code, "dependencies") => &DEPENDENCY_INPUTS,
        (RepositoryClass::Code, "build-output") => &BUILD_INPUTS,
        _ => return Ok(()),
    };
    for input in required {
        if !lock_globs.iter().any(|glob| glob == input) {
            return Err(format!(
                "cache {}/{} omits correctness input {input}",
                class.code(),
                id
            ));
        }
    }
    Ok(())
}
