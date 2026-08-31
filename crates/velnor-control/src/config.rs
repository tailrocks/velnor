//! Configuration and context application services.
//!
//! This module resolves one deterministic precedence chain. It never reads
//! process environment or `/proc`; callers provide captured layers explicitly.

use std::collections::{BTreeMap, BTreeSet};
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
                Setting::Value(value) => {
                    validate_endpoint(value)?;
                    endpoint = (Some(value.clone()), layer.source);
                }
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
    pub fn record(&mut self, change: ContextChange) -> Result<(), ConfigError> {
        validate_context_change(&change)?;
        self.entries.push(change);
        if self.entries.len() > self.capacity {
            let excess = self.entries.len() - self.capacity;
            self.entries.drain(..excess);
        }
        Ok(())
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
    resolved_path: std::sync::OnceLock<Result<std::path::PathBuf, ConfigError>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextFile {
    contexts: Vec<ContextConfig>,
    current: Option<String>,
}

impl FileContextStore {
    /// Open a store at an explicit path.
    #[must_use]
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        let path = path.into();
        let resolved_path = std::sync::OnceLock::new();
        let _ = resolved_path
            .set(std::path::absolute(&path).map_err(|error| ConfigError::Io(error.to_string())));
        Self { resolved_path }
    }

    /// Save or replace one context.
    pub fn set(&self, mut context: ContextConfig) -> Result<(), ConfigError> {
        validate_context_config(&context)?;
        let path = self.absolute_path()?;
        let mut lock = SidecarLock::acquire(&path)?;
        let mut file = read_file_at(lock.context_file.as_mut())?;
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
        write_file_at(&lock.path, &file)
    }

    /// Select one existing context and persist that selection.
    pub fn use_context(&self, name: &str) -> Result<ContextConfig, ConfigError> {
        validate_context_name(name)?;
        let path = self.absolute_path()?;
        let mut lock = SidecarLock::acquire(&path)?;
        let mut file = read_file_at(lock.context_file.as_mut())?;
        if !file.contexts.iter().any(|context| context.name == name) {
            return Err(ConfigError::ContextNotFound);
        }
        file.current = Some(name.to_owned());
        for context in &mut file.contexts {
            context.current = context.name == name;
        }
        write_file_at(&lock.path, &file)?;
        let context = file
            .contexts
            .into_iter()
            .find(|context| context.name == name)
            .ok_or(ConfigError::ContextNotFound)?;
        validate_context_config(&context)?;
        Ok(context)
    }

    /// Delete a non-current context.
    pub fn delete(&self, name: &str) -> Result<(), ConfigError> {
        validate_context_name(name)?;
        let path = self.absolute_path()?;
        let mut lock = SidecarLock::acquire(&path)?;
        let mut file = read_file_at(lock.context_file.as_mut())?;
        if file.current.as_deref() == Some(name) {
            return Err(ConfigError::ContextCurrent);
        }
        let before = file.contexts.len();
        file.contexts.retain(|context| context.name != name);
        if before == file.contexts.len() {
            return Err(ConfigError::ContextNotFound);
        }
        write_file_at(&lock.path, &file)
    }

    fn absolute_path(&self) -> Result<std::path::PathBuf, ConfigError> {
        self.resolved_path.get().cloned().ok_or_else(|| {
            ConfigError::Io("context path was not resolved at construction".to_owned())
        })?
    }
}

struct SidecarLock {
    _file: Option<std::fs::File>,
    path: AnchoredPath,
    context_file: Option<std::fs::File>,
}

struct OpenedLock {
    file: std::fs::File,
}

#[cfg(unix)]
struct AnchoredPath {
    parent: std::fs::File,
    name: std::ffi::OsString,
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
    let mode = metadata.permissions().mode() & 0o7777;
    if mode & 0o022 != 0 {
        return Err(ConfigError::Io(format!(
            "refusing to use {description} writable by group or other"
        )));
    }
    if mode != 0o600 {
        return Err(ConfigError::Io(format!(
            "refusing to use {description} without exact mode 0600"
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
        let path = open_anchored_parent(config_path, true)?.ok_or_else(|| {
            ConfigError::Io("context parent disappeared while opening it".to_owned())
        })?;
        let context_exists = inspect_regular_entry(&path.parent, &path.name, "context file")?;
        let lock_name = lock_file_name(&path.name);
        let opened =
            open_lock(&path.parent, &lock_name, !context_exists, true)?.ok_or_else(|| {
                if context_exists {
                    ConfigError::Io("refusing to use context file without sidecar lock".to_owned())
                } else {
                    ConfigError::Io("context lock disappeared while opening it".to_owned())
                }
            })?;
        let file = opened.file;
        file.lock()
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        let context_file = open_verified_file(&path.parent, &path.name, "context file")?;
        Ok(Self {
            _file: Some(file),
            path,
            context_file,
        })
    }

    fn acquire_existing_shared(config_path: &std::path::Path) -> Result<Option<Self>, ConfigError> {
        let Some(path) = open_anchored_parent(config_path, false)? else {
            return Ok(None);
        };
        let lock_name = lock_file_name(&path.name);
        let Some(opened) = open_lock(&path.parent, &lock_name, false, false)? else {
            if inspect_regular_entry(&path.parent, &path.name, "context file")? {
                return Err(ConfigError::Io(
                    "refusing to use context file without sidecar lock".to_owned(),
                ));
            }
            return Ok(None);
        };
        let file = opened.file;
        file.lock_shared()
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        let context_file = open_verified_file(&path.parent, &path.name, "context file")?;
        Ok(Some(Self {
            _file: Some(file),
            path,
            context_file,
        }))
    }
}

#[cfg(test)]
fn write_file_with_temp_path(
    path: &std::path::Path,
    _parent: &std::path::Path,
    file: &ContextFile,
    temp: &std::path::Path,
) -> Result<(), ConfigError> {
    let path = open_anchored_parent(path, true)?
        .ok_or_else(|| ConfigError::Io("context parent disappeared while opening it".to_owned()))?;
    let temp_name = temp
        .file_name()
        .ok_or_else(|| ConfigError::Io("context temporary path has no file name".to_owned()))?
        .to_owned();
    write_file_at_with_temp_name(&path, file, &temp_name)
}

fn write_file_at(path: &AnchoredPath, file: &ContextFile) -> Result<(), ConfigError> {
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(&path.name);
    temp_name.push(format!(".tmp-{}", uuid::Uuid::new_v4().simple()));
    write_file_at_with_temp_name(path, file, &temp_name)
}

fn write_file_at_with_temp_name(
    path: &AnchoredPath,
    file: &ContextFile,
    temp_name: &std::ffi::OsStr,
) -> Result<(), ConfigError> {
    use std::io::Write;

    let mut owns_temp = false;
    let result = (|| {
        validate_context_file(file)?;
        validate_existing_file(&path.parent, &path.name, "context file")?;
        let descriptor = rustix::fs::openat(
            &path.parent,
            temp_name,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map_err(|error| ConfigError::Io(std::io::Error::from(error).to_string()))?;
        let mut handle: std::fs::File = descriptor.into();
        owns_temp = true;
        rustix::fs::fchmod(&handle, rustix::fs::Mode::from_raw_mode(0o600))
            .map_err(|error| ConfigError::Io(std::io::Error::from(error).to_string()))?;
        validate_file_handle(&handle, "context temporary file")?;
        let contents = toml::to_string_pretty(file)
            .map_err(|_| ConfigError::Decode("context document failed serialization".to_owned()))?;
        handle
            .write_all(contents.as_bytes())
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        handle
            .sync_all()
            .map_err(|error| ConfigError::Io(error.to_string()))?;
        drop(handle);
        rustix::fs::renameat(&path.parent, temp_name, &path.parent, &path.name)
            .map_err(|error| ConfigError::Io(std::io::Error::from(error).to_string()))?;
        owns_temp = false;
        path.parent
            .sync_all()
            .map_err(|error| ConfigError::Io(error.to_string()))
    })();
    if owns_temp && result.is_err() {
        let _ = rustix::fs::unlinkat(&path.parent, temp_name, rustix::fs::AtFlags::empty());
    }
    result
}

fn context_file_name(path: &std::path::Path) -> Result<&std::ffi::OsStr, ConfigError> {
    path.file_name()
        .filter(|name| *name != std::ffi::OsStr::new(".") && *name != std::ffi::OsStr::new(".."))
        .ok_or_else(|| ConfigError::Io("context path has no valid file name".to_owned()))
}

fn open_anchored_parent(
    path: &std::path::Path,
    create: bool,
) -> Result<Option<AnchoredPath>, ConfigError> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::absolute(path).map_err(|error| ConfigError::Io(error.to_string()))?
    };
    let parent_path = path
        .parent()
        .ok_or_else(|| ConfigError::Io("context path has no parent".to_owned()))?;
    let name = context_file_name(&path)?.to_owned();
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

    let mut parent = open_root_directory()?;
    for component in parent_path.components() {
        if component == std::path::Component::RootDir {
            continue;
        }
        let std::path::Component::Normal(name) = component else {
            unreachable!("directory components were validated above");
        };
        let Some(child) = open_directory_child(&parent, name, create)? else {
            return Ok(None);
        };
        parent = child;
    }
    validate_directory_handle(&parent, "context directory")?;
    Ok(Some(AnchoredPath { parent, name }))
}

fn open_root_directory() -> Result<std::fs::File, ConfigError> {
    let descriptor = rustix::fs::open(
        std::path::Path::new("/"),
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| ConfigError::Io(std::io::Error::from(error).to_string()))?;
    let directory: std::fs::File = descriptor.into();
    validate_directory_handle(&directory, "context directory")?;
    Ok(directory)
}

fn open_directory_child(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    create: bool,
) -> Result<Option<std::fs::File>, ConfigError> {
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK;
    let open = || rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty());
    let descriptor = match open() {
        Ok(descriptor) => descriptor,
        Err(error) if error == rustix::io::Errno::NOENT && !create => return Ok(None),
        Err(error) if error == rustix::io::Errno::NOENT => {
            match rustix::fs::mkdirat(parent, name, rustix::fs::Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(ConfigError::Io(std::io::Error::from(error).to_string())),
            }
            open().map_err(|error| {
                if error == rustix::io::Errno::LOOP || error == rustix::io::Errno::NOTDIR {
                    ConfigError::Io(
                        "context directory contains a symlink or non-directory".to_owned(),
                    )
                } else {
                    ConfigError::Io(std::io::Error::from(error).to_string())
                }
            })?
        }
        Err(error) if error == rustix::io::Errno::LOOP || error == rustix::io::Errno::NOTDIR => {
            return Err(ConfigError::Io(
                "context directory contains a symlink or non-directory".to_owned(),
            ))
        }
        Err(error) => {
            return Err(ConfigError::Io(std::io::Error::from(error).to_string()));
        }
    };
    let directory: std::fs::File = descriptor.into();
    validate_directory_handle(&directory, "context directory")?;
    Ok(Some(directory))
}

fn open_lock(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    create: bool,
    writable: bool,
) -> Result<Option<OpenedLock>, ConfigError> {
    if !create && !inspect_regular_entry(parent, name, "context lock")? {
        return Ok(None);
    }
    let access = if writable {
        rustix::fs::OFlags::RDWR
    } else {
        rustix::fs::OFlags::RDONLY
    };
    let flags = access
        | rustix::fs::OFlags::CLOEXEC
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::NOCTTY;
    let descriptor = if create {
        match rustix::fs::openat(
            parent,
            name,
            flags | rustix::fs::OFlags::CREATE | rustix::fs::OFlags::EXCL,
            rustix::fs::Mode::from_raw_mode(0o600),
        ) {
            Ok(descriptor) => Ok((descriptor, true)),
            Err(error) if error == rustix::io::Errno::EXIST => {
                let _ = inspect_regular_entry(parent, name, "context lock")?;
                rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty())
                    .map(|descriptor| (descriptor, false))
            }
            Err(error) => Err(error),
        }
    } else {
        rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty())
            .map(|descriptor| (descriptor, false))
    };
    let (descriptor, created) = match descriptor {
        Ok(descriptor) => descriptor,
        Err(error) if error == rustix::io::Errno::NOENT && !create => return Ok(None),
        Err(error) if error == rustix::io::Errno::LOOP => {
            return Err(ConfigError::Io(
                "refusing to use a symbolic-link context lock".to_owned(),
            ))
        }
        Err(error) if error == rustix::io::Errno::ISDIR => {
            return Err(ConfigError::Io(
                "refusing to use a non-regular context lock".to_owned(),
            ))
        }
        Err(error) => return Err(ConfigError::Io(std::io::Error::from(error).to_string())),
    };
    let file: std::fs::File = descriptor.into();
    if created && let Err(error) = rustix::fs::fchmod(&file, rustix::fs::Mode::from_raw_mode(0o600))
    {
        return Err(ConfigError::Io(std::io::Error::from(error).to_string()));
    }
    validate_file_handle(&file, "context lock")?;
    Ok(Some(OpenedLock { file }))
}

fn lock_file_name(name: &std::ffi::OsStr) -> std::ffi::OsString {
    let mut lock_name = std::ffi::OsString::from(".");
    lock_name.push(name);
    lock_name.push(".lock");
    lock_name
}

fn read_file_at(context_file: Option<&mut std::fs::File>) -> Result<ContextFile, ConfigError> {
    let Some(file) = context_file else {
        return Ok(ContextFile {
            contexts: Vec::new(),
            current: None,
        });
    };
    let mut contents = String::new();
    std::io::Read::read_to_string(file, &mut contents)
        .map_err(|error| ConfigError::Io(error.to_string()))?;
    let file = toml::from_str::<ContextFile>(&contents)
        .map_err(|_| ConfigError::Decode("context document failed typed decoding".to_owned()))?;
    validate_context_file(&file)?;
    Ok(file)
}

fn validate_existing_file(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    description: &str,
) -> Result<(), ConfigError> {
    let _ = open_verified_file(parent, name, description)?;
    Ok(())
}

fn open_verified_file(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    description: &str,
) -> Result<Option<std::fs::File>, ConfigError> {
    if !inspect_regular_entry(parent, name, description)? {
        return Ok(None);
    }
    let descriptor = match rustix::fs::openat(
        parent,
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOCTTY,
        rustix::fs::Mode::empty(),
    ) {
        Ok(descriptor) => descriptor,
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
        Err(error) if error == rustix::io::Errno::LOOP => {
            let message = if description == "context file" {
                "refusing to use a symbolic-link context file"
            } else {
                "refusing to use a symbolic-link context lock"
            };
            return Err(ConfigError::Io(message.to_owned()));
        }
        Err(error) => return Err(ConfigError::Io(std::io::Error::from(error).to_string())),
    };
    let file: std::fs::File = descriptor.into();
    validate_file_handle(&file, description)?;
    Ok(Some(file))
}

fn inspect_regular_entry(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
    description: &str,
) -> Result<bool, ConfigError> {
    let stat = match rustix::fs::statat(parent, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(error) if error == rustix::io::Errno::NOENT => return Ok(false),
        Err(error) => return Err(ConfigError::Io(std::io::Error::from(error).to_string())),
    };
    match rustix::fs::FileType::from_raw_mode(stat.st_mode) {
        rustix::fs::FileType::Symlink => {
            let message = if description == "context file" {
                "refusing to use a symbolic-link context file"
            } else {
                "refusing to use a symbolic-link context lock"
            };
            Err(ConfigError::Io(message.to_owned()))
        }
        rustix::fs::FileType::RegularFile => {
            validate_file_stat(&stat, description)?;
            Ok(true)
        }
        _ => Err(ConfigError::Io(format!(
            "refusing to use a non-regular {description}"
        ))),
    }
}

fn validate_file_stat(stat: &rustix::fs::Stat, description: &str) -> Result<(), ConfigError> {
    if stat.st_uid != current_effective_uid() {
        return Err(ConfigError::Io(format!(
            "refusing to use {description} not owned by the current user"
        )));
    }
    let mode = stat.st_mode & 0o7777;
    if mode & 0o022 != 0 {
        return Err(ConfigError::Io(format!(
            "refusing to use {description} writable by group or other"
        )));
    }
    if mode != 0o600 {
        return Err(ConfigError::Io(format!(
            "refusing to use {description} without exact mode 0600"
        )));
    }
    Ok(())
}

fn validate_file_handle(file: &std::fs::File, description: &str) -> Result<(), ConfigError> {
    let metadata = file
        .metadata()
        .map_err(|error| ConfigError::Io(error.to_string()))?;
    if !metadata.is_file() {
        return Err(ConfigError::Io(format!(
            "refusing to use a non-regular {description}"
        )));
    }
    validate_file_metadata(&metadata, description)
}

fn validate_directory_handle(
    directory: &std::fs::File,
    description: &str,
) -> Result<(), ConfigError> {
    let metadata = directory
        .metadata()
        .map_err(|error| ConfigError::Io(error.to_string()))?;
    if !metadata.is_dir() {
        return Err(ConfigError::Io(format!(
            "refusing to use a non-directory {description}"
        )));
    }
    validate_directory_metadata(&metadata, description)
}

impl ContextStore for FileContextStore {
    fn list(&self) -> Result<Vec<ContextConfig>, ConfigError> {
        let path = self.absolute_path()?;
        let Some(lock) = SidecarLock::acquire_existing_shared(&path)? else {
            return Ok(Vec::new());
        };
        let mut lock = lock;
        let file = read_file_at(lock.context_file.as_mut())?;
        Ok(file.contexts)
    }

    fn select(&self, name: &str) -> Result<ContextConfig, ConfigError> {
        let path = self.absolute_path()?;
        let Some(lock) = SidecarLock::acquire_existing_shared(&path)? else {
            return Err(ConfigError::ContextNotFound);
        };
        let mut lock = lock;
        let file = read_file_at(lock.context_file.as_mut())?;
        let context = file
            .contexts
            .into_iter()
            .find(|context| context.name == name)
            .ok_or(ConfigError::ContextNotFound)?;
        validate_context_config(&context)?;
        Ok(context)
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

fn validate_context_config(context: &ContextConfig) -> Result<(), ConfigError> {
    validate_context_name(&context.name)?;
    validate_endpoint(&context.endpoint)?;
    if let Some(reference) = &context.credential {
        validate_reference(reference)?;
    }
    Ok(())
}

fn validate_context_change(change: &ContextChange) -> Result<(), ConfigError> {
    if !matches!(change.actor.as_str(), "cli" | "daemon" | "api") {
        return Err(ConfigError::Decode(
            "context journal change has an invalid actor".to_owned(),
        ));
    }
    validate_context_name(&change.new_context).map_err(|_| {
        ConfigError::Decode("context journal change has an invalid context".to_owned())
    })?;
    if let Some(old_context) = &change.old_context {
        validate_context_name(old_context).map_err(|_| {
            ConfigError::Decode("context journal change has an invalid context".to_owned())
        })?;
    }
    Ok(())
}

fn validate_endpoint(endpoint: &velnor_model::SanitizedUrl) -> Result<(), ConfigError> {
    if endpoint.as_str().is_empty() {
        return Err(ConfigError::Empty("endpoint"));
    }
    Ok(())
}

fn validate_context_file(file: &ContextFile) -> Result<(), ConfigError> {
    let mut names = BTreeSet::new();
    for context in &file.contexts {
        validate_context_config(context)?;
        if !names.insert(context.name.as_str()) {
            return Err(ConfigError::Decode(
                "context store contains a duplicate context".to_owned(),
            ));
        }
    }

    let current_flags = file
        .contexts
        .iter()
        .filter(|context| context.current)
        .map(|context| context.name.as_str())
        .collect::<Vec<_>>();
    match file.current.as_deref() {
        Some(current) if current_flags.as_slice() != [current] => Err(ConfigError::Decode(
            "context store has an invalid current context marker".to_owned(),
        )),
        Some(current) if !names.contains(current) => Err(ConfigError::Decode(
            "context store has an invalid current context marker".to_owned(),
        )),
        None if !current_flags.is_empty() => Err(ConfigError::Decode(
            "context store has an invalid current context marker".to_owned(),
        )),
        _ => Ok(()),
    }
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

    fn write_secure_file(path: &std::path::Path, contents: &str) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .expect("create secure test file");
        file.write_all(contents.as_bytes())
            .expect("write secure test file");
    }

    fn write_secure_lock(path: &std::path::Path) {
        let parent = path.parent().expect("context parent");
        let name = path.file_name().expect("context file name");
        let mut lock_name = std::ffi::OsString::from(".");
        lock_name.push(name);
        lock_name.push(".lock");
        write_secure_file(&parent.join(lock_name), "");
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
    fn context_journal_rejects_untrusted_values() {
        let mut journal = ContextJournal::new(1).expect("journal capacity");
        let valid = ContextChange {
            actor: "cli".to_owned(),
            old_context: None,
            new_context: "primary".to_owned(),
            occurred_at: Timestamp::now(),
        };
        journal.record(valid.clone()).expect("record valid change");
        assert_eq!(journal.entries(), std::slice::from_ref(&valid));

        let invalid_actor = ContextChange {
            actor: "/tmp/secret-token".to_owned(),
            ..valid.clone()
        };
        assert!(matches!(
            journal.record(invalid_actor),
            Err(ConfigError::Decode(message))
                if message == "context journal change has an invalid actor"
        ));

        let invalid_context = ContextChange {
            new_context: "../secret".to_owned(),
            ..valid.clone()
        };
        assert!(matches!(
            journal.record(invalid_context),
            Err(ConfigError::Decode(message))
                if message == "context journal change has an invalid context"
        ));
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
    fn empty_programmatic_endpoint_is_rejected_before_resolution() {
        let resolver = ConfigResolver::new(vec![ConfigLayer {
            source: ConfigSource::Command,
            endpoint: Setting::Value(velnor_model::SanitizedUrl::project("")),
            ..ConfigLayer::default()
        }])
        .expect("unique source");

        assert_eq!(
            resolver.resolve().expect_err("reject empty endpoint"),
            ConfigError::Empty("endpoint")
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
    fn valid_credential_reference_round_trips_through_context_store() {
        let temp = RelativeTempDir::new("valid-credential");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path);
        let mut context = test_context();
        context.credential = Some(SecretRef::named("GITHUB_TOKEN"));

        store.set(context.clone()).expect("write context");

        assert_eq!(store.list().expect("list context"), vec![context.clone()]);
        assert_eq!(store.select("primary").expect("select context"), context);
    }

    #[test]
    fn invalid_credential_reference_is_rejected_for_set_and_persisted_context() {
        let temp = RelativeTempDir::new("invalid-credential");
        let path = temp.path.join("nested").join("config.toml");
        let mut invalid_context = test_context();
        invalid_context.credential = Some(SecretRef::named("token=value"));
        let store = FileContextStore::new(path.clone());

        assert_eq!(
            store
                .set(invalid_context.clone())
                .expect_err("reject invalid set"),
            ConfigError::InvalidCredentialReference
        );
        assert!(!path.exists(), "invalid set must not persist a context");

        std::fs::create_dir_all(path.parent().expect("context parent"))
            .expect("create context parent");
        let contents = toml::to_string(&ContextFile {
            contexts: vec![invalid_context],
            current: Some("primary".to_owned()),
        })
        .expect("serialize invalid context");
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| {
                use std::io::Write;
                file.write_all(contents.as_bytes())
            })
            .expect("persist invalid context");
        write_secure_lock(&path);

        assert_eq!(
            FileContextStore::new(path)
                .list()
                .expect_err("reject invalid persisted context"),
            ConfigError::InvalidCredentialReference
        );
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
    fn context_store_rejects_non_private_config_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = RelativeTempDir::new("non-private-mode");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path.clone());
        store.set(test_context()).expect("write context");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("make config world-readable");

        assert_eq!(
            store.list().expect_err("reject non-private config mode"),
            ConfigError::Io("refusing to use context file without exact mode 0600".to_owned())
        );
    }

    #[test]
    fn context_store_rejects_duplicate_context_names() {
        let temp = RelativeTempDir::new("duplicate-context");
        let path = temp.path.join("config.toml");
        let contents = toml::to_string(&ContextFile {
            contexts: vec![test_context(), test_context()],
            current: Some("primary".to_owned()),
        })
        .expect("serialize duplicate contexts");
        write_secure_file(&path, &contents);
        write_secure_lock(&path);

        assert!(matches!(
            FileContextStore::new(path).list(),
            Err(ConfigError::Decode(message))
                if message == "context store contains a duplicate context"
        ));
    }

    #[test]
    fn context_store_rejects_inconsistent_current_marker() {
        let temp = RelativeTempDir::new("invalid-current");
        let path = temp.path.join("config.toml");
        let mut context = test_context();
        context.current = false;
        let contents = toml::to_string(&ContextFile {
            contexts: vec![context],
            current: Some("primary".to_owned()),
        })
        .expect("serialize invalid current marker");
        write_secure_file(&path, &contents);
        write_secure_lock(&path);

        assert!(matches!(
            FileContextStore::new(path).list(),
            Err(ConfigError::Decode(message))
                if message == "context store has an invalid current context marker"
        ));
    }

    #[test]
    fn context_store_rejects_unknown_document_fields() {
        let temp = RelativeTempDir::new("unknown-field");
        let path = temp.path.join("config.toml");
        write_secure_file(
            &path,
            "contexts = []\ncurrent = \"primary\"\nunexpected = true\n",
        );
        write_secure_lock(&path);

        assert!(matches!(
            FileContextStore::new(path).list(),
            Err(ConfigError::Decode(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn context_store_rejects_non_private_lock_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = RelativeTempDir::new("non-private-lock-mode");
        let path = temp.path.join("config.toml");
        let store = FileContextStore::new(path.clone());
        store.set(test_context()).expect("write context");
        let lock = path
            .parent()
            .expect("context parent")
            .join(".config.toml.lock");
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o400))
            .expect("make lock owner-readable only");

        assert_eq!(
            store.list().expect_err("reject non-private lock mode"),
            ConfigError::Io("refusing to use context lock without exact mode 0600".to_owned())
        );
    }

    #[test]
    fn context_store_rejects_existing_file_without_sidecar_lock() {
        let temp = RelativeTempDir::new("absent-lock");
        let path = temp.path.join("config.toml");
        let store = FileContextStore::new(path.clone());
        store.set(test_context()).expect("write context");
        std::fs::remove_file(
            path.parent()
                .expect("context parent")
                .join(".config.toml.lock"),
        )
        .expect("remove sidecar lock");

        assert_eq!(
            store
                .list()
                .expect_err("reject context without sidecar lock"),
            ConfigError::Io("refusing to use context file without sidecar lock".to_owned())
        );
        assert_eq!(
            store
                .set(named_context("secondary"))
                .expect_err("mutator must reject context without sidecar lock"),
            ConfigError::Io("refusing to use context file without sidecar lock".to_owned())
        );
        assert_eq!(
            store
                .use_context("primary")
                .expect_err("selection must reject context without sidecar lock"),
            ConfigError::Io("refusing to use context file without sidecar lock".to_owned())
        );
        assert_eq!(
            store
                .delete("primary")
                .expect_err("deletion must reject context without sidecar lock"),
            ConfigError::Io("refusing to use context file without sidecar lock".to_owned())
        );
        assert!(
            !path
                .parent()
                .expect("context parent")
                .join(".config.toml.lock")
                .exists(),
            "mutators must not recreate a missing lock"
        );
    }

    #[cfg(unix)]
    #[test]
    fn context_store_rejects_fifo_config_before_opening_it() {
        use std::os::unix::ffi::OsStrExt;

        let temp = RelativeTempDir::new("fifo-config");
        let parent = temp.path.join("nested");
        std::fs::create_dir(&parent).expect("context parent");
        let path = parent.join("config.toml");
        let path_bytes = path.as_os_str().as_bytes();
        let path_c = std::ffi::CString::new(path_bytes).expect("config fifo path");
        // SAFETY: The path is a valid, nul-free temporary test path.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);

        assert_eq!(
            FileContextStore::new(path)
                .list()
                .expect_err("reject config fifo"),
            ConfigError::Io("refusing to use a non-regular context file".to_owned())
        );
    }

    #[cfg(unix)]
    #[test]
    fn context_store_rejects_fifo_lock_before_opening_it() {
        use std::os::unix::ffi::OsStrExt;

        let temp = RelativeTempDir::new("fifo-lock");
        let path = temp.path.join("config.toml");
        let store = FileContextStore::new(path.clone());
        store.set(test_context()).expect("write context");
        let parent = path.parent().expect("context parent");
        let lock_name = std::ffi::OsString::from(".config.toml.lock");
        std::fs::remove_file(parent.join(&lock_name)).expect("remove regular lock");
        let lock_path = parent.join(&lock_name);
        let path_bytes = lock_path.as_os_str().as_bytes();
        let path_c = std::ffi::CString::new(path_bytes).expect("lock fifo path");
        // SAFETY: The path is a valid, nul-free temporary test path.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);

        assert_eq!(
            store.list().expect_err("reject lock fifo"),
            ConfigError::Io("refusing to use a non-regular context lock".to_owned())
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
    fn relative_context_store_anchors_path_before_first_use() {
        let temp = RelativeTempDir::new("first-use-cwd");
        let cwd = CurrentDirectoryGuard::new();
        let original = cwd.original.clone();
        let alternate = original.join(&temp.path).join("alternate");
        std::fs::create_dir_all(&alternate).expect("alternate directory");
        let path = temp.path.join("nested").join("config.toml");
        let expected = original.join(&path);
        let store = FileContextStore::new(path);

        cwd.change_to(&alternate);
        store.set(test_context()).expect("write context");

        assert!(expected.is_file());
        assert!(!alternate.join("nested/config.toml").exists());
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
    fn delete_rejects_invalid_context_names_before_touching_filesystem() {
        let temp = RelativeTempDir::new("invalid-delete");
        let path = temp.path.join("nested").join("config.toml");
        let store = FileContextStore::new(path);

        assert_eq!(
            store.delete("../secret").expect_err("reject invalid name"),
            ConfigError::Empty("context name")
        );
        assert!(!temp.path.join("nested").exists());
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
