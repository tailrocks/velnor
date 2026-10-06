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
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

/// Where packaged instance environment files live.
pub const ETC_DIR: &str = "/etc/velnor";

/// systemd's system-manager unit lookup roots, from highest to lowest
/// priority. `/lib` is conditional compatibility support and follows
/// `/usr/lib` in this resolver; merged-/usr systems resolve it to that same
/// directory.
const SYSTEMD_DROP_IN_ROOTS: &[&str] = &[
    "etc/systemd/system.control",
    "run/systemd/system.control",
    "run/systemd/transient",
    "run/systemd/generator.early",
    "etc/systemd/system",
    "etc/systemd/system.attached",
    "run/systemd/system",
    "run/systemd/system.attached",
    "run/systemd/generator",
    "usr/local/lib/systemd/system",
    "usr/lib/systemd/system",
    "lib/systemd/system",
    "run/systemd/generator.late",
];

/// The shipped template unit; one instance per `/etc/velnor/<instance>.env`.
const TEMPLATE_UNIT: &str = include_str!("../debian/velnor-daemon@.service");

/// The shipped bare unit, reading `/etc/velnor/velnor.env`.
const BARE_UNIT: &str = include_str!("../debian/velnor-daemon.service");

/// Shipped non-daemon units may keep their own configuration in `/etc/velnor`.
/// Those files are not instance records even when their basename also fits the
/// daemon template's `<instance>.env` grammar.
const NON_DAEMON_SERVICE_UNITS: &[&str] = &[include_str!(
    "../../velnor-tools/debian/velnor-fleet-policy-audit.service"
)];

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
/// An instance is a `/etc/velnor/<instance>.env` file whose stem is a valid
/// systemd instance name. Dots are valid inside the name; secret siblings end
/// in `.secrets.env` and are excluded before this grammar is applied.
/// `velnor.env` is the bare `velnor-daemon.service`.
pub fn enumerate() -> Result<Vec<DaemonInstance>> {
    enumerate_in(Path::new(ETC_DIR), Path::new("/"))
}

/// One packaged instance by systemd instance name, or by `VELNOR_NAME`.
pub fn resolve(selector: &str) -> Result<DaemonInstance> {
    resolve_in(Path::new(ETC_DIR), Path::new("/"), selector)
}

/// Whether this host has any packaged instance at all. A host without
/// `/etc/velnor/*.env` is a development machine, where the operator CLI
/// resolves its own process environment exactly as a development daemon does.
pub fn any_configured() -> bool {
    enumerate().map(|found| !found.is_empty()).unwrap_or(false)
}

/// Enumerate instances using `systemd_root` as the filesystem root for
/// the system-manager unit lookup paths. Pass `/` on the host or a staged
/// filesystem root in tests.
pub fn enumerate_in(etc: &Path, systemd_root: &Path) -> Result<Vec<DaemonInstance>> {
    let entries = match fs::read_dir(etc) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("read {}", etc.display())),
    };
    let mut instances = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("read an entry in {}", etc.display()))?;
        let path = entry.path();
        let Some(instance) = daemon_instance_name_of_env_file(&path) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        instances.push(resolve_instance_in(etc, systemd_root, &instance)?);
    }
    instances.sort_by(|left, right| left.instance.cmp(&right.instance));
    Ok(instances)
}

/// Resolve one instance using `systemd_root` as the filesystem root for
/// systemd's drop-in directories. Pass `/` on the host or a staged root in
/// tests.
pub fn resolve_in(etc: &Path, systemd_root: &Path, selector: &str) -> Result<DaemonInstance> {
    let selector = selector.trim();
    if selector.is_empty() {
        bail!("instance selector is empty");
    }
    let selected_env = etc.join(format!("{selector}.env"));
    if daemon_instance_name_of_env_file(&selected_env).as_deref() == Some(selector)
        && selected_env.is_file()
    {
        return resolve_instance_in(etc, systemd_root, selector);
    }
    let instances = enumerate_in(etc, systemd_root)?;
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
pub(crate) fn daemon_instance_name_of_env_file(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    if is_secrets_file(path) || is_non_daemon_service_env_file_name(file_name) {
        return None;
    }
    let stem = file_name.strip_suffix(".env")?;
    if !valid_instance_name(stem) {
        return None;
    }
    Some(stem.to_owned())
}

/// Accepted instance bytes can be embedded in the shipped `@.service` unit
/// without systemd escaping or path traversal.
pub(crate) fn valid_instance_name(instance: &str) -> bool {
    !matches!(instance, "" | "." | "..")
        && format!("velnor-daemon@{instance}.service").len() <= 255
        && instance
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

/// Whether a basename is an environment file consumed by a shipped
/// non-daemon Velnor unit. Derive the names from those units' actual
/// `EnvironmentFile=` directives so daemon discovery and the package inventory
/// use the same classification.
pub(crate) fn is_non_daemon_service_env_file_name(file_name: &str) -> bool {
    NON_DAEMON_SERVICE_UNITS.iter().any(|unit| {
        unit.lines().any(|line| {
            let Some(value) = line.trim().strip_prefix("EnvironmentFile=") else {
                return false;
            };
            let value = value.trim().strip_prefix('-').unwrap_or(value.trim());
            Path::new(value).file_name().and_then(|name| name.to_str()) == Some(file_name)
        })
    })
}

/// Validate an env file reserved for a shipped non-daemon service. These files
/// are deliberately excluded from daemon discovery, but still must not be a
/// symlink, malformed environment, or a hidden daemon-root override.
pub(crate) fn verify_non_daemon_service_env_file(path: &Path) -> Result<()> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("non-daemon service environment filename is not UTF-8")?;
    if !is_non_daemon_service_env_file_name(file_name) {
        bail!("environment file is not reserved for a shipped non-daemon service");
    }
    let metadata = fs::symlink_metadata(path).with_context(|| {
        format!(
            "inspect non-daemon service environment file {}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "non-daemon service environment entry is not a regular file: {}",
            path.display()
        );
    }
    let text = fs::read_to_string(path).with_context(|| {
        format!(
            "read non-daemon service environment file {}",
            path.display()
        )
    })?;
    for (key, _) in parse_environment_file(&text)? {
        if matches!(
            key.as_str(),
            "VELNOR_NAME"
                | "VELNOR_LABELS"
                | "VELNOR_STORAGE_ROOT"
                | "VELNOR_WORK_DIR"
                | "VELNOR_SLOTS"
                | "VELNOR_TRUST_SCOPE"
        ) {
            bail!(
                "non-daemon service environment file contains a daemon-root override: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn resolve_instance_in(etc: &Path, systemd_root: &Path, instance: &str) -> Result<DaemonInstance> {
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
    // systemd searches configuration roots by precedence, then its exact,
    // template, dash-prefix, and type-level candidates within each root. It
    // keeps the first file for each basename, then parses survivors globally
    // in filename order.
    for drop_in in sorted_conf_files(systemd_root, drop_in_dirs(systemd_root, &unit_name))? {
        if drop_in.masked {
            continue;
        }
        let text = fs::read_to_string(&drop_in.path)
            .with_context(|| format!("read drop-in {}", drop_in.path.display()))?;
        let layered = UnitEnvironment::parse(&text, systemd_instance)
            .with_context(|| format!("parse drop-in {}", drop_in.path.display()))?;
        unit.layer(layered);
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

fn drop_in_dirs(systemd_root: &Path, unit_name: &str) -> Vec<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let candidates = drop_in_candidate_names(unit_name);
    for root in SYSTEMD_DROP_IN_ROOTS {
        let config_dir = systemd_root.join(root);
        dirs.push(
            candidates
                .iter()
                .map(|candidate| config_dir.join(format!("{candidate}.d")))
                .collect(),
        );
    }
    // systemd searches the unit-type drop-in family only after every exact,
    // template, and prefix candidate in every lookup root.
    for root in SYSTEMD_DROP_IN_ROOTS {
        let config_dir = systemd_root.join(root);
        dirs.push(vec![config_dir.join("service.d")]);
    }
    dirs
}

fn drop_in_candidate_names(unit_name: &str) -> Vec<String> {
    fn expand(name: &str, names: &mut Vec<String>) {
        if names.iter().any(|candidate| candidate == name) {
            return;
        }
        names.push(name.to_owned());

        let Some(stem) = name.strip_suffix(".service") else {
            return;
        };
        let (prefix, instance) = match stem.split_once('@') {
            Some((prefix, instance)) if !instance.is_empty() => (prefix, Some(instance)),
            Some((prefix, _)) => (prefix, None),
            None => (stem, None),
        };

        if instance.is_some() {
            expand(&format!("{prefix}@.service"), names);
        }

        // Mirror unit_file_expand_dropin_names(): trim one trailing dash,
        // then retain the next dash so multi-level prefixes recurse in
        // systemd's candidate order.
        let mut prefix = prefix.to_owned();
        let mut chopped = false;
        loop {
            let Some(dash) = prefix.rfind('-') else {
                return;
            };
            if dash == 0 {
                return;
            }
            if dash + 1 < prefix.len() || chopped {
                prefix.truncate(dash + 1);
                break;
            }
            prefix.truncate(dash);
            chopped = true;
        }

        let candidate = match instance {
            Some(instance) => format!("{prefix}@{instance}.service"),
            None => format!("{prefix}.service"),
        };
        expand(&candidate, names);
    }

    let mut names = Vec::new();
    expand(unit_name, &mut names);
    names
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DropInFile {
    path: PathBuf,
    masked: bool,
}

#[derive(Debug)]
enum RootedPathComponent {
    Parent,
    Normal(OsString),
}

fn rooted_path_components(path: &Path) -> Result<VecDeque<RootedPathComponent>> {
    let mut components = VecDeque::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir | std::path::Component::RootDir => {}
            std::path::Component::ParentDir => {
                components.push_back(RootedPathComponent::Parent);
            }
            std::path::Component::Normal(name) => {
                components.push_back(RootedPathComponent::Normal(name.to_owned()));
            }
            std::path::Component::Prefix(_) => {
                bail!("systemd symlink target has an unsupported path prefix")
            }
        }
    }
    Ok(components)
}

/// Resolve a path as systemd does beneath its configured root. Absolute
/// symlink targets start at `systemd_root`; relative targets start at the
/// symlink's parent. A link that traverses above the root fails closed.
fn resolve_systemd_path(systemd_root: &Path, path: &Path) -> Result<PathBuf> {
    let relative = path.strip_prefix(systemd_root).with_context(|| {
        format!(
            "path {} is outside staged systemd root {}",
            path.display(),
            systemd_root.display()
        )
    })?;
    let mut pending = rooted_path_components(relative)?;
    let mut resolved = PathBuf::new();
    let mut symlink_hops = 0_u8;

    while let Some(component) = pending.pop_front() {
        match component {
            RootedPathComponent::Parent => {
                if !resolved.pop() {
                    bail!(
                        "systemd path escapes root {}: {}",
                        systemd_root.display(),
                        path.display()
                    );
                }
            }
            RootedPathComponent::Normal(name) => {
                let candidate = systemd_root.join(&resolved).join(&name);
                let metadata = match fs::symlink_metadata(&candidate) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        resolved.push(name);
                        continue;
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("inspect systemd path {}", candidate.display())
                        });
                    }
                };
                if !metadata.file_type().is_symlink() {
                    resolved.push(name);
                    continue;
                }

                symlink_hops = symlink_hops.saturating_add(1);
                if symlink_hops > 40 {
                    bail!("too many symlinks while resolving {}", path.display());
                }
                let target = fs::read_link(&candidate)
                    .with_context(|| format!("read symlink {}", candidate.display()))?;
                if target.is_absolute() {
                    resolved.clear();
                }
                let mut target_components = rooted_path_components(&target)?;
                while let Some(component) = target_components.pop_back() {
                    pending.push_front(component);
                }
            }
        }
    }

    Ok(systemd_root.join(resolved))
}

fn sorted_conf_files(
    systemd_root: &Path,
    dirs: impl IntoIterator<Item = Vec<PathBuf>>,
) -> Result<Vec<DropInFile>> {
    // conf_files_list_strv() gives earlier lookup directories priority for a
    // duplicate basename, then sorts the surviving files globally by name.
    let mut files = BTreeMap::new();
    let mut seen_dirs = BTreeSet::new();
    for candidate_dirs in dirs {
        for dir in candidate_dirs {
            // systemd stops trying less specific drop-in directories in this
            // lookup root if a candidate path cannot be chased, then tries
            // the next root.
            let resolved_dir = match resolve_systemd_path(systemd_root, &dir) {
                Ok(path) => path,
                Err(_) => break,
            };
            if !seen_dirs.insert(resolved_dir.clone()) {
                continue;
            }
            let entries = match fs::read_dir(&resolved_dir) {
                Ok(entries) => entries,
                // conf_files_list_impl() logs and skips directories it cannot
                // open, including a candidate path occupied by a regular file.
                Err(_) => continue,
            };
            process_read_dir_entries(entries, |entry| {
                let path = entry.path();
                if !path.extension().is_some_and(|ext| ext == "conf") {
                    return Ok(());
                }

                let name = path
                    .file_name()
                    .context("drop-in path has no filename")?
                    .to_owned();
                if files.contains_key(&name) {
                    return Ok(());
                }

                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    // conf_files_list_strv() ignores entries it cannot chase
                    // or verify, leaving lower-priority copies eligible.
                    Err(_) => return Ok(()),
                };
                let (resolved_path, masked) = if metadata.file_type().is_symlink() {
                    let target = match resolve_systemd_path(systemd_root, &path) {
                        Ok(target) => target,
                        Err(_) => return Ok(()),
                    };
                    if target == systemd_root.join("dev/null") {
                        (target, true)
                    } else {
                        let target_metadata = match fs::metadata(&target) {
                            Ok(metadata) => metadata,
                            Err(_) => return Ok(()),
                        };
                        if !target_metadata.is_file() {
                            return Ok(());
                        }
                        (target, false)
                    }
                } else {
                    if !metadata.is_file() {
                        return Ok(());
                    }
                    (path, false)
                };
                files.insert(
                    name,
                    DropInFile {
                        path: resolved_path,
                        masked,
                    },
                );
                Ok(())
            })
            .with_context(|| format!("read entries in {}", resolved_dir.display()))?;
        }
    }
    Ok(files.into_values().collect())
}

fn process_read_dir_entries<T>(
    entries: impl IntoIterator<Item = std::io::Result<T>>,
    mut process: impl FnMut(T) -> Result<()>,
) -> Result<()> {
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == std::io::ErrorKind::OutOfMemory => {
                return Err(error).context("read a drop-in directory entry");
            }
            Err(_) => break,
        };
        process(entry)?;
    }
    Ok(())
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
    environment_reset: bool,
    environment_files: Vec<EnvironmentFile>,
    environment_files_reset: bool,
    state_directory: Option<String>,
    state_directory_reset: bool,
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
        let mut in_service_section = false;
        for raw in logical_lines(text) {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') {
                in_service_section = line
                    .strip_prefix('[')
                    .and_then(|section| section.strip_suffix(']'))
                    == Some("Service");
                continue;
            }
            if !in_service_section {
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
                        unit.environment_reset = true;
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
                        unit.environment_files_reset = true;
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
                    if value.is_empty() {
                        unit.state_directory = None;
                        unit.state_directory_reset = true;
                    } else if unit.state_directory.is_none() {
                        // Several may be listed; the daemon's config resolver
                        // takes the first, as systemd exports it first.
                        unit.state_directory = value.split_whitespace().next().map(str::to_owned);
                    }
                }
                _ => {}
            }
        }
        Ok(unit)
    }

    /// Apply a drop-in over this fragment with systemd's semantics: list
    /// directives append, empty assignments reset their accumulated lists,
    /// and the resolver retains the first StateDirectory= entry.
    fn layer(&mut self, other: Self) {
        if other.environment_reset {
            self.environment.clear();
        }
        self.environment.extend(other.environment);
        if other.environment_files_reset {
            self.environment_files.clear();
        }
        self.environment_files.extend(other.environment_files);
        if other.state_directory_reset {
            self.state_directory = None;
        }
        if self.state_directory.is_none() {
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

/// Join backslash-continued unit-file lines. systemd replaces the backslash
/// and newline with a separating space.
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

/// Parse a systemd `EnvironmentFile=` without applying unit-file line joining.
/// In particular, escaped newlines inside environment values are removed,
/// quotes have different rules from unit-file quoting, and comments are only
/// recognized before an assignment starts.
pub(crate) fn parse_environment_file(text: &str) -> Result<Vec<(String, String)>> {
    #[derive(Clone, Copy)]
    enum State {
        PreKey,
        Key,
        PreValue,
        Value,
        ValueEscape,
        SingleQuoted,
        DoubleQuoted,
        DoubleQuotedEscape,
        Comment,
        CommentEscape,
    }

    for ch in text.chars() {
        let codepoint = u32::from(ch);
        if ch == '\0'
            || ch == '\u{feff}'
            || (0xfdd0..=0xfdef).contains(&codepoint)
            || codepoint & 0xffff == 0xfffe
            || codepoint & 0xffff == 0xffff
        {
            bail!("environment file contains a character systemd rejects");
        }
    }

    let mut assignments = Vec::new();
    let mut key = String::new();
    let mut value = String::new();
    let mut last_value_whitespace = None;
    let mut state = State::PreKey;

    let commit = |key: &mut String,
                  value: &mut String,
                  assignments: &mut Vec<(String, String)>|
     -> Result<()> {
        let name = key.trim_end_matches(is_environment_file_whitespace);
        if name.is_empty() || !valid_environment_name(name) {
            bail!("invalid environment file key: {name:?}");
        }
        assignments.push((name.to_owned(), std::mem::take(value)));
        key.clear();
        Ok(())
    };

    for ch in text.chars() {
        use State::{
            Comment, CommentEscape, DoubleQuoted, DoubleQuotedEscape, Key, PreKey, PreValue,
            SingleQuoted, Value, ValueEscape,
        };

        match state {
            PreKey if ch == '\n' => {}
            PreKey if is_environment_file_whitespace(ch) => {}
            PreKey if ch == '#' || ch == ';' => state = Comment,
            PreKey => {
                key.push(ch);
                state = Key;
            }
            Key if ch == '\n' => {
                // systemd ignores lines without an '=' separator.
                key.clear();
                state = PreKey;
            }
            Key if ch == '=' => {
                state = PreValue;
            }
            Key => key.push(ch),
            PreValue if ch == '\n' => {
                commit(&mut key, &mut value, &mut assignments)?;
                last_value_whitespace = None;
                state = PreKey;
            }
            PreValue if is_environment_file_whitespace(ch) => {}
            PreValue if ch == '\\' => {
                last_value_whitespace = None;
                state = ValueEscape;
            }
            PreValue if ch == '\'' => state = SingleQuoted,
            PreValue if ch == '"' => state = DoubleQuoted,
            PreValue => {
                value.push(ch);
                state = Value;
            }
            Value if ch == '\n' => {
                if let Some(index) = last_value_whitespace.take() {
                    value.truncate(index);
                }
                commit(&mut key, &mut value, &mut assignments)?;
                last_value_whitespace = None;
                state = PreKey;
            }
            Value if ch == '\\' => {
                // systemd stops treating earlier spaces as trailing when a
                // backslash escapes the next byte or continues the line.
                last_value_whitespace = None;
                state = ValueEscape;
            }
            Value => {
                if is_environment_file_whitespace(ch) {
                    if last_value_whitespace.is_none() {
                        last_value_whitespace = Some(value.len());
                    }
                } else {
                    last_value_whitespace = None;
                }
                value.push(ch);
            }
            ValueEscape if ch == '\n' => state = Value,
            ValueEscape => {
                value.push(ch);
                last_value_whitespace = None;
                state = Value;
            }
            SingleQuoted if ch == '\'' => state = PreValue,
            SingleQuoted => value.push(ch),
            DoubleQuoted if ch == '"' => state = PreValue,
            DoubleQuoted if ch == '\\' => state = DoubleQuotedEscape,
            DoubleQuoted => value.push(ch),
            DoubleQuotedEscape if ch == '\n' => state = DoubleQuoted,
            DoubleQuotedEscape => {
                if matches!(ch, '\\' | '"' | '$' | '`') {
                    value.push(ch);
                } else {
                    value.push('\\');
                    value.push(ch);
                }
                state = DoubleQuoted;
            }
            Comment if ch == '\\' => state = CommentEscape,
            Comment if ch == '\n' => state = PreKey,
            Comment => {}
            CommentEscape if ch == '\n' => {
                // systemd changed this case in v254: older managers keep the
                // following physical line in the comment, newer ones parse
                // it normally. Reject it so the resolver cannot infer a
                // different VELNOR root than the installed manager.
                bail!("version-dependent EnvironmentFile comment continuation");
            }
            CommentEscape => state = Comment,
        }
    }

    match state {
        State::PreValue
        | State::Value
        | State::ValueEscape
        | State::SingleQuoted
        | State::DoubleQuoted
        | State::DoubleQuotedEscape => {
            // systemd commits these parser states at EOF. A final backslash
            // in either escape state is discarded; only an unquoted VALUE
            // state trims trailing whitespace.
            if matches!(state, State::Value) {
                if let Some(index) = last_value_whitespace.take() {
                    value.truncate(index);
                }
            }
            commit(&mut key, &mut value, &mut assignments)?;
        }
        State::PreKey | State::Key | State::Comment | State::CommentEscape => {}
    }
    Ok(assignments)
}

fn is_environment_file_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\r')
}

fn valid_environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
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
        let systemd_root = root.clone();

        let dogfood = resolve_in(&etc, &systemd_root, "dogfood").unwrap();
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
        let systemd_root = root.clone();

        let bare = resolve_in(&etc, &systemd_root, BARE_INSTANCE).unwrap();
        assert_eq!(bare.unit, "velnor-daemon.service");
        assert_eq!(bare.name, "velnor-java-monorepo");
        assert_eq!(bare.state_directory, PathBuf::from("/var/lib/velnor"));
        assert_eq!(bare.config_dir, PathBuf::from("/var/lib/velnor/runner"));
        assert_eq!(
            bare.daemon_dir,
            PathBuf::from("/var/lib/velnor/runner/daemons/velnor-java-monorepo")
        );

        // No VELNOR_TRUST_SCOPE: the daemon's flag default, fail closed.
        let fixture = resolve_in(&etc, &systemd_root, "fixture").unwrap();
        assert_eq!(fixture.trust_scope, crate::trust_scope::FAIL_CLOSED);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enumeration_skips_secrets_backups_and_non_env_files() {
        let root = temp_root("enumerate");
        let etc = stage_sentry_like_etc(&root);
        let systemd_root = root.clone();
        fs::write(
            etc.join("fleet-policy-audit.env"),
            "VELNOR_FLEET_ORGS=example\n",
        )
        .unwrap();
        fs::write(
            etc.join("alpha.beta.env"),
            "VELNOR_NAME=velnor-alpha.beta\nVELNOR_SLOTS=2\nVELNOR_WORK_DIR=/var/lib/velnor-alpha.beta/work\n",
        )
        .unwrap();
        fs::write(
            etc.join("alpha.beta.secrets.env"),
            "GITHUB_TOKEN=ghp_secret\n",
        )
        .unwrap();

        let names: Vec<String> = enumerate_in(&etc, &systemd_root)
            .unwrap()
            .into_iter()
            .map(|instance| instance.instance)
            .collect();
        assert_eq!(names, ["alpha.beta", "dogfood", "fixture", "velnor"]);
        assert_eq!(
            resolve_in(&etc, &systemd_root, "alpha.beta").unwrap().unit,
            "velnor-daemon@alpha.beta.service"
        );
        assert!(daemon_instance_name_of_env_file(&etc.join("alpha.beta.secrets.env")).is_none());
        assert!(is_non_daemon_service_env_file_name(
            "fleet-policy-audit.env"
        ));
        assert!(resolve_in(&etc, &systemd_root, "fleet-policy-audit").is_err());

        // A host without /etc/velnor has no packaged instances.
        assert!(enumerate_in(&root.join("missing"), &systemd_root)
            .unwrap()
            .is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn systemd_instance_name_grammar_accepts_dots_and_rejects_unsafe_names() {
        for valid in ["alpha", "alpha.beta", "alpha.beta-gamma_2", "alpha:beta"] {
            assert!(valid_instance_name(valid), "{valid:?} should be accepted");
        }
        for invalid in [
            "",
            ".",
            "..",
            "../alpha",
            "alpha/beta",
            "alpha\\beta",
            "alpha@beta",
            "alpha beta",
            "alpha\nbeta",
        ] {
            assert!(
                !valid_instance_name(invalid),
                "{invalid:?} should be rejected"
            );
        }
        assert!(!valid_instance_name(&"a".repeat(234)));
    }

    #[test]
    fn reserved_service_environment_is_validated_without_becoming_a_daemon() {
        use std::os::unix::fs::symlink;

        let root = temp_root("reserved-service-env");
        let etc = root.join("etc/velnor");
        fs::create_dir_all(&etc).unwrap();
        let fleet_env = etc.join("fleet-policy-audit.env");
        fs::write(&fleet_env, "VELNOR_FLEET_ORGS=example\n").unwrap();
        assert!(verify_non_daemon_service_env_file(&fleet_env).is_ok());
        assert!(daemon_instance_name_of_env_file(&fleet_env).is_none());

        fs::write(
            &fleet_env,
            "VELNOR_FLEET_ORGS=example\nVELNOR_WORK_DIR=/tmp/attacker\n",
        )
        .unwrap();
        assert!(verify_non_daemon_service_env_file(&fleet_env).is_err());

        let target = root.join("target.env");
        fs::write(&target, "VELNOR_FLEET_ORGS=example\n").unwrap();
        fs::remove_file(&fleet_env).unwrap();
        symlink(&target, &fleet_env).unwrap();
        assert!(verify_non_daemon_service_env_file(&fleet_env).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selector_accepts_velnor_name_and_names_known_instances_on_miss() {
        let root = temp_root("selector");
        let etc = stage_sentry_like_etc(&root);
        let systemd_root = root.clone();

        let by_name = resolve_in(&etc, &systemd_root, "velnor-dogfood").unwrap();
        assert_eq!(by_name.instance, "dogfood");

        let missing = resolve_in(&etc, &systemd_root, "nope")
            .unwrap_err()
            .to_string();
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
        let systemd_root = root.clone();
        let drop_ins = systemd_root.join("etc/systemd/system");
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

        let dogfood = resolve_in(&etc, &systemd_root, "dogfood").unwrap();
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
        let moved = resolve_in(&etc, &systemd_root, "dogfood").unwrap();
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
    fn drop_ins_use_instance_candidate_for_same_name_and_global_name_order() {
        let root = temp_root("drop-in-precedence");
        let etc = stage_sentry_like_etc(&root);
        let systemd_root = root.clone();
        let drop_ins = systemd_root.join("etc/systemd/system");
        let template = drop_ins.join("velnor-daemon@.service.d");
        let instance = drop_ins.join("velnor-daemon@dogfood.service.d");
        fs::create_dir_all(&template).unwrap();
        fs::create_dir_all(&instance).unwrap();

        fs::write(
            template.join("10-same-name.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_SAME_NAME=template\n",
        )
        .unwrap();
        fs::write(
            instance.join("10-same-name.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_SAME_NAME=instance\n",
        )
        .unwrap();
        fs::write(
            template.join("20-template.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_SORT_ORDER=template\n",
        )
        .unwrap();
        fs::write(
            instance.join("30-instance.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_SORT_ORDER=instance\n",
        )
        .unwrap();

        let resolved = resolve_in(&etc, &systemd_root, "dogfood").unwrap();
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_SAME_NAME")
                .map(String::as_str),
            Some("instance")
        );
        // Different filenames are parsed globally in alphanumeric order.
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_SORT_ORDER")
                .map(String::as_str),
            Some("instance")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn masked_instance_drop_in_suppresses_template_copy() {
        use std::os::unix::fs::symlink;

        let root = temp_root("masked-drop-in");
        let etc = stage_sentry_like_etc(&root);
        let systemd_root = root.clone();
        let drop_ins = systemd_root.join("etc/systemd/system");
        let template = drop_ins.join("velnor-daemon@.service.d");
        let instance = drop_ins.join("velnor-daemon@dogfood.service.d");
        fs::create_dir_all(&template).unwrap();
        fs::create_dir_all(&instance).unwrap();
        fs::write(
            template.join("10-storage.conf"),
            "[Service]\nEnvironment=VELNOR_STORAGE_ROOT=/tmp/wrong-root\n",
        )
        .unwrap();
        symlink("/dev/null", instance.join("10-storage.conf")).unwrap();

        let resolved = resolve_in(&etc, &systemd_root, "dogfood").unwrap();
        assert_eq!(resolved.storage_root, PathBuf::from("/var"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn drop_ins_follow_systemd_roots_duplicate_priority_and_global_name_order() {
        use std::os::unix::fs::symlink;

        let root = temp_root("drop-in-config-roots");
        let etc = stage_sentry_like_etc(&root);
        let systemd_root = root.clone();
        let etc_instance = root.join("etc/systemd/system/velnor-daemon@dogfood.service.d");
        let etc_template = root.join("etc/systemd/system/velnor-daemon@.service.d");
        let run_instance = root.join("run/systemd/system/velnor-daemon@dogfood.service.d");
        let vendor_template = root.join("usr/lib/systemd/system/velnor-daemon@.service.d");
        for dir in [
            &etc_instance,
            &etc_template,
            &run_instance,
            &vendor_template,
        ] {
            fs::create_dir_all(dir).unwrap();
        }

        // Invalid higher-priority entries are ignored, leaving lower-root
        // copies with the same basename eligible.
        symlink(
            "../../../../../outside.conf",
            etc_instance.join("20-invalid-symlink.conf"),
        )
        .unwrap();
        fs::create_dir(etc_instance.join("21-nonregular.conf")).unwrap();
        fs::write(
            run_instance.join("20-invalid-symlink.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_INVALID_SYMLINK=run\n",
        )
        .unwrap();
        fs::write(
            run_instance.join("21-nonregular.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_NONREGULAR=run\n",
        )
        .unwrap();

        // `/lib/systemd/system` aliases `/usr/lib/systemd/system` on merged-
        // usr hosts. Include that alias in the staged root as it is on-host.
        fs::create_dir_all(root.join("lib/systemd")).unwrap();
        symlink(
            "../../usr/lib/systemd/system",
            root.join("lib/systemd/system"),
        )
        .unwrap();

        // The same basename is resolved by config-root priority first, then
        // the exact instance directory before the template directory.
        for (dir, value) in [
            (&etc_template, "etc-template"),
            (&etc_instance, "etc-instance"),
            (&run_instance, "run-instance"),
            (&vendor_template, "vendor-template"),
        ] {
            fs::write(
                dir.join("05-duplicate.conf"),
                format!("[Service]\nEnvironment=VELNOR_TEST_DUPLICATE={value}\n"),
            )
            .unwrap();
        }
        fs::write(
            etc_template.join("06-cross-root.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_ROOT_PRIORITY=etc-template\n",
        )
        .unwrap();
        fs::write(
            run_instance.join("06-cross-root.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_ROOT_PRIORITY=run-instance\n",
        )
        .unwrap();

        // Files from all roots are then applied in one global filename order,
        // not grouped by root. The 30 file must win despite its lower-priority
        // source root.
        fs::write(
            vendor_template.join("10-vendor.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_ORDER=vendor VELNOR_TEST_VENDOR=seen\n",
        )
        .unwrap();
        fs::write(
            etc_template.join("20-etc.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_ORDER=etc VELNOR_TEST_ETC=seen\n",
        )
        .unwrap();
        fs::write(
            run_instance.join("30-run.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_ORDER=run VELNOR_TEST_RUN=seen\n",
        )
        .unwrap();

        let resolved = resolve_in(&etc, &systemd_root, "dogfood").unwrap();
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_DUPLICATE")
                .map(String::as_str),
            Some("etc-instance")
        );
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_ROOT_PRIORITY")
                .map(String::as_str),
            Some("etc-template")
        );
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_ORDER")
                .map(String::as_str),
            Some("run")
        );
        for key in ["VELNOR_TEST_VENDOR", "VELNOR_TEST_ETC", "VELNOR_TEST_RUN"] {
            assert_eq!(
                resolved.environment.get(key).map(String::as_str),
                Some("seen")
            );
        }
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_INVALID_SYMLINK")
                .map(String::as_str),
            Some("run")
        );
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_NONREGULAR")
                .map(String::as_str),
            Some("run")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unchaseable_drop_in_directory_skips_rest_of_root_but_checks_lower_roots() {
        use std::os::unix::fs::symlink;

        let root = temp_root("unchaseable-drop-in-directory");
        let etc = stage_sentry_like_etc(&root);
        let systemd_root = root.clone();
        let etc_systemd = root.join("etc/systemd/system");
        let etc_exact = etc_systemd.join("velnor-daemon@dogfood.service.d");
        let etc_prefix = etc_systemd.join("velnor-.service.d");
        let run_prefix = root.join("run/systemd/system/velnor-.service.d");
        fs::create_dir_all(&etc_exact).unwrap();
        fs::create_dir_all(&etc_prefix).unwrap();
        fs::create_dir_all(&run_prefix).unwrap();
        fs::write(
            etc_exact.join("10-exact.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_EXACT_ROOT=etc\n",
        )
        .unwrap();
        fs::write(
            etc_prefix.join("20-prefix.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_BLOCKED_PREFIX=etc\n",
        )
        .unwrap();
        fs::write(
            run_prefix.join("30-prefix.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_LOWER_ROOT=run\n",
        )
        .unwrap();
        symlink(
            "../../../../../outside-template.d",
            etc_systemd.join("velnor-daemon@.service.d"),
        )
        .unwrap();

        let resolved = resolve_in(&etc, &systemd_root, "dogfood").unwrap();
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_EXACT_ROOT")
                .map(String::as_str),
            Some("etc")
        );
        assert!(!resolved
            .environment
            .contains_key("VELNOR_TEST_BLOCKED_PREFIX"));
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_LOWER_ROOT")
                .map(String::as_str),
            Some("run")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn regular_file_at_drop_in_directory_path_is_skipped() {
        let root = temp_root("regular-file-drop-in-directory");
        let etc = stage_sentry_like_etc(&root);
        let systemd_root = root.clone();
        let etc_systemd = root.join("etc/systemd/system");
        fs::create_dir_all(&etc_systemd).unwrap();
        fs::write(
            etc_systemd.join("velnor-daemon@dogfood.service.d"),
            "not a directory",
        )
        .unwrap();

        let template = etc_systemd.join("velnor-daemon@.service.d");
        fs::create_dir_all(&template).unwrap();
        fs::write(
            template.join("10-template.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_LOWER_CANDIDATE=etc-template\n",
        )
        .unwrap();

        let run_exact = root.join("run/systemd/system/velnor-daemon@dogfood.service.d");
        fs::create_dir_all(&run_exact).unwrap();
        fs::write(
            run_exact.join("20-run.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_LOWER_ROOT=run\n",
        )
        .unwrap();

        let resolved = resolve_in(&etc, &systemd_root, "dogfood").unwrap();
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_LOWER_CANDIDATE")
                .map(String::as_str),
            Some("etc-template")
        );
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_LOWER_ROOT")
                .map(String::as_str),
            Some("run")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn drop_in_directory_read_error_stops_only_that_directory() {
        let mut processed = Vec::new();
        process_read_dir_entries(
            [
                Ok::<_, std::io::Error>(1),
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected readdir failure",
                )),
                Ok(2),
            ],
            |entry| {
                processed.push(entry);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(processed, [1]);

        assert!(process_read_dir_entries(
            [Err::<(), _>(std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                "injected allocation failure",
            ))],
            |_| Ok(())
        )
        .is_err());
    }

    #[test]
    fn systemd_drop_in_candidates_include_prefixes_and_type_level_service() {
        let names = drop_in_candidate_names("velnor-daemon@dogfood.service");
        assert_eq!(
            names,
            [
                "velnor-daemon@dogfood.service",
                "velnor-daemon@.service",
                "velnor-.service",
                "velnor-@dogfood.service",
                "velnor-@.service",
            ]
        );
        assert_eq!(
            drop_in_candidate_names("velnor-api-worker@dogfood.service"),
            [
                "velnor-api-worker@dogfood.service",
                "velnor-api-worker@.service",
                "velnor-api-.service",
                "velnor-.service",
                "velnor-api-@dogfood.service",
                "velnor-api-@.service",
                "velnor-@dogfood.service",
                "velnor-@.service",
            ]
        );

        let root = temp_root("drop-in-candidates");
        let etc = stage_sentry_like_etc(&root);
        let systemd_root = root.clone();
        let systemd = root.join("etc/systemd/system");
        let exact = systemd.join("velnor-daemon@dogfood.service.d");
        let template = systemd.join("velnor-daemon@.service.d");
        let prefix = systemd.join("velnor-.service.d");
        let prefix_instance = systemd.join("velnor-@dogfood.service.d");
        let prefix_template = systemd.join("velnor-@.service.d");
        let service = systemd.join("service.d");
        for dir in [
            &exact,
            &template,
            &prefix,
            &prefix_instance,
            &prefix_template,
            &service,
        ] {
            fs::create_dir_all(dir).unwrap();
        }

        for (dir, key, value) in [
            (&exact, "VELNOR_TEST_EXACT", "yes"),
            (&template, "VELNOR_TEST_TEMPLATE", "yes"),
            (&prefix, "VELNOR_TEST_PREFIX", "yes"),
            (&prefix_instance, "VELNOR_TEST_PREFIX_INSTANCE", "yes"),
            (&prefix_template, "VELNOR_TEST_PREFIX_TEMPLATE", "yes"),
            (&service, "VELNOR_TEST_SERVICE_TYPE", "yes"),
        ] {
            fs::write(
                dir.join(format!("20-{key}.conf")),
                format!("[Service]\nEnvironment={key}={value}\n"),
            )
            .unwrap();
        }
        fs::write(
            exact.join("30-exact-priority.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_CANDIDATE_PRIORITY=exact\n",
        )
        .unwrap();
        fs::write(
            service.join("30-exact-priority.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_CANDIDATE_PRIORITY=service\n",
        )
        .unwrap();

        // All unit and prefix candidates precede type-level candidates, even
        // across roots. Within the type-level pass, `/etc` still beats `/run`.
        let run_exact = root.join("run/systemd/system/velnor-daemon@dogfood.service.d");
        fs::create_dir_all(&run_exact).unwrap();
        let run_service = root.join("run/systemd/system/service.d");
        fs::create_dir_all(&run_service).unwrap();
        fs::write(
            service.join("40-root-priority.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_ROOT_CANDIDATE=etc-type\n",
        )
        .unwrap();
        fs::write(
            run_exact.join("40-root-priority.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_ROOT_CANDIDATE=run-exact\n",
        )
        .unwrap();
        fs::write(
            service.join("41-type-root-priority.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_TYPE_ROOT_PRIORITY=etc-type\n",
        )
        .unwrap();
        fs::write(
            run_service.join("41-type-root-priority.conf"),
            "[Service]\nEnvironment=VELNOR_TEST_TYPE_ROOT_PRIORITY=run-type\n",
        )
        .unwrap();

        let resolved = resolve_in(&etc, &systemd_root, "dogfood").unwrap();
        for key in [
            "VELNOR_TEST_EXACT",
            "VELNOR_TEST_TEMPLATE",
            "VELNOR_TEST_PREFIX",
            "VELNOR_TEST_PREFIX_INSTANCE",
            "VELNOR_TEST_PREFIX_TEMPLATE",
            "VELNOR_TEST_SERVICE_TYPE",
        ] {
            assert_eq!(
                resolved.environment.get(key).map(String::as_str),
                Some("yes")
            );
        }
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_CANDIDATE_PRIORITY")
                .map(String::as_str),
            Some("exact")
        );
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_ROOT_CANDIDATE")
                .map(String::as_str),
            Some("run-exact")
        );
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_TYPE_ROOT_PRIORITY")
                .map(String::as_str),
            Some("etc-type")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn systemd_control_generator_attached_and_vendor_roots_are_searched() {
        use std::os::unix::fs::symlink;

        let root = temp_root("drop-in-manager-roots");
        let etc = stage_sentry_like_etc(&root);
        let systemd_root = root.clone();
        let active_roots = [
            ("etc/systemd/system.control", "etc-control"),
            ("run/systemd/system.control", "run-control"),
            ("run/systemd/transient", "transient"),
            ("run/systemd/generator.early", "generator-early"),
            ("etc/systemd/system", "etc"),
            ("etc/systemd/system.attached", "etc-attached"),
            ("run/systemd/system", "run"),
            ("run/systemd/system.attached", "run-attached"),
            ("run/systemd/generator", "generator"),
            ("usr/local/lib/systemd/system", "usr-local"),
            ("usr/lib/systemd/system", "usr-lib"),
            ("run/systemd/generator.late", "generator-late"),
        ];

        for (index, (root_name, label)) in active_roots.iter().enumerate() {
            let unit_dir = root.join(root_name).join("velnor-daemon@dogfood.service.d");
            fs::create_dir_all(&unit_dir).unwrap();
            fs::write(
                unit_dir.join("05-root-priority.conf"),
                format!("[Service]\nEnvironment=VELNOR_TEST_ROOT_PRIORITY={label}\n"),
            )
            .unwrap();
            fs::write(
                unit_dir.join(format!("10-root-{index:02}.conf")),
                format!("[Service]\nEnvironment=VELNOR_TEST_ROOT_{index}=seen\n"),
            )
            .unwrap();
        }

        // Exercise the merged-/usr `/lib` alias. It resolves to the existing
        // `/usr/lib` candidate and must not add a second copy of its files.
        fs::create_dir_all(root.join("lib/systemd")).unwrap();
        symlink(
            "../../usr/lib/systemd/system",
            root.join("lib/systemd/system"),
        )
        .unwrap();

        let resolved = resolve_in(&etc, &systemd_root, "dogfood").unwrap();
        assert_eq!(
            resolved
                .environment
                .get("VELNOR_TEST_ROOT_PRIORITY")
                .map(String::as_str),
            Some("etc-control")
        );
        for index in 0..active_roots.len() {
            let key = format!("VELNOR_TEST_ROOT_{index}");
            assert_eq!(
                resolved.environment.get(&key).map(String::as_str),
                Some("seen")
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_systemd_symlinks_resolve_inside_root_and_reject_escape() {
        use std::os::unix::fs::symlink;

        let root = temp_root("staged-systemd-symlinks");
        fs::create_dir_all(root.join("etc")).unwrap();
        fs::write(root.join("etc/absolute-target.conf"), "absolute=staged\n").unwrap();
        fs::write(root.join("relative-target.conf"), "relative=staged\n").unwrap();
        symlink("/etc/absolute-target.conf", root.join("absolute.conf")).unwrap();
        symlink("relative-target.conf", root.join("relative.conf")).unwrap();
        symlink("../outside.conf", root.join("escape.conf")).unwrap();

        assert_eq!(
            resolve_systemd_path(&root, &root.join("absolute.conf")).unwrap(),
            root.join("etc/absolute-target.conf")
        );
        assert_eq!(
            resolve_systemd_path(&root, &root.join("relative.conf")).unwrap(),
            root.join("relative-target.conf")
        );
        assert!(resolve_systemd_path(&root, &root.join("escape.conf")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_list_assignments_reset_prior_environment_and_files() {
        let mut unit = UnitEnvironment::parse(
            "[Service]\nEnvironment=BASE=unit\nEnvironmentFile=/etc/velnor/base.env\nStateDirectory=unit\n",
            Some("dogfood"),
        )
        .unwrap();
        unit.layer(
            UnitEnvironment::parse(
                "[Service]\nEnvironment=EARLIER=drop-in\nEnvironmentFile=/etc/velnor/earlier.env\nStateDirectory=earlier\n",
                Some("dogfood"),
            )
            .unwrap(),
        );
        unit.layer(
            UnitEnvironment::parse(
                "[Service]\nEnvironment=\nEnvironment=AFTER=template\nEnvironmentFile=\nEnvironmentFile=/etc/velnor/after.env\nStateDirectory=\nStateDirectory=after second\n",
                Some("dogfood"),
            )
            .unwrap(),
        );

        assert_eq!(
            unit.environment,
            BTreeMap::from([("AFTER".into(), "template".into())])
        );
        assert_eq!(
            unit.environment_files,
            [EnvironmentFile {
                path: PathBuf::from("/etc/velnor/after.env"),
                optional: false,
            }]
        );
        assert_eq!(unit.state_directory.as_deref(), Some("after"));
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
    fn unit_environment_ignores_directives_outside_service_section() {
        let unit = UnitEnvironment::parse(
            "[Unit]\nEnvironment=invalid without equals\nEnvironmentFile=/etc/velnor/unit.env\nStateDirectory=wrong\n\
             [Service]\nEnvironment=KEPT=yes\nEnvironmentFile=/etc/velnor/service.env\nStateDirectory=right\n\
             [Install]\nEnvironment=INSTALL=ignored\nEnvironmentFile=/etc/velnor/install.env\nStateDirectory=wrong-again\n",
            Some("dogfood"),
        )
        .unwrap();

        assert_eq!(
            unit.environment,
            BTreeMap::from([("KEPT".into(), "yes".into())])
        );
        assert_eq!(
            unit.environment_files,
            [EnvironmentFile {
                path: PathBuf::from("/etc/velnor/service.env"),
                optional: false,
            }]
        );
        assert_eq!(unit.state_directory.as_deref(), Some("right"));
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
                ("CONTINUED".to_owned(), "first second".to_owned()),
                ("EMPTY".to_owned(), String::new()),
            ]
        );
        assert!(parse_environment_file("NOEQUALS\n").unwrap().is_empty());
        assert!(parse_environment_file(
            r#"# version-dependent comment continuation \
VELNOR_STORAGE_ROOT=/tmp/wrong
"#
        )
        .is_err());
        assert_eq!(
            split_quoted_words("A=1 \"B=two words\" C='x y'").unwrap(),
            ["A=1", "B=two words", "C=x y"]
        );
    }

    #[test]
    fn environment_file_eof_commits_systemd_escape_and_quote_states() {
        assert_eq!(
            parse_environment_file("UNQUOTED=tail\\").unwrap(),
            [("UNQUOTED".to_owned(), "tail".to_owned())]
        );
        assert_eq!(
            parse_environment_file("DOUBLE=\"tail\\").unwrap(),
            [("DOUBLE".to_owned(), "tail".to_owned())]
        );
        assert_eq!(
            parse_environment_file("SINGLE='open").unwrap(),
            [("SINGLE".to_owned(), "open".to_owned())]
        );
    }

    #[test]
    fn environment_file_preserves_backslash_escaped_trailing_whitespace() {
        assert_eq!(
            parse_environment_file("EOF=tail\\ ").unwrap(),
            [("EOF".to_owned(), "tail ".to_owned())]
        );
        assert_eq!(
            parse_environment_file("LINE=tail\\ \n").unwrap(),
            [("LINE".to_owned(), "tail ".to_owned())]
        );
        assert_eq!(
            parse_environment_file("CONTINUED=tail\\ \\\nend\n").unwrap(),
            [("CONTINUED".to_owned(), "tail end".to_owned())]
        );
        assert_eq!(
            parse_environment_file("PLAIN=tail  \nEOF=tail  ").unwrap(),
            [
                ("PLAIN".to_owned(), "tail".to_owned()),
                ("EOF".to_owned(), "tail".to_owned()),
            ]
        );
    }

    #[test]
    fn environment_file_escapes_quotes_and_comments_match_systemd() {
        let parsed = parse_environment_file(
            r#"# comment
; another comment
UNQUOTED=space\ here\\slash\#hash\;semi
DOUBLE="quote: \" slash: \\ dollar: \$ tick: \` unknown: \q newline: \n"
SINGLE='literal \\slash "quote" #;'
DOUBLE_CONTINUED="left\
right"
MULTILINE='first
second'
INLINE=left#hash ;semicolon
"#,
        )
        .unwrap();

        assert_eq!(
            parsed,
            vec![
                (
                    "UNQUOTED".to_owned(),
                    "space here\\slash#hash;semi".to_owned()
                ),
                (
                    "DOUBLE".to_owned(),
                    "quote: \" slash: \\ dollar: $ tick: ` unknown: \\q newline: \\n".to_owned()
                ),
                (
                    "SINGLE".to_owned(),
                    r#"literal \\slash "quote" #;"#.to_owned()
                ),
                ("DOUBLE_CONTINUED".to_owned(), "leftright".to_owned()),
                ("MULTILINE".to_owned(), "first\nsecond".to_owned()),
                ("INLINE".to_owned(), "left#hash ;semicolon".to_owned()),
            ]
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
