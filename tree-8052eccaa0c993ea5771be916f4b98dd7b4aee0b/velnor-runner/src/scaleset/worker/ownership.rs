//! Recorded ownership identity for one homogeneous scale-set worker.
//!
//! Every Docker object a worker creates — DinD container, runner container,
//! per-worker network, workspace volume, DinD data volume — carries the
//! ownership labels below, and every object name derives deterministically
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

/// Label carrying the stable ownership id on every worker-owned object.
pub const OWNERSHIP_LABEL: &str = "velnor.scaleset.ownership";
/// Label carrying the GitHub runner name on every worker-owned object.
pub const RUNNER_LABEL: &str = "velnor.scaleset.runner";
/// Label carrying the scale-set id on every worker-owned object.
pub const SCALE_SET_LABEL: &str = "velnor.scaleset.set";
/// Label distinguishing the two containers of a worker pair.
pub const WORKER_ROLE_LABEL: &str = "velnor.scaleset.role";
/// Role value for the private DinD daemon container.
pub const ROLE_DIND: &str = "dind";
/// Role value for the official runner container.
pub const ROLE_RUNNER: &str = "runner";

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

    /// Workspace volume name (`_work`, tool cache, file-command dirs).
    #[must_use]
    pub fn workspace_volume(&self) -> String {
        format!("{OBJECT_PREFIX}-work-{}", self.ownership.slug())
    }

    /// Tool cache volume name (shares the worker's workspace volume).
    #[must_use]
    pub fn tool_cache_volume(&self) -> String {
        self.workspace_volume()
    }

    /// DinD data volume name (`/var/lib/docker` inside the daemon).
    #[must_use]
    pub fn dind_data_volume(&self) -> String {
        format!("{OBJECT_PREFIX}-dindata-{}", self.ownership.slug())
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

    /// `--label` argv pairs for one object of `role` (`dind`|`runner`).
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
            first.workspace_volume(),
            "velnor-scaleset-work-s7-velnor-set-0007-2ad92676"
        );
        assert_eq!(
            first.tool_cache_volume(),
            "velnor-scaleset-work-s7-velnor-set-0007-2ad92676"
        );
        assert_eq!(
            first.dind_data_volume(),
            "velnor-scaleset-dindata-s7-velnor-set-0007-2ad92676"
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
            assert_ne!(a.workspace_volume(), other.workspace_volume());
            assert_ne!(a.tool_cache_volume(), other.tool_cache_volume());
            assert_ne!(a.dind_data_volume(), other.dind_data_volume());
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
            slash_identity.workspace_volume(),
            space_identity.workspace_volume()
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
