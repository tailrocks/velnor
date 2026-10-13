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
        Self { path: path.into() }
    }

    /// Save or replace one context.
    pub fn set(&self, mut context: ContextConfig) -> Result<(), ConfigError> {
        validate_context_name(&context.name)?;
        if context.endpoint.as_str().is_empty() {
            return Err(ConfigError::Empty("endpoint"));
        }
        let mut file = self.read_file()?;
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
        self.write_file(&file)
    }

    /// Select one existing context and persist that selection.
    pub fn use_context(&self, name: &str) -> Result<ContextConfig, ConfigError> {
        validate_context_name(name)?;
        let mut file = self.read_file()?;
        if !file.contexts.iter().any(|context| context.name == name) {
            return Err(ConfigError::ContextNotFound);
        }
        file.current = Some(name.to_owned());
        for context in &mut file.contexts {
            context.current = context.name == name;
        }
        self.write_file(&file)?;
        file.contexts
            .into_iter()
            .find(|context| context.name == name)
            .ok_or(ConfigError::ContextNotFound)
    }

    /// Delete a non-current context.
    pub fn delete(&self, name: &str) -> Result<(), ConfigError> {
        let mut file = self.read_file()?;
        if file.current.as_deref() == Some(name) {
            return Err(ConfigError::ContextCurrent);
        }
        let before = file.contexts.len();
        file.contexts.retain(|context| context.name != name);
        if before == file.contexts.len() {
            return Err(ConfigError::ContextNotFound);
        }
        self.write_file(&file)
    }

    fn read_file(&self) -> Result<ContextFile, ConfigError> {
        match std::fs::read_to_string(&self.path) {
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

    fn write_file(&self, file: &ContextFile) -> Result<(), ConfigError> {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let parent = self
            .path
            .parent()
            .ok_or_else(|| ConfigError::Io("context path has no parent".to_owned()))?;
        std::fs::create_dir_all(parent).map_err(|error| ConfigError::Io(error.to_string()))?;
        inspect_directory_chain(parent)?;
        let name = self
            .path
            .file_name()
            .and_then(|item| item.to_str())
            .unwrap_or("config.toml");
        if std::fs::symlink_metadata(&self.path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(ConfigError::Io(
                "refusing to replace a symbolic-link context file".to_owned(),
            ));
        }
        let temp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
        let contents =
            toml::to_string_pretty(file).map_err(|error| ConfigError::Decode(error.to_string()))?;
        let result = (|| {
            let mut handle = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|error| ConfigError::Io(error.to_string()))?;
            handle
                .write_all(contents.as_bytes())
                .map_err(|error| ConfigError::Io(error.to_string()))?;
            handle
                .sync_all()
                .map_err(|error| ConfigError::Io(error.to_string()))?;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| ConfigError::Io(error.to_string()))?;
            std::fs::rename(&temp, &self.path)
                .map_err(|error| ConfigError::Io(error.to_string()))?;
            std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| ConfigError::Io(error.to_string()))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
}

fn inspect_directory_chain(path: &std::path::Path) -> Result<(), ConfigError> {
    let mut current = std::path::PathBuf::from("/");
    for component in path.components() {
        if component == std::path::Component::RootDir {
            continue;
        }
        let std::path::Component::Normal(name) = component else {
            return Err(ConfigError::Io(
                "context directory contains a non-canonical component".to_owned(),
            ));
        };
        current.push(name);
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ConfigError::Io(
                "context directory contains a symlink or non-directory".to_owned(),
            ));
        }
    }
    Ok(())
}

impl ContextStore for FileContextStore {
    fn list(&self) -> Result<Vec<ContextConfig>, ConfigError> {
        Ok(self.read_file()?.contexts)
    }

    fn select(&self, name: &str) -> Result<ContextConfig, ConfigError> {
        self.read_file()?
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
}
