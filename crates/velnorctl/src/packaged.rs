//! Which daemon a command addresses.
//!
//! On a packaged host every daemon is a `velnor-daemon@<instance>` unit whose
//! paths come from its unit environment; `velnor_runner::daemon_instance`
//! replays that environment. On a development host there are no packaged
//! instances and the process environment is the daemon's environment, exactly
//! as it is for `velnorctl daemon`. This module makes that choice once, so
//! `get`, `status`, `host`, and `cache` cannot each assume a layout.

use std::path::PathBuf;

use velnor_model::ExitClass;
use velnor_runner::daemon_instance::{self, DaemonInstance};

use crate::{CommandError, GlobalArgs};

/// The daemon a command addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selected {
    /// A packaged systemd instance, with every path its unit gives the daemon.
    Packaged(Box<DaemonInstance>),
    /// A development daemon under this process's own socket root, addressed by
    /// its socket instance name (`velnorctl daemon --name`) when one was
    /// requested; `None` is the unnamed daemon (`default` socket instance,
    /// config at the runner config base).
    Local(Option<String>),
}

impl Selected {
    /// The control endpoint of the selected daemon.
    pub fn endpoint(&self) -> Result<velnor_client::UnixEndpoint, CommandError> {
        match self {
            Self::Packaged(instance) => velnor_client::UnixEndpoint::in_socket_root(
                &velnor_client::socket_root_for_storage_root(Some(&instance.storage_root)),
                &instance.name,
            ),
            Self::Local(name) => {
                velnor_client::UnixEndpoint::from_instance(name.as_deref().unwrap_or("default"))
            }
        }
        .map_err(|error| CommandError::new(ExitClass::Usage, "endpoint.invalid", error.to_string()))
    }

    /// The daemon-scoped directory (`journal.db`, `health.sock`, slot
    /// configs) of the selected daemon.
    pub fn daemon_dir(&self) -> Result<PathBuf, CommandError> {
        match self {
            Self::Packaged(instance) => Ok(instance.daemon_dir.clone()),
            Self::Local(name) => {
                let base = velnor_runner::config_dir(None).map_err(|error| {
                    CommandError::operation(format!("resolve runner config dir: {error}"))
                })?;
                Ok(velnor_runner::runner::daemon_config_dir_under(
                    &base,
                    name.as_deref(),
                ))
            }
        }
    }

    /// `VELNOR_SLOTS` of a packaged instance; development daemons declare
    /// their slot count on the command line, so `None`.
    #[must_use]
    pub fn slots(&self) -> Option<usize> {
        match self {
            Self::Packaged(instance) => instance.slots,
            Self::Local(_) => None,
        }
    }

    #[must_use]
    pub fn packaged(&self) -> Option<&DaemonInstance> {
        match self {
            Self::Packaged(instance) => Some(instance),
            Self::Local(_) => None,
        }
    }
}

/// The instance the operator asked for: the global `--instance`, else
/// `VELNOR_INSTANCE`.
#[must_use]
pub fn requested(globals: &GlobalArgs) -> Option<String> {
    globals
        .instance
        .clone()
        .or_else(|| std::env::var("VELNOR_INSTANCE").ok())
        .filter(|value| !value.trim().is_empty())
}

/// Every packaged instance on this host.
pub fn instances() -> Result<Vec<DaemonInstance>, CommandError> {
    daemon_instance::enumerate().map_err(|error| {
        CommandError::operation(format!(
            "enumerate packaged daemon instances under {}: {error:#}",
            daemon_instance::ETC_DIR
        ))
    })
}

/// Select the daemon for `globals` against this host's packaged instances.
pub fn select(globals: &GlobalArgs) -> Result<Selected, CommandError> {
    select_from(requested(globals).as_deref(), instances()?)
}

/// Pure selection: `requested` against the packaged `instances`.
///
/// * Packaged instances exist: a request names one (by instance or
///   `VELNOR_NAME`); no request selects the only instance, and is a usage
///   error listing the choices when there are several. A request that names
///   none is a usage error listing the choices — it is never treated as a
///   development daemon, because on a packaged host that would silently
///   address a socket no daemon listens on.
/// * No packaged instances: the development daemon named by the request, or
///   `default`.
pub fn select_from(
    requested: Option<&str>,
    instances: Vec<DaemonInstance>,
) -> Result<Selected, CommandError> {
    if instances.is_empty() {
        return Ok(Selected::Local(requested.map(str::to_owned)));
    }
    let known = || {
        instances
            .iter()
            .map(|instance| instance.instance.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match requested {
        Some(selector) => {
            let selector = selector.trim();
            let mut matches = instances
                .iter()
                .filter(|instance| instance.instance == selector || instance.name == selector);
            match (matches.next(), matches.next()) {
                (Some(instance), None) => Ok(Selected::Packaged(Box::new(instance.clone()))),
                (Some(_), Some(_)) => Err(CommandError::new(
                    ExitClass::Usage,
                    "instance.ambiguous",
                    format!("{selector} names more than one packaged instance; known instances: {}", known()),
                )),
                (None, _) => Err(CommandError::new(
                    ExitClass::Usage,
                    "instance.unknown",
                    format!(
                        "no packaged daemon instance {selector} under {}; known instances: {}",
                        daemon_instance::ETC_DIR,
                        known()
                    ),
                )),
            }
        }
        None => match instances.as_slice() {
            [only] => Ok(Selected::Packaged(Box::new(only.clone()))),
            _ => Err(CommandError::new(
                ExitClass::Usage,
                "instance.required",
                format!(
                    "this host runs several packaged daemon instances; pass --instance NAME (known: {})",
                    known()
                ),
            )),
        },
    }
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
    use std::fs;
    use std::path::Path;

    /// Stage the env files the Sentry audit found and resolve them through
    /// the runner's own instance resolver.
    fn sentry_like_instances(root: &Path) -> Vec<DaemonInstance> {
        let etc = root.join("etc/velnor");
        fs::create_dir_all(&etc).unwrap();
        fs::write(
            etc.join("dogfood.env"),
            "VELNOR_NAME=velnor-dogfood\nVELNOR_SLOTS=5\nVELNOR_WORK_DIR=/var/lib/velnor-dogfood/work\nVELNOR_TRUST_SCOPE=trusted\n",
        )
        .unwrap();
        fs::write(
            etc.join("velnor.env"),
            "VELNOR_NAME=velnor-java-monorepo\nVELNOR_SLOTS=4\nVELNOR_WORK_DIR=/var/lib/velnor/work\n",
        )
        .unwrap();
        daemon_instance::enumerate_in(&etc, &root.join("systemd")).unwrap()
    }

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnorctl-packaged-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn packaged_socket_root_is_the_instance_run_root() {
        // `velnor_client` and the runner's storage layout spell the same
        // rule for the control-plane socket root; a packaged instance's
        // endpoint therefore lands where its daemon binds.
        let root = temp_root("socket-root");
        let instances = sentry_like_instances(&root);
        for instance in &instances {
            assert_eq!(
                velnor_client::socket_root_for_storage_root(Some(&instance.storage_root)),
                instance.run_root
            );
            let endpoint = Selected::Packaged(Box::new(instance.clone()))
                .endpoint()
                .unwrap();
            assert_eq!(
                endpoint.socket_path(velnor_client::SocketKind::Control),
                instance.control_socket()
            );
        }
        let dogfood = instances
            .iter()
            .find(|instance| instance.instance == "dogfood")
            .unwrap();
        assert_eq!(
            dogfood.control_socket(),
            Path::new("/run/velnor/velnor-dogfood/control.sock")
        );
        assert_eq!(
            velnor_client::socket_root_for_storage_root(Some(Path::new("/srv/velnor"))),
            Path::new("/srv/velnor/run/velnor")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selection_follows_the_packaged_instances_when_present() {
        let root = temp_root("select");
        let instances = sentry_like_instances(&root);

        // By systemd instance name and by VELNOR_NAME.
        let by_instance = select_from(Some("dogfood"), instances.clone()).unwrap();
        let by_name = select_from(Some("velnor-dogfood"), instances.clone()).unwrap();
        assert_eq!(by_instance, by_name);
        let Selected::Packaged(dogfood) = by_instance else {
            panic!("expected a packaged instance");
        };
        assert_eq!(
            dogfood.daemon_dir,
            Path::new("/var/lib/velnor-dogfood/runner/daemons/velnor-dogfood")
        );
        assert_eq!(Selected::Packaged(dogfood).slots(), Some(5));

        // Several instances and no request: the operator must choose, and is
        // told the choices.
        let error = select_from(None, instances.clone()).unwrap_err();
        assert_eq!(error.class, ExitClass::Usage);
        assert_eq!(error.reason, "instance.required");
        assert!(
            error.message.contains("dogfood, velnor"),
            "{}",
            error.message
        );

        // A name that is not packaged never falls through to a dev socket.
        let error = select_from(Some("primary"), instances.clone()).unwrap_err();
        assert_eq!(error.reason, "instance.unknown");

        // One instance and no request selects it.
        let only = vec![instances[0].clone()];
        assert_eq!(
            select_from(None, only.clone()).unwrap(),
            Selected::Packaged(Box::new(instances[0].clone()))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn development_host_addresses_a_local_daemon() {
        assert_eq!(
            select_from(None, Vec::new()).unwrap(),
            Selected::Local(None)
        );
        assert_eq!(
            select_from(Some("primary"), Vec::new()).unwrap(),
            Selected::Local(Some("primary".to_owned()))
        );
        assert_eq!(
            Selected::Local(Some("primary".to_owned()))
                .endpoint()
                .unwrap(),
            velnor_client::UnixEndpoint::from_instance("primary").unwrap()
        );
        assert_eq!(
            Selected::Local(None).endpoint().unwrap(),
            velnor_client::UnixEndpoint::from_instance("default").unwrap()
        );
        // The unnamed development daemon keeps its config at the base the
        // runner resolves for this process; a named one under daemons/<name>.
        let base = velnor_runner::config_dir(None).unwrap();
        assert_eq!(Selected::Local(None).daemon_dir().unwrap(), base);
        assert_eq!(
            Selected::Local(Some("primary".to_owned()))
                .daemon_dir()
                .unwrap(),
            base.join("daemons").join("primary")
        );
    }
}
