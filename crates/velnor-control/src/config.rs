//! Configuration and context application services.
//!
//! This module resolves one deterministic precedence chain. It never reads
//! process environment or `/proc`; callers provide captured layers explicitly.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use velnor_model::{
    ConfigSource, ContextConfig, DesiredConfig, EffectiveConfig, SecretRef, Sourced, Timestamp,
};

/// One field's contribution from a configuration layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Setting<T> {
    /// Layer does not define this field.
    #[default]
    Unset,
    /// Layer explicitly defines a value.
    Value(T),
    /// Layer explicitly clears a collection or optional field.
    ExplicitEmpty,
}

/// A captured configuration layer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigLayer {
    /// Source represented by this layer.
    pub source: ConfigSource,
    /// Endpoint setting.
    pub endpoint: Setting<velnor_model::SanitizedUrl>,
    /// Instance name setting.
    pub name: Setting<String>,
    /// Slot-count setting.
    pub slots: Setting<u32>,
    /// Full label collection replacement.
    pub labels: Setting<BTreeMap<String, String>>,
    /// Credential reference setting; values are never accepted here.
    pub credential: Setting<SecretRef>,
}

/// Resolver for the fixed low-to-high precedence order.
#[derive(Debug, Clone)]
pub struct ConfigResolver {
    layers: Vec<ConfigLayer>,
}

impl ConfigResolver {
    /// Build a resolver from explicitly captured layers.
    pub fn new(mut layers: Vec<ConfigLayer>) -> Result<Self, ConfigError> {
        layers.sort_by_key(|layer| source_rank(layer.source));
        for pair in layers.windows(2) {
            if source_rank(pair[0].source) == source_rank(pair[1].source) {
                return Err(ConfigError::DuplicateSource(pair[0].source));
            }
        }
        Ok(Self { layers })
    }

    /// Resolve every field and retain source provenance.
    pub fn resolve(&self) -> Result<EffectiveConfig, ConfigError> {
        let mut endpoint = (None, ConfigSource::Builtin);
        let mut name = ("velnor".to_owned(), ConfigSource::Builtin);
        let mut slots = (1_u32, ConfigSource::Builtin);
        let mut labels = (BTreeMap::new(), ConfigSource::Builtin);
        let mut credential = (None, ConfigSource::Builtin);

        for layer in &self.layers {
            match &layer.endpoint {
                Setting::Unset => {}
                Setting::Value(value) => endpoint = (Some(value.clone()), layer.source),
                Setting::ExplicitEmpty => endpoint = (None, layer.source),
            }
            apply_setting(&mut name, &layer.name, layer.source, Some(String::new()))?;
            apply_setting(&mut slots, &layer.slots, layer.source, None)?;
            apply_setting(
                &mut labels,
                &layer.labels,
                layer.source,
                Some(BTreeMap::new()),
            )?;
            match &layer.credential {
                Setting::Unset => {}
                Setting::Value(reference) => {
                    validate_reference(reference)?;
                    credential = (Some(reference.clone()), layer.source);
                }
                Setting::ExplicitEmpty => credential = (None, layer.source),
            }
        }

        if name.0.is_empty() {
            return Err(ConfigError::Empty("name"));
        }
        if slots.0 == 0 {
            return Err(ConfigError::InvalidSlots);
        }
        Ok(EffectiveConfig {
            endpoint: Sourced {
                value: endpoint.0,
                source: endpoint.1,
            },
            name: Sourced {
                value: name.0,
                source: name.1,
            },
            slots: Sourced {
                value: slots.0,
                source: slots.1,
            },
            labels: Sourced {
                value: labels.0,
                source: labels.1,
            },
            credential: Sourced {
                value: credential.0,
                source: credential.1,
            },
            captured_at: Timestamp::now(),
        })
    }

    /// Convert a desired model into one instance layer.
    pub fn desired_layer(desired: &DesiredConfig) -> ConfigLayer {
        ConfigLayer {
            source: ConfigSource::Instance,
            endpoint: desired
                .endpoint
                .clone()
                .map_or(Setting::Unset, Setting::Value),
            name: desired.name.clone().map_or(Setting::Unset, Setting::Value),
            slots: desired.slots.map_or(Setting::Unset, Setting::Value),
            labels: desired
                .labels
                .clone()
                .map_or(Setting::Unset, Setting::Value),
            credential: desired
                .credential
                .clone()
                .map_or(Setting::Unset, Setting::Value),
        }
    }
}

/// Bounded context-change journal containing no endpoint credentials or values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextJournal {
    capacity: usize,
    entries: Vec<ContextChange>,
}

impl ContextJournal {
    /// Construct a journal with a fixed row budget.
    pub fn new(capacity: usize) -> Result<Self, ConfigError> {
        if capacity == 0 {
            return Err(ConfigError::InvalidJournalCapacity);
        }
        Ok(Self {
            capacity,
            entries: Vec::new(),
        })
    }

    /// Record a sanitized context change, retaining only the latest rows.
    pub fn record(&mut self, change: ContextChange) {
        self.entries.push(change);
        if self.entries.len() > self.capacity {
            let excess = self.entries.len() - self.capacity;
            self.entries.drain(..excess);
        }
    }

    /// Current bounded entries in chronological order.
    #[must_use]
    pub fn entries(&self) -> &[ContextChange] {
        &self.entries
    }
}

/// One sanitized context selection change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextChange {
    /// Actor source (CLI, daemon, or API).
    pub actor: String,
    /// Previous context name.
    pub old_context: Option<String>,
    /// New context name.
    pub new_context: String,
    /// Change time.
    pub occurred_at: Timestamp,
}

/// Context persistence boundary; file format stays outside domain services.
pub trait ContextStore: Send + Sync {
    /// List sanitized contexts.
    fn list(&self) -> Result<Vec<ContextConfig>, ConfigError>;
    /// Select one context by name.
    fn select(&self, name: &str) -> Result<ContextConfig, ConfigError>;
}

/// File-backed context store with atomic `0600` writes.
pub struct FileContextStore {
    path: std::path::PathBuf,
    resolved_path: std::sync::OnceLock<std::path::PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContextFile {
    contexts: Vec<ContextConfig>,
    current: Option<String>,
}

impl FileContextStore {
    /// Open a store at an explicit path.
    #[must_use]
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: path.into(),
            resolved_path: std::sync::OnceLock::new(),
        }
    }

    /// Save or replace one context.
    pub fn set(&self, mut context: ContextConfig) -> Result<(), ConfigError> {
        validate_context_name(&context.name)?;
        if context.endpoint.as_str().is_empty() {
            return Err(ConfigError::Empty("endpoint"));
        }
        let path = self.absolute_path()?;
        let _lock = SidecarLock::acquire(&path)?;
        let mut file = read_file(&path)?;
        let is_current =
            file.current.as_deref() == Some(context.name.as_str()) || file.current.is_none();
        if is_current {
            file.current = Some(context.name.clone());
        }
        context.current = is_current;
        if let Some(existing) = file
            .contexts
            .iter_mut()
            .find(|item| item.name == context.name)
        {
            *existing = context;
        } else {
            file.contexts.push(context);
        }
        write_file(&path, &file)
    }

    /// Select one existing context and persist that selection.
    pub fn use_context(&self, name: &str) -> Result<ContextConfig, ConfigError> {
        validate_context_name(name)?;
        let path = self.absolute_path()?;
        let _lock = SidecarLock::acquire(&path)?;
        let mut file = read_file(&path)?;
        if !file.contexts.iter().any(|context| context.name == name) {
            return Err(ConfigError::ContextNotFound);
        }
        file.current = Some(name.to_owned());
        for context in &mut file.contexts {
            context.current = context.name == name;
        }
        write_file(&path, &file)?;
        file.contexts
            .into_iter()
            .find(|context| context.name == name)
            .ok_or(ConfigError::ContextNotFound)
    }

    /// Delete a non-current context.
    pub fn delete(&self, name: &str) -> Result<(), ConfigError> {
        let path = self.absolute_path()?;
        let _lock = SidecarLock::acquire(&path)?;
        let mut file = read_file(&path)?;
        if file.current.as_deref() == Some(name) {
            return Err(ConfigError::ContextCurrent);
        }
        let before = file.contexts.len();
        file.contexts.retain(|context| context.name != name);
        if before == file.contexts.len() {
            return Err(ConfigError::ContextNotFound);
        }
        write_file(&path, &file)
    }

    fn absolute_path(&self) -> Result<std::path::PathBuf, ConfigError> {
        if let Some(path) = self.resolved_path.get() {
            return Ok(path.clone());
        }
        let path =
            std::path::absolute(&self.path).map_err(|error| ConfigError::Io(error.to_string()))?;
        let _ = self.resolved_path.set(path.clone());
        Ok(self.resolved_path.get().cloned().unwrap_or(path))
    }
}

struct SidecarLock {
    _file: std::fs::File,
}

#[cfg(unix)]
fn current_effective_uid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> std::ffi::c_uint;
    }

    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { geteuid() }
}

#[cfg(unix)]
fn validate_file_metadata(
    metadata: &std::fs::Metadata,
    description: &str,
) -> Result<(), ConfigError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if metadata.uid() != current_effective_uid() {
        return Err(ConfigError::Io(format!(
            "refusing to use {description} not owned by the current user"
        )));
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err(ConfigError::Io(format!(
            "refusing to use {description} writable by group or other"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_file_metadata(
    _metadata: &std::fs::Metadata,
    _description: &str,
) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(unix)]
fn validate_directory_metadata(
    metadata: &std::fs::Metadata,
    description: &str,
) -> Result<(), ConfigError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let uid = metadata.uid();
    let mode = metadata.permissions().mode() & 0o7777;
    let is_root_sticky_temp = uid == 0 && mode == 0o1777;
    if uid != current_effective_uid() && uid != 0 {
        return Err(ConfigError::Io(format!(
            "refusing to use {description} not owned by the current user or root"
        )));
    }
    if mode & 0o022 != 0 && !is_root_sticky_temp {
        return Err(ConfigError::Io(format!(
            "refusing to use {description} writable by group or other"
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_directory_metadata(
    _metadata: &std::fs::Metadata,
    _description: &str,
) -> Result<(), ConfigError> {
    Ok(())
}

impl SidecarLock {
    fn acquire(config_path: &std::path::Path) -> Result<Self, ConfigError> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let parent = ensure_parent(config_path)?;
        inspect_final_file(config_path)?;
        let name = context_file_name(config_path)?;
        let path = parent.join(format!(".{name}.lock"));
        inspect_lock_path(&path)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        let metadata = file
            .metadata()
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        validate_file_metadata(&metadata, "context lock")?;
        if !metadata.is_file() {
            return Err(ConfigError::Io(
                "refusing to use a non-regular context lock".to_owned(),
            ));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        file.lock()
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        Ok(Self { _file: file })
    }

    fn acquire_existing_shared(config_path: &std::path::Path) -> Result<Option<Self>, ConfigError> {
        let Some(parent) = inspect_read_parent(config_path)? else {
            return Ok(None);
        };
        inspect_final_file(config_path)?;
        let name = context_file_name(config_path)?;
        let path = parent.join(format!(".{name}.lock"));
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ConfigError::Io(
                    "refusing to use a symbolic-link context lock".to_owned(),
                ));
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(ConfigError::Io(
                    "refusing to use a non-regular context lock".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ConfigError::Io(error.to_string())),
        }

        let file = match std::fs::OpenOptions::new().read(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ConfigError::Io(error.to_string())),
        };
        let metadata = file
            .metadata()
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        validate_file_metadata(&metadata, "context lock")?;
        if !metadata.is_file() {
            return Err(ConfigError::Io(
                "refusing to use a non-regular context lock".to_owned(),
            ));
        }
        file.lock_shared()
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        Ok(Some(Self { _file: file }))
    }
}

fn read_file(path: &std::path::Path) -> Result<ContextFile, ConfigError> {
    inspect_read_path(path)?;
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            toml::from_str(&contents).map_err(|error| ConfigError::Decode(error.to_string()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ContextFile {
            contexts: Vec::new(),
            current: None,
        }),
        Err(error) => Err(ConfigError::Io(error.to_string())),
    }
}

fn write_file(path: &std::path::Path, file: &ContextFile) -> Result<(), ConfigError> {
    let parent = ensure_parent(path)?;
    inspect_final_file(path)?;
    let name = context_file_name(path)?;
    let temp = parent.join(format!(".{name}.tmp-{}", uuid::Uuid::new_v4().simple()));
    write_file_with_temp_path(path, parent, file, &temp)
}

fn write_file_with_temp_path(
    path: &std::path::Path,
    parent: &std::path::Path,
    file: &ContextFile,
    temp: &std::path::Path,
) -> Result<(), ConfigError> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut owns_temp = false;
    let result = (|| {
        let mut handle = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(temp)
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        owns_temp = true;
        let metadata = handle
            .metadata()
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        validate_file_metadata(&metadata, "context temporary file")?;
        if !metadata.is_file() {
            return Err(ConfigError::Io(
                "refusing to use a non-regular context temporary file".to_owned(),
            ));
        }
        handle
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        let contents =
            toml::to_string_pretty(file).map_err(|error| ConfigError::Decode(error.to_string()))?;
        handle
            .write_all(contents.as_bytes())
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        handle
            .sync_all()
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        drop(handle);
        std::fs::rename(temp, path).map_err(|error| ConfigError::Io(error.to_string()))?;
        owns_temp = false;
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| ConfigError::Io(error.to_string()))
    })();
    if owns_temp && result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

fn ensure_parent(path: &std::path::Path) -> Result<&std::path::Path, ConfigError> {
    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::Io("context path has no parent".to_owned()))?;
    inspect_directory_chain(parent, true)?;
    std::fs::create_dir_all(parent).map_err(|error| ConfigError::Io(error.to_string()))?;
    inspect_directory_chain(parent, false)?;
    Ok(parent)
}

fn inspect_read_path(path: &std::path::Path) -> Result<(), ConfigError> {
    let _ = inspect_read_parent(path)?;
    inspect_final_file(path)
}

fn inspect_read_parent(path: &std::path::Path) -> Result<Option<&std::path::Path>, ConfigError> {
    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::Io("context path has no parent".to_owned()))?;
    inspect_directory_chain(parent, true)?;
    if parent.exists() {
        Ok(Some(parent))
    } else {
        Ok(None)
    }
}

fn inspect_final_file(path: &std::path::Path) -> Result<(), ConfigError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(ConfigError::Io(
            "refusing to use a symbolic-link context file".to_owned(),
        )),
        Ok(metadata) if !metadata.is_file() => Err(ConfigError::Io(
            "refusing to use a non-regular context file".to_owned(),
        )),
        Ok(metadata) => validate_file_metadata(&metadata, "context file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ConfigError::Io(error.to_string())),
    }
}

fn inspect_lock_path(path: &std::path::Path) -> Result<(), ConfigError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(ConfigError::Io(
            "refusing to use a symbolic-link context lock".to_owned(),
        )),
        Ok(metadata) if !metadata.is_file() => Err(ConfigError::Io(
            "refusing to use a non-regular context lock".to_owned(),
        )),
        Ok(metadata) => validate_file_metadata(&metadata, "context lock"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ConfigError::Io(error.to_string())),
    }
}

fn context_file_name(path: &std::path::Path) -> Result<&str, ConfigError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ConfigError::Io("context path has no valid file name".to_owned()))
}

fn inspect_directory_chain(path: &std::path::Path, allow_missing: bool) -> Result<(), ConfigError> {
    for component in path.components() {
        if !matches!(
            component,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        ) {
            return Err(ConfigError::Io(
                "context directory contains a non-canonical component".to_owned(),
            ));
        }
    }

    let mut current = std::path::PathBuf::from("/");
    for component in path.components() {
        if component == std::path::Component::RootDir {
            continue;
        }
        let std::path::Component::Normal(name) = component else {
            unreachable!("directory components were validated above");
        };
        current.push(name);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(())
            }
            Err(error) => return Err(ConfigError::Io(error.to_string())),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ConfigError::Io(
                "context directory contains a symlink or non-directory".to_owned(),
            ));
        }
        validate_directory_metadata(&metadata, "context directory")?;
    }
    Ok(())
}

impl ContextStore for FileContextStore {
    fn list(&self) -> Result<Vec<ContextConfig>, ConfigError> {
        let path = self.absolute_path()?;
        let _lock = SidecarLock::acquire_existing_shared(&path)?;
        Ok(read_file(&path)?.contexts)
    }

    fn select(&self, name: &str) -> Result<ContextConfig, ConfigError> {
        let path = self.absolute_path()?;
        let _lock = SidecarLock::acquire_existing_shared(&path)?;
        read_file(&path)?
            .contexts
            .into_iter()
            .find(|context| context.name == name)
            .ok_or(ConfigError::ContextNotFound)
    }
}

/// Configuration service failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// Two layers supplied the same source.
    DuplicateSource(ConfigSource),
    /// A required field was explicitly empty.
    Empty(&'static str),
    /// Slot count cannot be zero.
    InvalidSlots,
    /// Credential reference is not a valid name-only reference.
    InvalidCredentialReference,
    /// Journal must retain at least one row.
    InvalidJournalCapacity,
    /// Context name was not found.
    ContextNotFound,
    /// The selected context cannot be removed.
    ContextCurrent,
    /// File IO failed; detailed paths stay out of the display string.
    Io(String),
    /// Context document failed typed decoding.
    Decode(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateSource(source) => {
                write!(formatter, "duplicate config source {source:?}")
            }
            Self::Empty(field) => write!(formatter, "config field {field} cannot be empty"),
            Self::InvalidSlots => formatter.write_str("config slots must be greater than zero"),
            Self::InvalidCredentialReference => {
                formatter.write_str("credential reference must be a name-only value")
            }
            Self::InvalidJournalCapacity => {
                formatter.write_str("context journal capacity must be greater than zero")
            }
            Self::ContextNotFound => formatter.write_str("context was not found"),
            Self::ContextCurrent => formatter.write_str("current context cannot be deleted"),
            Self::Io(_) => formatter.write_str("context store IO failed"),
            Self::Decode(_) => formatter.write_str("context store is invalid"),
        }
    }
}

impl std::error::Error for ConfigError {}

fn source_rank(source: ConfigSource) -> u8 {
    match source {
        ConfigSource::Builtin => 0,
        ConfigSource::Context => 1,
        ConfigSource::Instance => 2,
        ConfigSource::Systemd => 3,
        ConfigSource::Process => 4,
        ConfigSource::Command => 5,
    }
}

fn apply_setting<T: Clone>(
    target: &mut (T, ConfigSource),
    setting: &Setting<T>,
    source: ConfigSource,
    empty: Option<T>,
) -> Result<(), ConfigError> {
    match setting {
        Setting::Unset => {}
        Setting::Value(value) => target.0 = value.clone(),
        Setting::ExplicitEmpty => {
            target.0 = empty.ok_or(ConfigError::Empty("field"))?;
        }
    }
    if !matches!(setting, Setting::Unset) {
        target.1 = source;
    }
    Ok(())
}

fn validate_reference(reference: &SecretRef) -> Result<(), ConfigError> {
    if reference.name.is_empty()
        || reference.name.len() > 128
        || !reference
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(ConfigError::InvalidCredentialReference);
    }
    Ok(())
}

fn validate_context_name(name: &str) -> Result<(), ConfigError> {
    if name.is_empty()
        || name.len() > 64
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
    {
        return Err(ConfigError::Empty("context name"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static CURRENT_DIRECTORY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct RelativeTempDir {
        path: std::path::PathBuf,
        _cwd_lock: std::sync::MutexGuard<'static, ()>,
    }

    impl RelativeTempDir {
        fn new(label: &str) -> Self {
            let cwd_lock = CURRENT_DIRECTORY_LOCK.lock().expect("cwd lock");
            let path = std::path::PathBuf::from(format!(
                ".velnor-config-{label}-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir(&path).expect("temporary directory");
            Self {
                path,
                _cwd_lock: cwd_lock,
            }
        }
    }

    struct CurrentDirectoryGuard {
        original: std::path::PathBuf,
    }

    impl CurrentDirectoryGuard {
        fn new() -> Self {
            Self {
                original: std::env::current_dir().expect("current directory"),
            }
        }

        fn change_to(&self, path: &std::path::Path) {
            std::env::set_current_dir(path).expect("change current directory");
        }
    }

    impl Drop for CurrentDirectoryGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.original).expect("restore current directory");
        }
    }

    impl Drop for RelativeTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn test_context() -> ContextConfig {
        named_context("primary")
    }

    fn named_context(name: &str) -> ContextConfig {
        ContextConfig {
            name: name.to_owned(),
            endpoint: velnor_model::SanitizedUrl::project("https://velnor.example.test"),
            credential: None,
            current: true,
        }
    }

    #[test]
    fn precedence_is_total_and_provenance_is_retained() {
        let resolver = ConfigResolver::new(vec![
            ConfigLayer {
                source: ConfigSource::Builtin,
                name: Setting::Value("builtin".to_owned()),
                ..ConfigLayer::default()
            },
            ConfigLayer {
                source: ConfigSource::Command,
                name: Setting::Value("operator".to_owned()),
                ..ConfigLayer::default()
            },
        ])
        .expect("unique sources");
        let effective = resolver.resolve().expect("valid config");
        assert_eq!(effective.name.value, "operator");
        assert_eq!(effective.name.source, ConfigSource::Command);
    }

    #[test]
    fn invalid_credential_reference_fails_before_serialization() {
        let resolver = ConfigResolver::new(vec![ConfigLayer {
            source: ConfigSource::Command,
            credential: Setting::Value(SecretRef::named("token=value")),
            ..ConfigLayer::default()
        }])
        .expect("unique source");
        assert_eq!(
            resolver.resolve().unwrap_err(),
            ConfigError::InvalidCredentialReference
        );
    }

    #[test]
    fn relative_context_store_round_trips() {
        let temp = RelativeTempDir::new("round-trip");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path);
        let context = test_context();

        store.set(context.clone()).expect("write context");

        assert_eq!(store.select("primary").expect("read context"), context);
    }

    #[test]
    fn missing_context_reads_do_not_create_filesystem_entries() {
        let temp = RelativeTempDir::new("missing-reads");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path);
        let before: Vec<_> = std::fs::read_dir(&temp.path)
            .expect("read temporary directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect();

        assert!(store.list().expect("list missing store").is_empty());
        assert_eq!(
            store.select("missing").expect_err("select missing context"),
            ConfigError::ContextNotFound
        );

        let after: Vec<_> = std::fs::read_dir(&temp.path)
            .expect("read temporary directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect();
        assert_eq!(before, after);
        assert!(!temp.path.join("nested").exists());
    }

    #[test]
    fn existing_context_reads_do_not_change_filesystem_metadata() {
        use std::os::unix::fs::PermissionsExt;

        let temp = RelativeTempDir::new("read-only");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path.clone());
        store.set(test_context()).expect("write context");

        let parent = path.parent().expect("context parent");
        let lock_path = parent.join(".config.toml.lock");
        let before_entries: Vec<_> = std::fs::read_dir(parent)
            .expect("read context directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect();
        let before_config = std::fs::symlink_metadata(&path).expect("config metadata");
        let before_lock = std::fs::symlink_metadata(&lock_path).expect("lock metadata");

        assert_eq!(store.list().expect("list contexts"), vec![test_context()]);
        assert_eq!(
            store.select("primary").expect("select context"),
            test_context()
        );

        let after_entries: Vec<_> = std::fs::read_dir(parent)
            .expect("read context directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect();
        let after_config = std::fs::symlink_metadata(&path).expect("config metadata");
        let after_lock = std::fs::symlink_metadata(&lock_path).expect("lock metadata");
        assert_eq!(before_entries, after_entries);
        assert_eq!(
            before_config.modified().expect("config mtime"),
            after_config.modified().expect("config mtime")
        );
        assert_eq!(
            before_lock.modified().expect("lock mtime"),
            after_lock.modified().expect("lock mtime")
        );
        assert_eq!(
            before_config.permissions().mode() & 0o777,
            after_config.permissions().mode() & 0o777
        );
        assert_eq!(
            before_lock.permissions().mode() & 0o777,
            after_lock.permissions().mode() & 0o777
        );
    }

    #[test]
    fn relative_context_store_writes_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let temp = RelativeTempDir::new("permissions");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path.clone());

        store.set(test_context()).expect("write context");

        assert_eq!(
            std::fs::metadata(&path)
                .expect("context metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let lock_path = std::path::PathBuf::from(format!(
            ".{}.lock",
            path.file_name()
                .expect("context file name")
                .to_string_lossy()
        ));
        assert_eq!(
            std::fs::metadata(path.parent().expect("context parent").join(lock_path))
                .expect("lock metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn context_store_rejects_group_or_other_writable_config() {
        use std::os::unix::fs::PermissionsExt;

        let temp = RelativeTempDir::new("unsafe-mode");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path.clone());
        store.set(test_context()).expect("write context");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o620))
            .expect("make config group-writable");

        assert_eq!(
            store.list().expect_err("reject unsafe config mode"),
            ConfigError::Io("refusing to use context file writable by group or other".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn context_store_rejects_group_or_other_writable_ancestor() {
        use std::os::unix::fs::PermissionsExt;

        let temp = RelativeTempDir::new("unsafe-ancestor-mode");
        let parent = temp.path.join("nested");
        std::fs::create_dir(&parent).expect("nested directory");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777))
            .expect("make ancestor group-writable");
        let store = FileContextStore::new(parent.join("config.toml"));

        assert_eq!(
            store
                .set(test_context())
                .expect_err("reject unsafe ancestor mode"),
            ConfigError::Io(
                "refusing to use context directory writable by group or other".to_owned()
            )
        );
    }

    #[test]
    fn relative_context_store_keeps_resolved_path_when_cwd_changes() {
        let temp = RelativeTempDir::new("cwd");
        let cwd = CurrentDirectoryGuard::new();
        let original = cwd.original.clone();
        let alternate = original.join(&temp.path).join("alternate");
        std::fs::create_dir_all(&alternate).expect("alternate directory");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path);

        store.set(test_context()).expect("write context");
        cwd.change_to(&alternate);
        let selected = store.select("primary").expect("read context");

        assert_eq!(selected, test_context());
    }

    #[test]
    fn concurrent_distinct_sets_preserve_both_contexts() {
        let temp = RelativeTempDir::new("concurrent");
        let path = temp.path.join("nested").join("config.toml");
        let first_store = FileContextStore::new(path.clone());
        let second_store = FileContextStore::new(path);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

        std::thread::scope(|scope| {
            let first_barrier = std::sync::Arc::clone(&barrier);
            let first_store = &first_store;
            let first = scope.spawn(move || {
                first_barrier.wait();
                first_store.set(named_context("first"))
            });
            let second_barrier = std::sync::Arc::clone(&barrier);
            let second_store = &second_store;
            let second = scope.spawn(move || {
                second_barrier.wait();
                second_store.set(named_context("second"))
            });

            barrier.wait();
            first
                .join()
                .expect("first writer thread")
                .expect("first write");
            second
                .join()
                .expect("second writer thread")
                .expect("second write");
        });

        let mut names: Vec<_> = first_store
            .list()
            .expect("list contexts")
            .into_iter()
            .map(|context| context.name)
            .collect();
        names.sort();
        assert_eq!(names, ["first", "second"]);
    }

    #[test]
    fn relative_context_store_supports_list_use_and_delete() {
        let temp = RelativeTempDir::new("operations");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path);
        let primary = test_context();
        let secondary = named_context("secondary");

        store.set(primary).expect("write primary context");
        store.set(secondary).expect("write secondary context");
        assert_eq!(store.list().expect("list contexts").len(), 2);

        let selected = store
            .use_context("secondary")
            .expect("select secondary context");
        assert!(selected.current);
        assert_eq!(
            store.select("secondary").expect("read selected context"),
            selected
        );

        store.delete("primary").expect("delete old context");
        assert_eq!(
            store.list().expect("list remaining context"),
            vec![selected]
        );
    }

    #[test]
    fn relative_context_store_leaves_no_temporary_files() {
        let temp = RelativeTempDir::new("temporary-files");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path);

        store.set(test_context()).expect("write context");

        let nested = temp.path.join("nested");
        let temporary_files: Vec<_> = std::fs::read_dir(nested)
            .expect("read context directory")
            .map(|entry| entry.expect("directory entry").file_name())
            .filter(|name| name.to_string_lossy().starts_with(".config.toml.tmp-"))
            .collect();
        assert!(
            temporary_files.is_empty(),
            "temporary files: {temporary_files:?}"
        );
    }

    #[test]
    fn context_lock_enforces_shared_and_exclusive_modes() {
        let temp = RelativeTempDir::new("lock-modes");
        let path = temp.path.join("nested").join("config.toml");
        let absolute_path = std::path::absolute(&path).expect("absolute context path");
        SidecarLock::acquire(&absolute_path).expect("create lock");
        let held_shared = SidecarLock::acquire_existing_shared(&absolute_path)
            .expect("acquire shared lock")
            .expect("existing shared lock");
        let lock_path = absolute_path
            .parent()
            .expect("context parent")
            .join(".config.toml.lock");
        let shared_probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("shared probe");
        shared_probe
            .try_lock_shared()
            .expect("shared lock should coexist");
        drop(shared_probe);
        let exclusive_probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("exclusive probe");
        assert!(
            exclusive_probe.try_lock().is_err(),
            "exclusive lock must wait for shared holders"
        );
        drop(exclusive_probe);
        drop(held_shared);

        let held_exclusive = SidecarLock::acquire(&absolute_path).expect("exclusive lock");
        let shared_probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("shared probe");
        assert!(
            shared_probe.try_lock_shared().is_err(),
            "shared lock must wait for exclusive holders"
        );
        drop(shared_probe);
        drop(held_exclusive);
    }

    #[test]
    fn context_store_rejects_final_config_symlink() {
        use std::os::unix::fs::symlink;

        let temp = RelativeTempDir::new("final-symlink");
        let target = temp.path.join("target.toml");
        let path = temp.path.join("config.toml");
        std::fs::write(&target, "contexts = []\ncurrent = none\n").expect("target file");
        symlink("target.toml", &path).expect("config symlink");
        let store = FileContextStore::new(path);

        assert_eq!(
            store
                .set(test_context())
                .expect_err("reject config symlink"),
            ConfigError::Io("refusing to use a symbolic-link context file".to_owned())
        );
    }

    #[test]
    fn context_store_rejects_lock_symlink() {
        use std::os::unix::fs::symlink;

        let temp = RelativeTempDir::new("lock-symlink");
        let nested = temp.path.join("nested");
        std::fs::create_dir(&nested).expect("nested directory");
        let target = temp.path.join("target.lock");
        let lock_path = nested.join(".config.toml.lock");
        std::fs::write(&target, "not a lock").expect("lock target");
        symlink("../target.lock", &lock_path).expect("lock symlink");
        let store = FileContextStore::new(nested.join("config.toml"));

        assert_eq!(
            store.set(test_context()).expect_err("reject lock symlink"),
            ConfigError::Io("refusing to use a symbolic-link context lock".to_owned())
        );
    }

    #[test]
    fn failed_atomic_write_cleans_owned_temporary_file() {
        let temp = RelativeTempDir::new("failed-write");
        let parent = temp.path.join("nested");
        std::fs::create_dir(&parent).expect("nested directory");
        let path = parent.join("config.toml");
        let temp_path = parent.join(".config.toml.tmp-test");
        std::fs::create_dir(&path).expect("blocking directory");

        let result = write_file_with_temp_path(
            &path,
            &parent,
            &ContextFile {
                contexts: vec![test_context()],
                current: Some("primary".to_owned()),
            },
            &temp_path,
        );

        assert!(result.is_err(), "directory must block atomic replacement");
        assert!(!temp_path.exists(), "owned temporary file must be removed");
    }

    #[test]
    fn relative_context_store_rejects_symlink_ancestor() {
        use std::os::unix::fs::symlink;

        let temp = RelativeTempDir::new("symlink");
        let target = temp.path.join("target");
        std::fs::create_dir(&target).expect("target directory");
        let link = temp.path.join("link");
        symlink("target", &link).expect("directory symlink");
        let store = FileContextStore::new(link.join("config.toml"));

        let error = store
            .set(test_context())
            .expect_err("reject symlink ancestor");

        assert_eq!(
            error,
            ConfigError::Io("context directory contains a symlink or non-directory".to_owned())
        );
    }
}
