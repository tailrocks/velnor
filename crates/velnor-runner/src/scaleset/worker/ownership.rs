//! Recorded ownership identity for one homogeneous scale-set worker.
//!
//! Every Docker object a worker creates — volume holder, DinD container,
//! runner container, and per-worker network — carries the ownership labels
//! below, and every object name derives deterministically
//! from the stable [`OwnershipId`]. There is no random suffix anywhere on
//! the provision path: a retried provision converges on the same names and
//! adopts matching objects instead of leaking duplicates.
//!
//! Collision isolation: two workers may run jobs with the same inner
//! container names or the same inner ports. Outer names are namespaced by
//! the ownership id, and each worker pair lives on its own bridge network
//! with the runner joined to its DinD's network namespace, so inner names
//! and ports can never observe each other.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

/// The creation contract stored on the management-owned container, not in
/// job environment. Used to reconcile daemon-side binds after a restart.
pub const STATE_SOURCE_LABEL: &str = "velnor.scaleset.state-source";

/// Resolve a discovery name once. Never pass the name to a later mutation.
pub(crate) fn container_id(runner: &mut dyn super::WorkerRunner, name: &str) -> Result<String> {
    let output = runner.run(
        "docker",
        &[
            "inspect".into(),
            "--format".into(),
            "{{.Id}}".into(),
            "--".into(),
            name.into(),
        ],
    )?;
    if output.code != 0 {
        if crate::docker::client::daemon_reports_missing(&output.stderr) {
            return Err(super::RestartObjectMissing.into());
        }
        anyhow::bail!("container identity lookup failed (exit {})", output.code);
    }
    super::dind::parse_container_id(&output.stdout)
        .context("container identity lookup returned no id")
}

/// Deliberately excludes Config.Env (JIT credentials). HostConfig.Binds alone
/// is insufficient: --mount and --volumes-from also affect the actual Mounts.
pub(crate) const ISOLATION_FORMAT: &str = r#"{{json .Id}}{{"\t"}}{{json .Mounts}}{{"\t"}}{{json .HostConfig.Privileged}}{{"\t"}}{{json .HostConfig.PortBindings}}{{"\t"}}{{json .HostConfig.PublishAllPorts}}{{"\t"}}{{json .HostConfig.GroupAdd}}{{"\t"}}{{json .Config.Entrypoint}}{{"\t"}}{{json .Config.Cmd}}{{"\t"}}{{json .Config.User}}{{"\t"}}{{json .Config.Labels}}{{"\t"}}{{json .HostConfig.Mounts}}"#;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Mount {
    #[serde(rename = "Type")]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub driver: String,
    #[serde(rename = "RW")]
    pub writable: bool,
    #[serde(default)]
    pub propagation: String,
}

/// Attest every effective mount, not only the requested inheritance. Callers
/// provide the holder's attested volumes, and retain the returned ID for all
/// later mutations. An extra bind/anonymous image volume is a contract error.
pub(crate) fn attest_isolation(
    runner: &mut dyn super::WorkerRunner,
    identity: &WorkerIdentity,
    target: &str,
    role: &str,
    volumes: &[Mount],
    state_dir: Option<&std::path::Path>,
) -> Result<String> {
    let output = runner.run(
        "docker",
        &[
            "inspect".into(),
            "--format".into(),
            ISOLATION_FORMAT.into(),
            "--".into(),
            target.into(),
        ],
    )?;
    if output.code != 0 {
        anyhow::bail!(
            "container isolation inspection failed (exit {})",
            output.code
        );
    }
    let result = validate_isolation(identity, target, role, volumes, state_dir, &output.stdout);
    if let Err(error) = &result {
        eprintln!("temporary isolation diagnostic: {error:#}");
    }
    result
}

fn validate_isolation(
    identity: &WorkerIdentity,
    target: &str,
    role: &str,
    volumes: &[Mount],
    state_dir: Option<&std::path::Path>,
    projection: &str,
) -> Result<String> {
    use super::{dind, runner};
    let fields: Vec<_> = projection.trim().split('\t').collect();
    if fields.len() != 11 {
        anyhow::bail!("malformed container isolation projection");
    }
    let id: String = serde_json::from_str(fields[0])?;
    if id.is_empty()
        || (target != identity.dind_container()
            && target != identity.runner_container()
            && target != identity.volume_holder_container()
            && id != target)
    {
        anyhow::bail!("immutable container identity mismatch");
    }
    let mounts: Vec<Mount> = serde_json::from_str(fields[1])?;
    let privileged: bool = serde_json::from_str(fields[2])?;
    let ports: Option<BTreeMap<String, serde_json::Value>> = serde_json::from_str(fields[3])?;
    let publish_all: bool = serde_json::from_str(fields[4])?;
    let groups: Option<Vec<String>> = serde_json::from_str(fields[5])?;
    let entrypoint: Option<Vec<String>> = serde_json::from_str(fields[6])?;
    let command: Option<Vec<String>> = serde_json::from_str(fields[7])?;
    let user: String = serde_json::from_str(fields[8])?;
    let labels: BTreeMap<String, String> = serde_json::from_str(fields[9])?;
    let requested: Option<Vec<serde_json::Value>> = serde_json::from_str(fields[10])?;
    for (key, value) in identity.labels() {
        if labels.get(&key) != Some(&value) {
            anyhow::bail!("container ownership changed during isolation attestation");
        }
    }
    if labels.get(WORKER_ROLE_LABEL).map(String::as_str) != Some(role)
        || privileged != (role == ROLE_DIND)
        || publish_all
        || !ports.unwrap_or_default().is_empty()
    {
        anyhow::bail!("container privilege/port/role contract mismatch");
    }
    let (expected_entrypoint, expected_command) = match role {
        ROLE_DIND => (
            vec![dind::DIND_ENTRYPOINT.to_string()],
            dind::daemon_command(),
        ),
        ROLE_RUNNER => (vec![], vec![runner::RUNNER_START_COMMAND.to_string()]),
        ROLE_VOLUME_HOLDER => (vec![], vec![dind::VOLUME_HOLDER_COMMAND.to_string()]),
        _ => anyhow::bail!("unknown worker role"),
    };
    if entrypoint.unwrap_or_default() != expected_entrypoint || command != Some(expected_command) {
        anyhow::bail!("container executable contract mismatch");
    }
    if role == ROLE_RUNNER {
        if user != "runner" || groups.as_deref() != Some(&[dind::DIND_SOCKET_GID.to_string()]) {
            anyhow::bail!("runner user/private socket group mismatch");
        }
    } else if role == ROLE_DIND && !matches!(user.as_str(), "" | "0" | "root") {
        anyhow::bail!("DinD must use its root image user");
    }
    let expected_state = if role == ROLE_VOLUME_HOLDER {
        None
    } else {
        let recorded = labels
            .get(STATE_SOURCE_LABEL)
            .context("container missing state bind intent")?;
        if !std::path::Path::new(recorded).is_absolute()
            || state_dir.is_some_and(|path| path != std::path::Path::new(recorded))
        {
            anyhow::bail!("container state bind intent mismatch");
        }
        Some(recorded)
    };
    if mounts.len() != volumes.len() + usize::from(expected_state.is_some()) {
        anyhow::bail!("container has missing or additional effective mounts");
    }
    let mut seen = std::collections::BTreeSet::new();
    for mount in &mounts {
        if !seen.insert(&mount.destination) || !mount.writable {
            anyhow::bail!("container duplicate/read-only mount");
        }
        if mount.destination == dind::STATE_MOUNT && expected_state.is_some() {
            if mount.kind != "bind"
                || Some(&mount.source) != expected_state
                || mount.propagation != "rprivate"
            {
                anyhow::bail!("container state bind mismatch");
            }
        } else if !volumes.iter().any(|expected| {
            mount.kind == "volume"
                && mount.name == expected.name
                && mount.source == expected.source
                && mount.destination == expected.destination
                && mount.driver == "local"
        }) {
            anyhow::bail!("container inherited volume identity mismatch");
        }
    }
    if role == ROLE_VOLUME_HOLDER {
        let requested = requested.context("holder omitted anonymous mount requests")?;
        if requested.len() != volumes.len() {
            anyhow::bail!("holder anonymous mount request count mismatch");
        }
        let mut targets = std::collections::BTreeSet::new();
        for mount in &requested {
            let destination = mount
                .get("Target")
                .and_then(serde_json::Value::as_str)
                .context("holder mount missing target")?;
            if mount.get("Type").and_then(serde_json::Value::as_str) != Some("volume")
                || mount
                    .get("Source")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                || !targets.insert(destination)
                || !volumes.iter().any(|v| v.destination == destination)
            {
                anyhow::bail!("holder has non-anonymous/extra mount request");
            }
        }
    } else if !requested.unwrap_or_default().is_empty() {
        anyhow::bail!("worker has unexpected direct mount requests");
    }
    Ok(id)
}

/// Label carrying the stable ownership id on every worker-owned object.
pub const OWNERSHIP_LABEL: &str = "velnor.scaleset.ownership";
/// Label carrying the GitHub runner name on every worker-owned object.
pub const RUNNER_LABEL: &str = "velnor.scaleset.runner";
/// Label carrying the scale-set id on every worker-owned object.
pub const SCALE_SET_LABEL: &str = "velnor.scaleset.set";
/// Label distinguishing the worker-owned container roles.
pub const WORKER_ROLE_LABEL: &str = "velnor.scaleset.role";
/// Role value for the private DinD daemon container.
pub const ROLE_DIND: &str = "dind";
/// Role value for the official runner container.
pub const ROLE_RUNNER: &str = "runner";
/// Role value for the never-started anonymous-volume holder container.
pub const ROLE_VOLUME_HOLDER: &str = "volume-holder";

/// Outer-name prefix for every Docker object the adapter owns.
pub const OBJECT_PREFIX: &str = "velnor-scaleset";

/// Stable ownership identity for one worker: `f(scale_set_id, runner_name)`.
///
/// The id is stable across daemon restarts and provision retries, so it
/// doubles as the provisioning idempotency key: re-provisioning derives
/// the same container, network, volume, and workspace names and adopts
/// live objects instead of creating new ones.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OwnershipId {
    scale_set_id: i32,
    runner_name: String,
}

impl OwnershipId {
    /// Bind an ownership id to one acquired job's runner name.
    ///
    /// Runner names come from the adapter's journal counter
    /// (`<set>-<seq>`), never from GitHub traffic, so binding cannot
    /// collide with an unrelated worker.
    #[must_use]
    pub fn bind(scale_set_id: i32, runner_name: &str) -> Self {
        Self {
            scale_set_id,
            runner_name: runner_name.to_string(),
        }
    }

    /// Canonical id string: `<set-id>/<runner-name>`.
    #[must_use]
    pub fn as_str(&self) -> String {
        format!("{}/{}", self.scale_set_id, self.runner_name)
    }

    #[must_use]
    pub fn scale_set_id(&self) -> i32 {
        self.scale_set_id
    }

    #[must_use]
    pub fn runner_name(&self) -> &str {
        &self.runner_name
    }

    /// Filesystem/Docker-safe slug: `s<set>-<sanitized-runner-name>-<hash>`.
    ///
    /// Docker object names and host paths cannot carry `/`, so the slug —
    /// not the canonical id — seeds every derived name. Sanitization alone
    /// collides (`a/b` vs `a b` → `a-b`), so a short stable hash of the
    /// canonical id disambiguates: distinct ids never share Docker object
    /// names, and the adoption gate keeps failing closed on top. The hash
    /// is sha256 (first 32 bits, hex), never `DefaultHasher`: slug
    /// derivation must be stable across processes and restarts because the
    /// recorded identity is the provisioning idempotency key.
    #[must_use]
    pub fn slug(&self) -> String {
        let sanitized: String = self
            .runner_name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.as_str().as_bytes());
        let hash = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
        format!("s{}-{sanitized}-{hash:08x}", self.scale_set_id)
    }
}

/// Recorded identity of every object one worker owns.
///
/// Pure derivation: constructing this performs no I/O. Provisioning
/// records the struct (journal `ScaleSetProvisionIntended` +
/// `scaleset_workers` row) BEFORE the first Docker call, so a crash
/// between Docker calls still converges on these exact names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerIdentity {
    ownership: OwnershipId,
}

impl WorkerIdentity {
    #[must_use]
    pub fn new(ownership: OwnershipId) -> Self {
        Self { ownership }
    }

    #[must_use]
    pub fn ownership(&self) -> &OwnershipId {
        &self.ownership
    }

    /// Never-started holder container name for the worker's anonymous volumes.
    #[must_use]
    pub fn volume_holder_container(&self) -> String {
        format!("{OBJECT_PREFIX}-volume-holder-{}", self.ownership.slug())
    }

    /// Private DinD daemon container name.
    #[must_use]
    pub fn dind_container(&self) -> String {
        format!("{OBJECT_PREFIX}-dind-{}", self.ownership.slug())
    }

    /// Official runner container name.
    #[must_use]
    pub fn runner_container(&self) -> String {
        format!("{OBJECT_PREFIX}-runner-{}", self.ownership.slug())
    }

    /// Per-worker bridge network name. Each worker pair gets its own
    /// network so inner ports never collide across workers.
    #[must_use]
    pub fn network(&self) -> String {
        format!("{OBJECT_PREFIX}-net-{}", self.ownership.slug())
    }

    /// Host directory holding this worker's socket + diagnostics.
    /// Joined under the daemon's scale-set state dir by the caller.
    #[must_use]
    pub fn state_dir_name(&self) -> String {
        self.ownership.slug()
    }

    /// Labels stamped on every object this worker creates.
    #[must_use]
    pub fn labels(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (OWNERSHIP_LABEL.to_string(), self.ownership.as_str()),
            (RUNNER_LABEL.to_string(), self.ownership.runner_name.clone()),
            (
                SCALE_SET_LABEL.to_string(),
                self.ownership.scale_set_id.to_string(),
            ),
        ])
    }

    /// `--label` argv pairs for one object of `role`.
    #[must_use]
    pub fn label_args(&self, role: &str) -> Vec<String> {
        let mut args = Vec::new();
        for (key, value) in self.labels() {
            args.push("--label".to_string());
            args.push(format!("{key}={value}"));
        }
        args.push("--label".to_string());
        args.push(format!("{WORKER_ROLE_LABEL}={role}"));
        args
    }

    /// Parse the ownership id back out of an inspected label set.
    /// Returns `None` when the object is not worker-owned.
    #[must_use]
    pub fn ownership_of(labels: &BTreeMap<String, String>) -> Option<String> {
        labels.get(OWNERSHIP_LABEL).cloned()
    }
}

/// Stable Engine-shaped fixtures. Security-negative cases mutate these
/// documents, independently of Docker argument construction.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    pub fn holder() -> super::super::dind::VolumeHolderAttestation {
        let mounts = [
            ("anonymous-work", "/home/runner/_work"),
            ("anonymous-tools", "/opt/hostedtoolcache"),
            ("anonymous-docker", "/var/lib/docker"),
        ]
        .into_iter()
        .map(|(name, destination)| Mount {
            kind: "volume".into(),
            name: name.into(),
            source: format!("/var/lib/docker/volumes/{name}/_data"),
            destination: destination.into(),
            driver: "local".into(),
            writable: true,
            propagation: String::new(),
        })
        .collect();
        super::super::dind::VolumeHolderAttestation {
            id: "holder-object-id".into(),
            mounts,
        }
    }

    pub fn isolation(identity: &WorkerIdentity, id: &str, role: &str, state: &str) -> String {
        let mut labels = identity.labels();
        labels.insert(WORKER_ROLE_LABEL.into(), role.into());
        let mut mounts: Vec<serde_json::Value> = holder().mounts.iter().map(|mount| serde_json::json!({
            "Type":"volume","Name":mount.name,"Source":mount.source,"Destination":mount.destination,
            "Driver":"local","RW":true,"Propagation":"",
        })).collect();
        if role != ROLE_VOLUME_HOLDER {
            labels.insert(STATE_SOURCE_LABEL.into(), state.into());
            mounts.push(serde_json::json!({"Type":"bind","Source":state,"Destination":"/velnor/scaleset","RW":true,"Propagation":"rprivate"}));
        }
        let entrypoint = if role == ROLE_DIND {
            serde_json::json!(["dockerd-entrypoint.sh"])
        } else {
            serde_json::Value::Null
        };
        let command = match role {
            ROLE_DIND => serde_json::json!([
                "dockerd",
                "--host=unix:///velnor/scaleset/dind.sock",
                "--group=123"
            ]),
            ROLE_RUNNER => serde_json::json!(["/home/runner/run.sh"]),
            _ => serde_json::json!(["/bin/true"]),
        };
        let requested = if role == ROLE_VOLUME_HOLDER {
            serde_json::json!([
                {"Type":"volume","Target":"/home/runner/_work","Source":""},
                {"Type":"volume","Target":"/opt/hostedtoolcache","Source":""},
                {"Type":"volume","Target":"/var/lib/docker","Source":""}
            ])
        } else {
            serde_json::Value::Null
        };
        [
            serde_json::json!(id),
            serde_json::json!(mounts),
            serde_json::json!(role == ROLE_DIND),
            serde_json::json!({}),
            serde_json::json!(false),
            if role == ROLE_RUNNER {
                serde_json::json!(["123"])
            } else {
                serde_json::Value::Null
            },
            entrypoint,
            command,
            serde_json::json!(if role == ROLE_DIND { "" } else { "runner" }),
            serde_json::json!(labels),
            requested,
        ]
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\t")
    }

    pub fn volume(identity: &WorkerIdentity, name: &str) -> String {
        let mut labels = identity.labels();
        labels.insert(WORKER_ROLE_LABEL.into(), ROLE_VOLUME_HOLDER.into());
        [
            serde_json::json!(name),
            serde_json::json!("local"),
            serde_json::json!(labels),
            serde_json::Value::Null,
            serde_json::json!(format!("/var/lib/docker/volumes/{name}/_data")),
        ]
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\t")
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

    fn identity() -> WorkerIdentity {
        WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007"))
    }

    #[test]
    fn names_derive_deterministically_from_ownership() {
        let first = identity();
        let second = identity();
        assert_eq!(first, second);
        assert_eq!(
            first.dind_container(),
            "velnor-scaleset-dind-s7-velnor-set-0007-2ad92676"
        );
        assert_eq!(
            first.runner_container(),
            "velnor-scaleset-runner-s7-velnor-set-0007-2ad92676"
        );
        assert_eq!(
            first.network(),
            "velnor-scaleset-net-s7-velnor-set-0007-2ad92676"
        );
        assert_eq!(
            first.volume_holder_container(),
            "velnor-scaleset-volume-holder-s7-velnor-set-0007-2ad92676"
        );
    }

    #[test]
    fn distinct_workers_never_share_an_object_name() {
        let a = WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0007"));
        let b = WorkerIdentity::new(OwnershipId::bind(7, "velnor-set-0008"));
        let c = WorkerIdentity::new(OwnershipId::bind(9, "velnor-set-0007"));
        for other in [&b, &c] {
            assert_ne!(a.dind_container(), other.dind_container());
            assert_ne!(a.runner_container(), other.runner_container());
            assert_ne!(a.network(), other.network());
            assert_ne!(a.volume_holder_container(), other.volume_holder_container());
        }
    }

    #[test]
    fn slug_sanitizes_unsafe_characters() {
        let id = OwnershipId::bind(7, "we/ird name!");
        assert_eq!(id.slug(), "s7-we-ird-name--8ef91d72");
        assert_eq!(id.as_str(), "7/we/ird name!");
    }

    #[test]
    fn slug_hash_disambiguates_colliding_sanitizations() {
        // `a/b` and `a b` sanitize identically; the canonical-id hash
        // keeps their Docker object names apart.
        let slash = OwnershipId::bind(7, "a/b");
        let space = OwnershipId::bind(7, "a b");
        assert_ne!(slash.slug(), space.slug());
        let slash_identity = WorkerIdentity::new(slash);
        let space_identity = WorkerIdentity::new(space);
        assert_ne!(
            slash_identity.runner_container(),
            space_identity.runner_container()
        );
        assert_ne!(
            slash_identity.dind_container(),
            space_identity.dind_container()
        );
        assert_ne!(slash_identity.network(), space_identity.network());
        assert_ne!(
            slash_identity.volume_holder_container(),
            space_identity.volume_holder_container()
        );
        // And derivation is stable: the same id re-derives the same slug
        // on every call, so retries converge instead of leaking.
        assert_eq!(
            slash_identity.runner_container(),
            slash_identity.runner_container()
        );
        assert_eq!(
            WorkerIdentity::new(OwnershipId::bind(7, "a/b"))
                .ownership()
                .slug(),
            slash_identity.ownership().slug()
        );
    }

    #[test]
    fn labels_carry_ownership_runner_and_set() {
        let labels = identity().labels();
        assert_eq!(
            labels.get(OWNERSHIP_LABEL).map(String::as_str),
            Some("7/velnor-set-0007")
        );
        assert_eq!(
            labels.get(RUNNER_LABEL).map(String::as_str),
            Some("velnor-set-0007")
        );
        assert_eq!(labels.get(SCALE_SET_LABEL).map(String::as_str), Some("7"));
        let args = identity().label_args(ROLE_DIND);
        assert!(args.contains(&format!("{WORKER_ROLE_LABEL}={ROLE_DIND}")));
        let runner_args = identity().label_args(ROLE_RUNNER);
        assert!(runner_args.contains(&format!("{WORKER_ROLE_LABEL}={ROLE_RUNNER}")));
        let holder_args = identity().label_args(ROLE_VOLUME_HOLDER);
        assert!(holder_args.contains(&format!("{WORKER_ROLE_LABEL}={ROLE_VOLUME_HOLDER}")));
    }

    #[test]
    fn ownership_parses_back_from_labels() {
        let labels = identity().labels();
        assert_eq!(
            WorkerIdentity::ownership_of(&labels).as_deref(),
            Some("7/velnor-set-0007")
        );
        assert_eq!(WorkerIdentity::ownership_of(&BTreeMap::new()), None);
    }
}
