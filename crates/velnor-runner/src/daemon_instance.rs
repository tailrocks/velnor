//! Packaged daemon instances, resolved the way systemd resolves them.
//!
//! A packaged host runs one `velnor-daemon@<instance>.service` per target
//! scope (plus, optionally, the bare `velnor-daemon.service`). Each unit
//! builds its environment from the shipped unit's `Environment=` lines, its
//! drop-ins, and `EnvironmentFile=/etc/velnor/<instance>.env`, and the daemon
//! derives every path from that environment: the storage layout from
//! `VELNOR_STORAGE_ROOT`, the config directory from `STATE_DIRECTORY`, the
//! work directory from `VELNOR_WORK_DIR`, the trust boundary from
//! `VELNOR_TRUST_SCOPE`, the control socket from the storage root plus
//! `VELNOR_NAME`.
//!
//! `velnorctl` used to read its own process environment for the same
//! answers, so on a packaged host it inspected the untrusted stores of a
//! storage root nobody ran a daemon in, and reached for a control socket
//! under a root no daemon listened on, unless the operator exported the
//! daemon's variables by hand. This module is the one resolver both sides
//! share: it replays the unit's environment for an instance and then calls
//! the same `config`, `storage`, `trust_scope`, and `runner` functions the
//! daemon calls. Nothing here spells a path the daemon does not.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

/// Where packaged instance environment files live.
pub const ETC_DIR: &str = "/etc/velnor";

/// Where systemd looks for drop-ins of the packaged units.
const SYSTEMD_DROP_IN_ROOT: &str = "/etc/systemd/system";

/// The shipped template unit; one instance per `/etc/velnor/<instance>.env`.
const TEMPLATE_UNIT: &str = include_str!("../debian/velnor-daemon@.service");

/// The shipped bare unit, reading `/etc/velnor/velnor.env`.
const BARE_UNIT: &str = include_str!("../debian/velnor-daemon.service");

/// Environment file name of the bare unit, which doubles as its instance name.
pub const BARE_INSTANCE: &str = "velnor";

/// One packaged daemon instance with every path the daemon derives from its
/// unit environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonInstance {
    /// systemd instance name (`%i`), or [`BARE_INSTANCE`] for the bare unit.
    pub instance: String,
    /// The systemd unit that runs this instance.
    pub unit: String,
    /// The environment file that names it.
    pub env_file: PathBuf,
    /// `VELNOR_NAME`: the runner identity, socket instance, and daemon
    /// directory component.
    pub name: String,
    /// `VELNOR_URL`, when set.
    pub url: Option<String>,
    /// `VELNOR_SLOTS`, when set and numeric.
    pub slots: Option<usize>,
    /// `VELNOR_STORAGE_ROOT` (the unit ships `/var`).
    pub storage_root: PathBuf,
    /// The daemon's runtime root (`<storage>/run/velnor`, `/run/velnor` for
    /// `/var`): leases, coordinator locks, job claims, control sockets.
    pub run_root: PathBuf,
    /// `<storage>/lib/velnor`.
    pub lib_root: PathBuf,
    /// `<storage>/cache/velnor/v1`.
    pub cache_root: PathBuf,
    /// `<storage>/log/velnor`.
    pub log_root: PathBuf,
    /// `STATE_DIRECTORY` as systemd exports it for the unit.
    pub state_directory: PathBuf,
    /// The daemon's config base: `config::resolve_config_dir` over the unit
    /// environment.
    pub config_dir: PathBuf,
    /// The daemon-scoped directory (`journal.db`, `health.sock`, slot
    /// configs): `runner::daemon_config_dir` over the unit environment.
    pub daemon_dir: PathBuf,
    /// `VELNOR_WORK_DIR`, or the daemon's default under the config base.
    pub work_dir: PathBuf,
    /// The pool trust boundary the daemon resolves from `VELNOR_TRUST_SCOPE`.
    pub trust_scope: String,
    /// The directory holding `control.sock` and `admin.sock`.
    pub socket_dir: PathBuf,
    /// The full unit environment (secrets files excluded).
    pub environment: BTreeMap<String, String>,
}

impl DaemonInstance {
    /// `<socket_dir>/control.sock`.
    #[must_use]
    pub fn control_socket(&self) -> PathBuf {
        self.socket_dir.join("control.sock")
    }

    /// `<daemon_dir>/health.sock`.
    #[must_use]
    pub fn health_socket(&self) -> PathBuf {
        self.daemon_dir.join("health.sock")
    }

    /// `<daemon_dir>/journal.db`.
    #[must_use]
    pub fn journal_db(&self) -> PathBuf {
        self.daemon_dir.join("journal.db")
    }

    /// The storage layout the daemon runs under.
    pub(crate) fn storage_layout(&self) -> crate::storage::StorageLayout {
        crate::storage::StorageLayout::from_prefix(&self.storage_root)
    }
}

/// Every packaged instance configured on this host, by instance name.
///
/// An instance is a `/etc/velnor/<instance>.env` file whose stem has no
/// further dots (`dogfood.env`, not `dogfood.secrets.env`, not
/// `dogfood.env.bak`). `velnor.env` is the bare `velnor-daemon.service`.
pub fn enumerate() -> Result<Vec<DaemonInstance>> {
    enumerate_in(Path::new(ETC_DIR), Path::new(SYSTEMD_DROP_IN_ROOT))
}

/// One packaged instance by systemd instance name, or by `VELNOR_NAME`.
pub fn resolve(selector: &str) -> Result<DaemonInstance> {
    resolve_in(
        Path::new(ETC_DIR),
        Path::new(SYSTEMD_DROP_IN_ROOT),
        selector,
    )
}

/// Whether this host has any packaged instance at all. A host without
/// `/etc/velnor/*.env` is a development machine, where the operator CLI
/// resolves its own process environment exactly as a development daemon does.
pub fn any_configured() -> bool {
    enumerate().map(|found| !found.is_empty()).unwrap_or(false)
}

pub fn enumerate_in(etc: &Path, drop_in_root: &Path) -> Result<Vec<DaemonInstance>> {
    let entries = match fs::read_dir(etc) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", etc.display())),
    };
    let mut instances = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("read an entry in {}", etc.display()))?;
        let path = entry.path();
        let Some(instance) = instance_name_of_env_file(&path) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        instances.push(resolve_instance_in(etc, drop_in_root, &instance)?);
    }
    instances.sort_by(|left, right| left.instance.cmp(&right.instance));
    Ok(instances)
}

pub fn resolve_in(etc: &Path, drop_in_root: &Path, selector: &str) -> Result<DaemonInstance> {
    let selector = selector.trim();
    if selector.is_empty() {
        bail!("instance selector is empty");
    }
    if etc.join(format!("{selector}.env")).is_file() {
        return resolve_instance_in(etc, drop_in_root, selector);
    }
    let instances = enumerate_in(etc, drop_in_root)?;
    let mut by_name = instances
        .iter()
        .filter(|instance| instance.name == selector);
    match (by_name.next(), by_name.next()) {
        (Some(instance), None) => Ok(instance.clone()),
        (Some(_), Some(_)) => bail!(
            "VELNOR_NAME {selector} is shared by more than one instance under {}; \
             select by instance name",
            etc.display()
        ),
        (None, _) => {
            let known: Vec<&str> = instances
                .iter()
                .map(|instance| instance.instance.as_str())
                .collect();
            if known.is_empty() {
                bail!(
                    "no packaged daemon instance {selector}: {} has no <instance>.env files",
                    etc.display()
                );
            }
            bail!(
                "no packaged daemon instance {selector} under {}; known instances: {}",
                etc.display(),
                known.join(", ")
            );
        }
    }
}

/// `<instance>.env` → `instance`; anything else (secrets, backups, the
/// `execution.toml`) → `None`.
fn instance_name_of_env_file(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".env")?;
    if stem.is_empty() || stem.contains('.') || stem.contains('/') {
        return None;
    }
    Some(stem.to_owned())
}

fn resolve_instance_in(etc: &Path, drop_in_root: &Path, instance: &str) -> Result<DaemonInstance> {
    let (unit_text, unit_name, systemd_instance) = if instance == BARE_INSTANCE {
        (BARE_UNIT, "velnor-daemon.service".to_owned(), None)
    } else {
        (
            TEMPLATE_UNIT,
            format!("velnor-daemon@{instance}.service"),
            Some(instance),
        )
    };
    let mut unit = UnitEnvironment::parse(unit_text, systemd_instance)
        .with_context(|| format!("parse shipped unit for {unit_name}"))?;
    // Drop-ins layer over the shipped fragment in systemd's order: the
    // template's `.d` first, then the instance's own `.d`, each sorted.
    for drop_in_dir in drop_in_dirs(drop_in_root, &unit_name, systemd_instance) {
        for drop_in in sorted_conf_files(&drop_in_dir)? {
            let text = fs::read_to_string(&drop_in)
                .with_context(|| format!("read drop-in {}", drop_in.display()))?;
            let layered = UnitEnvironment::parse(&text, systemd_instance)
                .with_context(|| format!("parse drop-in {}", drop_in.display()))?;
            unit.layer(layered);
        }
    }
    let env_file = etc.join(format!("{instance}.env"));
    let mut environment = unit.environment.clone();
    for file in &unit.environment_files {
        // The unit lists `/etc/velnor/...`; resolve relative to the `etc`
        // under inspection so tests can stage a directory.
        let path = relocate_under_etc(&file.path, etc);
        if is_secrets_file(&path) {
            // The operator CLI resolves paths, not credentials. Secrets files
            // carry only the GitHub token and are unreadable to most callers.
            continue;
        }
        match fs::read_to_string(&path) {
            Ok(text) => {
                for (key, value) in parse_environment_file(&text)
                    .with_context(|| format!("parse {}", path.display()))?
                {
                    environment.insert(key, value);
                }
            }
            Err(error) if file.optional && error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", path.display()));
            }
        }
    }

    let state_directory = unit
        .state_directory
        .as_deref()
        .map(systemd_state_directory_path)
        .with_context(|| format!("{unit_name} declares no StateDirectory="))?;
    let storage_root = environment
        .get("VELNOR_STORAGE_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .with_context(|| format!("{unit_name} environment sets no VELNOR_STORAGE_ROOT"))?;
    let name = environment
        .get("VELNOR_NAME")
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_owned())
        .with_context(|| format!("{} sets no VELNOR_NAME", env_file.display()))?;
    let config_dir = crate::config::resolve_config_dir(crate::config::ResolveConfigDir {
        explicit: None,
        velnor_config_dir: environment.get("VELNOR_CONFIG_DIR").map(Into::into),
        state_directory: Some(state_directory.clone().into()),
        velnor_storage_root: Some(storage_root.clone().into()),
        xdg_state_home: None,
        home: None,
    })?;
    let daemon_dir = crate::runner::daemon_config_dir_under(&config_dir, Some(&name));
    let work_dir = environment
        .get("VELNOR_WORK_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::runner::default_daemon_work_dir(&daemon_dir));
    let trust_scope =
        crate::trust_scope::configured(environment.get("VELNOR_TRUST_SCOPE").map(String::as_str));
    let layout = crate::storage::StorageLayout::from_prefix(&storage_root);
    // The control-plane socket root is the storage layout's runtime root
    // (`velnor_client::socket_root_for_storage_root` spells the same rule; the
    // velnorctl test `packaged_socket_root_is_the_instance_run_root` pins the
    // two together). Under it, the daemon binds `<VELNOR_NAME>/control.sock`.
    let socket_dir = layout.run_root.join(&name);
    let slots = environment
        .get("VELNOR_SLOTS")
        .and_then(|value| value.trim().parse::<usize>().ok());

    Ok(DaemonInstance {
        instance: instance.to_owned(),
        unit: unit_name,
        env_file,
        name,
        url: environment
            .get("VELNOR_URL")
            .filter(|value| !value.trim().is_empty())
            .cloned(),
        slots,
        storage_root,
        run_root: layout.run_root,
        lib_root: layout.lib_root,
        cache_root: layout.cache_root,
        log_root: layout.log_root,
        state_directory,
        config_dir,
        daemon_dir,
        work_dir,
        trust_scope,
        socket_dir,
        environment,
    })
}

fn drop_in_dirs(root: &Path, unit_name: &str, instance: Option<&str>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if instance.is_some() {
        dirs.push(root.join("velnor-daemon@.service.d"));
    }
    dirs.push(root.join(format!("{unit_name}.d")));
    dirs
}

fn sorted_conf_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", dir.display())),
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "conf") && path.is_file())
        .collect();
    files.sort();
    Ok(files)
}

/// systemd.exec: a relative `StateDirectory=name` lives under `/var/lib`.
fn systemd_state_directory_path(value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new("/var/lib").join(path)
    }
}

fn relocate_under_etc(path: &Path, etc: &Path) -> PathBuf {
    match path.strip_prefix(ETC_DIR) {
        Ok(relative) => etc.join(relative),
        Err(_) => path.to_path_buf(),
    }
}

fn is_secrets_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "secrets.env" || name.ends_with(".secrets.env"))
}

/// The `[Service]` environment declarations of one unit fragment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct UnitEnvironment {
    environment: BTreeMap<String, String>,
    environment_files: Vec<EnvironmentFile>,
    state_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvironmentFile {
    path: PathBuf,
    optional: bool,
}

impl UnitEnvironment {
    /// Parse `Environment=`, `EnvironmentFile=`, and `StateDirectory=` with
    /// `%i` replaced by `instance`. Other directives are ignored.
    fn parse(text: &str, instance: Option<&str>) -> Result<Self> {
        let mut unit = Self::default();
        for raw in logical_lines(text) {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = specifiers(value.trim(), instance);
            match key.trim() {
                "Environment" => {
                    if value.is_empty() {
                        // `Environment=` alone resets the list.
                        unit.environment.clear();
                        continue;
                    }
                    for assignment in split_quoted_words(&value)? {
                        let (name, val) = assignment.split_once('=').with_context(|| {
                            format!("Environment= entry without '=': {assignment}")
                        })?;
                        unit.environment.insert(name.to_owned(), val.to_owned());
                    }
                }
                "EnvironmentFile" => {
                    if value.is_empty() {
                        unit.environment_files.clear();
                        continue;
                    }
                    let (optional, path) = match value.strip_prefix('-') {
                        Some(rest) => (true, rest),
                        None => (false, value.as_str()),
                    };
                    unit.environment_files.push(EnvironmentFile {
                        path: PathBuf::from(path),
                        optional,
                    });
                }
                "StateDirectory" => {
                    unit.state_directory = if value.is_empty() {
                        None
                    } else {
                        // Several may be listed; the daemon's config resolver
                        // takes the first, as systemd exports it first.
                        value.split_whitespace().next().map(str::to_owned)
                    };
                }
                _ => {}
            }
        }
        Ok(unit)
    }

    /// Apply a drop-in over this fragment with systemd's semantics: list
    /// directives append (or reset when empty, handled in `parse` by the
    /// drop-in's own state), scalars replace.
    fn layer(&mut self, other: Self) {
        self.environment.extend(other.environment);
        self.environment_files.extend(other.environment_files);
        if other.state_directory.is_some() {
            self.state_directory = other.state_directory;
        }
    }
}

/// systemd `%i` (instance) and `%%` specifiers; everything else is left as is.
fn specifiers(value: &str, instance: Option<&str>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('i') => out.push_str(instance.unwrap_or("")),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Join backslash-continued lines, as systemd does for unit and env files.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if let Some(stripped) = line.strip_suffix('\\') {
            current.push_str(stripped);
            current.push(' ');
            continue;
        }
        current.push_str(line);
        lines.push(std::mem::take(&mut current));
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Split on whitespace, honouring single and double quotes.
fn split_quoted_words(value: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut in_word = false;
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some('"') if ch == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                in_word = true;
            }
            None if ch.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            None if ch == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                    in_word = true;
                }
            }
            None => {
                current.push(ch);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        bail!("unterminated quote in {value:?}");
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

/// Parse a systemd `EnvironmentFile=`: `KEY=VALUE` per logical line,
/// comments on `#`/`;`, optional single or double quotes around the value,
/// backslash continuation.
fn parse_environment_file(text: &str) -> Result<Vec<(String, String)>> {
    let mut assignments = Vec::new();
    for raw in logical_lines(text) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("environment file line without '=': {line:?}");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("environment file line with empty key: {line:?}");
        }
        let value = value.trim();
        let value = match value.chars().next() {
            Some(open @ ('"' | '\'')) if value.len() >= 2 && value.ends_with(open) => {
                let inner = &value[1..value.len() - 1];
                if open == '"' {
                    unescape_double_quoted(inner)
                } else {
                    inner.to_owned()
                }
            }
            _ => value.to_owned(),
        };
        assignments.push((key.to_owned(), value));
    }
    Ok(assignments)
}

fn unescape_double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-daemon-instance-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    /// The exact files the Sentry audit found: a trusted instance with its
    /// own work dir, a secrets sibling, a backup copy, the bare unit's env,
    /// and the package's execution.toml.
    fn stage_sentry_like_etc(root: &Path) -> PathBuf {
        let etc = root.join("etc/velnor");
        fs::create_dir_all(&etc).unwrap();
        fs::write(
            etc.join("dogfood.env"),
            "VELNOR_URL=https://github.com/tailrocks/velnor\n\
             VELNOR_NAME=velnor-dogfood\n\
             VELNOR_LABELS=velnor,velnor-target-mvp,dogfood\n\
             VELNOR_SLOTS=5\n\
             VELNOR_WORK_DIR=/var/lib/velnor-dogfood/work\n\
             VELNOR_TRUST_SCOPE=trusted\n\
             VELNOR_GITHUB_HTTP_TRANSPORT=native\n",
        )
        .unwrap();
        fs::write(etc.join("dogfood.secrets.env"), "GITHUB_TOKEN=ghp_secret\n").unwrap();
        fs::write(etc.join("dogfood.env.bak-persist"), "VELNOR_NAME=stale\n").unwrap();
        fs::write(
            etc.join("velnor.env"),
            "# bare unit\n\
             VELNOR_URL=https://github.com/ChainArgos/java-monorepo\n\
             VELNOR_NAME=\"velnor-java-monorepo\"\n\
             VELNOR_SLOTS=4\n\
             VELNOR_WORK_DIR=/var/lib/velnor/work\n\
             VELNOR_TRUST_SCOPE=trusted\n",
        )
        .unwrap();
        fs::write(
            etc.join("fixture.env"),
            "VELNOR_URL=https://github.com/tailrocks/velnor-actions-fixture\n\
             VELNOR_NAME=velnor-fixture\n\
             VELNOR_SLOTS=2\n\
             VELNOR_WORK_DIR=/var/lib/velnor-fixture/work\n",
        )
        .unwrap();
        fs::write(
            etc.join("execution.toml"),
            "[execution]\nbackend = \"docker\"\n",
        )
        .unwrap();
        etc
    }

    #[test]
    fn template_instance_resolves_every_path_the_daemon_derives() {
        let root = temp_root("template");
        let etc = stage_sentry_like_etc(&root);
        let drop_ins = root.join("systemd");

        let dogfood = resolve_in(&etc, &drop_ins, "dogfood").unwrap();
        assert_eq!(dogfood.instance, "dogfood");
        assert_eq!(dogfood.unit, "velnor-daemon@dogfood.service");
        assert_eq!(dogfood.name, "velnor-dogfood");
        assert_eq!(dogfood.slots, Some(5));
        assert_eq!(dogfood.storage_root, PathBuf::from("/var"));
        assert_eq!(dogfood.run_root, PathBuf::from("/run/velnor"));
        assert_eq!(dogfood.lib_root, PathBuf::from("/var/lib/velnor"));
        assert_eq!(
            dogfood.state_directory,
            PathBuf::from("/var/lib/velnor-dogfood")
        );
        // The same resolver the daemon uses: STATE_DIRECTORY wins.
        assert_eq!(
            dogfood.config_dir,
            PathBuf::from("/var/lib/velnor-dogfood/runner")
        );
        assert_eq!(
            dogfood.daemon_dir,
            PathBuf::from("/var/lib/velnor-dogfood/runner/daemons/velnor-dogfood")
        );
        assert_eq!(
            dogfood.health_socket(),
            PathBuf::from("/var/lib/velnor-dogfood/runner/daemons/velnor-dogfood/health.sock")
        );
        assert_eq!(
            dogfood.work_dir,
            PathBuf::from("/var/lib/velnor-dogfood/work")
        );
        assert_eq!(dogfood.trust_scope, "trusted");
        assert_eq!(
            dogfood.control_socket(),
            PathBuf::from("/run/velnor/velnor-dogfood/control.sock")
        );
        // Secrets are never loaded.
        assert!(!dogfood.environment.contains_key("GITHUB_TOKEN"));
        // The unit's own environment is present.
        assert_eq!(
            dogfood.environment.get("VELNOR_CAPABILITY_VALIDATION"),
            Some(&"strict".to_owned())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bare_unit_and_untrusted_defaults_follow_the_daemon() {
        let root = temp_root("bare");
        let etc = stage_sentry_like_etc(&root);
        let drop_ins = root.join("systemd");

        let bare = resolve_in(&etc, &drop_ins, BARE_INSTANCE).unwrap();
        assert_eq!(bare.unit, "velnor-daemon.service");
        assert_eq!(bare.name, "velnor-java-monorepo");
        assert_eq!(bare.state_directory, PathBuf::from("/var/lib/velnor"));
        assert_eq!(bare.config_dir, PathBuf::from("/var/lib/velnor/runner"));
        assert_eq!(
            bare.daemon_dir,
            PathBuf::from("/var/lib/velnor/runner/daemons/velnor-java-monorepo")
        );

        // No VELNOR_TRUST_SCOPE: the daemon's flag default, fail closed.
        let fixture = resolve_in(&etc, &drop_ins, "fixture").unwrap();
        assert_eq!(fixture.trust_scope, crate::trust_scope::FAIL_CLOSED);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enumeration_skips_secrets_backups_and_non_env_files() {
        let root = temp_root("enumerate");
        let etc = stage_sentry_like_etc(&root);
        let drop_ins = root.join("systemd");

        let names: Vec<String> = enumerate_in(&etc, &drop_ins)
            .unwrap()
            .into_iter()
            .map(|instance| instance.instance)
            .collect();
        assert_eq!(names, ["dogfood", "fixture", "velnor"]);

        // A host without /etc/velnor has no packaged instances.
        assert!(enumerate_in(&root.join("missing"), &drop_ins)
            .unwrap()
            .is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selector_accepts_velnor_name_and_names_known_instances_on_miss() {
        let root = temp_root("selector");
        let etc = stage_sentry_like_etc(&root);
        let drop_ins = root.join("systemd");

        let by_name = resolve_in(&etc, &drop_ins, "velnor-dogfood").unwrap();
        assert_eq!(by_name.instance, "dogfood");

        let missing = resolve_in(&etc, &drop_ins, "nope").unwrap_err().to_string();
        assert!(
            missing.contains("known instances: dogfood, fixture, velnor"),
            "{missing}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn drop_ins_and_env_file_override_the_shipped_unit_in_systemd_order() {
        let root = temp_root("drop-ins");
        let etc = stage_sentry_like_etc(&root);
        let drop_ins = root.join("systemd");
        fs::create_dir_all(drop_ins.join("velnor-daemon@.service.d")).unwrap();
        fs::create_dir_all(drop_ins.join("velnor-daemon@dogfood.service.d")).unwrap();
        fs::write(
            drop_ins.join("velnor-daemon@.service.d/10-storage.conf"),
            "[Service]\nEnvironment=VELNOR_STORAGE_ROOT=/srv/velnor-%i VELNOR_JOB_PEAK_BYTES=1\n",
        )
        .unwrap();
        fs::write(
            drop_ins.join("velnor-daemon@dogfood.service.d/override.conf"),
            "[Service]\nEnvironment=\"VELNOR_JOB_PEAK_BYTES=2\"\n",
        )
        .unwrap();

        let dogfood = resolve_in(&etc, &drop_ins, "dogfood").unwrap();
        assert_eq!(dogfood.storage_root, PathBuf::from("/srv/velnor-dogfood"));
        assert_eq!(
            dogfood.run_root,
            PathBuf::from("/srv/velnor-dogfood/run/velnor")
        );
        assert_eq!(
            dogfood.control_socket(),
            PathBuf::from("/srv/velnor-dogfood/run/velnor/velnor-dogfood/control.sock")
        );
        assert_eq!(
            dogfood.environment.get("VELNOR_JOB_PEAK_BYTES"),
            Some(&"2".to_owned())
        );

        // The env file wins over unit and drop-in environment.
        fs::write(
            etc.join("dogfood.env"),
            "VELNOR_NAME=velnor-dogfood\nVELNOR_STORAGE_ROOT=/var/lib/velnor-dogfood/store\n",
        )
        .unwrap();
        let moved = resolve_in(&etc, &drop_ins, "dogfood").unwrap();
        assert_eq!(
            moved.storage_root,
            PathBuf::from("/var/lib/velnor-dogfood/store")
        );
        assert_eq!(
            moved.run_root,
            PathBuf::from("/var/lib/velnor-dogfood/store/run/velnor")
        );
        // No VELNOR_WORK_DIR: the daemon's default under its config base.
        assert_eq!(
            moved.work_dir,
            PathBuf::from("/var/lib/velnor-dogfood/runner/daemons/velnor-dogfood/_work")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shipped_units_parse_to_the_environment_systemd_shows() {
        let template = UnitEnvironment::parse(TEMPLATE_UNIT, Some("dogfood")).unwrap();
        assert_eq!(
            template.environment.get("VELNOR_STORAGE_ROOT"),
            Some(&"/var".to_owned())
        );
        assert_eq!(template.state_directory.as_deref(), Some("velnor-dogfood"));
        assert_eq!(
            template.environment_files,
            vec![
                EnvironmentFile {
                    path: PathBuf::from("/etc/velnor/dogfood.env"),
                    optional: false,
                },
                EnvironmentFile {
                    path: PathBuf::from("/etc/velnor/dogfood.secrets.env"),
                    optional: true,
                },
            ]
        );

        let bare = UnitEnvironment::parse(BARE_UNIT, None).unwrap();
        assert_eq!(bare.state_directory.as_deref(), Some("velnor"));
        assert_eq!(
            bare.environment_files[0].path,
            PathBuf::from("/etc/velnor/velnor.env")
        );
    }

    #[test]
    fn environment_file_syntax_matches_systemd() {
        let parsed = parse_environment_file(
            "# comment\n\
             ; another\n\
             PLAIN=value with spaces  \n\
             DOUBLE=\"quoted \\\"inner\\\" value\"\n\
             SINGLE='single quoted'\n\
             CONTINUED=first \\\n\
             second\n\
             EMPTY=\n",
        )
        .unwrap();
        assert_eq!(
            parsed,
            vec![
                ("PLAIN".to_owned(), "value with spaces".to_owned()),
                ("DOUBLE".to_owned(), "quoted \"inner\" value".to_owned()),
                ("SINGLE".to_owned(), "single quoted".to_owned()),
                ("CONTINUED".to_owned(), "first  second".to_owned()),
                ("EMPTY".to_owned(), String::new()),
            ]
        );
        assert!(parse_environment_file("NOEQUALS\n").is_err());
        assert_eq!(
            split_quoted_words("A=1 \"B=two words\" C='x y'").unwrap(),
            ["A=1", "B=two words", "C=x y"]
        );
    }
    /// The packaged gc timer must run a command that can reclaim every
    /// instance: no single instance's env file, no hard-wired work dir, so
    /// `velnorctl cache gc` enumerates `/etc/velnor/*.env` itself and applies
    /// each instance's storage root and trust scope. Both units must be in the
    /// deb's asset list, or the timer the runbook tells operators to enable
    /// does not exist on the host (as on Sentry at v0.1.158).
    #[test]
    fn packaged_cache_gc_unit_reclaims_every_instance_and_is_shipped() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let unit_path = manifest_dir.join("../velnor-tools/debian/velnor-cache-gc.service");
        let unit = fs::read_to_string(&unit_path).unwrap();
        let exec_start = unit
            .lines()
            .find_map(|line| line.strip_prefix("ExecStart="))
            .expect("ExecStart=");
        assert!(
            exec_start.ends_with("/usr/bin/velnorctl cache gc --yes"),
            "gc must let velnorctl enumerate the instances: {exec_start}"
        );
        assert!(
            exec_start.contains("flock --shared --no-fork /run/velnor/package-transaction.lock"),
            "gc must not run during a package transaction: {exec_start}"
        );
        let directives = unit
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        for forbidden in [
            "EnvironmentFile=",
            "--work-dir",
            "VELNOR_WORK_DIR",
            "--instance",
        ] {
            assert!(
                !directives.contains(forbidden),
                "{} pins gc to one instance via {forbidden}",
                unit_path.display()
            );
        }
        let timer =
            fs::read_to_string(manifest_dir.join("../velnor-tools/debian/velnor-cache-gc.timer"))
                .unwrap();
        assert!(timer.contains("WantedBy=timers.target"), "{timer}");

        let cargo_toml = fs::read_to_string(manifest_dir.join("Cargo.toml")).unwrap();
        for asset in [
            "[\"../velnor-tools/debian/velnor-cache-gc.service\", \"lib/systemd/system/velnor-cache-gc.service\", \"644\"]",
            "[\"../velnor-tools/debian/velnor-cache-gc.timer\", \"lib/systemd/system/velnor-cache-gc.timer\", \"644\"]",
        ] {
            assert!(cargo_toml.contains(asset), "deb assets do not ship {asset}");
        }
        // The package enables the timer itself (a real command, not an echoed
        // hint), honouring an operator mask. That is only sound because the
        // maintainer-script gates exempt lock-serialised maintenance oneshots
        // and their timers: the old gate refused to configure while any
        // `velnor*.timer` was active, so enabling this one would have blocked
        // every later upgrade — in preinst as much as in postinst.
        let postinst = fs::read_to_string(manifest_dir.join("debian/postinst")).unwrap();
        let enable = postinst
            .lines()
            .find(|line| line.contains("systemctl enable --now velnor-cache-gc.timer"))
            .expect("postinst enables velnor-cache-gc.timer");
        assert!(
            !enable.trim_start().starts_with("echo"),
            "enablement must be a command, not an operator hint: {enable}"
        );
        assert!(postinst.contains("masked|masked-runtime)"));
        for script in ["debian/postinst", "debian/preinst"] {
            let text = fs::read_to_string(manifest_dir.join(script)).unwrap();
            assert!(
                text.contains("scheduled_oneshot_under_transaction_lock \"$unit\" && continue"),
                "{script} must exempt lock-serialised oneshots and their timers from the drain gate"
            );
            assert!(text.contains(
                "*\"argv[]=/usr/bin/flock --shared --no-fork $PACKAGE_TRANSACTION_LOCK \"*) return 0 ;;"
            ));
            assert!(text.contains(
                "[ \"$(systemctl show --property=Type --value \"$1\" 2>/dev/null || true)\" = oneshot ] || return 1"
            ));
        }
        // The exemption is exactly what the shipped gc unit satisfies.
        assert!(unit.contains("Type=oneshot"));
    }
}
