//! One-time purge of the pre-key trust-scope filesystem roots.
//!
//! This module names only the old root grammar. It never opens or copies a
//! scope's files; the mount-aware remover owns traversal and refuses symlinks,
//! mount crossings, and paths outside the configured anchor.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Read},
    os::unix::fs::OpenOptionsExt as _,
    path::{Component, Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};
const LOCK_WAIT: Duration = Duration::from_secs(30);
const LOCK_RETRY: Duration = Duration::from_millis(10);
const PACKAGE_TRANSACTION_LOCK: &str = "/run/velnor/package-transaction.lock";
const DBUS_STANDARD_SYSTEM_SERVICE_DIRS: &[&str] = &[
    "/etc/dbus-1/system-services",
    "/run/dbus-1/system-services",
    "/usr/local/share/dbus-1/system-services",
    "/usr/share/dbus-1/system-services",
    "/lib/dbus-1/system-services",
];
const GC_LOCK: &str = "gc.lock";
const FILESYSTEM_COORDINATOR_LOCK: &str = "filesystem-coordinator.lock";
const LEGACY_LEASE_ROOT: &str = "leases";
const KEYED_LEASE_ROOT: &str = "leases__trust_scope_v2";

const CANONICAL_CLASSES: &[&str] = &[
    "cargo",
    "mise",
    "targets",
    "caches",
    "git-mirrors",
    "compiler/mbx",
    "compiler/sccache",
];

const WORK_FAMILIES: &[&str] = &[
    "_velnor_cargo",
    "_velnor_mise",
    "_velnor_targets",
    "_velnor_caches",
    "_velnor_mbx",
    "_velnor_sccache",
    "_velnor_git",
];

const LEGACY_LEASE_CLASSES: &[&str] = &[
    "actions-cache",
    "artifacts",
    "cargo",
    "mise",
    "mbx",
    "sccache",
    "targets",
];
const ROOT_ENVIRONMENT_KEYS: &[&str] = &[
    "VELNOR_NAME",
    "VELNOR_LABELS",
    "VELNOR_STORAGE_ROOT",
    "VELNOR_WORK_DIR",
    "VELNOR_SLOTS",
    "VELNOR_TRUST_SCOPE",
];

type DirectoryIdentity = crate::leftover_disk::FilesystemDirectoryIdentity;
type AnchorIdentities = BTreeMap<PathBuf, Option<DirectoryIdentity>>;

#[derive(Debug)]
struct PhysicalPlanPaths {
    candidates: BTreeMap<PathBuf, PathBuf>,
    preserved: BTreeMap<PathBuf, PathBuf>,
    candidate_identities: BTreeMap<PathBuf, PhysicalDirectorySnapshot>,
    preserved_identities: BTreeMap<PathBuf, PhysicalDirectorySnapshot>,
    // Keep protected directory descriptors open through deletion. This pins
    // their inode and mount identity while each candidate is rechecked and
    // removed relative to its separately pinned anchor.
    _preserved_pins: BTreeMap<PathBuf, Option<PinnedDirectory>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PhysicalDirectorySnapshot {
    root: Option<DirectoryIdentity>,
    locations: Vec<PhysicalPathLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PhysicalPathLocation {
    identity: DirectoryIdentity,
    suffix: PathBuf,
}

#[derive(Debug)]
struct PinnedDirectory {
    _file: File,
    identity: DirectoryIdentity,
}

impl PartialEq for PhysicalPlanPaths {
    fn eq(&self, other: &Self) -> bool {
        self.candidates == other.candidates
            && self.preserved == other.preserved
            && self.candidate_identities == other.candidate_identities
            && self.preserved_identities == other.preserved_identities
            && self
                ._preserved_pins
                .iter()
                .map(|(path, pin)| (path, pin.as_ref().map(|pin| &pin.identity)))
                .eq(other
                    ._preserved_pins
                    .iter()
                    .map(|(path, pin)| (path, pin.as_ref().map(|pin| &pin.identity))))
    }
}

impl Eq for PhysicalPlanPaths {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CandidateRoot {
    instance: String,
    class: &'static str,
    anchor: PathBuf,
    anchor_identity: Option<DirectoryIdentity>,
    candidate_identity: Option<DirectoryIdentity>,
    legacy_lease_entries: Option<Vec<crate::leftover_disk::FilesystemEntry>>,
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct InstancePlan {
    candidates: Vec<CandidateRoot>,
    preserved: Vec<PathBuf>,
    anchor_identities: AnchorIdentities,
}

/// Purge the exact old roots for every configured packaged daemon instance.
/// An unconfigured host is a no-op; no process environment fallback exists.
pub(crate) fn purge_configured_legacy_roots() -> Result<()> {
    verify_package_purge_preconditions()?;
    purge_instances_with(
        crate::daemon_instance::enumerate,
        verify_complete_instance_inventory,
        verify_effective_unit,
    )
}

/// Prove that configured roots and effective systemd units are replayable,
/// without creating roots, taking locks, or deleting anything. Debian runs
/// this before its first package-owned mutation.
pub(crate) fn verify_configured_legacy_roots() -> Result<()> {
    verify_package_purge_preconditions()?;
    let instances = crate::daemon_instance::enumerate()?;
    verify_complete_instance_inventory(&instances)?;
    for instance in &instances {
        verify_effective_unit(instance)?;
    }
    validate_plans(&plans_for_instances(&instances)?)?;
    Ok(())
}

fn purge_instances_with(
    mut enumerate: impl FnMut() -> Result<Vec<crate::daemon_instance::DaemonInstance>>,
    verify_inventory: impl Fn(&[crate::daemon_instance::DaemonInstance]) -> Result<()>,
    verify_unit: impl Fn(&crate::daemon_instance::DaemonInstance) -> Result<()>,
) -> Result<()> {
    let instances = enumerate()?;
    if instances.is_empty() {
        verify_inventory(&instances)?;
        return Ok(());
    }

    // Prove every unit before acquiring locks or touching any store. A bad
    // instance must not leave a partially purged host.
    verify_inventory(&instances)?;
    for instance in &instances {
        verify_unit(instance)?;
    }
    prepare_runtime_roots(&instances)?;
    let initial_plans = plans_for_instances(&instances)?;
    let initial_physical_paths = validate_plans(&initial_plans)?;

    let run_roots = unique_run_roots(&instances)?;
    let _locks = acquire_instance_locks(&run_roots, LOCK_WAIT)?;

    // A configured env file or unit may have changed while waiting. Repeat
    // unit proof and inventory under the locks, then delete only this snapshot.
    let current_instances = enumerate()?;
    if current_instances != instances {
        bail!("configured daemon roots changed while legacy purge waited for locks");
    }
    verify_inventory(&current_instances)?;
    for instance in &current_instances {
        verify_unit(instance)?;
    }
    let plans = plans_for_instances(&current_instances)?;
    let physical_paths = validate_plans(&plans)?;
    if physical_paths != initial_physical_paths {
        bail!("physical legacy purge paths changed while waiting for locks");
    }
    validate_anchor_snapshots(&initial_plans, &plans)?;
    validate_candidate_snapshots(&initial_plans, &plans)?;
    let candidates = plans
        .iter()
        .flat_map(|plan| plan.candidates.iter().cloned())
        .collect::<Vec<_>>();
    let preserved = plans
        .iter()
        .flat_map(|plan| plan.preserved.iter().cloned())
        .collect::<Vec<_>>();
    remove_candidates(&candidates, &preserved, &physical_paths)
}

/// Exercise the same lock, inventory, snapshot, and deletion path with a
/// caller-supplied instance list. Unit tests use temporary configured roots;
/// effective systemd state is the only production-only gate omitted here.
#[cfg(test)]
pub(crate) fn purge_instances_for_tests(
    instances: &[crate::daemon_instance::DaemonInstance],
) -> Result<()> {
    purge_instances_with(|| Ok(instances.to_vec()), |_| Ok(()), |_| Ok(()))
}

fn plans_for_instances(
    instances: &[crate::daemon_instance::DaemonInstance],
) -> Result<Vec<InstancePlan>> {
    instances.iter().map(plan_for_instance).collect()
}

fn plan_for_instance(instance: &crate::daemon_instance::DaemonInstance) -> Result<InstancePlan> {
    let shared_work_root = legacy_shared_work_root(instance);
    let anchor_paths = [
        instance.cache_root.clone(),
        instance.work_dir.clone(),
        shared_work_root,
        instance.run_root.clone(),
    ];
    let mut anchor_identities = capture_anchor_identities(&anchor_paths)?;
    let slot_anchor_identity = anchor_identities
        .get(&instance.work_dir)
        .and_then(Option::as_ref);
    let slot_roots = configured_slot_work_roots(instance, slot_anchor_identity)?;
    for (path, identity) in capture_anchor_identities(&slot_roots)? {
        if let Some(previous) = anchor_identities.insert(path.clone(), identity.clone()) {
            if previous != identity {
                bail!(
                    "configured slot root changed during inventory: {}",
                    path.display()
                );
            }
        }
    }
    let mut candidates = inventory_for_instance(instance, &slot_roots, &anchor_identities)?;
    for candidate in &mut candidates {
        candidate.anchor_identity = anchor_identities.get(&candidate.anchor).cloned().flatten();
    }
    pin_candidate_identities(&mut candidates)?;
    Ok(InstancePlan {
        candidates,
        preserved: preserved_roots(instance, &slot_roots),
        anchor_identities,
    })
}

fn capture_anchor_identities(paths: &[PathBuf]) -> Result<AnchorIdentities> {
    let mut identities = std::collections::BTreeMap::new();
    for path in paths {
        require_absolute_normalized(path, "configured cleanup anchor")?;
        let identity = match fs::symlink_metadata(path) {
            Ok(_) => Some(
                crate::leftover_disk::filesystem_directory_identity(path).with_context(|| {
                    format!("capture configured cleanup anchor {}", path.display())
                })?,
            ),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect configured cleanup anchor {}", path.display())
                });
            }
        };
        if let Some(previous) = identities.insert(path.clone(), identity.clone()) {
            if previous != identity {
                bail!(
                    "configured cleanup anchor changed during inventory: {}",
                    path.display()
                );
            }
        }
    }
    Ok(identities)
}

fn validate_anchor_snapshots(initial: &[InstancePlan], current: &[InstancePlan]) -> Result<()> {
    fn combined(plans: &[InstancePlan]) -> Result<AnchorIdentities> {
        let mut anchors = std::collections::BTreeMap::new();
        for plan in plans {
            for (path, identity) in &plan.anchor_identities {
                if let Some(previous) = anchors.insert(path.clone(), identity.clone()) {
                    if previous != *identity {
                        bail!(
                            "configured cleanup anchor changed during inventory: {}",
                            path.display()
                        );
                    }
                }
            }
        }
        Ok(anchors)
    }

    if combined(initial)? != combined(current)? {
        bail!("configured cleanup anchor identity changed while legacy purge waited for locks");
    }
    Ok(())
}

fn validate_candidate_snapshots(initial: &[InstancePlan], current: &[InstancePlan]) -> Result<()> {
    fn combined(
        plans: &[InstancePlan],
    ) -> Result<
        BTreeMap<
            PathBuf,
            (
                Option<DirectoryIdentity>,
                Option<Vec<crate::leftover_disk::FilesystemEntry>>,
            ),
        >,
    > {
        let mut candidates = std::collections::BTreeMap::new();
        for plan in plans {
            for candidate in &plan.candidates {
                let snapshot = (
                    candidate.candidate_identity.clone(),
                    candidate.legacy_lease_entries.clone(),
                );
                if let Some(previous) = candidates.insert(candidate.path.clone(), snapshot.clone())
                {
                    if previous != snapshot {
                        bail!(
                            "legacy candidate identity changed during inventory: {}",
                            candidate.path.display()
                        );
                    }
                }
            }
        }
        Ok(candidates)
    }

    if combined(initial)? != combined(current)? {
        bail!("legacy candidate identities changed while purge waited for locks");
    }
    Ok(())
}

fn pin_candidate_identities(candidates: &mut [CandidateRoot]) -> Result<()> {
    for candidate in candidates {
        let identity = match fs::symlink_metadata(&candidate.path) {
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect legacy candidate {}", candidate.path.display())
                });
            }
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                bail!(
                    "refusing legacy candidate that is not a real directory: {}",
                    candidate.path.display()
                );
            }
            Ok(_) => Some(
                crate::leftover_disk::filesystem_directory_identity_under(
                    &candidate.anchor,
                    &candidate.path,
                    candidate.anchor_identity.as_ref().with_context(|| {
                        format!(
                            "trusted anchor was absent during discovery: {}",
                            candidate.anchor.display()
                        )
                    })?,
                )
                .with_context(|| {
                    format!(
                        "safely inspect legacy candidate {}",
                        candidate.path.display()
                    )
                })?,
            ),
        };
        if candidate
            .candidate_identity
            .as_ref()
            .is_some_and(|expected| Some(expected) != identity.as_ref())
        {
            bail!(
                "legacy candidate changed during inventory: {}",
                candidate.path.display()
            );
        }
        candidate.candidate_identity = identity;
    }
    Ok(())
}

fn inventory_for_instance(
    instance: &crate::daemon_instance::DaemonInstance,
    slot_work_roots: &[PathBuf],
    anchor_identities: &AnchorIdentities,
) -> Result<Vec<CandidateRoot>> {
    let cache_root = instance.cache_root.clone();
    let work_root = legacy_shared_work_root(instance);
    require_absolute_normalized(&cache_root, "configured cache root")?;
    require_absolute_normalized(&work_root, "configured shared work root")?;
    require_absolute_normalized(&instance.work_dir, "configured slot work root")?;

    let mut roots = Vec::new();
    for scope in discover_scope_children(
        &cache_root,
        &cache_root,
        anchor_identities.get(&cache_root).and_then(Option::as_ref),
    )? {
        for class in CANONICAL_CLASSES {
            roots.push(candidate(
                &instance.instance,
                class,
                &cache_root,
                cache_root.join(&scope).join(class),
            )?);
        }
    }
    for family in WORK_FAMILIES {
        let family_root = work_root.join(family);
        for scope in discover_scope_children(
            &work_root,
            &family_root,
            anchor_identities.get(&work_root).and_then(Option::as_ref),
        )? {
            roots.push(candidate(
                &instance.instance,
                family,
                &work_root,
                family_root.join(scope),
            )?);
        }
    }
    for slot_root in slot_work_roots {
        if slot_root != &instance.work_dir {
            let metadata = fs::symlink_metadata(slot_root)
                .with_context(|| format!("inspect configured slot root {}", slot_root.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "refusing configured slot root that is not a real directory: {}",
                    slot_root.display()
                );
            }
        }
        if slot_root != &work_root {
            let slot_identity = anchor_identities.get(slot_root).and_then(Option::as_ref);
            for family in WORK_FAMILIES {
                let family_root = slot_root.join(family);
                for scope in discover_scope_children(slot_root, &family_root, slot_identity)? {
                    roots.push(candidate(
                        &instance.instance,
                        family,
                        slot_root,
                        family_root.join(scope),
                    )?);
                }
            }
        }
        let stable_root = slot_root.join("stable-workspaces");
        for scope in discover_scope_children(
            &instance.work_dir,
            &stable_root,
            anchor_identities
                .get(&instance.work_dir)
                .and_then(Option::as_ref),
        )? {
            roots.push(candidate(
                &instance.instance,
                "stable-workspaces",
                &instance.work_dir,
                stable_root.join(scope),
            )?);
        }
    }
    let run_root = &instance.run_root;
    let legacy_lease_root = run_root.join(LEGACY_LEASE_ROOT);
    for class in discover_legacy_lease_classes(
        run_root,
        &legacy_lease_root,
        anchor_identities.get(run_root).and_then(Option::as_ref),
    )? {
        let class_root = legacy_lease_root.join(class);
        let expected_anchor = anchor_identities
            .get(run_root)
            .and_then(Option::as_ref)
            .context("configured runtime root identity is missing")?;
        if let Some((class_identity, entries)) =
            legacy_lease_class_entries(run_root, &class_root, expected_anchor)?
        {
            let mut candidate = candidate(&instance.instance, class, run_root, class_root)?;
            candidate.candidate_identity = Some(class_identity);
            candidate.legacy_lease_entries = Some(entries);
            roots.push(candidate);
        }
    }
    Ok(roots)
}

/// Old scope leases were flat sanitized `<scope>.json` files below a known
/// class directory. Keep that class directory intact when it contains any
/// entry outside that exact record grammar.
fn legacy_lease_class_entries(
    trusted_anchor: &Path,
    class_root: &Path,
    expected_anchor: &DirectoryIdentity,
) -> Result<
    Option<(
        DirectoryIdentity,
        Vec<crate::leftover_disk::FilesystemEntry>,
    )>,
> {
    let before = crate::leftover_disk::filesystem_directory_identity_under(
        trusted_anchor,
        class_root,
        expected_anchor,
    )
    .with_context(|| format!("safely inspect legacy lease class {}", class_root.display()))?;
    let mut entries =
        crate::leftover_disk::filesystem_entries_under(trusted_anchor, class_root, expected_anchor)
            .with_context(|| {
                format!(
                    "safely enumerate legacy lease class {}",
                    class_root.display()
                )
            })?;
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    if entries.iter().any(|entry| {
        entry.kind != crate::leftover_disk::FilesystemEntryKind::RegularFile
            || entry.is_mountpoint
            || entry
                .name
                .to_str()
                .is_none_or(|name| !is_legacy_lease_record_name(name))
    }) {
        return Ok(None);
    }
    let after = crate::leftover_disk::filesystem_directory_identity_under(
        trusted_anchor,
        class_root,
        expected_anchor,
    )
    .with_context(|| format!("recheck legacy lease class {}", class_root.display()))?;
    if before != after {
        bail!(
            "legacy lease class changed during inventory: {}",
            class_root.display()
        );
    }
    Ok(Some((before, entries)))
}

fn is_legacy_lease_record_name(value: &str) -> bool {
    value
        .strip_suffix(".json")
        .is_some_and(is_old_scope_component)
}

fn prepare_runtime_roots(instances: &[crate::daemon_instance::DaemonInstance]) -> Result<()> {
    for run_root in unique_run_roots(instances)? {
        fs::create_dir_all(&run_root)
            .with_context(|| format!("create configured runtime root {}", run_root.display()))?;
        let metadata = fs::symlink_metadata(&run_root)
            .with_context(|| format!("inspect configured runtime root {}", run_root.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "configured runtime root is not a real directory: {}",
                run_root.display()
            );
        }
    }
    Ok(())
}

/// List only immediate, sanitizer-shaped scope names under an exact legacy
/// family root. This reads directory-entry metadata, never old files.
fn discover_scope_children(
    anchor: &Path,
    root: &Path,
    expected_anchor: Option<&DirectoryIdentity>,
) -> Result<Vec<String>> {
    require_absolute_normalized(anchor, "configured cleanup anchor")?;
    require_absolute_normalized(root, "legacy family root")?;
    root.strip_prefix(anchor).with_context(|| {
        format!(
            "legacy family root {} is outside configured anchor {}",
            root.display(),
            anchor.display()
        )
    })?;
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", root.display())),
    };
    if metadata.file_type().is_symlink() {
        bail!("refusing symlink legacy family root: {}", root.display());
    }
    if !metadata.is_dir() {
        return Ok(Vec::new());
    }
    let expected_anchor = expected_anchor.with_context(|| {
        format!(
            "legacy family root exists without its configured anchor snapshot: {}",
            root.display()
        )
    })?;
    let entries = crate::leftover_disk::filesystem_entries_under(anchor, root, expected_anchor)
        .with_context(|| format!("safely enumerate {}", root.display()))?;
    let mut scopes = Vec::new();
    for entry in entries {
        let Some(name) = entry.name.to_str().map(str::to_owned) else {
            continue;
        };
        if !is_old_scope_component(&name) {
            continue;
        }
        if entry.kind == crate::leftover_disk::FilesystemEntryKind::Symlink {
            bail!(
                "refusing symlink legacy scope root: {}",
                root.join(&name).display()
            );
        }
        if entry.kind != crate::leftover_disk::FilesystemEntryKind::Directory {
            continue;
        }
        if entry.is_mountpoint
            || entry.identity.device != expected_anchor.device
            || entry.identity.mount != expected_anchor.mount
        {
            bail!(
                "refusing unsafe legacy scope root: {}",
                root.join(&name).display()
            );
        }
        scopes.push(name);
    }
    scopes.sort();
    Ok(scopes)
}

/// Known lease classes are a separate grammar from old scope-name children.
/// Ignore unknown siblings, but fail if a known class is redirected or
/// mounted so postinst cannot silently leave an old root behind.
fn discover_legacy_lease_classes(
    anchor: &Path,
    root: &Path,
    expected_anchor: Option<&DirectoryIdentity>,
) -> Result<Vec<&'static str>> {
    require_absolute_normalized(anchor, "configured cleanup anchor")?;
    require_absolute_normalized(root, "legacy lease root")?;
    root.strip_prefix(anchor).with_context(|| {
        format!(
            "legacy lease root {} is outside configured anchor {}",
            root.display(),
            anchor.display()
        )
    })?;
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", root.display())),
    };
    if metadata.file_type().is_symlink() {
        bail!("refusing symlink legacy lease root: {}", root.display());
    }
    if !metadata.is_dir() {
        return Ok(Vec::new());
    }
    let expected_anchor = expected_anchor.with_context(|| {
        format!(
            "legacy lease root exists without its configured anchor snapshot: {}",
            root.display()
        )
    })?;
    let entries = crate::leftover_disk::filesystem_entries_under(anchor, root, expected_anchor)
        .with_context(|| format!("safely enumerate legacy lease root {}", root.display()))?;
    let mut classes = Vec::new();
    for entry in entries {
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        let Some(class) = LEGACY_LEASE_CLASSES
            .iter()
            .find(|known| **known == name)
            .copied()
        else {
            continue;
        };
        if entry.kind == crate::leftover_disk::FilesystemEntryKind::Symlink {
            bail!(
                "refusing symlink legacy lease class root: {}",
                root.join(name).display()
            );
        }
        if entry.kind != crate::leftover_disk::FilesystemEntryKind::Directory {
            continue;
        }
        if entry.is_mountpoint
            || entry.identity.device != expected_anchor.device
            || entry.identity.mount != expected_anchor.mount
        {
            bail!(
                "refusing unsafe legacy lease class root: {}",
                root.join(name).display()
            );
        }
        classes.push(class);
    }
    classes.sort_unstable();
    Ok(classes)
}

fn is_old_scope_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && crate::container::sanitize_store_key(value) == value
}

fn candidate(
    instance: &str,
    class: &'static str,
    anchor: &Path,
    path: PathBuf,
) -> Result<CandidateRoot> {
    require_absolute_normalized(&path, "legacy candidate")?;
    path.strip_prefix(anchor).with_context(|| {
        format!(
            "legacy candidate {} is outside configured anchor {}",
            path.display(),
            anchor.display()
        )
    })?;
    Ok(CandidateRoot {
        instance: instance.to_owned(),
        class,
        anchor: anchor.to_path_buf(),
        anchor_identity: None,
        candidate_identity: None,
        legacy_lease_entries: None,
        path,
    })
}

/// Enumerate only configured slot directory names. No old trust-scope tree is
/// opened; the deletion helper later walks exact allowlisted candidate roots.
fn configured_slot_work_roots(
    instance: &crate::daemon_instance::DaemonInstance,
    expected_work_anchor: Option<&DirectoryIdentity>,
) -> Result<Vec<PathBuf>> {
    let slot_count = instance.slots.unwrap_or(1);
    if slot_count == 0 || u32::try_from(slot_count).is_err() {
        bail!(
            "instance {} has unsupported slot count {slot_count}",
            instance.instance
        );
    }
    require_absolute_normalized(&instance.work_dir, "configured slot work root")?;
    let mut roots = vec![instance.work_dir.clone()];
    match fs::symlink_metadata(&instance.work_dir) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(roots),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspect configured work root {}",
                    instance.work_dir.display()
                )
            });
        }
        Ok(_) => {}
    }
    let expected_work_anchor = expected_work_anchor
        .context("configured work directory exists without its identity snapshot")?;
    let entries = crate::leftover_disk::filesystem_entries_under(
        &instance.work_dir,
        &instance.work_dir,
        expected_work_anchor,
    )
    .with_context(|| {
        format!(
            "safely enumerate configured slot roots in {}",
            instance.work_dir.display()
        )
    })?;
    let mut slots = Vec::new();
    for entry in entries {
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        let Some(suffix) = name.strip_prefix("slot-") else {
            continue;
        };
        let Ok(index) = suffix.parse::<u32>() else {
            bail!(
                "configured work root has an unsupported slot entry: {}",
                instance.work_dir.join(name).display()
            );
        };
        if index == 0 || name != format!("slot-{index}") {
            bail!(
                "configured work root has an unsupported slot entry: {}",
                instance.work_dir.join(name).display()
            );
        }
        if entry.kind != crate::leftover_disk::FilesystemEntryKind::Directory
            || entry.is_mountpoint
            || entry.identity.device != expected_work_anchor.device
            || entry.identity.mount != expected_work_anchor.mount
        {
            bail!(
                "configured slot root is not a real same-mount directory: {}",
                instance.work_dir.join(name).display()
            );
        }
        slots.push((index, instance.work_dir.join(name)));
    }
    // The base layout was used for one-slot instances. Keep scanning it when
    // an instance is now multi-slot, and scan every actual slot directory so
    // a reduced configured slot count cannot strand old keyed/legacy roots.
    slots.sort_by_key(|(index, _)| *index);
    roots.extend(slots.into_iter().map(|(_, path)| path));
    Ok(roots)
}

fn preserved_roots(
    instance: &crate::daemon_instance::DaemonInstance,
    slot_work_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let cache_root = &instance.cache_root;
    let scope_root = cache_root.join(crate::container::sanitize_store_key(&instance.trust_scope));
    let work_root = legacy_shared_work_root(instance);
    let layout = instance.storage_layout();
    let work_artifacts =
        crate::store_catalog::StoreCatalog::for_work_root_with_layout(&work_root, &layout)
            .artifacts();
    let mut roots = vec![
        crate::trust_scope::filesystem_key_namespace(cache_root),
        cache_root.join("gha-cache"),
        cache_root.join("artifacts"),
        cache_root.join("runtime"),
        cache_root.join("logs"),
        scope_root.join("artifacts"),
        scope_root.join("runtime"),
        scope_root.join("logs"),
        scope_root.join("gha-cache"),
        layout.log_root,
        instance.run_root.join(KEYED_LEASE_ROOT),
        work_root.join("gha-cache"),
        work_artifacts,
    ];
    for slot_root in slot_work_roots {
        roots.push(crate::store_catalog::StoreCatalog::artifacts_in_slot_work_root(slot_root));
        roots.push(slot_root.join(crate::stable_workspace::STABLE_WORKSPACES_DIR));
        roots.push(slot_root.join("artifacts"));
        roots.push(slot_root.join("runtime"));
        roots.push(slot_root.join("logs"));
    }
    roots
}

fn validate_plans(plans: &[InstancePlan]) -> Result<PhysicalPlanPaths> {
    let candidates = plans
        .iter()
        .flat_map(|plan| plan.candidates.iter())
        .collect::<Vec<_>>();
    let preserved = plans
        .iter()
        .flat_map(|plan| plan.preserved.iter())
        .collect::<Vec<_>>();

    // Deletion canonicalizes each trusted anchor before walking it. Compare
    // paths in that same namespace, including prospective paths whose final
    // components do not exist yet. Otherwise a configured anchor alias can
    // make a candidate and protected root look disjoint lexically.
    let physical_paths = physical_plan_paths(&candidates, &preserved)?;
    for (index, left) in candidates.iter().enumerate() {
        for right in candidates.iter().skip(index + 1) {
            if left.path == right.path {
                if left.anchor_identity != right.anchor_identity
                    || left.candidate_identity != right.candidate_identity
                    || left.legacy_lease_entries != right.legacy_lease_entries
                {
                    bail!(
                        "duplicate legacy candidate has conflicting identity: {}",
                        left.path.display()
                    );
                }
                continue;
            }
        }
    }
    Ok(physical_paths)
}

fn physical_plan_paths(
    candidates: &[&CandidateRoot],
    preserved: &[&PathBuf],
) -> Result<PhysicalPlanPaths> {
    let mut physical_candidates = BTreeMap::new();
    let mut candidate_identities = BTreeMap::new();
    for candidate in candidates {
        let physical = prospective_physical_path(&candidate.path, "legacy candidate")?;
        if let Some(previous) = physical_candidates.insert(candidate.path.clone(), physical.clone())
            && previous != physical
        {
            bail!(
                "legacy candidate resolved inconsistently: {}",
                candidate.path.display()
            );
        }
        let (snapshot, _pin) =
            physical_directory_snapshot(&candidate.path, "legacy candidate", false)?;
        if let Some(identity) = &snapshot.root
            && candidate.candidate_identity.as_ref() != Some(identity)
        {
            bail!(
                "legacy candidate identity changed during physical validation: {}",
                candidate.path.display()
            );
        }
        if let Some(previous) =
            candidate_identities.insert(candidate.path.clone(), snapshot.clone())
            && previous != snapshot
        {
            bail!(
                "legacy candidate identity resolved inconsistently: {}",
                candidate.path.display()
            );
        }
    }
    let mut physical_preserved = BTreeMap::new();
    let mut preserved_identities = BTreeMap::new();
    let mut preserved_pins = BTreeMap::new();
    for path in preserved {
        let physical = prospective_physical_path(path, "preserved storage root")?;
        if let Some(previous) = physical_preserved.insert((*path).clone(), physical.clone())
            && previous != physical
        {
            bail!(
                "preserved storage root resolved inconsistently: {}",
                path.display()
            );
        }
        let (snapshot, pin) = physical_directory_snapshot(path, "preserved storage root", true)?;
        if let Some(previous) = preserved_identities.insert((*path).clone(), snapshot.clone())
            && previous != snapshot
        {
            bail!(
                "preserved storage root identity resolved inconsistently: {}",
                path.display()
            );
        }
        if let Some(previous) = preserved_pins.insert((*path).clone(), pin)
            && previous.as_ref().map(|pin| &pin.identity)
                != preserved_pins
                    .get(*path)
                    .and_then(Option::as_ref)
                    .map(|pin| &pin.identity)
        {
            bail!(
                "preserved storage root identity changed during validation: {}",
                path.display()
            );
        }
    }

    for candidate in candidates {
        let physical_candidate = physical_candidates
            .get(&candidate.path)
            .context("physical legacy candidate snapshot is missing")?;
        let candidate_identity = candidate_identities
            .get(&candidate.path)
            .context("physical legacy candidate identity snapshot is missing")?;
        for protected in preserved {
            let physical_protected = physical_preserved
                .get(*protected)
                .context("physical preserved-root snapshot is missing")?;
            let protected_identity = preserved_identities
                .get(*protected)
                .context("physical preserved-root identity snapshot is missing")?;
            if paths_overlap(physical_candidate, physical_protected) {
                let intentionally_kept_missing_legacy_child =
                    candidate.path.starts_with(*protected)
                        && physical_candidate.starts_with(physical_protected)
                        && candidate_path_is_missing(&candidate.path)?;
                if intentionally_kept_missing_legacy_child {
                    // A top-level canonical cache root can also be a valid
                    // old sanitizer-shaped scope name. Its absent
                    // `<scope>/<class>` descendants are never deletion
                    // targets; pinning the candidate as absent makes a later
                    // appearance fail before removal. Existing descendants
                    // remain ambiguous and fail closed below.
                    continue;
                }
                bail!(
                    "legacy candidate {} overlaps preserved data {}",
                    candidate.path.display(),
                    protected.display()
                );
            }
            if physical_locations_overlap(candidate_identity, protected_identity) {
                bail!(
                    "legacy candidate {} aliases or overlaps preserved storage root {}",
                    candidate.path.display(),
                    protected.display()
                );
            }
        }
    }
    for (index, left) in candidates.iter().enumerate() {
        let physical_left = physical_candidates
            .get(&left.path)
            .context("physical legacy candidate snapshot is missing")?;
        for right in candidates.iter().skip(index + 1) {
            if left.path == right.path {
                continue;
            }
            let physical_right = physical_candidates
                .get(&right.path)
                .context("physical legacy candidate snapshot is missing")?;
            if paths_overlap(physical_left, physical_right) {
                bail!(
                    "legacy candidates overlap: {} ({}) and {} ({})",
                    left.path.display(),
                    left.instance,
                    right.path.display(),
                    right.instance
                );
            }
            if physical_locations_overlap(
                candidate_identities
                    .get(&left.path)
                    .context("physical legacy candidate identity snapshot is missing")?,
                candidate_identities
                    .get(&right.path)
                    .context("physical legacy candidate identity snapshot is missing")?,
            ) {
                bail!(
                    "legacy candidates alias or overlap: {} ({}) and {} ({})",
                    left.path.display(),
                    left.instance,
                    right.path.display(),
                    right.instance
                );
            }
        }
    }
    // Re-read all projections after the complete overlap proof. A symlink or
    // missing-prefix transition during planning must not leave a mixed
    // snapshot that appears disjoint only because its roots were resolved at
    // different moments.
    for candidate in candidates {
        let expected = physical_candidates
            .get(&candidate.path)
            .context("physical legacy candidate snapshot is missing")?;
        let current = prospective_physical_path(&candidate.path, "legacy candidate")?;
        if current.as_path() != expected.as_path() {
            bail!(
                "legacy candidate topology changed during physical validation: {}",
                candidate.path.display()
            );
        }
        if physical_directory_snapshot(&candidate.path, "legacy candidate", false)?.0
            != *candidate_identities
                .get(&candidate.path)
                .context("physical legacy candidate identity snapshot is missing")?
        {
            bail!(
                "legacy candidate identity changed during physical validation: {}",
                candidate.path.display()
            );
        }
    }
    for path in preserved {
        let expected = physical_preserved
            .get(*path)
            .context("physical preserved-root snapshot is missing")?;
        let current = prospective_physical_path(path, "preserved storage root")?;
        if current.as_path() != expected.as_path() {
            bail!(
                "preserved-root topology changed during physical validation: {}",
                path.display()
            );
        }
        if physical_directory_snapshot(path, "preserved storage root", true)?.0
            != *preserved_identities
                .get(*path)
                .context("physical preserved-root identity snapshot is missing")?
        {
            bail!(
                "preserved-root identity changed during physical validation: {}",
                path.display()
            );
        }
    }
    Ok(PhysicalPlanPaths {
        candidates: physical_candidates,
        preserved: physical_preserved,
        candidate_identities,
        preserved_identities,
        _preserved_pins: preserved_pins,
    })
}

fn physical_directory_snapshot(
    path: &Path,
    label: &str,
    pin_root: bool,
) -> Result<(PhysicalDirectorySnapshot, Option<PinnedDirectory>)> {
    let mut locations = Vec::new();
    let root_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("{label} is not a real directory: {}", path.display());
            }
            Some(metadata)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {label} {}", path.display()));
        }
    };
    let (root, pin) = if root_metadata.is_some() {
        let (identity, file) = open_directory_identity(path, true, label)?;
        locations.push(PhysicalPathLocation {
            identity: identity.clone(),
            suffix: PathBuf::new(),
        });
        let pin = pin_root.then_some(PinnedDirectory {
            _file: file,
            identity: identity.clone(),
        });
        (Some(identity), pin)
    } else {
        (None, None)
    };
    for ancestor in path.ancestors().skip(1) {
        match fs::symlink_metadata(ancestor) {
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect {label} ancestor {}", ancestor.display()));
            }
            Ok(_) => {}
        }
        let (identity, _file) = open_directory_identity(ancestor, false, label)?;
        let suffix = path.strip_prefix(ancestor).with_context(|| {
            format!(
                "derive physical {label} suffix below {}",
                ancestor.display()
            )
        })?;
        locations.push(PhysicalPathLocation {
            identity,
            suffix: suffix.to_path_buf(),
        });
    }
    Ok((PhysicalDirectorySnapshot { root, locations }, pin))
}

/// Detect overlap through symlink and bind-mount aliases. Mount IDs remain
/// part of the snapshot for change detection, but the same directory inode
/// can have different mount IDs after a bind mount, so alias proof compares
/// device/inode and the remaining path below each shared physical ancestor.
fn physical_locations_overlap(
    left: &PhysicalDirectorySnapshot,
    right: &PhysicalDirectorySnapshot,
) -> bool {
    left.locations.iter().any(|left| {
        right.locations.iter().any(|right| {
            left.identity.device == right.identity.device
                && left.identity.inode == right.identity.inode
                && (left.suffix.starts_with(&right.suffix)
                    || right.suffix.starts_with(&left.suffix))
        })
    })
}

fn open_directory_identity(
    path: &Path,
    no_follow: bool,
    label: &str,
) -> Result<(DirectoryIdentity, File)> {
    let no_follow_flag = if no_follow { libc::O_NOFOLLOW } else { 0 };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | no_follow_flag)
        .open(path)
        .with_context(|| format!("pin {label} directory {}", path.display()))?;
    let identity = crate::leftover_disk::filesystem_object_identity(&file)
        .with_context(|| format!("identify {label} directory {}", path.display()))?;
    Ok((identity, file))
}

fn candidate_path_is_missing(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error).with_context(|| {
            format!(
                "inspect potentially preserved legacy candidate {}",
                path.display()
            )
        }),
    }
}

/// Resolve the existing prefix of a prospective directory and append any
/// missing tail. Symlink ancestors are therefore projected to the same
/// canonical namespace used by deletion anchors, while absent configured
/// leaves remain comparable. An unresolvable or non-directory ancestor fails
/// closed.
fn prospective_physical_path(path: &Path, label: &str) -> Result<PathBuf> {
    require_absolute_normalized(path, label)?;
    let mut existing = path;
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => {
                let canonical = fs::canonicalize(existing).with_context(|| {
                    format!("resolve physical {label} prefix {}", existing.display())
                })?;
                let missing_tail = path.strip_prefix(existing).with_context(|| {
                    format!("resolve missing suffix of {label} {}", path.display())
                })?;
                if missing_tail.components().next().is_some()
                    && !fs::metadata(&canonical)
                        .with_context(|| {
                            format!("inspect physical {label} prefix {}", canonical.display())
                        })?
                        .is_dir()
                {
                    bail!(
                        "physical {label} prefix is not a directory: {}",
                        existing.display()
                    );
                }
                let projected = canonical.join(missing_tail);
                require_absolute_normalized(&projected, label)?;
                return Ok(projected);
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                existing = existing.parent().with_context(|| {
                    format!(
                        "find existing physical prefix for {label} {}",
                        path.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect physical {label} prefix {}", existing.display())
                });
            }
        }
    }
}

fn legacy_shared_work_root(instance: &crate::daemon_instance::DaemonInstance) -> PathBuf {
    if instance.slots.unwrap_or(1) == 1 {
        crate::container::daemon_shared_root(instance.work_dir.clone())
    } else {
        instance.work_dir.clone()
    }
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn require_absolute_normalized(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute() {
        bail!("{label} must be absolute: {}", path.display());
    }
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => {}
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                bail!("{label} must be normalized: {}", path.display());
            }
        }
    }
    Ok(())
}

fn unique_run_roots(instances: &[crate::daemon_instance::DaemonInstance]) -> Result<Vec<PathBuf>> {
    let mut roots = BTreeSet::new();
    for instance in instances {
        require_absolute_normalized(&instance.run_root, "configured runtime root")?;
        roots.insert(instance.run_root.clone());
    }
    Ok(roots.into_iter().collect())
}

fn acquire_instance_locks(run_roots: &[PathBuf], timeout: Duration) -> Result<Vec<File>> {
    let deadline = Instant::now() + timeout;
    let mut locks = Vec::with_capacity(run_roots.len() * 2);
    for run_root in run_roots {
        require_absolute_normalized(run_root, "configured runtime root")?;
        fs::create_dir_all(run_root)
            .with_context(|| format!("create configured runtime root {}", run_root.display()))?;
        let metadata = fs::symlink_metadata(run_root)
            .with_context(|| format!("inspect configured runtime root {}", run_root.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "configured runtime root is not a real directory: {}",
                run_root.display()
            );
        }
        locks.push(acquire_lock_file(&run_root.join(GC_LOCK), deadline)?);
        locks.push(acquire_lock_file(
            &run_root.join(FILESYSTEM_COORDINATOR_LOCK),
            deadline,
        )?);
    }
    Ok(locks)
}

fn acquire_lock_file(path: &Path, deadline: Instant) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("open purge coordination lock {}", path.display()))?;
    if !file
        .metadata()
        .with_context(|| format!("inspect purge coordination lock {}", path.display()))?
        .is_file()
    {
        bail!(
            "purge coordination lock is not a regular file: {}",
            path.display()
        );
    }
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::WOULDBLOCK) if Instant::now() < deadline => {
                thread::sleep(LOCK_RETRY.min(deadline.saturating_duration_since(Instant::now())));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                bail!(
                    "timed out waiting for purge coordination lock {}",
                    path.display()
                );
            }
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("lock purge coordination file {}", path.display())));
            }
        }
    }
}

fn remove_candidates(
    candidates: &[CandidateRoot],
    preserved: &[PathBuf],
    expected_physical_paths: &PhysicalPlanPaths,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    let mut removed = BTreeSet::<PathBuf>::new();
    for candidate in candidates {
        if !seen.insert(candidate.path.clone()) {
            continue;
        }
        let remaining_candidates = candidates
            .iter()
            .filter(|candidate| !removed.contains(&candidate.path))
            .cloned()
            .collect::<Vec<_>>();
        recheck_physical_plan_paths(&remaining_candidates, preserved, expected_physical_paths)?;
        for removed_path in &removed {
            match fs::symlink_metadata(removed_path) {
                Ok(_) => {
                    bail!(
                        "removed legacy candidate reappeared before deletion completed: {}",
                        removed_path.display()
                    );
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "recheck removed legacy candidate {}",
                            removed_path.display()
                        )
                    });
                }
            }
        }
        let metadata = match fs::symlink_metadata(&candidate.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "inspect legacy {} root {}",
                        candidate.class,
                        candidate.path.display()
                    )
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "refusing legacy {} root that is not a real directory: {}",
                candidate.class,
                candidate.path.display()
            );
        }
        let anchor_identity = candidate.anchor_identity.as_ref().with_context(|| {
            format!(
                "trusted anchor was absent during discovery: {}",
                candidate.anchor.display()
            )
        })?;
        let candidate_identity = candidate.candidate_identity.as_ref().with_context(|| {
            format!(
                "candidate was absent during discovery: {}",
                candidate.path.display()
            )
        })?;
        let current_candidate_identity = crate::leftover_disk::filesystem_directory_identity_under(
            &candidate.anchor,
            &candidate.path,
            anchor_identity,
        )
        .with_context(|| {
            format!(
                "recheck legacy candidate identity {}",
                candidate.path.display()
            )
        })?;
        if &current_candidate_identity != candidate_identity {
            bail!(
                "legacy candidate changed before removal: {}",
                candidate.path.display()
            );
        }
        if let Some(expected_entries) = &candidate.legacy_lease_entries {
            let mut current_entries = crate::leftover_disk::filesystem_entries_under(
                &candidate.anchor,
                &candidate.path,
                anchor_identity,
            )
            .with_context(|| {
                format!(
                    "recheck legacy lease records in {}",
                    candidate.path.display()
                )
            })?;
            current_entries.sort_by(|left, right| left.name.cmp(&right.name));
            if &current_entries != expected_entries {
                bail!(
                    "legacy lease class changed before removal: {}",
                    candidate.path.display()
                );
            }
        }
        // Re-resolve every candidate and protected root immediately before
        // unlinking. Candidate and anchor identities are separately pinned by
        // the mount-aware remover; this catches aliases introduced by changed
        // symlink or missing-prefix topology while the package lock is held.
        let remaining_candidates = candidates
            .iter()
            .filter(|candidate| !removed.contains(&candidate.path))
            .cloned()
            .collect::<Vec<_>>();
        recheck_physical_plan_paths(&remaining_candidates, preserved, expected_physical_paths)?;
        crate::leftover_disk::remove_dir_all_on_device_under_identities(
            &candidate.anchor,
            &candidate.path,
            anchor_identity.device,
            anchor_identity,
            candidate_identity,
        )
        .with_context(|| {
            format!(
                "safely remove legacy {} root for instance {} at {}",
                candidate.class,
                candidate.instance,
                candidate.path.display()
            )
        })?;
        removed.insert(candidate.path.clone());
    }
    Ok(())
}

fn recheck_physical_plan_paths(
    candidates: &[CandidateRoot],
    preserved: &[PathBuf],
    expected: &PhysicalPlanPaths,
) -> Result<()> {
    let candidates = candidates.iter().collect::<Vec<_>>();
    let preserved = preserved.iter().collect::<Vec<_>>();
    let current = physical_plan_paths(&candidates, &preserved)?;
    if current.preserved != expected.preserved
        || current.preserved_identities != expected.preserved_identities
        || current
            ._preserved_pins
            .iter()
            .map(|(path, pin)| (path, pin.as_ref().map(|pin| &pin.identity)))
            .ne(expected
                ._preserved_pins
                .iter()
                .map(|(path, pin)| (path, pin.as_ref().map(|pin| &pin.identity))))
    {
        bail!("physical preserved storage roots changed before candidate removal");
    }
    for (path, physical) in &current.candidates {
        if expected.candidates.get(path) != Some(physical)
            || expected.candidate_identities.get(path) != current.candidate_identities.get(path)
        {
            bail!(
                "physical legacy candidate changed before removal: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn verify_complete_instance_inventory(
    instances: &[crate::daemon_instance::DaemonInstance],
) -> Result<()> {
    verify_complete_instance_inventory_with_systemd_path(
        instances,
        Path::new("/run/systemd/system"),
    )
}

fn verify_complete_instance_inventory_with_systemd_path(
    instances: &[crate::daemon_instance::DaemonInstance],
    systemd_runtime: &Path,
) -> Result<()> {
    require_systemd_effective_unit_proof(systemd_runtime)?;
    verify_instance_environment_inventory(Path::new(crate::daemon_instance::ETC_DIR), instances)?;
    verify_manager_environment()?;

    let expected = instances
        .iter()
        .map(|instance| {
            validate_instance_unit_name(instance)?;
            Ok(instance.unit.clone())
        })
        .collect::<Result<BTreeSet<_>>>()?;
    let loaded = systemctl_listing(&[
        "list-units",
        "--all",
        "--type=service",
        "--no-legend",
        "--no-pager",
        "--plain",
        "--full",
    ])?;
    let unit_files = systemctl_listing(&[
        "list-unit-files",
        "--type=service",
        "--no-legend",
        "--no-pager",
        "--full",
    ])?;
    validate_systemd_unit_inventory(&expected, &loaded, &unit_files)
}

/// The package migration is a host-wide operation. Both Debian's maintainer
/// scripts and direct hidden CLI dispatch reach this proof before root
/// inventory, runtime-root creation, locking, or deletion.
fn verify_package_purge_preconditions() -> Result<()> {
    verify_package_transaction_lock()?;
    verify_package_host_drain()?;
    verify_no_live_runner_processes()
}

#[cfg(target_os = "linux")]
fn verify_package_transaction_lock() -> Result<()> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::MetadataExt as _;

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(PACKAGE_TRANSACTION_LOCK)
        .with_context(|| format!("open package transaction lock {PACKAGE_TRANSACTION_LOCK}"))?;
    let metadata = file
        .metadata()
        .context("inspect package transaction lock descriptor")?;
    if !metadata.is_file() {
        bail!("package transaction lock is not a regular file");
    }
    let descriptor_path = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .context("resolve pinned package transaction lock descriptor")?;
    if descriptor_path != Path::new(PACKAGE_TRANSACTION_LOCK) {
        bail!("pinned package transaction lock descriptor has an unexpected path");
    }
    let locks = fs::read_to_string("/proc/locks").context("read kernel flock inventory")?;
    let owner = exclusive_flock_owner(metadata.dev(), metadata.ino(), &locks)?;
    if !process_is_current_or_ancestor(owner, std::process::id())? {
        bail!("exclusive package transaction lock owner is not a caller ancestor");
    }
    Ok(())
}

fn require_systemd_effective_unit_proof(systemd_runtime: &Path) -> Result<()> {
    if !systemd_runtime.is_dir() {
        bail!("refusing legacy purge without systemd effective-unit proof");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn verify_package_transaction_lock() -> Result<()> {
    bail!("legacy package migration requires Linux /proc flock proof")
}

#[cfg(target_os = "linux")]
fn exclusive_flock_owner(device: u64, inode: u64, locks: &str) -> Result<u32> {
    let major = ((device >> 8) & 0x0fff) | ((device >> 32) & 0xffff_f000);
    let minor = (device & 0x00ff) | ((device >> 12) & 0xffff_ff00);
    let mut owner = None;
    for line in locks.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.contains(&"->") {
            // A blocked shared service is expected to queue behind the
            // package's exclusive lock; it does not own the lock.
            continue;
        }
        let Some(index) = fields.iter().position(|field| *field == "FLOCK") else {
            continue;
        };
        if fields.len() <= index + 4 || fields[index + 1] != "ADVISORY" {
            bail!("kernel returned a malformed flock record");
        }
        let mut identity = fields[index + 4].split(':');
        let (Some(found_major), Some(found_minor), Some(found_inode), None) = (
            identity.next(),
            identity.next(),
            identity.next(),
            identity.next(),
        ) else {
            bail!("kernel returned a malformed flock file identity");
        };
        let found_major = u64::from_str_radix(found_major, 16)
            .context("parse flock device major from /proc/locks")?;
        let found_minor = u64::from_str_radix(found_minor, 16)
            .context("parse flock device minor from /proc/locks")?;
        let found_inode = found_inode
            .parse::<u64>()
            .context("parse flock inode from /proc/locks")?;
        if (found_major, found_minor, found_inode) != (major, minor, inode) {
            continue;
        }
        match fields[index + 2] {
            "WRITE" => {
                let pid = fields[index + 3]
                    .parse::<u32>()
                    .context("parse exclusive flock owner PID")?;
                if owner.replace(pid).is_some() {
                    bail!("multiple exclusive package transaction lock owners were reported");
                }
            }
            "READ" => bail!("package transaction lock is held shared, not exclusively"),
            _ => bail!("kernel returned an unsupported package transaction lock mode"),
        }
    }
    owner.context("no exclusive package transaction flock is held")
}

#[cfg(target_os = "linux")]
fn process_is_current_or_ancestor(owner: u32, mut current: u32) -> Result<bool> {
    while current > 1 {
        if current == owner {
            return Ok(true);
        }
        let status = fs::read_to_string(format!("/proc/{current}/status"))
            .with_context(|| format!("read package-lock owner ancestry for PID {current}"))?;
        let parent = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:").map(str::trim))
            .context("process status has no PPid field")?
            .parse::<u32>()
            .context("parse process parent PID")?;
        if parent == current {
            break;
        }
        current = parent;
    }
    Ok(false)
}

fn verify_package_host_drain() -> Result<()> {
    if !Path::new("/run/systemd/system").is_dir() {
        bail!("refusing legacy purge without systemd host-drain proof");
    }

    verify_supported_systemd_activation_version()?;
    verify_supported_system_bus_daemon()?;
    let systemd_manager_owner = systemd_manager_owner()?;
    reload_system_bus_activation_cache()?;

    let services = systemctl_listing(&[
        "list-units",
        "--all",
        "--type=service",
        "--no-legend",
        "--no-pager",
        "--plain",
        "--full",
    ])?;
    let loaded_services = parse_active_unit_rows(&services, "service")?;
    for (unit, state) in &loaded_services {
        if matches!(state.as_str(), "inactive" | "failed") {
            continue;
        }
        if unit.starts_with("velnor") {
            if verify_transaction_oneshot_service(&unit).is_ok() {
                continue;
            }
            bail!("active or unverified Velnor service prevents legacy purge: {unit} ({state})");
        }
        if service_invokes_packaged_runner(unit)? {
            bail!("active service invokes the packaged Velnor runner: {unit} ({state})");
        }
    }

    // `list-units --all` omits installed units that systemd has never loaded.
    // Both loaded inactive services and installed unit files can be started
    // on demand when a D-Bus name is requested, so inspect effective
    // Type=/BusName= and Exec* properties for their union. Only reject
    // candidates that can invoke Velnor; ordinary inactive D-Bus services
    // (common on desktop and server hosts) remain valid.
    let service_unit_files = systemctl_listing(&[
        "list-unit-files",
        "--type=service",
        "--no-legend",
        "--no-pager",
        "--full",
    ])?;
    verify_dbus_activatable_services(&loaded_services, &service_unit_files)?;

    let timers = systemctl_listing(&[
        "list-units",
        "--all",
        "--type=timer",
        "--no-legend",
        "--no-pager",
        "--plain",
        "--full",
    ])?;
    for (timer, state) in parse_active_unit_rows(&timers, "timer")? {
        if matches!(state.as_str(), "inactive" | "failed") {
            continue;
        }
        let triggers = systemctl_show_unit(&timer, "Triggers")?;
        if timer.starts_with("velnor") {
            let expected = transaction_timer_service(&timer)
                .with_context(|| format!("active Velnor timer is not allowlisted: {timer}"))?;
            verify_transaction_timer(&timer, &expected)?;
            verify_transaction_oneshot_service(&expected)?;
        } else {
            for target in triggers
                .split_whitespace()
                .filter(|name| name.ends_with(".service"))
            {
                if service_invokes_packaged_runner(target)? {
                    bail!("active timer {timer} can start a service invoking the Velnor runner: {target}");
                }
                if target.starts_with("velnor") {
                    bail!("active timer {timer} can start a Velnor service: {target}");
                }
            }
        }
    }
    for kind in ["socket", "path"] {
        let activation_units = systemctl_listing(&[
            "list-units",
            "--all",
            &format!("--type={kind}"),
            "--no-legend",
            "--no-pager",
            "--plain",
            "--full",
        ])?;
        for (activation, state) in parse_active_unit_rows(&activation_units, kind)? {
            if matches!(state.as_str(), "inactive" | "failed") {
                continue;
            }
            bail!("active {kind} activation unit prevents legacy purge: {activation} ({state})");
        }
    }
    verify_pending_systemd_activation_jobs(&systemd_manager_owner)?;
    verify_active_service_drain()?;
    Ok(())
}

fn verify_pending_systemd_activation_jobs(expected_manager_owner: &str) -> Result<()> {
    let barrier = Command::new("/usr/bin/busctl")
        .args([
            "--system",
            "call",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "ListJobs",
        ])
        .output()
        .context("inspect queued systemd activation jobs with Manager.ListJobs")?;
    if !barrier.status.success() {
        bail!("system manager rejected the ListJobs job inventory");
    }
    let barrier = String::from_utf8(barrier.stdout)
        .context("system manager returned invalid UTF-8 in ListJobs response")?;
    validate_systemd_list_jobs_response(&barrier)?;
    let manager_owner = systemd_manager_owner()?;
    if manager_owner != expected_manager_owner {
        bail!("systemd bus owner changed during the D-Bus activation drain");
    }

    // `ListJobs` is a snapshot, not an activation fence. The exclusive
    // package transaction lock prevents shipped Velnor ExecStart wrappers
    // from entering while purge runs; this snapshot and the final unit/process
    // scans reject already-running or queued work. D-Bus direct Exec commands
    // that can invoke Velnor are rejected by the activation-file scan.
    let jobs = systemctl_listing(&["list-jobs", "--no-legend", "--no-pager", "--full"])?;
    for (unit, _job_type, state) in parse_systemd_job_rows(&jobs)? {
        if !matches!(state.as_str(), "waiting" | "running") {
            continue;
        }
        if unit.ends_with(".service") {
            if unit.starts_with("velnor") {
                if verify_transaction_oneshot_service(&unit).is_ok() {
                    continue;
                }
                bail!("pending systemd job can activate a Velnor service: {unit}");
            }
            if service_invokes_packaged_runner(&unit)? {
                bail!("pending systemd job can activate a service invoking Velnor: {unit}");
            }
        } else if unit.ends_with(".socket") || unit.ends_with(".path") || unit.ends_with(".timer") {
            bail!("pending systemd activation-unit job prevents legacy purge: {unit}");
        }
    }
    Ok(())
}

fn systemd_manager_owner() -> Result<String> {
    let output = Command::new("/usr/bin/busctl")
        .args([
            "--system",
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetNameOwner",
            "s",
            "org.freedesktop.systemd1",
        ])
        .output()
        .context("query the systemd manager owner on the system bus")?;
    if !output.status.success() {
        bail!("system bus could not resolve the systemd manager owner");
    }
    let response = String::from_utf8(output.stdout)
        .context("system bus returned invalid UTF-8 in the manager owner")?;
    parse_busctl_unique_name(&response)
}

fn parse_busctl_unique_name(response: &str) -> Result<String> {
    let fields = response.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 2 || fields[0] != "s" {
        bail!("system bus returned a malformed systemd manager owner");
    }
    let owner = busctl_string_field(fields[1])
        .context("system bus returned a malformed systemd manager owner")?;
    let Some((bus_id, connection_id)) = owner
        .strip_prefix(':')
        .and_then(|name| name.split_once('.'))
    else {
        bail!("system bus returned a non-unique systemd manager owner");
    };
    if bus_id.is_empty()
        || connection_id.is_empty()
        || !bus_id.bytes().all(|byte| byte.is_ascii_digit())
        || !connection_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("system bus returned an invalid unique systemd manager owner");
    }
    Ok(owner.to_owned())
}

fn validate_systemd_list_jobs_response(response: &str) -> Result<()> {
    let fields = response.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 2 || fields[0] != "a(usssoo)" {
        bail!("system manager returned an unsupported ListJobs response signature");
    }
    let count = fields[1]
        .parse::<usize>()
        .context("system manager returned a malformed ListJobs count")?;
    let expected_fields = count
        .checked_mul(6)
        .and_then(|values| values.checked_add(2))
        .context("system manager returned an overflowing ListJobs count")?;
    if fields.len() != expected_fields {
        bail!("system manager returned a truncated or malformed ListJobs response");
    }
    for job in fields[2..].chunks_exact(6) {
        let unit = busctl_string_field(job[1])
            .context("system manager returned a malformed ListJobs unit name")?;
        let job_type = busctl_string_field(job[2])
            .context("system manager returned a malformed ListJobs job type")?;
        let state = busctl_string_field(job[3])
            .context("system manager returned a malformed ListJobs job state")?;
        let job_path = busctl_string_field(job[4])
            .context("system manager returned a malformed ListJobs job path")?;
        let unit_path = busctl_string_field(job[5])
            .context("system manager returned a malformed ListJobs unit path")?;
        if job[0].parse::<u32>().is_err()
            || unit.is_empty()
            || job_type.is_empty()
            || state.is_empty()
            || !job_path.starts_with("/org/freedesktop/systemd1/job/")
            || !unit_path.starts_with("/org/freedesktop/systemd1/unit/")
        {
            bail!("system manager returned a malformed ListJobs row");
        }
    }
    Ok(())
}

fn busctl_string_field(field: &str) -> Option<&str> {
    let value = match (field.strip_prefix('"'), field.strip_suffix('"')) {
        (Some(inner), Some(_)) => inner.strip_suffix('"')?,
        (None, None) => field,
        _ => return None,
    };
    (!value.is_empty()
        && !value
            .chars()
            .any(|character| matches!(character, '"' | '\\')))
    .then_some(value)
}

fn parse_systemd_job_rows(raw: &str) -> Result<Vec<(String, String, String)>> {
    let mut jobs = Vec::new();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        if line.trim() == "No jobs running." && jobs.is_empty() {
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 4
            || fields[0].is_empty()
            || !fields[0].bytes().all(|byte| byte.is_ascii_digit())
            || fields[1].is_empty()
            || !fields[2]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            || !matches!(
                fields[3],
                "waiting" | "running" | "done" | "canceled" | "failed" | "dependency" | "skipped"
            )
        {
            bail!("system manager returned a malformed list-jobs row");
        }
        jobs.push((
            fields[1].to_owned(),
            fields[2].to_owned(),
            fields[3].to_owned(),
        ));
    }
    Ok(jobs)
}

fn reload_system_bus_activation_cache() -> Result<()> {
    let output = Command::new("/usr/bin/busctl")
        .args([
            "--system",
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ReloadConfig",
        ])
        .output()
        .context("synchronously reload the system D-Bus activation cache")?;
    if !output.status.success() {
        bail!("system bus rejected ReloadConfig; activation cache is unproven");
    }
    Ok(())
}

fn verify_supported_systemd_activation_version() -> Result<()> {
    let output = Command::new("/usr/bin/systemctl")
        .args(["show", "--property=Version", "--value"])
        .output()
        .context("query running systemd manager version for D-Bus activation drain")?;
    if !output.status.success() {
        bail!("systemd refused manager-version query for D-Bus activation drain");
    }
    let version = String::from_utf8(output.stdout)
        .context("systemd returned invalid UTF-8 in manager-version output")?;
    validate_systemd_activation_version(&version)
}

fn validate_systemd_activation_version(version: &str) -> Result<()> {
    let first_line = version
        .lines()
        .next()
        .context("systemd version output is empty")?;
    let mut fields = first_line.split_whitespace();
    let first = fields
        .next()
        .context("systemd version output has no numeric version")?;
    let version = if first == "systemd" {
        fields
            .next()
            .context("systemd version output has no numeric version")?
    } else {
        first
    };
    if !version.split('.').all(|component| {
        !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        bail!("systemd version output has a malformed numeric version");
    }
    let major = version
        .split('.')
        .next()
        .context("systemd version output has no numeric version")?
        .parse::<u32>()
        .context("systemd version output has a malformed numeric version")?;
    // `.busname` was a kdbus-era unit type, removed in systemd v235. The
    // package supports current Debian/Ubuntu bases (systemd v235+); refusing
    // older managers keeps the omitted legacy activation mechanism explicit.
    if major < 235 {
        bail!("systemd v235 or newer is required for the supported D-Bus activation drain");
    }
    Ok(())
}

fn verify_supported_system_bus_daemon() -> Result<()> {
    let active_state = systemctl_show_unit("dbus.service", "ActiveState")?;
    if active_state.trim() != "active" {
        bail!("cannot prove the system bus daemon while dbus.service is not active");
    }
    let need_daemon_reload = systemctl_show_unit("dbus.service", "NeedDaemonReload")?;
    if need_daemon_reload.trim() != "no" {
        bail!("cannot prove the system bus daemon while dbus.service has stale unit state");
    }
    let exec_start = systemctl_show_unit("dbus.service", "ExecStart")?;
    dbus_config_file_from_exec_start(&exec_start)?;
    Ok(())
}

fn verify_active_service_drain() -> Result<()> {
    let services = systemctl_listing(&[
        "list-units",
        "--all",
        "--type=service",
        "--no-legend",
        "--no-pager",
        "--plain",
        "--full",
    ])?;
    for (unit, state) in parse_active_unit_rows(&services, "service")? {
        if matches!(state.as_str(), "inactive" | "failed") {
            continue;
        }
        if unit.starts_with("velnor") {
            if verify_transaction_oneshot_service(&unit).is_ok() {
                continue;
            }
            bail!("active or activating Velnor service prevents legacy purge: {unit} ({state})");
        }
        if service_invokes_packaged_runner(&unit)? {
            bail!(
                "active or activating service invokes the packaged Velnor runner: {unit} ({state})"
            );
        }
    }
    Ok(())
}

fn verify_dbus_activatable_services(
    loaded_services: &[(String, String)],
    service_unit_files: &str,
) -> Result<()> {
    let mut services = loaded_services
        .iter()
        .map(|(unit, _)| unit.clone())
        .collect::<BTreeSet<_>>();
    services.extend(parse_service_unit_file_names(service_unit_files)?);

    for unit in services {
        if service_is_dbus_activatable(&unit)? && service_invokes_packaged_runner(&unit)? {
            bail!("D-Bus-activatable service can invoke the packaged Velnor runner: {unit}");
        }
    }
    let dbus_service_dirs = effective_dbus_system_service_dirs()?;
    verify_dbus_activation_service_files_in(&dbus_service_dirs, |unit| {
        service_invokes_packaged_runner(unit)
    })?;
    Ok(())
}

fn effective_dbus_system_service_dirs() -> Result<Vec<PathBuf>> {
    let active_state = systemctl_show_unit("dbus.service", "ActiveState")?;
    if active_state.trim() != "active" {
        bail!("cannot prove D-Bus activation directories while dbus.service is not active");
    }
    let need_daemon_reload = systemctl_show_unit("dbus.service", "NeedDaemonReload")?;
    if need_daemon_reload.trim() != "no" {
        bail!("cannot prove D-Bus activation directories while dbus.service has stale unit state");
    }
    let exec_start = systemctl_show_unit("dbus.service", "ExecStart")?;
    let config_file = dbus_config_file_from_exec_start(&exec_start)?;
    collect_dbus_system_service_dirs(&config_file)
}

fn dbus_config_file_from_exec_start(raw: &str) -> Result<PathBuf> {
    let argv = parse_exec_start_property(raw)?;
    let executable = argv
        .first()
        .context("D-Bus system unit has an empty ExecStart")?;
    let executable_path = Path::new(executable);
    if !executable_path.is_absolute() || executable.as_str() != "/usr/bin/dbus-daemon" {
        bail!("D-Bus activation lock proof requires exact /usr/bin/dbus-daemon runtime");
    }
    let mut config_file = None;
    let mut system_scope = false;
    let mut index = 1;
    while index < argv.len() {
        let argument = &argv[index];
        if argument == "--config-file" {
            index += 1;
            let path = argv
                .get(index)
                .context("D-Bus system unit has an empty --config-file argument")?;
            if config_file.replace(PathBuf::from(path)).is_some() {
                bail!("D-Bus system unit specifies --config-file more than once");
            }
        } else if let Some(path) = argument.strip_prefix("--config-file=") {
            if path.is_empty() || config_file.replace(PathBuf::from(path)).is_some() {
                bail!("D-Bus system unit has an unsupported --config-file option");
            }
        } else if argument == "--system" {
            system_scope = true;
        } else if argument == "--session" {
            bail!("dbus.service is configured as a session bus");
        } else if matches!(
            argument.as_str(),
            "--nofork"
                | "--nopidfile"
                | "--systemd-activation"
                | "--syslog"
                | "--syslog-only"
                | "--address=systemd:"
        ) {
            // These are the normal system-bus service options used by
            // Debian's dbus-daemon unit. Other options can redirect or alter
            // the bus, so their effective activation config is unproven.
        } else {
            bail!("dbus-daemon ExecStart has an unsupported system-bus option: {argument}");
        }
        index += 1;
    }

    if config_file.is_none() && !system_scope {
        bail!("dbus-daemon ExecStart does not prove the system-bus configuration");
    }
    let path = config_file.unwrap_or_else(|| PathBuf::from("/usr/share/dbus-1/system.conf"));
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
        || !is_supported_dbus_config_path(&path)
    {
        bail!("D-Bus system configuration path is not absolute and normalized");
    }
    Ok(path)
}

#[derive(Default)]
struct DbusConfigDirectives {
    includes: Vec<DbusConfigInclude>,
    includedirs: Vec<PathBuf>,
    servicedirs: Vec<PathBuf>,
    standard_system_servicedirs: bool,
}

struct DbusConfigInclude {
    path: PathBuf,
    ignore_missing: bool,
    if_selinux_enabled: bool,
    selinux_root_relative: bool,
}

fn collect_dbus_system_service_dirs(config_file: &Path) -> Result<Vec<PathBuf>> {
    collect_dbus_system_service_dirs_with_selinux(config_file, dbus_selinux_enabled()?)
}

fn collect_dbus_system_service_dirs_with_selinux(
    config_file: &Path,
    selinux_enabled: bool,
) -> Result<Vec<PathBuf>> {
    let mut directories = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut stack = BTreeSet::new();
    collect_dbus_system_service_dirs_from_config(
        config_file,
        &mut directories,
        &mut visited,
        &mut stack,
        selinux_enabled,
    )?;
    Ok(directories.into_iter().collect())
}

fn collect_dbus_system_service_dirs_from_config(
    config_file: &Path,
    directories: &mut BTreeSet<PathBuf>,
    visited: &mut BTreeSet<PathBuf>,
    stack: &mut BTreeSet<PathBuf>,
    selinux_enabled: bool,
) -> Result<()> {
    let canonical_file = fs::canonicalize(config_file).with_context(|| {
        format!(
            "resolve D-Bus system configuration {}",
            config_file.display()
        )
    })?;
    if stack.contains(&canonical_file) {
        bail!(
            "D-Bus system configuration has an include cycle at {}",
            canonical_file.display()
        );
    }
    if !visited.insert(canonical_file.clone()) {
        return Ok(());
    }
    if visited.len() > 4096 {
        bail!("D-Bus system configuration exceeds the include-file limit");
    }
    stack.insert(canonical_file.clone());
    let contents = fs::read_to_string(&canonical_file).with_context(|| {
        format!(
            "read D-Bus system configuration {}",
            canonical_file.display()
        )
    })?;
    let directives = parse_dbus_config_directives(&contents).with_context(|| {
        format!(
            "parse D-Bus system configuration {}",
            canonical_file.display()
        )
    })?;

    if directives.standard_system_servicedirs {
        for standard_dir in DBUS_STANDARD_SYSTEM_SERVICE_DIRS {
            let path = PathBuf::from(standard_dir);
            match fs::canonicalize(&path) {
                Ok(canonical) => {
                    if !fs::metadata(&canonical)
                        .context("inspect standard D-Bus service directory")?
                        .is_dir()
                    {
                        bail!(
                            "standard D-Bus service path is not a directory: {}",
                            path.display()
                        );
                    }
                    directories.insert(canonical);
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    if fs::symlink_metadata(&path).is_ok() {
                        bail!(
                            "standard D-Bus service directory has an unresolved symlink: {}",
                            path.display()
                        );
                    }
                    directories.insert(path);
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "resolve standard D-Bus service directory {}",
                            path.display()
                        )
                    });
                }
            }
        }
    }

    for service_dir in directives.servicedirs {
        if !service_dir.is_absolute()
            || service_dir
                .components()
                .any(|component| component == Component::ParentDir)
            || !is_supported_dbus_config_path(&service_dir)
        {
            bail!(
                "D-Bus servicedir path is not absolute and normalized: {}",
                service_dir.display()
            );
        }
        let canonical = fs::canonicalize(&service_dir).with_context(|| {
            format!(
                "resolve configured D-Bus servicedir {}",
                service_dir.display()
            )
        })?;
        if !fs::metadata(&canonical)
            .context("inspect configured D-Bus servicedir")?
            .is_dir()
        {
            bail!(
                "configured D-Bus servicedir is not a directory: {}",
                service_dir.display()
            );
        }
        directories.insert(canonical);
    }

    let parent = canonical_file
        .parent()
        .context("D-Bus config file has no parent directory")?;
    for include_dir in directives.includedirs {
        let include_dir = resolve_dbus_config_path(parent, &include_dir)?;
        let canonical_dir = fs::canonicalize(&include_dir).with_context(|| {
            format!(
                "resolve D-Bus configuration include directory {}",
                include_dir.display()
            )
        })?;
        if !fs::metadata(&canonical_dir)
            .context("inspect D-Bus configuration include directory")?
            .is_dir()
        {
            bail!(
                "D-Bus includedir path is not a directory: {}",
                include_dir.display()
            );
        }
        let mut included_files = fs::read_dir(&canonical_dir)
            .with_context(|| format!("read D-Bus includedir {}", canonical_dir.display()))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<std::io::Result<Vec<_>>>()?;
        included_files.sort();
        for included_file in included_files.into_iter().filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "conf")
        }) {
            if !fs::metadata(&included_file)
                .with_context(|| format!("inspect D-Bus include {}", included_file.display()))?
                .is_file()
            {
                bail!(
                    "D-Bus includedir entry is not a file: {}",
                    included_file.display()
                );
            }
            collect_dbus_system_service_dirs_from_config(
                &included_file,
                directories,
                visited,
                stack,
                selinux_enabled,
            )?;
        }
    }

    for include in directives.includes {
        if include.if_selinux_enabled && !selinux_enabled {
            continue;
        }
        if include.selinux_root_relative {
            bail!("cannot prove activation paths in a SELinux-policy-relative D-Bus include");
        }
        let include_path = resolve_dbus_config_path(parent, &include.path)?;
        match fs::metadata(&include_path) {
            Ok(metadata) if metadata.is_file() => {
                collect_dbus_system_service_dirs_from_config(
                    &include_path,
                    directories,
                    visited,
                    stack,
                    selinux_enabled,
                )?;
            }
            Err(error) if error.kind() == ErrorKind::NotFound && include.ignore_missing => {
                if fs::symlink_metadata(&include_path).is_ok() {
                    bail!(
                        "optional D-Bus include has an unresolved symlink: {}",
                        include_path.display()
                    );
                }
            }
            Ok(_) => bail!(
                "D-Bus include is not a regular file: {}",
                include_path.display()
            ),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect D-Bus include {}", include_path.display()));
            }
        }
    }

    stack.remove(&canonical_file);
    Ok(())
}

fn resolve_dbus_config_path(parent: &Path, path: &Path) -> Result<PathBuf> {
    if !is_supported_dbus_config_path(path) {
        bail!("D-Bus configuration contains an unsupported path");
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        parent.join(path)
    })
}

fn dbus_selinux_enabled() -> Result<bool> {
    let selinux_root = Path::new("/sys/fs/selinux");
    let enforce = selinux_root.join("enforce");
    match fs::symlink_metadata(&enforce) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
            match fs::read_to_string(&enforce)
                .context("read SELinux enforcement state for D-Bus config")?
                .trim()
            {
                "0" | "1" => Ok(true),
                _ => bail!("SELinux enforcement state has an unsupported value"),
            }
        }
        Ok(_) => bail!("SELinux enforcement state is not a regular file"),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            match fs::symlink_metadata(selinux_root) {
                Err(root_error) if root_error.kind() == ErrorKind::NotFound => Ok(false),
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    bail!("SELinux state is present but its enforcement flag is unresolved")
                }
                Ok(_) => bail!("SELinux state path is not a real directory"),
                Err(root_error) => {
                    Err(root_error).context("inspect SELinux state path for D-Bus config")
                }
            }
        }
        Err(error) => Err(error).context("inspect SELinux state for D-Bus config"),
    }
}

fn is_supported_dbus_config_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.to_string_lossy().chars().any(|character| {
            character.is_whitespace() || character.is_control() || matches!(character, ':' | '&')
        })
}

fn parse_dbus_config_directives(contents: &str) -> Result<DbusConfigDirectives> {
    validate_dbus_xml_entity_references(contents)?;
    let mut directives = DbusConfigDirectives::default();
    let mut cursor = 0;
    while let Some(relative_start) = contents[cursor..].find('<') {
        let start = cursor + relative_start;
        if contents[start..].starts_with("<!--") {
            let end = contents[start + 4..]
                .find("-->")
                .context("D-Bus config has an unterminated XML comment")?;
            cursor = start + 4 + end + 3;
            continue;
        }
        if contents[start..].starts_with("<![CDATA[") {
            let end = contents[start + 9..]
                .find("]]>")
                .context("D-Bus config has an unterminated CDATA section")?;
            cursor = start + 9 + end + 3;
            continue;
        }
        if contents[start..].starts_with("<?") {
            let end = contents[start + 2..]
                .find("?>")
                .context("D-Bus config has an unterminated XML processing instruction")?;
            cursor = start + 2 + end + 2;
            continue;
        }
        if contents[start..].starts_with("<!") {
            cursor = find_xml_declaration_end(contents, start + 2)? + 1;
            continue;
        }

        let end = find_xml_tag_end(contents, start + 1)?;
        let tag = contents[start + 1..end].trim();
        if tag.starts_with('/') {
            cursor = end + 1;
            continue;
        }
        let (name, attributes, self_closing) = parse_dbus_xml_open_tag(tag)?;
        match name.as_str() {
            "standard_system_servicedirs" => {
                if !attributes.is_empty() || !self_closing {
                    bail!("D-Bus standard_system_servicedirs directive has unsupported syntax");
                }
                directives.standard_system_servicedirs = true;
                cursor = end + 1;
            }
            "servicedir" | "includedir" | "include" => {
                if self_closing {
                    bail!("D-Bus {name} directive has no path");
                }
                let (raw_path, next_cursor) = xml_element_text(contents, end + 1, &name)?;
                let path = PathBuf::from(raw_path);
                match name.as_str() {
                    "servicedir" => {
                        if !attributes.is_empty() || !path.is_absolute() {
                            bail!("D-Bus servicedir has unsupported attributes or a relative path");
                        }
                        directives.servicedirs.push(path);
                    }
                    "includedir" => {
                        if !attributes.is_empty() {
                            bail!("D-Bus includedir has unsupported attributes");
                        }
                        directives.includedirs.push(path);
                    }
                    "include" => {
                        let ignore_missing = attributes.get("ignore_missing").map_or(
                            Ok(false),
                            |value| match value.as_str() {
                                "yes" => Ok(true),
                                "no" => Ok(false),
                                _ => bail!("D-Bus include has an unsupported ignore_missing value"),
                            },
                        )?;
                        let selinux_root_relative = attributes
                            .get("selinux_root_relative")
                            .is_some_and(|value| value == "yes");
                        for attribute in attributes.keys() {
                            if !matches!(
                                attribute.as_str(),
                                "ignore_missing" | "if_selinux_enabled" | "selinux_root_relative"
                            ) {
                                bail!("D-Bus include has an unsupported attribute: {attribute}");
                            }
                        }
                        if attributes
                            .get("if_selinux_enabled")
                            .is_some_and(|value| !matches!(value.as_str(), "yes" | "no"))
                            || attributes
                                .get("selinux_root_relative")
                                .is_some_and(|value| !matches!(value.as_str(), "yes" | "no"))
                        {
                            bail!("D-Bus include has an unsupported SELinux condition");
                        }
                        directives.includes.push(DbusConfigInclude {
                            path,
                            ignore_missing,
                            if_selinux_enabled: attributes
                                .get("if_selinux_enabled")
                                .is_some_and(|value| value == "yes"),
                            selinux_root_relative,
                        });
                    }
                    _ => unreachable!(),
                }
                cursor = next_cursor;
            }
            _ => cursor = end + 1,
        }
    }
    Ok(directives)
}

fn validate_dbus_xml_entity_references(contents: &str) -> Result<()> {
    let mut cursor = 0;
    while cursor < contents.len() {
        let remaining = &contents[cursor..];
        if remaining.starts_with("<!--") {
            let end = remaining[4..]
                .find("-->")
                .context("D-Bus config has an unterminated XML comment")?;
            cursor += 4 + end + 3;
            continue;
        }
        if remaining.starts_with("<![CDATA[") {
            let end = remaining[9..]
                .find("]]>")
                .context("D-Bus config has an unterminated CDATA section")?;
            cursor += 9 + end + 3;
            continue;
        }
        if remaining.starts_with("<?") {
            let end = remaining[2..]
                .find("?>")
                .context("D-Bus config has an unterminated XML processing instruction")?;
            cursor += 2 + end + 2;
            continue;
        }
        if remaining.starts_with("<!") {
            cursor = find_xml_declaration_end(contents, cursor + 2)? + 1;
            continue;
        }
        if remaining.starts_with('<') {
            cursor = find_xml_tag_end(contents, cursor + 1)? + 1;
            continue;
        }
        let text_end = remaining.find('<').unwrap_or(remaining.len());
        decode_dbus_xml_text(&remaining[..text_end])?;
        cursor += text_end;
    }
    Ok(())
}

fn find_xml_tag_end(contents: &str, start: usize) -> Result<usize> {
    let mut quote = None;
    for (offset, byte) in contents.as_bytes().iter().enumerate().skip(start) {
        if let Some(current_quote) = quote {
            if *byte == current_quote {
                quote = None;
            }
        } else if matches!(*byte, b'\'' | b'"') {
            quote = Some(*byte);
        } else if *byte == b'>' {
            return Ok(offset);
        }
    }
    bail!("D-Bus config has an unterminated XML tag")
}

fn find_xml_declaration_end(contents: &str, start: usize) -> Result<usize> {
    let mut quote = None;
    let mut subset_depth = 0_u32;
    for (offset, byte) in contents.as_bytes().iter().enumerate().skip(start) {
        if let Some(current_quote) = quote {
            if *byte == current_quote {
                quote = None;
            }
        } else {
            match *byte {
                b'\'' | b'"' => quote = Some(*byte),
                b'[' => subset_depth += 1,
                b']' => subset_depth = subset_depth.saturating_sub(1),
                b'>' if subset_depth == 0 => return Ok(offset),
                _ => {}
            }
        }
    }
    bail!("D-Bus config has an unterminated XML declaration")
}

fn parse_dbus_xml_open_tag(tag: &str) -> Result<(String, BTreeMap<String, String>, bool)> {
    let tag = tag.trim();
    let self_closing = tag.ends_with('/');
    let tag = if self_closing {
        tag[..tag.len() - 1].trim_end()
    } else {
        tag
    };
    let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
    let name = &tag[..name_end];
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':'))
    {
        bail!("D-Bus config has an unsupported XML element name");
    }
    let mut attributes = BTreeMap::new();
    let mut remaining = tag[name_end..].trim();
    while !remaining.is_empty() {
        let key_end = remaining
            .find(|character: char| character.is_whitespace() || character == '=')
            .unwrap_or(remaining.len());
        let key = &remaining[..key_end];
        remaining = remaining[key_end..].trim_start();
        if key.is_empty() || !remaining.starts_with('=') {
            bail!("D-Bus config has a malformed XML attribute");
        }
        remaining = remaining[1..].trim_start();
        let quote = remaining
            .chars()
            .next()
            .filter(|quote| matches!(quote, '\'' | '"'))
            .context("D-Bus config XML attribute is not quoted")?;
        remaining = &remaining[quote.len_utf8()..];
        let value_end = remaining
            .find(quote)
            .context("D-Bus config XML attribute has no closing quote")?;
        let value = decode_dbus_xml_text(&remaining[..value_end])?;
        if attributes.insert(key.to_owned(), value).is_some() {
            bail!("D-Bus config has a duplicate XML attribute: {key}");
        }
        remaining = remaining[value_end + quote.len_utf8()..].trim_start();
    }
    Ok((name.to_owned(), attributes, self_closing))
}

fn xml_element_text(contents: &str, start: usize, name: &str) -> Result<(String, usize)> {
    let closing = format!("</{name}>");
    let end = contents[start..]
        .find(&closing)
        .map(|offset| start + offset)
        .with_context(|| format!("D-Bus {name} directive has no closing tag"))?;
    let text = contents[start..end].trim();
    if text.contains('<') || text.contains('>') {
        bail!("D-Bus {name} directive has nested XML syntax");
    }
    let text = decode_dbus_xml_text(text)?;
    if text.is_empty() {
        bail!("D-Bus {name} directive has an empty path");
    }
    Ok((text, end + closing.len()))
}

fn decode_dbus_xml_text(raw: &str) -> Result<String> {
    let mut decoded = String::new();
    let mut remaining = raw;
    while let Some(entity_start) = remaining.find('&') {
        decoded.push_str(&remaining[..entity_start]);
        let entity_body = &remaining[entity_start + 1..];
        let entity_end = entity_body
            .find(';')
            .context("D-Bus config path has an unterminated XML entity")?;
        let entity = &entity_body[..entity_end];
        let character = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "apos" => '\'',
            "quot" => '"',
            _ if entity.starts_with("#x") => {
                let value = u32::from_str_radix(&entity[2..], 16)
                    .context("D-Bus config has an invalid hexadecimal XML entity")?;
                char::from_u32(value).context("D-Bus config XML entity is not a Unicode scalar")?
            }
            _ if entity.starts_with('#') => {
                let value = entity[1..]
                    .parse::<u32>()
                    .context("D-Bus config has an invalid decimal XML entity")?;
                char::from_u32(value).context("D-Bus config XML entity is not a Unicode scalar")?
            }
            _ => bail!("D-Bus config path uses an unsupported XML entity: &{entity};"),
        };
        decoded.push(character);
        remaining = &entity_body[entity_end + 1..];
    }
    decoded.push_str(remaining);
    Ok(decoded)
}

fn verify_dbus_activation_service_files_in<F>(
    service_dirs: &[PathBuf],
    mut service_invokes_runner: F,
) -> Result<()>
where
    F: FnMut(&str) -> Result<bool>,
{
    for directory in service_dirs {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "read D-Bus system activation directory {}",
                        directory.display()
                    )
                });
            }
        };
        for entry in entries {
            let entry = entry.context("read D-Bus system activation directory entry")?;
            let path = entry.path();
            if path
                .extension()
                .map_or(true, |extension| extension != "service")
            {
                continue;
            }
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    if fs::symlink_metadata(&path).is_ok() {
                        bail!(
                            "D-Bus activation file has an unresolved symlink: {}",
                            path.display()
                        );
                    }
                    continue;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("inspect D-Bus system activation file {}", path.display())
                    });
                }
            };
            if !metadata.is_file() {
                continue;
            }
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("read D-Bus system activation file {}", path.display()))?;
            let (systemd_services, commands) = parse_dbus_activation_service(&contents)
                .with_context(|| {
                    format!("parse D-Bus system activation file {}", path.display())
                })?;

            for unit in systemd_services {
                if !is_systemd_service_unit_name(&unit) {
                    bail!("D-Bus activation file has an unsupported SystemdService value: {unit}");
                }
                if service_invokes_runner(&unit)? {
                    bail!("D-Bus activation file can start a service invoking the Velnor runner: {unit}");
                }
            }
            if commands
                .iter()
                .any(|command| dbus_exec_invokes_velnor(command))
            {
                bail!(
                    "D-Bus activation file can directly invoke the Velnor runner: {}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn parse_dbus_activation_service(contents: &str) -> Result<(Vec<String>, Vec<String>)> {
    let mut in_service_section = false;
    let mut systemd_services = Vec::new();
    let mut commands = Vec::new();
    let mut saw_systemd_service = false;
    let mut saw_exec = false;

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.ends_with('\\') {
            bail!("D-Bus activation file uses unsupported line continuation syntax");
        }
        if line.starts_with('[') {
            if !line.ends_with(']') {
                bail!("D-Bus activation file has a malformed group header");
            }
            in_service_section = line == "[D-BUS Service]";
            continue;
        }
        if !in_service_section {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .context("D-Bus activation property has no equals sign")?;
        let key = key.trim();
        let value = value.trim();
        match key {
            "SystemdService" => {
                if saw_systemd_service || value.is_empty() {
                    bail!("D-Bus activation file has a duplicate or empty SystemdService");
                }
                saw_systemd_service = true;
                systemd_services.push(value.to_owned());
            }
            "Exec" => {
                if saw_exec || value.is_empty() {
                    bail!("D-Bus activation file has a duplicate or empty Exec");
                }
                saw_exec = true;
                if value
                    .chars()
                    .any(|character| matches!(character, '\\' | '\'' | '"' | ';'))
                {
                    bail!("D-Bus activation file uses unsupported Exec quoting or escapes");
                }
                commands.push(value.to_owned());
            }
            _ => {}
        }
    }

    if !saw_exec && !saw_systemd_service {
        bail!("D-Bus activation file has neither Exec nor SystemdService");
    }
    Ok((systemd_services, commands))
}

fn is_systemd_service_unit_name(unit: &str) -> bool {
    let Some(base) = unit.strip_suffix(".service") else {
        return false;
    };
    !base.is_empty()
        && unit.len() <= 255
        && !base.starts_with(".")
        && !base.starts_with("@")
        && !base.contains("..")
        && base.matches('@').count() <= 1
        && base.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'.' | b'@' | b'-')
        })
}

fn dbus_exec_invokes_velnor(command: &str) -> bool {
    let executable = command.split_whitespace().next().unwrap_or_default();
    let systemd_command = format!("{{ path={executable} ; argv[]={command} ; }}");
    command_properties_invoke_velnor(&systemd_command)
}

fn parse_service_unit_file_names(raw: &str) -> Result<BTreeSet<String>> {
    let mut services = BTreeSet::new();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 2 || !fields[0].ends_with(".service") || fields[1].is_empty() {
            bail!("systemd returned a malformed service unit-file inventory row");
        }
        services.insert(fields[0].to_owned());
    }
    Ok(services)
}

fn service_is_dbus_activatable(unit: &str) -> Result<bool> {
    let unit_type = systemctl_show_unit(unit, "Type")?;
    let bus_name = systemctl_show_unit(unit, "BusName")?;
    Ok(has_dbus_activation_properties(&unit_type, &bus_name))
}

fn has_dbus_activation_properties(unit_type: &str, bus_name: &str) -> bool {
    unit_type.trim() == "dbus" || !bus_name.trim().is_empty()
}

fn parse_active_unit_rows(raw: &str, kind: &str) -> Result<Vec<(String, String)>> {
    let suffix = format!(".{kind}");
    let mut active = Vec::new();
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 4
            || !fields[0].ends_with(&suffix)
            || fields[1].is_empty()
            || fields[3].is_empty()
            || !matches!(
                fields[2],
                "active"
                    | "reloading"
                    | "inactive"
                    | "failed"
                    | "activating"
                    | "deactivating"
                    | "maintenance"
            )
        {
            bail!("systemd returned a malformed {kind} unit inventory row");
        }
        active.push((fields[0].to_owned(), fields[2].to_owned()));
    }
    Ok(active)
}

fn transaction_timer_service(timer: &str) -> Option<String> {
    match timer {
        "velnor-cache-gc.timer" => return Some("velnor-cache-gc.service".to_owned()),
        "velnor-doctor.timer" => return Some("velnor-doctor.service".to_owned()),
        "velnor-fleet-policy-audit.timer" => {
            return Some("velnor-fleet-policy-audit.service".to_owned());
        }
        _ => {}
    }
    let instance = timer
        .strip_prefix("velnor-doctor@")?
        .strip_suffix(".timer")?;
    if !crate::daemon_instance::valid_instance_name(instance) {
        return None;
    }
    Some(format!("velnor-doctor@{instance}.service"))
}

fn verify_transaction_timer(timer: &str, expected_service: &str) -> Result<()> {
    if transaction_timer_service(timer).as_deref() != Some(expected_service) {
        bail!("active Velnor timer has an unsupported target: {timer}");
    }
    if systemctl_show_unit(timer, "NeedDaemonReload")?.trim() != "no"
        || !systemctl_show_unit(timer, "DropInPaths")?.trim().is_empty()
    {
        bail!("active Velnor timer has stale or custom unit configuration: {timer}");
    }
    let fragment = systemctl_show_unit(timer, "FragmentPath")?;
    let fragment_name = if timer.starts_with("velnor-doctor@") {
        "velnor-doctor@.timer"
    } else {
        timer
    };
    let expected_contents = match fragment_name {
        "velnor-cache-gc.timer" => include_str!("../../velnor-tools/debian/velnor-cache-gc.timer"),
        "velnor-doctor.timer" => include_str!("../debian/velnor-doctor.timer"),
        "velnor-doctor@.timer" => include_str!("../debian/velnor-doctor@.timer"),
        "velnor-fleet-policy-audit.timer" => {
            include_str!("../../velnor-tools/debian/velnor-fleet-policy-audit.timer")
        }
        _ => bail!("active Velnor timer is not allowlisted: {timer}"),
    };
    let fragment_path = Path::new(fragment.trim());
    if !["/usr/lib/systemd/system", "/lib/systemd/system"]
        .iter()
        .any(|root| fragment_path == Path::new(root).join(fragment_name))
        || fs::read_to_string(fragment_path).ok().as_deref() != Some(expected_contents)
    {
        bail!("active Velnor timer does not use its exact packaged fragment: {timer}");
    }
    if systemctl_show_unit(timer, "Triggers")?.trim() != expected_service {
        bail!("active Velnor timer has an unsupported trigger: {timer}");
    }
    Ok(())
}

fn expected_transaction_oneshot_argv(unit: &str) -> Option<Vec<String>> {
    let argv = match unit {
        "velnor-cache-gc.service" => vec![
            "/usr/bin/flock",
            "--shared",
            "--no-fork",
            PACKAGE_TRANSACTION_LOCK,
            "/usr/bin/velnorctl",
            "cache",
            "gc",
            "--yes",
        ],
        "velnor-doctor.service" => vec![
            "/usr/bin/flock",
            "--shared",
            "--no-fork",
            PACKAGE_TRANSACTION_LOCK,
            "/usr/bin/velnorctl",
            "doctor",
            "--url",
            "${VELNOR_URL}",
            "--name",
            "${VELNOR_NAME}",
            "--slots",
            "${VELNOR_SLOTS}",
        ],
        unit if unit.starts_with("velnor-doctor@") && unit.ends_with(".service") => {
            let instance = unit
                .strip_prefix("velnor-doctor@")?
                .strip_suffix(".service")?;
            if !crate::daemon_instance::valid_instance_name(instance) {
                return None;
            }
            vec![
                "/usr/bin/flock",
                "--shared",
                "--no-fork",
                PACKAGE_TRANSACTION_LOCK,
                "/usr/bin/velnorctl",
                "doctor",
                "--url",
                "${VELNOR_URL}",
                "--name",
                "${VELNOR_NAME}",
                "--slots",
                "${VELNOR_SLOTS}",
            ]
        }
        _ => return None,
    };
    Some(argv.into_iter().map(str::to_owned).collect())
}

fn verify_transaction_oneshot_service(unit: &str) -> Result<()> {
    let fleet_policy_audit = unit == "velnor-fleet-policy-audit.service";
    let expected_argv =
        if fleet_policy_audit {
            None
        } else {
            Some(expected_transaction_oneshot_argv(unit).with_context(|| {
                format!("active transaction service is not allowlisted: {unit}")
            })?)
        };
    if systemctl_show_unit(unit, "Type")?.trim() != "oneshot" {
        bail!("allowlisted transaction service has an unexpected Type: {unit}");
    }
    if systemctl_show_unit(unit, "NeedDaemonReload")?.trim() != "no"
        || !systemctl_show_unit(unit, "DropInPaths")?.trim().is_empty()
    {
        bail!("allowlisted transaction service has stale or custom unit configuration: {unit}");
    }
    for property in [
        "ExecCondition",
        "ExecStartPre",
        "ExecStartPost",
        "ExecReload",
        "ExecStop",
        "ExecStopPost",
    ] {
        if !systemctl_show_unit(unit, property)?.trim().is_empty() {
            bail!("allowlisted transaction service has an extra command ({property}): {unit}");
        }
    }
    let fragment = systemctl_show_unit(unit, "FragmentPath")?;
    let fragment_name = if unit.starts_with("velnor-doctor@") {
        "velnor-doctor@.service"
    } else {
        unit
    };
    let expected_contents = match fragment_name {
        "velnor-cache-gc.service" => {
            include_str!("../../velnor-tools/debian/velnor-cache-gc.service")
        }
        "velnor-doctor.service" => include_str!("../debian/velnor-doctor.service"),
        "velnor-doctor@.service" => include_str!("../debian/velnor-doctor@.service"),
        "velnor-fleet-policy-audit.service" => {
            include_str!("../../velnor-tools/debian/velnor-fleet-policy-audit.service")
        }
        _ => bail!("transaction service is not allowlisted: {unit}"),
    };
    let fragment_path = Path::new(fragment.trim());
    if !["/usr/lib/systemd/system", "/lib/systemd/system"]
        .iter()
        .any(|root| fragment_path == Path::new(root).join(fragment_name))
        || fs::read_to_string(fragment_path).ok().as_deref() != Some(expected_contents)
    {
        bail!("allowlisted transaction service does not use its exact packaged fragment: {unit}");
    }
    let exec_start = systemctl_show_unit(unit, "ExecStart")?;
    if fleet_policy_audit {
        verify_locked_shell_exec_start(&exec_start)?;
    } else if parse_exec_start_property(&exec_start)?
        != expected_argv.context("transaction argv missing")?
    {
        bail!("allowlisted transaction service has a different ExecStart: {unit}");
    }
    Ok(())
}

fn service_invokes_packaged_runner(unit: &str) -> Result<bool> {
    let output = Command::new("/usr/bin/systemctl")
        .args([
            "show",
            "--no-pager",
            "--property=ExecCondition",
            "--property=ExecStartPre",
            "--property=ExecStart",
            "--property=ExecStartPost",
            "--property=ExecReload",
            "--property=ExecStop",
            "--property=ExecStopPost",
            "--value",
            unit,
        ])
        .output()
        .with_context(|| format!("query command properties for active service {unit}"))?;
    if !output.status.success() {
        bail!("systemd refused command-property query for active service {unit}");
    }
    let commands = String::from_utf8(output.stdout)
        .with_context(|| format!("systemd returned invalid command data for {unit}"))?;
    Ok(command_properties_invoke_velnor(&commands))
}

fn command_properties_invoke_velnor(commands: &str) -> bool {
    if commands.contains("velnor-runner") || commands.contains("velnorctl") {
        return true;
    }
    for group in commands.split("argv[]=").skip(1) {
        let argv = group.split(" ; ").next().unwrap_or(group);
        let args = argv.split_whitespace().collect::<Vec<_>>();
        if exec_argv_invokes_velnor(&args) {
            return true;
        }
    }
    false
}

fn exec_argv_invokes_velnor(args: &[&str]) -> bool {
    let mut args = args;
    loop {
        let Some(executable) = args.first() else {
            return true;
        };
        let executable = executable
            .trim_matches(|character: char| matches!(character, '{' | '(' | '}' | ')' | ';'));
        if executable.rsplit('/').next() != Some("env") {
            break;
        }
        let Some(command_index) = env_wrapped_command_index(args) else {
            return true;
        };
        let Some(command_args) = args.get(command_index..) else {
            return true;
        };
        if command_args.is_empty() {
            return true;
        }
        args = command_args;
    }

    if args.iter().skip(1).any(|argument| {
        matches!(
            argument.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '_'
            }),
            "daemon" | "controller" | "slot" | "job" | "guardian"
        )
    }) {
        return true;
    }

    let Some(executable) = args.first() else {
        return true;
    };
    let executable =
        executable.trim_matches(|character: char| matches!(character, '{' | '(' | '}' | ')' | ';'));
    let basename = executable.rsplit('/').next().unwrap_or(executable);
    if is_command_interpreter(basename) {
        if let Some(inline_program_is_safe) = interpreter_inline_program_is_safe(basename, args) {
            if !inline_program_is_safe {
                return true;
            }
            // `python -c pass` is the only inline program we can prove inert
            // from systemd's flattened argv property. Other code may
            // construct the runner command without naming it in argv.
            return false;
        }
        let Some(script) = args
            .iter()
            .skip(1)
            .copied()
            .find(|argument| !argument.starts_with('-'))
        else {
            return false;
        };
        wrapper_script_may_invoke_velnor(script)
    } else if has_script_suffix(executable) || executable_script_has_shebang(executable) {
        wrapper_script_may_invoke_velnor(executable)
    } else {
        false
    }
}

fn env_wrapped_command_index(args: &[&str]) -> Option<usize> {
    let mut index = 1;
    while index < args.len() {
        let argument = args[index];
        if argument == "--" {
            return (index + 1 < args.len()).then_some(index + 1);
        }
        if argument == "-S"
            || argument == "--split-string"
            || argument.starts_with("-S")
            || argument.starts_with("--split-string=")
        {
            return None;
        }
        if matches!(argument, "-u" | "--unset" | "-C" | "--chdir") {
            index += 2;
            continue;
        }
        if argument.starts_with("--unset=") || argument.starts_with("--chdir=") {
            index += 1;
            continue;
        }
        if argument.starts_with('-') {
            if matches!(
                argument,
                "-i" | "--ignore-environment" | "-0" | "--null" | "-v" | "--debug"
            ) {
                index += 1;
                continue;
            }
            return None;
        }
        if argument.contains('=') {
            index += 1;
            continue;
        }
        return Some(index);
    }
    None
}

fn interpreter_inline_program_is_safe(basename: &str, args: &[&str]) -> Option<bool> {
    let inline_option = args
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, argument)| match basename {
            "sh" | "bash" | "dash" | "zsh" | "ksh" | "fish" => argument
                .strip_prefix('-')
                .is_some_and(|options| !options.starts_with('-') && options.contains('c')),
            name if is_python_interpreter(name) => argument.starts_with("-c"),
            "perl" | "ruby" => argument.starts_with("-e"),
            "node" => {
                argument.starts_with("-e")
                    || **argument == "--eval"
                    || argument.starts_with("--eval=")
            }
            "php" => argument.starts_with("-r"),
            _ => false,
        });
    let Some((option_index, option)) = inline_option else {
        return None;
    };

    let is_exact_python_pass = is_python_interpreter(basename)
        && *option == "-c"
        && args.len() == option_index + 2
        && args.get(option_index + 1) == Some(&"pass");
    Some(is_exact_python_pass)
}

fn is_command_interpreter(basename: &str) -> bool {
    matches!(
        basename,
        "sh" | "bash" | "dash" | "zsh" | "ksh" | "fish" | "env" | "perl" | "ruby" | "node" | "php"
    ) || is_python_interpreter(basename)
}

fn has_script_suffix(path: &str) -> bool {
    [".sh", ".bash", ".py", ".pl", ".rb", ".js"]
        .iter()
        .any(|suffix| path.ends_with(suffix))
}

fn executable_script_has_shebang(path: &str) -> bool {
    File::open(path).ok().is_some_and(|mut file| {
        let mut prefix = [0_u8; 2];
        file.read_exact(&mut prefix).is_ok() && prefix == *b"#!"
    })
}

fn wrapper_script_may_invoke_velnor(path: &str) -> bool {
    let path = Path::new(path);
    if !path.is_absolute() {
        return true;
    }
    let Ok(metadata) = fs::metadata(path) else {
        return true;
    };
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return true;
    }
    let Ok(contents) = fs::read_to_string(path) else {
        return true;
    };
    contents.contains("velnor-runner")
        || contents.contains("velnorctl")
        || contents
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|token| matches!(token, "daemon" | "controller" | "slot" | "job" | "guardian"))
}

fn is_python_interpreter(basename: &str) -> bool {
    let Some(version) = basename.strip_prefix("python") else {
        return false;
    };
    if version.is_empty() {
        return true;
    }
    let version = version
        .strip_suffix(|character: char| character.is_ascii_lowercase())
        .unwrap_or(version);
    !version.is_empty()
        && version.split('.').all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
}

#[cfg(target_os = "linux")]
fn process_argv_invokes_velnor(cmdline: &[u8]) -> bool {
    let args = cmdline
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(String::from_utf8_lossy)
        .collect::<Vec<_>>();
    args.iter().skip(1).any(|argument| {
        argument
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|token| matches!(token, "daemon" | "controller" | "slot" | "job" | "guardian"))
    })
}

fn verify_locked_shell_exec_start(raw: &str) -> Result<()> {
    let value = raw.trim();
    let expected_prefix = format!(
        "{{ path=/usr/bin/flock ; argv[]=/usr/bin/flock --shared --no-fork {PACKAGE_TRANSACTION_LOCK} /bin/sh -c "
    );
    if value.matches("{ path=").count() != 1
        || !value.starts_with(&expected_prefix)
        || !value.contains(" ; ignore_errors=no ; start_time=")
        || !value.ends_with('}')
    {
        bail!("fleet policy audit ExecStart is not one locked shell invocation");
    }
    Ok(())
}

fn verify_no_live_runner_processes() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt as _;

        let self_pid = std::process::id();
        let packaged_runner = match fs::metadata("/usr/bin/velnor-runner") {
            Ok(metadata) => {
                let digest = sha256_file(File::open("/usr/bin/velnor-runner")?)?;
                Some(((metadata.dev(), metadata.ino()), metadata.len(), digest))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("inspect packaged runner identity"),
        };
        let processes =
            fs::read_dir("/proc").context("enumerate host processes for purge drain")?;
        for entry in processes {
            let entry = entry.context("read host process entry")?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            if pid == self_pid {
                continue;
            }
            let cmdline_path = entry.path().join("cmdline");
            let cmdline = match fs::read(&cmdline_path) {
                Ok(cmdline) => cmdline,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) if error.kind() == ErrorKind::PermissionDenied => {
                    if entry.path().exists() {
                        bail!("cannot inspect command line for host process {pid}");
                    }
                    continue;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("inspect command line for host process {pid}"));
                }
            };
            if process_argv_invokes_velnor(&cmdline) {
                bail!("Velnor runner service process {pid} is still live");
            }
            let executable = match fs::read_link(entry.path().join("exe")) {
                Ok(executable) => executable,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) if error.kind() == ErrorKind::PermissionDenied => {
                    if entry.path().exists() {
                        bail!("cannot inspect executable for host process {pid}");
                    }
                    continue;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("inspect executable for host process {pid}"));
                }
            };
            let executable_name = executable
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .strip_suffix(" (deleted)")
                .unwrap_or_else(|| {
                    executable
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                });
            if executable_name == "velnor-runner" {
                bail!("Velnor runner process {pid} is still live");
            }
            if let Some((expected_identity, expected_size, expected_digest)) = packaged_runner {
                match File::open(entry.path().join("exe")) {
                    Ok(mut file) => {
                        let metadata = file.metadata().with_context(|| {
                            format!("inspect executable identity for host process {pid}")
                        })?;
                        if (metadata.dev(), metadata.ino()) == expected_identity {
                            bail!("packaged Velnor runner process {pid} is still live");
                        }
                        if metadata.len() == expected_size && sha256_file(file)? == expected_digest
                        {
                            bail!("copied packaged Velnor runner process {pid} is still live");
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) if error.kind() == ErrorKind::PermissionDenied => {
                        if entry.path().exists() {
                            bail!("cannot inspect executable identity for host process {pid}");
                        }
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("inspect executable identity for host process {pid}")
                        });
                    }
                }
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        bail!("legacy package migration requires Linux process inventory proof")
    }
}

#[cfg(target_os = "linux")]
fn sha256_file(mut file: File) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .context("read runner executable bytes")?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let digest = hasher.finalize();
    let mut result = [0_u8; 32];
    result.copy_from_slice(&digest);
    Ok(result)
}

fn validate_systemd_unit_inventory(
    expected: &BTreeSet<String>,
    loaded: &str,
    unit_files: &str,
) -> Result<()> {
    for line in loaded.lines() {
        let Some(unit) = line.split_whitespace().next() else {
            continue;
        };
        if is_non_daemon_env_collision_unit(unit) {
            bail!("daemon unit collides with a reserved non-daemon environment file: {unit}");
        }
        if is_daemon_unit_name(unit) && unit != "velnor-daemon@.service" && !expected.contains(unit)
        {
            bail!("systemd has an unenumerated daemon unit: {unit}");
        }
    }
    for line in unit_files.lines() {
        let mut fields = line.split_whitespace();
        let Some(unit) = fields.next() else {
            continue;
        };
        let Some(state) = fields.next() else {
            bail!("systemd returned a malformed unit-file inventory");
        };
        if is_non_daemon_env_collision_unit(unit) {
            bail!("daemon unit collides with a reserved non-daemon environment file: {unit}");
        }
        if is_daemon_unit_name(unit)
            && unit != "velnor-daemon@.service"
            && !matches!(state, "disabled" | "masked" | "masked-runtime")
            && !expected.contains(unit)
        {
            bail!("systemd has an enabled unenumerated daemon unit: {unit}");
        }
    }
    Ok(())
}

fn is_non_daemon_env_collision_unit(unit: &str) -> bool {
    let Some(instance) = unit
        .strip_prefix("velnor-daemon@")
        .and_then(|unit| unit.strip_suffix(".service"))
    else {
        return false;
    };
    crate::daemon_instance::is_non_daemon_service_env_file_name(&format!("{instance}.env"))
}

fn validate_instance_unit_name(instance: &crate::daemon_instance::DaemonInstance) -> Result<()> {
    let expected_unit = if instance.instance == crate::daemon_instance::BARE_INSTANCE {
        "velnor-daemon.service".to_owned()
    } else {
        if !crate::daemon_instance::valid_instance_name(&instance.instance) {
            bail!(
                "daemon instance name cannot be replayed safely by the packaged unit resolver: {}",
                instance.env_file.display()
            );
        }
        format!("velnor-daemon@{}.service", instance.instance)
    };
    if instance.unit != expected_unit {
        bail!("enumerated daemon unit does not match its instance configuration");
    }
    Ok(())
}

fn is_daemon_unit_name(name: &str) -> bool {
    name == "velnor-daemon.service"
        || name == "velnor-daemon@.service"
        || name.starts_with("velnor-daemon")
}

fn systemctl_listing(args: &[&str]) -> Result<String> {
    let output = Command::new("/usr/bin/systemctl")
        .args(args)
        .output()
        .context("enumerate effective Velnor daemon units")?;
    if !output.status.success() {
        bail!("systemd refused daemon-unit inventory query");
    }
    String::from_utf8(output.stdout).context("systemd returned invalid UTF-8 in unit inventory")
}

fn verify_instance_environment_inventory(
    etc: &Path,
    instances: &[crate::daemon_instance::DaemonInstance],
) -> Result<()> {
    let directory = match fs::symlink_metadata(etc) {
        Ok(directory) => directory,
        Err(error) if error.kind() == ErrorKind::NotFound && instances.is_empty() => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("inspect daemon environment directory {}", etc.display())
            });
        }
    };
    if directory.file_type().is_symlink() || !directory.is_dir() {
        bail!(
            "daemon environment path is not a real directory: {}",
            etc.display()
        );
    }
    let entries = match fs::read_dir(etc) {
        Ok(entries) => entries,
        Err(error) => return Err(error).with_context(|| format!("read {}", etc.display())),
    };
    let mut found = BTreeSet::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("read an entry in {}", etc.display()))?;
        let file_name = entry.file_name();
        let file_name = file_name.to_str().with_context(|| {
            format!("unreplayable environment filename under {}", etc.display())
        })?;
        if file_name == "secrets.env" || file_name.ends_with(".secrets.env") {
            verify_unenumerated_secret_file(&entry.path())?;
            continue;
        }
        if crate::daemon_instance::is_non_daemon_service_env_file_name(file_name) {
            crate::daemon_instance::verify_non_daemon_service_env_file(&entry.path())?;
            continue;
        }
        let Some(stem) = file_name.strip_suffix(".env") else {
            continue;
        };
        if stem != crate::daemon_instance::BARE_INSTANCE
            && !crate::daemon_instance::valid_instance_name(stem)
        {
            bail!(
                "daemon environment filename cannot be replayed safely: {}",
                entry.path().display()
            );
        }
        let metadata = fs::symlink_metadata(entry.path()).with_context(|| {
            format!("inspect daemon environment file {}", entry.path().display())
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "daemon environment entry is not a regular file: {}",
                entry.path().display()
            );
        }
        found.insert(stem.to_owned());
    }
    let enumerated = instances
        .iter()
        .map(|instance| instance.instance.clone())
        .collect::<BTreeSet<_>>();
    if found != enumerated {
        bail!("daemon environment files do not match the replayable instance inventory");
    }
    Ok(())
}

fn verify_unenumerated_secret_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect secrets environment file {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "secrets environment entry is not a regular file: {}",
            path.display()
        );
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("read secrets environment file {}", path.display()))?;
    for (key, _) in crate::daemon_instance::parse_environment_file(&text)
        .with_context(|| format!("parse secrets environment file {}", path.display()))?
    {
        if key.starts_with("VELNOR_") {
            bail!(
                "secrets environment file contains a daemon override: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn verify_manager_environment() -> Result<()> {
    let output = Command::new("/usr/bin/systemctl")
        .arg("show-environment")
        .output()
        .context("query systemd manager environment")?;
    if !output.status.success() {
        bail!("systemd refused manager-environment query");
    }
    let environment = String::from_utf8(output.stdout)
        .context("systemd returned invalid UTF-8 in manager environment")?;
    reject_manager_environment_overrides(&environment)
}

fn reject_manager_environment_overrides(environment: &str) -> Result<()> {
    for line in environment.lines() {
        let Some((key, _)) = line.split_once('=') else {
            continue;
        };
        if ROOT_ENVIRONMENT_KEYS.contains(&key) {
            bail!("systemd manager environment overrides a daemon root or trust input");
        }
    }
    Ok(())
}

fn verify_effective_unit(instance: &crate::daemon_instance::DaemonInstance) -> Result<()> {
    verify_static_unit_files(instance)?;
    verify_regular_instance_environment_file(instance)?;
    verify_root_environment_file(instance)?;
    verify_secret_environment_file(instance)?;
    validate_instance_roots(instance)?;
    require_systemd_effective_unit_proof(Path::new("/run/systemd/system"))?;

    let fragment = systemctl_show(instance, "FragmentPath")?;
    let expected_fragment = packaged_fragment_path(instance)?;
    let actual_fragment = fs::canonicalize(fragment.trim())
        .with_context(|| format!("resolve effective fragment for {}", instance.unit))?;
    let expected_fragment =
        fs::canonicalize(&expected_fragment).context("resolve packaged daemon unit fragment")?;
    if actual_fragment != expected_fragment {
        bail!(
            "unit {} uses unsupported fragment {} (expected {})",
            instance.unit,
            fragment.trim(),
            expected_fragment.display()
        );
    }
    let need_daemon_reload = systemctl_show(instance, "NeedDaemonReload")?;
    if need_daemon_reload.trim() != "no" {
        bail!(
            "unit {} has stale or unknown manager state; daemon-reload proof is required",
            instance.unit
        );
    }
    verify_effective_dropins(instance)?;

    let state = systemctl_show(instance, "ActiveState")?;
    if state.trim() != "inactive" {
        bail!(
            "refusing legacy purge while unit {} is {}",
            instance.unit,
            state.trim()
        );
    }
    let root_directory = systemctl_show(instance, "RootDirectory")?;
    if !matches!(root_directory.trim(), "" | "/") {
        bail!(
            "unit {} has unsupported RootDirectory={}",
            instance.unit,
            root_directory.trim()
        );
    }
    let root_image = systemctl_show(instance, "RootImage")?;
    if !root_image.trim().is_empty() {
        bail!(
            "unit {} has unsupported RootImage={}",
            instance.unit,
            root_image.trim()
        );
    }
    validate_effective_working_directory(instance, &systemctl_show(instance, "WorkingDirectory")?)?;
    validate_effective_exec_start(instance, &systemctl_show(instance, "ExecStart")?)?;
    verify_effective_environment_files(instance)?;
    Ok(())
}

fn validate_effective_working_directory(
    instance: &crate::daemon_instance::DaemonInstance,
    raw: &str,
) -> Result<()> {
    let expected = if instance.instance == crate::daemon_instance::BARE_INSTANCE {
        "/var/lib/velnor".to_owned()
    } else {
        format!("/var/lib/velnor-{}", instance.instance)
    };
    if raw.trim() != expected.as_str() {
        bail!(
            "unit {} has unsupported effective WorkingDirectory",
            instance.unit
        );
    }
    Ok(())
}

fn validate_effective_exec_start(
    instance: &crate::daemon_instance::DaemonInstance,
    raw: &str,
) -> Result<()> {
    let effective = parse_exec_start_property(raw)?;
    if effective != expected_exec_start() {
        bail!(
            "unit {} has unsupported effective ExecStart; expected the packaged literal daemon command",
            instance.unit
        );
    }
    Ok(())
}

fn verify_effective_environment_files(
    instance: &crate::daemon_instance::DaemonInstance,
) -> Result<()> {
    let actual = parse_environment_files_property(&systemctl_show(instance, "EnvironmentFiles")?)?;
    let secret_path = configured_secret_environment_file(instance)?;
    let expected = vec![(instance.env_file.clone(), false), (secret_path, true)];
    if actual != expected {
        bail!(
            "unit {} has unsupported effective EnvironmentFiles; only the packaged instance and optional secrets files are accepted",
            instance.unit
        );
    }
    Ok(())
}

fn parse_environment_files_property(raw: &str) -> Result<Vec<(PathBuf, bool)>> {
    let fields = raw.split_whitespace().collect::<Vec<_>>();
    if fields.is_empty() || fields.len() % 2 != 0 {
        bail!("systemd EnvironmentFiles property has unsupported format");
    }
    let mut files = Vec::with_capacity(fields.len() / 2);
    for pair in fields.chunks_exact(2) {
        let path = PathBuf::from(pair[0]);
        if !path.is_absolute() {
            bail!("systemd EnvironmentFiles contains a non-absolute path");
        }
        let optional = match pair[1] {
            "(ignore_errors=no)" => false,
            "(ignore_errors=yes)" => true,
            _ => bail!("systemd EnvironmentFiles has unsupported ignore_errors value"),
        };
        files.push((path, optional));
    }
    Ok(files)
}

/// The packaged secrets file is loaded after the ordinary instance env file,
/// but daemon_instance intentionally does not read secrets. Inspect only keys
/// and reject root-bearing overrides without ever logging secret values.
fn verify_secret_environment_file(instance: &crate::daemon_instance::DaemonInstance) -> Result<()> {
    let path = configured_secret_environment_file(instance)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspect configured secret environment file {}",
                    path.display()
                )
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "configured secret environment file is not a regular file: {}",
            path.display()
        );
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read configured secret environment file {}", path.display()))?;
    for (key, _) in crate::daemon_instance::parse_environment_file(&text).with_context(|| {
        format!(
            "parse configured secret environment file {}",
            path.display()
        )
    })? {
        if key.starts_with("VELNOR_") {
            bail!(
                "configured secret environment file contains a VELNOR override: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn verify_regular_instance_environment_file(
    instance: &crate::daemon_instance::DaemonInstance,
) -> Result<()> {
    let metadata = fs::symlink_metadata(&instance.env_file).with_context(|| {
        format!(
            "inspect configured instance environment file {}",
            instance.env_file.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "configured instance environment file is not a regular file: {}",
            instance.env_file.display()
        );
    }
    Ok(())
}

/// Replay root assignments with the same systemd EnvironmentFile parser the
/// resolver uses. Duplicate roots and values that do not match the resolved
/// instance fail before any purge plan is accepted.
fn verify_root_environment_file(instance: &crate::daemon_instance::DaemonInstance) -> Result<()> {
    let text = fs::read_to_string(&instance.env_file).with_context(|| {
        format!(
            "read configured instance environment file {}",
            instance.env_file.display()
        )
    })?;
    let parsed = crate::daemon_instance::parse_environment_file(&text).with_context(|| {
        format!(
            "parse configured instance environment file {}",
            instance.env_file.display()
        )
    })?;
    let mut parsed_roots = BTreeMap::new();
    for (key, value) in parsed {
        if ROOT_ENVIRONMENT_KEYS.contains(&key.as_str())
            && parsed_roots.insert(key.clone(), value).is_some()
        {
            bail!(
                "configured instance environment file repeats root key {key}: {}",
                instance.env_file.display()
            );
        }
    }
    for (key, value) in &parsed_roots {
        let unsupported_path_value = matches!(
            key.as_str(),
            "VELNOR_STORAGE_ROOT" | "VELNOR_WORK_DIR" | "VELNOR_SLOTS"
        ) && value.chars().any(char::is_whitespace);
        if value.is_empty()
            || value
                .chars()
                .any(|character| matches!(character, '\\' | '\'' | '"'))
            || unsupported_path_value
        {
            bail!(
                "configured instance environment file has an unsupported root value for {key}: {}",
                instance.env_file.display()
            );
        }
        if instance.environment.get(key).map(String::as_str) != Some(value.as_str()) {
            bail!(
                "configured instance root value for {key} differs from the enumerated environment: {}",
                instance.env_file.display()
            );
        }
    }
    if !parsed_roots.contains_key("VELNOR_WORK_DIR") || !parsed_roots.contains_key("VELNOR_SLOTS") {
        bail!(
            "configured instance environment file omits work or slot roots: {}",
            instance.env_file.display()
        );
    }
    let expected_scope = instance
        .environment
        .get("VELNOR_TRUST_SCOPE")
        .map(|scope| crate::trust_scope::configured(Some(scope)))
        .unwrap_or_else(|| crate::trust_scope::FAIL_CLOSED.to_owned());
    if instance.trust_scope != expected_scope {
        bail!("enumerated trust scope differs from its supported environment spelling");
    }
    Ok(())
}

fn configured_secret_environment_file(
    instance: &crate::daemon_instance::DaemonInstance,
) -> Result<PathBuf> {
    let file_name = if instance.instance == crate::daemon_instance::BARE_INSTANCE {
        "secrets.env".to_owned()
    } else {
        format!("{}.secrets.env", instance.instance)
    };
    Ok(instance
        .env_file
        .parent()
        .context("configured instance env file has no parent")?
        .join(file_name))
}

fn packaged_fragment_path(instance: &crate::daemon_instance::DaemonInstance) -> Result<PathBuf> {
    let name = if instance.instance == crate::daemon_instance::BARE_INSTANCE {
        "velnor-daemon.service"
    } else {
        "velnor-daemon@.service"
    };
    for root in ["/usr/lib/systemd/system", "/lib/systemd/system"] {
        let path = Path::new(root).join(name);
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!("packaged systemd unit fragment {name} is missing")
}

fn verify_static_unit_files(instance: &crate::daemon_instance::DaemonInstance) -> Result<()> {
    let fragment = packaged_fragment_path(instance)?;
    let expected = if instance.instance == crate::daemon_instance::BARE_INSTANCE {
        include_str!("../debian/velnor-daemon.service")
    } else {
        include_str!("../debian/velnor-daemon@.service")
    };
    let actual = fs::read_to_string(&fragment)
        .with_context(|| format!("read packaged unit fragment {}", fragment.display()))?;
    if actual != expected {
        bail!(
            "packaged unit fragment was modified: {}",
            fragment.display()
        );
    }

    let template = "velnor-daemon@.service";
    for root in unit_search_roots() {
        let concrete = root.join(&instance.unit);
        if concrete != fragment && fs::symlink_metadata(&concrete).is_ok() {
            bail!(
                "unsupported custom daemon unit fragment exists: {}",
                concrete.display()
            );
        }
        let candidate = root.join(template);
        if candidate != fragment && fs::symlink_metadata(&candidate).is_ok() {
            let contents = fs::read_to_string(&candidate)
                .with_context(|| format!("read daemon unit candidate {}", candidate.display()))?;
            let expected = include_str!("../debian/velnor-daemon@.service");
            if contents != expected {
                bail!(
                    "unsupported custom daemon unit fragment exists: {}",
                    candidate.display()
                );
            }
        }
    }

    Ok(())
}

fn unit_search_roots() -> Vec<PathBuf> {
    [
        "/etc/systemd/system",
        "/run/systemd/system",
        "/usr/local/lib/systemd/system",
        "/usr/lib/systemd/system",
        "/lib/systemd/system",
        "/usr/local/share/systemd/system",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

fn verify_effective_dropins(instance: &crate::daemon_instance::DaemonInstance) -> Result<()> {
    let raw = systemctl_show(instance, "DropInPaths")?;
    verify_dropin_paths_property(instance, &raw)
}

fn verify_dropin_paths_property(
    instance: &crate::daemon_instance::DaemonInstance,
    raw: &str,
) -> Result<()> {
    if !raw.trim().is_empty() {
        bail!(
            "unit {} has effective drop-ins; purge requires the unmodified packaged unit",
            instance.unit
        );
    }
    Ok(())
}

fn systemctl_show(
    instance: &crate::daemon_instance::DaemonInstance,
    property: &str,
) -> Result<String> {
    systemctl_show_unit(&instance.unit, property)
}

fn systemctl_show_unit(unit: &str, property: &str) -> Result<String> {
    let property_arg = format!("--property={property}");
    let output = Command::new("/usr/bin/systemctl")
        .args(["show", "--no-pager"])
        .arg(property_arg)
        .args(["--value"])
        .arg(unit)
        .output()
        .with_context(|| format!("query systemd property {property} for {unit}"))?;
    if !output.status.success() {
        bail!(
            "systemd refused property {property} for {unit}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("systemd returned invalid UTF-8 for {property} on {unit}"))
}

fn expected_exec_start() -> Vec<String> {
    vec![
        "/usr/bin/velnor-runner".to_owned(),
        "daemon".to_owned(),
        "--name".to_owned(),
        "${VELNOR_NAME}".to_owned(),
        "--labels".to_owned(),
        "${VELNOR_LABELS}".to_owned(),
        "--slots".to_owned(),
        "${VELNOR_SLOTS}".to_owned(),
        "--work-dir".to_owned(),
        "${VELNOR_WORK_DIR}".to_owned(),
        "--replace".to_owned(),
    ]
}

/// Re-derive the paths used by the daemon from its enumerated VELNOR
/// environment. ExecStart is checked separately as the literal packaged
/// template; systemd does not show environment-expanded argv in `show`.
fn validate_instance_roots(instance: &crate::daemon_instance::DaemonInstance) -> Result<()> {
    let environment = &instance.environment;
    let storage_root = environment
        .get("VELNOR_STORAGE_ROOT")
        .filter(|value| !value.is_empty())
        .context("configured unit has no VELNOR_STORAGE_ROOT")?;
    let configured_storage_root = PathBuf::from(storage_root);
    require_absolute_normalized(&configured_storage_root, "VELNOR_STORAGE_ROOT")?;
    if configured_storage_root != instance.storage_root {
        bail!("effective VELNOR_STORAGE_ROOT diverges from the enumerated storage root");
    }
    let layout = crate::storage::StorageLayout::from_prefix(&configured_storage_root);
    if instance.cache_root != layout.cache_root
        || instance.lib_root != layout.lib_root
        || instance.run_root != layout.run_root
        || instance.log_root != layout.log_root
    {
        bail!("enumerated daemon storage roots do not match VELNOR_STORAGE_ROOT");
    }
    let name = environment
        .get("VELNOR_NAME")
        .filter(|value| !value.is_empty())
        .context("configured unit has no VELNOR_NAME")?;
    if name != &instance.name {
        bail!("enumerated VELNOR_NAME diverges from instance identity");
    }
    if name.trim() != name {
        bail!("configured VELNOR_NAME has surrounding whitespace");
    }
    environment
        .get("VELNOR_LABELS")
        .filter(|value| !value.is_empty())
        .context("configured unit has no VELNOR_LABELS")?;
    let raw_slots = environment
        .get("VELNOR_SLOTS")
        .filter(|value| !value.is_empty())
        .context("configured unit has no VELNOR_SLOTS")?;
    let slots = raw_slots
        .trim()
        .parse::<usize>()
        .context("configured VELNOR_SLOTS is not numeric")?;
    if slots == 0 || u32::try_from(slots).is_err() || instance.slots != Some(slots) {
        bail!("configured VELNOR_SLOTS is unsupported or diverges from enumerated slots");
    }
    let raw_work_dir = environment
        .get("VELNOR_WORK_DIR")
        .filter(|value| !value.is_empty())
        .context("configured unit has no VELNOR_WORK_DIR")?;
    let configured_work_dir = PathBuf::from(raw_work_dir);
    require_absolute_normalized(&configured_work_dir, "VELNOR_WORK_DIR")?;
    if configured_work_dir != instance.work_dir {
        bail!("effective VELNOR_WORK_DIR diverges from the enumerated work root");
    }

    Ok(())
}

fn parse_exec_start_property(raw: &str) -> Result<Vec<String>> {
    let value = raw.trim();
    let body = value
        .strip_prefix("{ path=")
        .and_then(|value| value.strip_suffix('}'))
        .context("systemd ExecStart property is not one command record")?;
    if value.matches("{ path=").count() != 1 {
        bail!("systemd ExecStart property has multiple command records");
    }
    let (path, fields) = body
        .split_once(" ; argv[]=")
        .context("systemd ExecStart property has no argv[] field")?;
    let (argv, fields) = fields
        .split_once(" ; ignore_errors=")
        .context("systemd ExecStart property has no ignore_errors field")?;
    let (ignore_errors, _) = fields
        .split_once(" ; ")
        .context("systemd ExecStart property is truncated")?;
    if ignore_errors != "no" {
        bail!("systemd ExecStart command has unsupported ignore_errors value");
    }
    if path.is_empty()
        || argv
            .chars()
            .any(|character| matches!(character, '\\' | '\'' | '"' | '\n' | '\r' | '\t'))
    {
        bail!("systemd ExecStart property contains an unsupported escaped argument");
    }
    let args = argv
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if args.first().map(String::as_str) != Some(path) {
        bail!("systemd ExecStart path does not equal argv[0]");
    }
    Ok(args)
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
    use std::collections::BTreeMap;

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn test_tempdir() -> std::io::Result<TestTempDir> {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "velnor-stale-trust-{}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(TestTempDir { path }),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            ErrorKind::AlreadyExists,
            "could not reserve a unique Velnor test directory",
        ))
    }

    fn instance(
        name: &str,
        cache_root: PathBuf,
        work_dir: PathBuf,
        _run_root: PathBuf,
        scope: &str,
        slots: Option<usize>,
    ) -> crate::daemon_instance::DaemonInstance {
        let storage_root = cache_root
            .ancestors()
            .nth(3)
            .unwrap_or(&cache_root)
            .to_path_buf();
        let layout = crate::storage::StorageLayout::from_prefix(&storage_root);
        crate::daemon_instance::DaemonInstance {
            instance: name.to_owned(),
            unit: format!("velnor-daemon@{name}.service"),
            env_file: PathBuf::from(format!("/etc/velnor/{name}.env")),
            name: name.to_owned(),
            url: None,
            slots,
            storage_root,
            run_root: layout.run_root,
            lib_root: layout.lib_root,
            cache_root,
            log_root: layout.log_root,
            state_directory: PathBuf::from("/state/velnor"),
            config_dir: PathBuf::from("/config/velnor"),
            daemon_dir: PathBuf::from("/config/velnor/daemon"),
            work_dir,
            trust_scope: scope.to_owned(),
            socket_dir: PathBuf::from("/run/velnor/test"),
            environment: BTreeMap::new(),
        }
    }

    fn mkdir(path: &Path) {
        fs::create_dir_all(path).unwrap();
    }

    fn anchor_snapshot(instance: &crate::daemon_instance::DaemonInstance) -> AnchorIdentities {
        capture_anchor_identities(&[
            instance.cache_root.clone(),
            instance.work_dir.clone(),
            legacy_shared_work_root(instance),
        ])
        .unwrap()
    }

    fn seed_old_roots(instance: &crate::daemon_instance::DaemonInstance, scopes: &[&str]) {
        mkdir(&instance.cache_root);
        mkdir(&instance.work_dir);
        let shared_work = legacy_shared_work_root(instance);
        mkdir(&shared_work);
        let slot_count = instance.slots.unwrap_or(1);
        if slot_count > 1 {
            for index in 1..=slot_count {
                mkdir(&instance.work_dir.join(format!("slot-{index}")));
            }
        }

        for scope in scopes {
            for class in CANONICAL_CLASSES {
                mkdir(&instance.cache_root.join(scope).join(class));
            }
            for family in WORK_FAMILIES {
                mkdir(&shared_work.join(family).join(scope));
            }
            for class in LEGACY_LEASE_CLASSES {
                let lease = instance
                    .run_root
                    .join(LEGACY_LEASE_ROOT)
                    .join(class)
                    .join(format!("{scope}.json"));
                mkdir(lease.parent().unwrap());
                fs::write(lease, b"opaque legacy lease record").unwrap();
            }
        }
        let anchors = anchor_snapshot(instance);
        let slot_roots = configured_slot_work_roots(
            instance,
            anchors.get(&instance.work_dir).and_then(Option::as_ref),
        )
        .unwrap();
        for slot_root in slot_roots {
            for scope in scopes {
                mkdir(&slot_root.join("stable-workspaces").join(scope));
            }
        }
    }

    #[test]
    fn inventories_exact_canonical_work_and_stable_roots_for_custom_instances() {
        let temp = test_tempdir().unwrap();
        let cache_a = temp.path().join("custom-a/cache/velnor/v1");
        let work_a = temp.path().join("custom-a/work");
        let cache_b = temp.path().join("custom-b/cache/velnor/v1");
        let work_b = temp.path().join("custom-b/work");
        let a = instance(
            "alpha",
            cache_a.clone(),
            work_a.clone(),
            temp.path().join("run-a"),
            "pool/a",
            Some(2),
        );
        let b = instance(
            "beta",
            cache_b.clone(),
            work_b.clone(),
            temp.path().join("run-b"),
            "pool_b",
            Some(1),
        );
        seed_old_roots(&a, &["pool_a", "previous_scope"]);
        seed_old_roots(&b, &["pool_b"]);
        let plan_a = plan_for_instance(&a).unwrap();
        let plan_b = plan_for_instance(&b).unwrap();
        let inventory_a = plan_a.candidates;
        let inventory_b = plan_b.candidates;

        assert_eq!(inventory_a.len(), 41);
        assert_eq!(inventory_b.len(), 22);
        let paths_a = inventory_a
            .iter()
            .map(|candidate| &candidate.path)
            .collect::<BTreeSet<_>>();
        for scope in ["pool_a", "previous_scope"] {
            for class in CANONICAL_CLASSES {
                assert!(paths_a.contains(&cache_a.join(scope).join(class)));
            }
            for family in WORK_FAMILIES {
                assert!(paths_a.contains(&work_a.join(family).join(scope)));
            }
        }
        assert!(paths_a.contains(&work_a.join("slot-1/stable-workspaces/pool_a")));
        assert!(paths_a.contains(&work_a.join("slot-2/stable-workspaces/pool_a")));
        assert!(paths_a.contains(&work_a.join("stable-workspaces/pool_a")));
        assert!(paths_a.contains(&work_a.join("slot-1/stable-workspaces/previous_scope")));
        assert!(paths_a.contains(&work_a.join("slot-2/stable-workspaces/previous_scope")));
        for class in LEGACY_LEASE_CLASSES {
            assert!(paths_a.contains(&a.run_root.join("leases").join(class)));
            assert!(inventory_b
                .iter()
                .any(|candidate| candidate.path == b.run_root.join("leases").join(class)));
        }
        assert!(inventory_b
            .iter()
            .any(|candidate| candidate.path == work_b.join("stable-workspaces/pool_b")));
    }

    #[test]
    fn inventories_base_and_historical_slots_after_slot_count_changes() {
        let temp = test_tempdir().unwrap();
        let work = temp.path().join("work");
        let instance = instance(
            "custom",
            temp.path().join("cache"),
            work.clone(),
            temp.path().join("run"),
            "current",
            Some(2),
        );
        seed_old_roots(&instance, &["old_scope"]);
        mkdir(&work.join("slot-5/stable-workspaces/old_scope"));
        mkdir(&work.join("slot-5/_velnor_caches/old_scope"));
        mkdir(&work.join("slot-5/_velnor_sccache/old_scope"));

        let plan = plan_for_instance(&instance).unwrap();
        let paths = plan
            .candidates
            .iter()
            .map(|candidate| candidate.path.clone())
            .collect::<BTreeSet<_>>();
        assert!(paths.contains(&work.join("stable-workspaces/old_scope")));
        assert!(paths.contains(&work.join("slot-1/stable-workspaces/old_scope")));
        assert!(paths.contains(&work.join("slot-2/stable-workspaces/old_scope")));
        assert!(paths.contains(&work.join("slot-5/stable-workspaces/old_scope")));
        assert!(paths.contains(&work.join("slot-5/_velnor_caches/old_scope")));
        assert!(paths.contains(&work.join("slot-5/_velnor_sccache/old_scope")));
    }

    #[test]
    fn multi_slot_configured_work_root_ending_in_slot_name_stays_configured() {
        let temp = test_tempdir().unwrap();
        let cache = temp.path().join("custom/cache/velnor/v1");
        let work = temp.path().join("custom/slot-1");
        let instance = instance(
            "custom",
            cache,
            work.clone(),
            temp.path().join("run"),
            "pool/a",
            Some(2),
        );
        seed_old_roots(&instance, &["pool_a"]);

        let plan = plan_for_instance(&instance).unwrap();
        assert!(plan
            .candidates
            .iter()
            .any(|candidate| candidate.path == work.join("_velnor_cargo/pool_a")));
        assert!(
            !plan
                .candidates
                .iter()
                .any(|candidate| candidate.path
                    == work.parent().unwrap().join("_velnor_cargo/pool_a"))
        );
    }

    #[test]
    fn single_slot_runtime_store_root_lifts_slot_work_dir_and_stable_root_stays_configured() {
        let temp = test_tempdir().unwrap();
        let cache = temp.path().join("custom/cache/velnor/v1");
        let work = temp.path().join("custom/slot-1");
        let instance = instance(
            "custom",
            cache,
            work.clone(),
            temp.path().join("run"),
            "pool/a",
            Some(1),
        );
        seed_old_roots(&instance, &["pool_a"]);

        let plan = plan_for_instance(&instance).unwrap();
        let shared = work.parent().unwrap();
        assert!(plan
            .candidates
            .iter()
            .any(|candidate| { candidate.path == shared.join("_velnor_cargo/pool_a") }));
        assert!(!plan
            .candidates
            .iter()
            .any(|candidate| candidate.path == work.join("_velnor_cargo/pool_a")));
        assert!(plan
            .candidates
            .iter()
            .any(|candidate| { candidate.path == work.join("stable-workspaces/pool_a") }));
    }

    #[test]
    fn purge_removes_only_allowlisted_old_roots_and_preserves_keyed_and_unknown_siblings() {
        let temp = test_tempdir().unwrap();
        let cache = temp.path().join("custom/cache/velnor/v1");
        let work = temp.path().join("custom/work");
        let instance = instance(
            "custom",
            cache.clone(),
            work.clone(),
            temp.path().join("run"),
            "new/scope",
            Some(1),
        );
        seed_old_roots(&instance, &["pool_a", "previous_scope"]);
        let mbx_alias = cache.join("pool_a/compiler/mbx");
        mkdir(&mbx_alias.join("incremental"));
        fs::write(
            mbx_alias.join("incremental/ambiguous.rlib"),
            b"opaque legacy bytes",
        )
        .unwrap();
        let work_artifacts = crate::store_catalog::StoreCatalog::for_work_root_with_layout(
            &work,
            &instance.storage_layout(),
        )
        .artifacts();

        let kept = [
            cache.join("pool_a/artifacts/run-1"),
            cache.join("pool_a/runtime/run-1"),
            cache.join("pool_a/logs/run-1"),
            cache.join("gha-cache/tenant-a"),
            cache.join("artifacts/run-2"),
            cache.join("runtime/run-2"),
            cache.join("logs/run-2"),
            cache.join("pool_a/unrelated"),
            cache.join("unknown#sibling/entry"),
            crate::trust_scope::filesystem_key_namespace(&cache).join("keyed-sibling/cargo"),
            work_artifacts.join("pool_a/run-3"),
            work.join("_velnor_runtime/pool_a"),
            work.join("_velnor_logs/pool_a"),
            work.join("_velnor_cargo__trust_scope_v1/keyed-sibling"),
            work.join("slot-1/stable-workspaces__trust_scope_v1/keyed-sibling"),
            work.join("_velnor_cargo/unknown#sibling/entry"),
            work.join("slot-1/stable-workspaces/unknown#sibling/entry"),
            work.join("unrelated-sibling/anything"),
            instance.run_root.join("leases__trust_scope_v2/cargo"),
            instance.run_root.join("leases/unknown-class"),
        ];
        for path in &kept {
            mkdir(path);
            fs::write(path.join("keep"), b"preserved").unwrap();
        }

        purge_instances_for_tests(std::slice::from_ref(&instance)).unwrap();
        for scope in ["pool_a", "previous_scope"] {
            for class in CANONICAL_CLASSES {
                assert!(!cache.join(scope).join(class).exists());
            }
            for family in WORK_FAMILIES {
                assert!(!work.join(family).join(scope).exists());
            }
            assert!(!work.join("stable-workspaces").join(scope).exists());
        }
        for class in LEGACY_LEASE_CLASSES {
            assert!(!instance.run_root.join("leases").join(class).exists());
        }
        assert!(
            !mbx_alias.exists(),
            "old MBX alias must be purged without migration"
        );
        assert!(
            !crate::trust_scope::filesystem_key_path(&cache, "pool/a")
                .join("compiler/mbx/incremental/ambiguous.rlib")
                .exists(),
            "legacy MBX payload must not be read or copied to the keyed root"
        );
        for path in &kept {
            assert!(path.join("keep").is_file(), "{}", path.display());
        }
    }

    #[test]
    fn reserved_sanitized_cache_scope_collision_aborts_before_any_removal() {
        let temp = test_tempdir().unwrap();
        let cache = temp.path().join("custom/cache/velnor/v1");
        let work = temp.path().join("custom/work");
        let instance = instance(
            "custom",
            cache.clone(),
            work,
            temp.path().join("run"),
            "pool/a",
            Some(1),
        );
        mkdir(&cache);
        for scope in ["gha-cache", "artifacts", "runtime", "logs"] {
            mkdir(&cache.join(scope));
        }
        mkdir(&cache.join("gha-cache/tenant-a"));
        fs::write(cache.join("gha-cache/tenant-a/payload"), b"tenant data").unwrap();

        let initial = plan_for_instance(&instance).unwrap();
        for scope in ["gha-cache", "artifacts", "runtime", "logs"] {
            assert!(initial
                .candidates
                .iter()
                .any(|candidate| { candidate.path == cache.join(scope).join("cargo") }));
        }
        validate_plans(&[initial]).unwrap();

        let ambiguous = cache.join("artifacts/cargo");
        mkdir(&ambiguous);
        fs::write(ambiguous.join("preserve"), b"artifact data").unwrap();

        let plan = plan_for_instance(&instance).unwrap();
        assert!(plan
            .candidates
            .iter()
            .any(|candidate| { candidate.path == cache.join("artifacts/cargo") }));
        assert!(validate_plans(&[plan]).is_err());
        assert_eq!(
            fs::read(ambiguous.join("preserve")).unwrap(),
            b"artifact data"
        );
        assert_eq!(
            fs::read(cache.join("gha-cache/tenant-a/payload")).unwrap(),
            b"tenant data"
        );
    }

    #[test]
    fn refuses_symlink_candidate_and_symlinked_candidate_ancestor() {
        let temp = test_tempdir().unwrap();
        let anchor = temp.path().join("anchor");
        let outside = temp.path().join("outside");
        mkdir(&anchor);
        mkdir(&outside.join("scope/cargo"));
        let redirect = anchor.join("scope");
        #[cfg(target_os = "macos")]
        std::os::unix::fs::symlink(&outside.join("scope"), &redirect).unwrap();
        #[cfg(target_os = "linux")]
        std::os::unix::fs::symlink(&outside.join("scope"), &redirect).unwrap();
        let mut ancestor_candidates = [CandidateRoot {
            instance: "test".into(),
            class: "cargo",
            anchor: anchor.clone(),
            anchor_identity: Some(
                crate::leftover_disk::filesystem_directory_identity(&anchor).unwrap(),
            ),
            candidate_identity: None,
            legacy_lease_entries: None,
            path: redirect.join("cargo"),
        }];
        assert!(pin_candidate_identities(&mut ancestor_candidates).is_err());
        assert!(outside.join("scope/cargo").is_dir());

        let direct = anchor.join("direct");
        std::os::unix::fs::symlink(&outside.join("scope"), &direct).unwrap();
        let mut direct_candidates = [CandidateRoot {
            instance: "test".into(),
            class: "cargo",
            anchor_identity: Some(
                crate::leftover_disk::filesystem_directory_identity(&anchor).unwrap(),
            ),
            candidate_identity: None,
            legacy_lease_entries: None,
            anchor,
            path: direct,
        }];
        assert!(pin_candidate_identities(&mut direct_candidates).is_err());
        assert!(outside.join("scope/cargo").is_dir());
    }

    #[test]
    fn configured_scope_symlink_and_known_lease_symlink_abort_inventory() {
        let temp = test_tempdir().unwrap();
        let cache = temp.path().join("storage/cache/velnor/v1");
        let work = temp.path().join("work");
        let run = temp.path().join("run");
        let outside = temp.path().join("outside");
        mkdir(&cache);
        mkdir(&work);
        mkdir(&outside.join("cargo"));
        let instance = instance(
            "custom",
            cache.clone(),
            work,
            run.clone(),
            "current",
            Some(1),
        );

        std::os::unix::fs::symlink(&outside, cache.join("pool_a")).unwrap();
        assert!(plan_for_instance(&instance).is_err());
        assert!(outside.join("cargo").is_dir());

        fs::remove_file(cache.join("pool_a")).unwrap();
        let lease_root = instance.run_root.join(LEGACY_LEASE_ROOT);
        mkdir(&lease_root);
        std::os::unix::fs::symlink(&outside, lease_root.join("cargo")).unwrap();
        assert!(plan_for_instance(&instance).is_err());
        assert!(outside.join("cargo").is_dir());
    }

    #[test]
    fn refuses_symlinked_configured_slot_root() {
        let temp = test_tempdir().unwrap();
        let work = temp.path().join("work");
        let outside = temp.path().join("outside");
        mkdir(&work);
        mkdir(&outside);
        std::os::unix::fs::symlink(&outside, work.join("slot-1")).unwrap();
        let instance = instance(
            "custom",
            temp.path().join("storage/cache/velnor/v1"),
            work.clone(),
            temp.path().join("run"),
            "scope",
            Some(2),
        );
        let anchor_identity = crate::leftover_disk::filesystem_directory_identity(&work).unwrap();
        assert!(configured_slot_work_roots(&instance, Some(&anchor_identity)).is_err());
        assert!(outside.is_dir());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refuses_mismatched_mount_identity_before_mutation() {
        let temp = test_tempdir().unwrap();
        let anchor = temp.path().join("anchor");
        let candidate = anchor.join("old-root");
        mkdir(&candidate.join("child"));
        let anchor_identity = crate::leftover_disk::filesystem_directory_identity(&anchor).unwrap();
        let mount = crate::leftover_disk::filesystem_mount_identity(&anchor)
            .unwrap()
            .unwrap();
        let crate::leftover_disk::FilesystemMountIdentity::LinuxMountId(mount_id) = mount else {
            panic!("expected Linux mount identity");
        };
        let wrong = crate::leftover_disk::FilesystemMountIdentity::LinuxMountId(mount_id + 1);
        assert!(
            crate::leftover_disk::remove_dir_all_on_device_and_mount_under(
                &anchor,
                &candidate,
                anchor_identity.device,
                wrong,
            )
            .is_err()
        );
        assert!(candidate.join("child").is_dir());

        let candidate_identity = crate::leftover_disk::filesystem_directory_identity_under(
            &anchor,
            &candidate,
            &anchor_identity,
        )
        .unwrap();
        let mut wrong_candidate_identity = candidate_identity.clone();
        wrong_candidate_identity.inode += 1;
        assert!(
            crate::leftover_disk::remove_dir_all_on_device_under_identities(
                &anchor,
                &candidate,
                anchor_identity.device,
                &anchor_identity,
                &wrong_candidate_identity,
            )
            .is_err()
        );
        assert!(candidate.join("child").is_dir());
    }

    #[test]
    fn gc_and_filesystem_locks_bound_manual_gc_overlap_before_deletion() {
        let temp = test_tempdir().unwrap();
        let run = temp.path().join("run");
        let anchor = temp.path().join("work");
        let candidate_path = anchor.join("_velnor_cargo/scope");
        mkdir(&candidate_path);
        let anchor_identity = crate::leftover_disk::filesystem_directory_identity(&anchor).unwrap();
        let candidate = CandidateRoot {
            instance: "test".into(),
            class: "_velnor_cargo",
            anchor_identity: Some(anchor_identity.clone()),
            candidate_identity: Some(
                crate::leftover_disk::filesystem_directory_identity_under(
                    &anchor,
                    &candidate_path,
                    &anchor_identity,
                )
                .unwrap(),
            ),
            legacy_lease_entries: None,
            anchor,
            path: candidate_path.clone(),
        };

        for lock_name in [GC_LOCK, FILESYSTEM_COORDINATOR_LOCK] {
            mkdir(&run);
            let held = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(run.join(lock_name))
                .unwrap();
            rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).unwrap();
            assert!(purge_candidates_with_locks(
                &[candidate.clone()],
                &[run.clone()],
                Duration::from_millis(30),
            )
            .is_err());
            assert!(candidate_path.is_dir());
            drop(held);
        }

        purge_candidates_with_locks(&[candidate], &[run], Duration::from_secs(1)).unwrap();
        assert!(!candidate_path.exists());
    }

    fn purge_candidates_with_locks(
        candidates: &[CandidateRoot],
        run_roots: &[PathBuf],
        timeout: Duration,
    ) -> Result<()> {
        let _locks = acquire_instance_locks(run_roots, timeout)?;
        let physical_paths = physical_plan_paths(&candidates.iter().collect::<Vec<_>>(), &[])?;
        remove_candidates(candidates, &[], &physical_paths)
    }

    #[test]
    fn exec_start_proof_requires_exact_packaged_command_and_rejects_root_divergence() {
        let temp = test_tempdir().unwrap();
        let cache = temp.path().join("custom/cache/velnor/v1");
        let work = temp.path().join("work");
        let mut instance = instance(
            "custom",
            cache,
            work.clone(),
            temp.path().join("run"),
            "scope",
            Some(2),
        );
        instance.environment.extend([
            (
                "VELNOR_STORAGE_ROOT".to_owned(),
                instance.storage_root.display().to_string(),
            ),
            ("VELNOR_NAME".to_owned(), "custom".to_owned()),
            ("VELNOR_LABELS".to_owned(), "velnor,test".to_owned()),
            ("VELNOR_SLOTS".to_owned(), "2".to_owned()),
            ("VELNOR_WORK_DIR".to_owned(), work.display().to_string()),
        ]);
        validate_instance_roots(&instance).unwrap();
        let expected = expected_exec_start();
        let direct_daemon_argv = [
            "/usr/bin/velnor-runner",
            "daemon",
            "--name",
            "${VELNOR_NAME}",
            "--labels",
            "${VELNOR_LABELS}",
            "--slots",
            "${VELNOR_SLOTS}",
            "--work-dir",
            "${VELNOR_WORK_DIR}",
            "--replace",
        ]
        .map(str::to_owned);
        for packaged_unit in [
            include_str!("../debian/velnor-daemon.service"),
            include_str!("../debian/velnor-daemon@.service"),
        ] {
            let packaged_unit_argv = packaged_unit
                .split_once("ExecStart=")
                .expect("packaged daemon ExecStart")
                .1
                .split_once("Restart=")
                .expect("packaged daemon Restart setting")
                .0
                .replace("\\\n", " ")
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            assert_eq!(packaged_unit_argv, direct_daemon_argv);
            assert_eq!(expected, packaged_unit_argv);
        }
        let rendered = format!(
            "{{ path={} ; argv[]={} ; ignore_errors=no ; start_time=[n/a] ; }}",
            expected[0],
            expected.join(" ")
        );
        assert_eq!(parse_exec_start_property(&rendered).unwrap(), expected);
        validate_effective_exec_start(&instance, &rendered).unwrap();
        validate_effective_working_directory(&instance, "/var/lib/velnor-custom\n").unwrap();
        let divergent = rendered.replace("${VELNOR_WORK_DIR}", "/custom/other-work");
        assert_ne!(parse_exec_start_property(&divergent).unwrap(), expected);
        assert!(validate_effective_exec_start(&instance, &divergent).is_err());
        assert!(validate_effective_working_directory(&instance, "/custom/work\n").is_err());
        let wrapped = format!(
            "{{ path=/usr/bin/flock ; argv[]=/usr/bin/flock --shared --no-fork {PACKAGE_TRANSACTION_LOCK} {} ; ignore_errors=no ; start_time=[n/a] ; }}",
            expected.join(" ")
        );
        assert_ne!(parse_exec_start_property(&wrapped).unwrap(), expected);
        assert!(validate_effective_exec_start(&instance, &wrapped).is_err());
    }

    #[test]
    fn candidate_overlap_fails_before_deletion() {
        let temp = test_tempdir().unwrap();
        let cache = temp.path().join("cache");
        let work_a = temp.path().join("work");
        let work_b = work_a.join("_velnor_cargo/scope");
        let a = instance(
            "a",
            cache.clone(),
            work_a.clone(),
            temp.path().join("run-a"),
            "scope",
            Some(1),
        );
        let b = instance(
            "b",
            cache.join("nested"),
            work_b.clone(),
            temp.path().join("run-b"),
            "scope",
            Some(1),
        );
        mkdir(&work_a.join("_velnor_cargo/scope"));
        mkdir(&work_b);
        let plans = plans_for_instances(&[a, b]).unwrap();
        assert!(validate_plans(&plans).is_err());
    }

    #[test]
    fn candidate_symlink_alias_into_protected_root_fails_before_deletion() {
        use std::os::unix::fs::symlink;

        let temp = test_tempdir().unwrap();
        let protected_root = temp.path().join("keyed/artifacts");
        let candidate_path = protected_root.join("old-scope");
        mkdir(&candidate_path);
        let marker = candidate_path.join("must-survive");
        fs::write(&marker, b"protected").unwrap();
        let alias = temp.path().join("configured-cache");
        symlink(temp.path().join("keyed"), &alias).unwrap();
        let candidate_path = alias.join("artifacts/old-scope");
        let anchor_identity = crate::leftover_disk::filesystem_directory_identity(&alias).unwrap();
        let candidate_identity = crate::leftover_disk::filesystem_directory_identity_under(
            &alias,
            &candidate_path,
            &anchor_identity,
        )
        .unwrap();
        let candidate = CandidateRoot {
            instance: "alias".to_owned(),
            class: "cargo",
            anchor: alias,
            anchor_identity: Some(anchor_identity),
            candidate_identity: Some(candidate_identity),
            legacy_lease_entries: None,
            path: candidate_path,
        };
        let plan = InstancePlan {
            candidates: vec![candidate],
            preserved: vec![protected_root],
            anchor_identities: AnchorIdentities::new(),
        };

        assert!(validate_plans(&[plan]).is_err());
        assert_eq!(fs::read(&marker).unwrap(), b"protected");
    }

    #[test]
    fn keyed_stable_workspace_root_is_protected_through_physical_alias() {
        use std::os::unix::fs::symlink;

        let temp = test_tempdir().unwrap();
        let work = temp.path().join("work");
        let slot = work.join("slot-1");
        let keyed_root = slot.join(crate::stable_workspace::STABLE_WORKSPACES_DIR);
        mkdir(&keyed_root.join("legacy-shaped-child"));
        let alias = temp.path().join("candidate-anchor");
        symlink(&keyed_root, &alias).unwrap();
        let instance = instance(
            "custom",
            temp.path().join("cache"),
            work.clone(),
            temp.path().join("run"),
            "scope",
            Some(2),
        );
        let candidate_path = alias.join("legacy-shaped-child");
        let anchor_identity = crate::leftover_disk::filesystem_directory_identity(&alias).unwrap();
        let candidate = CandidateRoot {
            instance: "custom".to_owned(),
            class: "stable-workspaces",
            anchor: alias.clone(),
            anchor_identity: Some(anchor_identity.clone()),
            candidate_identity: Some(
                crate::leftover_disk::filesystem_directory_identity_under(
                    &alias,
                    &candidate_path,
                    &anchor_identity,
                )
                .unwrap(),
            ),
            legacy_lease_entries: None,
            path: candidate_path,
        };
        let preserved = preserved_roots(&instance, &[slot]);
        assert!(preserved.contains(&keyed_root));
        assert!(preserved
            .contains(&crate::store_catalog::StoreCatalog::artifacts_in_slot_work_root(&work)));
        assert!(!preserved.contains(&work.join("_velnor_runtime")));
        assert!(!preserved.contains(&work.join("_velnor_logs")));
        for family in [
            "_velnor_cargo__trust_scope_v1",
            "_velnor_mise__trust_scope_v1",
            "_velnor_targets__trust_scope_v1",
            "_velnor_caches__trust_scope_v1",
            "_velnor_mbx__trust_scope_v1",
            "_velnor_sccache__trust_scope_v1",
            "_velnor_git__trust_scope_v1",
        ] {
            assert!(
                !preserved.contains(&work.join(family)),
                "obsolete cache family remains protected: {family}"
            );
        }
        let plan = InstancePlan {
            candidates: vec![candidate],
            preserved,
            anchor_identities: AnchorIdentities::new(),
        };

        assert!(validate_plans(&[plan]).is_err());
        assert!(keyed_root.join("legacy-shaped-child").is_dir());
    }

    #[test]
    fn prospective_overlap_through_symlink_is_proved_for_missing_leaves() {
        use std::os::unix::fs::symlink;

        let temp = test_tempdir().unwrap();
        let physical = temp.path().join("storage");
        mkdir(&physical);
        let alias = temp.path().join("configured-root");
        symlink(&physical, &alias).unwrap();
        let candidate = CandidateRoot {
            instance: "alias".to_owned(),
            class: "cargo",
            anchor: alias.clone(),
            anchor_identity: None,
            candidate_identity: None,
            legacy_lease_entries: None,
            path: alias.join("future/legacy-root"),
        };
        let plan = InstancePlan {
            candidates: vec![candidate],
            preserved: vec![physical.join("future")],
            anchor_identities: AnchorIdentities::new(),
        };

        assert!(validate_plans(&[plan]).is_err());
    }

    #[test]
    fn deletion_recheck_rejects_new_symlink_alias_to_protected_root() {
        use std::os::unix::fs::symlink;

        let temp = test_tempdir().unwrap();
        let original = temp.path().join("original");
        let protected = temp.path().join("protected");
        mkdir(&original.join("old-scope"));
        mkdir(&protected);
        let alias = temp.path().join("configured-root");
        symlink(&original, &alias).unwrap();
        let anchor_identity = crate::leftover_disk::filesystem_directory_identity(&alias).unwrap();
        let candidate_path = alias.join("old-scope");
        let candidate_identity = crate::leftover_disk::filesystem_directory_identity_under(
            &alias,
            &candidate_path,
            &anchor_identity,
        )
        .unwrap();
        let candidate = CandidateRoot {
            instance: "alias".to_owned(),
            class: "cargo",
            anchor: alias.clone(),
            anchor_identity: Some(anchor_identity),
            candidate_identity: Some(candidate_identity),
            legacy_lease_entries: None,
            path: candidate_path,
        };
        let expected = physical_plan_paths(&[&candidate], &[&protected]).unwrap();

        fs::remove_file(&alias).unwrap();
        symlink(&protected, &alias).unwrap();

        assert!(recheck_physical_plan_paths(&[candidate], &[protected], &expected).is_err());
    }

    #[test]
    fn deletion_recheck_rejects_missing_candidate_that_appears() {
        let temp = test_tempdir().unwrap();
        let anchor = temp.path().join("anchor");
        let protected = temp.path().join("protected");
        mkdir(&anchor);
        mkdir(&protected);
        let candidate = CandidateRoot {
            instance: "late-root".to_owned(),
            class: "cargo",
            anchor: anchor.clone(),
            anchor_identity: Some(
                crate::leftover_disk::filesystem_directory_identity(&anchor).unwrap(),
            ),
            candidate_identity: None,
            legacy_lease_entries: None,
            path: anchor.join("future/legacy"),
        };
        let expected = physical_plan_paths(&[&candidate], &[&protected]).unwrap();
        mkdir(&candidate.path);

        assert!(recheck_physical_plan_paths(&[candidate], &[protected], &expected).is_err());
    }

    #[cfg(target_os = "linux")]
    struct BindMount(PathBuf);

    #[cfg(target_os = "linux")]
    impl Drop for BindMount {
        fn drop(&mut self) {
            use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

            let Ok(path) = CString::new(self.0.as_os_str().as_bytes()) else {
                return;
            };
            // SAFETY: this path is the mountpoint created by the paired test
            // helper; lazy unmount is scoped to this fixture only.
            unsafe { libc::umount2(path.as_ptr(), libc::MNT_DETACH) };
        }
    }

    #[cfg(target_os = "linux")]
    fn bind_mount_test_directory(source: &Path, target: &Path) -> Option<BindMount> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

        let source = CString::new(source.as_os_str().as_bytes()).unwrap();
        let target_c = CString::new(target.as_os_str().as_bytes()).unwrap();
        // SAFETY: both C strings are valid paths to fixture directories, and
        // MS_BIND creates only the requested mount alias.
        let result = unsafe {
            libc::mount(
                source.as_ptr(),
                target_c.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND,
                std::ptr::null(),
            )
        };
        if result == 0 {
            return Some(BindMount(target.to_path_buf()));
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM | libc::EACCES | libc::ENOSYS | libc::EINVAL) => None,
            _ => panic!(
                "bind-mount test fixture: {}",
                std::io::Error::last_os_error()
            ),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn physical_overlap_rejects_candidate_bind_mounted_from_protected_root() {
        let temp = test_tempdir().unwrap();
        let anchor = temp.path().join("anchor");
        let protected = temp.path().join("protected");
        let candidate_path = anchor.join("legacy");
        mkdir(&candidate_path);
        mkdir(&protected);
        fs::write(protected.join("marker"), b"keep").unwrap();
        let Some(_mount) = bind_mount_test_directory(&protected, &candidate_path) else {
            return;
        };
        let candidate = CandidateRoot {
            instance: "mounted-alias".to_owned(),
            class: "cargo",
            anchor: anchor.clone(),
            anchor_identity: Some(
                crate::leftover_disk::filesystem_directory_identity(&anchor).unwrap(),
            ),
            candidate_identity: Some(
                crate::leftover_disk::filesystem_directory_identity(&candidate_path).unwrap(),
            ),
            legacy_lease_entries: None,
            path: candidate_path,
        };
        let plan = InstancePlan {
            candidates: vec![candidate],
            preserved: vec![protected.clone()],
            anchor_identities: AnchorIdentities::new(),
        };

        assert!(validate_plans(&[plan]).is_err());
        assert_eq!(fs::read(protected.join("marker")).unwrap(), b"keep");
    }

    #[test]
    fn configured_root_paths_must_be_absolute_and_normalized() {
        assert!(require_absolute_normalized(Path::new("relative/work"), "root").is_err());
        assert!(require_absolute_normalized(Path::new("/var/lib/../tmp"), "root").is_err());
        assert!(require_absolute_normalized(Path::new("/var/lib/work"), "root").is_ok());
    }

    #[test]
    fn effective_unit_proof_is_required_even_for_an_empty_inventory() {
        let temp = test_tempdir().unwrap();
        let missing_systemd_runtime = temp.path().join("missing/system");
        assert!(require_systemd_effective_unit_proof(&missing_systemd_runtime).is_err());
        assert!(verify_complete_instance_inventory_with_systemd_path(
            &[],
            &missing_systemd_runtime
        )
        .is_err());
        fs::create_dir(temp.path().join("system")).unwrap();
        assert!(require_systemd_effective_unit_proof(&temp.path().join("system")).is_ok());
    }

    #[test]
    fn package_lock_constants_match_manual_gc_lock_names() {
        assert_eq!(GC_LOCK, "gc.lock");
        assert_eq!(FILESYSTEM_COORDINATOR_LOCK, "filesystem-coordinator.lock");
        assert_eq!(
            PACKAGE_TRANSACTION_LOCK,
            "/run/velnor/package-transaction.lock"
        );
    }

    #[test]
    fn installed_service_unit_file_inventory_is_strict_and_deduplicated() {
        let services = parse_service_unit_file_names(
            "foo.service disabled enabled\nfoo.service static -\nbar@.service alias -\n",
        )
        .unwrap();
        assert_eq!(
            services,
            BTreeSet::from(["bar@.service".to_owned(), "foo.service".to_owned(),])
        );
        assert!(parse_service_unit_file_names("No files found.\n").is_err());
        assert!(parse_service_unit_file_names("foo.service\n").is_err());
    }

    #[test]
    fn detects_dbus_activation_by_type_or_bus_name_only() {
        assert!(has_dbus_activation_properties("dbus\n", ""));
        assert!(has_dbus_activation_properties(
            "simple\n",
            "org.example.Service\n"
        ));
        assert!(!has_dbus_activation_properties("simple\n", "\n"));
    }

    #[test]
    fn dbus_activation_drain_requires_systemd_v235_or_newer() {
        assert!(validate_systemd_activation_version("systemd 234\n").is_err());
        assert!(validate_systemd_activation_version("systemd 235 (fixture)\n").is_ok());
        assert!(validate_systemd_activation_version("systemd 252.26 (fixture)\n").is_ok());
        assert!(validate_systemd_activation_version("252 (manager fixture)\n").is_ok());
        assert!(validate_systemd_activation_version("234 (old manager fixture)\n").is_err());
        assert!(validate_systemd_activation_version("systemd 235.bad\n").is_err());
        assert!(validate_systemd_activation_version("unknown 252\n").is_err());
    }

    #[test]
    fn dbus_activation_drain_validates_the_systemd_list_jobs_snapshot_and_queue() {
        assert!(validate_systemd_list_jobs_response("a(usssoo) 0\n").is_ok());
        assert!(validate_systemd_list_jobs_response(
            "a(usssoo) 1 123 foo.service start waiting /org/freedesktop/systemd1/job/123 /org/freedesktop/systemd1/unit/foo_2eservice\n"
        )
        .is_ok());
        assert!(validate_systemd_list_jobs_response(
            "a(usssoo) 1 123 \"foo.service\" \"start\" \"waiting\" \"/org/freedesktop/systemd1/job/123\" \"/org/freedesktop/systemd1/unit/foo_2eservice\"\n"
        )
        .is_ok());
        assert!(validate_systemd_list_jobs_response("a(usss) 0\n").is_err());
        assert!(validate_systemd_list_jobs_response("a(usssoo) 1\n").is_err());

        assert!(parse_systemd_job_rows("No jobs running.\n")
            .unwrap()
            .is_empty());
        assert_eq!(
            parse_systemd_job_rows("123 foo.service start waiting\n").unwrap(),
            vec![(
                "foo.service".to_owned(),
                "start".to_owned(),
                "waiting".to_owned()
            )]
        );
        assert!(parse_systemd_job_rows("123 foo.service start unknown\n").is_err());
        assert!(parse_systemd_job_rows("malformed systemd job row\n").is_err());
    }

    #[test]
    fn systemd_bus_owner_requires_a_valid_stable_unique_name() {
        assert_eq!(parse_busctl_unique_name("s \":1.44\"\n").unwrap(), ":1.44");
        assert!(parse_busctl_unique_name("s org.freedesktop.systemd1\n").is_err());
        assert!(parse_busctl_unique_name("s \":1.foo\"\n").is_err());
        assert!(parse_busctl_unique_name("s \":1.44\" extra\n").is_err());
    }

    #[test]
    fn dbus_system_config_is_taken_from_the_effective_bus_unit() {
        assert_eq!(
            dbus_config_file_from_exec_start(
                "{ path=/usr/bin/dbus-daemon ; argv[]=/usr/bin/dbus-daemon --system ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
            )
            .unwrap(),
            PathBuf::from("/usr/share/dbus-1/system.conf")
        );
        assert_eq!(
            dbus_config_file_from_exec_start(
                "{ path=/usr/bin/dbus-daemon ; argv[]=/usr/bin/dbus-daemon --config-file=/etc/dbus-1/custom-system.conf ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
            )
            .unwrap(),
            PathBuf::from("/etc/dbus-1/custom-system.conf")
        );
        assert!(dbus_config_file_from_exec_start(
            "{ path=/usr/bin/dbus-broker-launch ; argv[]=/usr/bin/dbus-broker-launch --scope system --audit ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        )
        .is_err());
        assert!(dbus_config_file_from_exec_start(
            "{ path=/opt/dbus-wrapper ; argv[]=/opt/dbus-wrapper --system ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        )
        .is_err());
        assert!(dbus_config_file_from_exec_start(
            "{ path=/opt/custom/dbus-daemon ; argv[]=/opt/custom/dbus-daemon --system ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        )
        .is_err());
        assert!(dbus_config_file_from_exec_start(
            "{ path=dbus-daemon ; argv[]=dbus-daemon --system ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        )
        .is_err());
    }

    #[test]
    fn dbus_config_recurses_custom_service_dirs_and_fails_on_unresolved_paths() {
        let temp = test_tempdir().unwrap();
        let config_dir = temp.path().join("dbus");
        let fragment_dir = config_dir.join("fragments");
        let first_services = temp.path().join("custom-system-services");
        let second_services = temp.path().join("second-system-services");
        mkdir(&fragment_dir);
        mkdir(&first_services);
        mkdir(&second_services);
        fs::write(
            config_dir.join("system.conf"),
            "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\" \"https://example.invalid/busconfig.dtd\">\n<busconfig>\n<!-- <servicedir>/ignored</servicedir> -->\n<standard_system_servicedirs/>\n<include>nested.conf</include>\n<includedir>fragments</includedir>\n</busconfig>\n",
        )
        .unwrap();
        fs::write(
            config_dir.join("nested.conf"),
            format!(
                "<busconfig><servicedir>{}</servicedir></busconfig>\n",
                first_services.display()
            ),
        )
        .unwrap();
        fs::write(
            fragment_dir.join("10-services.conf"),
            format!(
                "<busconfig><servicedir>{}</servicedir></busconfig>\n",
                second_services.display()
            ),
        )
        .unwrap();
        let directories = collect_dbus_system_service_dirs(&config_dir.join("system.conf"))
            .unwrap()
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert!(directories.contains(&fs::canonicalize(first_services).unwrap()));
        assert!(directories.contains(&fs::canonicalize(second_services).unwrap()));
        assert!(directories.contains(&PathBuf::from(DBUS_STANDARD_SYSTEM_SERVICE_DIRS[0])));

        fs::write(
            config_dir.join("missing-include.conf"),
            "<busconfig><include>not-installed.conf</include></busconfig>\n",
        )
        .unwrap();
        assert!(
            collect_dbus_system_service_dirs(&config_dir.join("missing-include.conf")).is_err()
        );
        fs::write(
            config_dir.join("missing-servicedir.conf"),
            "<busconfig><servicedir>/not-installed/dbus-services</servicedir></busconfig>\n",
        )
        .unwrap();
        assert!(
            collect_dbus_system_service_dirs(&config_dir.join("missing-servicedir.conf")).is_err()
        );
    }

    #[test]
    fn dbus_config_rejects_custom_entities_that_can_hide_activation_directives() {
        let config = "<!DOCTYPE busconfig [<!ENTITY services \"&lt;servicedir&gt;/tmp/services&lt;/servicedir&gt;\">]>\n<busconfig>&services;</busconfig>\n";
        assert!(parse_dbus_config_directives(config).is_err());
    }

    #[test]
    fn selinux_relative_dbus_include_fails_closed_when_active() {
        let temp = test_tempdir().unwrap();
        let config = temp.path().join("system.conf");
        fs::write(
            &config,
            "<busconfig><include if_selinux_enabled=\"yes\" selinux_root_relative=\"yes\">contexts/dbus_contexts</include></busconfig>\n",
        )
        .unwrap();

        assert!(
            collect_dbus_system_service_dirs_with_selinux(&config, false)
                .unwrap()
                .is_empty()
        );
        assert!(collect_dbus_system_service_dirs_with_selinux(&config, true).is_err());
    }

    #[test]
    fn dbus_activation_service_files_detect_direct_exec_and_systemd_aliases() {
        let temp = test_tempdir().unwrap();
        let directory = temp.path().join("system-services");
        mkdir(&directory);
        let extensionless_wrapper = temp.path().join("extensionless-wrapper");
        fs::write(
            &extensionless_wrapper,
            "exec /opt/copied-runner daemon --name alpha\n",
        )
        .unwrap();
        let activation_file = directory.join("org.example.Service.service");
        fs::write(
            &activation_file,
            format!(
                "[D-BUS Service]\nName=org.example.Service\nExec=/usr/bin/python3.12 {}\n",
                extensionless_wrapper.display()
            ),
        )
        .unwrap();
        assert!(
            verify_dbus_activation_service_files_in(std::slice::from_ref(&directory), |_| Ok(
                false
            ))
            .is_err()
        );

        fs::write(
            &activation_file,
            "[D-BUS Service]\nName=org.example.NestedEnv\nExec=/usr/bin/env /usr/bin/env python3.12 -c __import__(chr(111)+chr(115)).system(chr(47)+chr(111)+chr(112)+chr(116)+chr(47)+chr(118)+chr(101)+chr(108)+chr(110)+chr(111)+chr(114)+chr(45)+chr(114)+chr(117)+chr(110)+chr(110)+chr(101)+chr(114)+chr(32)+chr(100)+chr(97)+chr(101)+chr(109)+chr(111)+chr(110))\n",
        )
        .unwrap();
        assert!(
            verify_dbus_activation_service_files_in(std::slice::from_ref(&directory), |_| Ok(
                false
            ))
            .is_err(),
            "nested env wrappers must not hide executable Python code"
        );

        fs::write(
            &activation_file,
            "[D-BUS Service]\nName=org.example.Service\nExec=/opt/extensionless-wrapper daemon --name alpha\n",
        )
        .unwrap();
        assert!(
            verify_dbus_activation_service_files_in(std::slice::from_ref(&directory), |_| Ok(
                false
            ))
            .is_err()
        );

        fs::write(
            &activation_file,
            "[D-BUS Service]\nName=org.example.Service\nExec=/usr/sbin/avahi-daemon --no-drop-root\n",
        )
        .unwrap();
        assert!(
            verify_dbus_activation_service_files_in(std::slice::from_ref(&directory), |_| Ok(
                false
            ))
            .is_ok()
        );

        fs::write(
            &activation_file,
            "[D-BUS Service]\nName=org.example.Service\nExec=/usr/bin/example\nSystemdService=alias.service\n",
        )
        .unwrap();
        let mut queried = false;
        assert!(verify_dbus_activation_service_files_in(
            std::slice::from_ref(&directory),
            |unit| {
                assert_eq!(unit, "alias.service");
                queried = true;
                Ok(true)
            }
        )
        .is_err());
        assert!(queried, "SystemdService alias was not queried");

        fs::write(
            &activation_file,
            "[D-BUS Service]\nName=org.example.Constructed\nExec=/usr/bin/python3.12 -c __import__(chr(111)+chr(115)).system(chr(47)+chr(111)+chr(112)+chr(116)+chr(47)+chr(118)+chr(101)+chr(108)+chr(110)+chr(111)+chr(114)+chr(45)+chr(114)+chr(117)+chr(110)+chr(110)+chr(101)+chr(114)+chr(32)+chr(100)+chr(97)+chr(101)+chr(109)+chr(111)+chr(110))\n",
        )
        .unwrap();
        assert!(
            verify_dbus_activation_service_files_in(std::slice::from_ref(&directory), |_| Ok(
                false
            ))
            .is_err()
        );

        fs::write(
            &activation_file,
            "[D-BUS Service]\nName=org.example.Service\nExec=/usr/bin/example \\\n --option\n",
        )
        .unwrap();
        assert!(
            verify_dbus_activation_service_files_in(std::slice::from_ref(&directory), |_| Ok(
                false
            ))
            .is_err()
        );

        assert!(parse_dbus_activation_service(
            "[D-BUS Service]\nName=org.example.Service\nExec=/usr/bin/example\\x20--argument\n"
        )
        .is_err());
    }

    #[test]
    fn dbus_systemd_service_validation_uses_unit_syntax_and_exact_runner_query() {
        for valid in [
            "org.example.Service.service",
            "velnor-daemon@alpha.beta.service",
            "example@.service",
            // After stripping `.service`, `example.` is a valid dot-containing prefix.
            "example..service",
        ] {
            assert!(is_systemd_service_unit_name(valid), "{valid:?}");
        }
        for invalid in [
            ".service",
            "@example.service",
            "../example.service",
            "example@one@two.service",
            "example.service/extra",
        ] {
            assert!(!is_systemd_service_unit_name(invalid), "{invalid:?}");
        }

        let temp = test_tempdir().unwrap();
        let directory = temp.path().join("system-services");
        mkdir(&directory);
        fs::write(
            directory.join("org.example.Other.service"),
            "[D-BUS Service]\nName=org.example.Other\nExec=/usr/bin/example\nSystemdService=velnord-helper.service\n",
        )
        .unwrap();
        let mut queried = false;
        assert!(verify_dbus_activation_service_files_in(
            std::slice::from_ref(&directory),
            |unit| {
                assert_eq!(unit, "velnord-helper.service");
                queried = true;
                Ok(false)
            }
        )
        .is_ok());
        assert!(
            queried,
            "ordinary unit with a Velnor-like prefix was not queried"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn drain_proof_detects_renamed_runner_inside_shell_wrapper() {
        let temp = test_tempdir().unwrap();
        let extensionless = temp.path().join("extensionless-wrapper");
        fs::write(
            &extensionless,
            "exec /opt/runner-copy daemon --name alpha\n",
        )
        .unwrap();
        let unit_command = "{ path=/bin/sh ; argv[]=/bin/sh -c exec /opt/runner-copy daemon --name alpha ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }";
        assert!(command_properties_invoke_velnor(unit_command));
        assert!(command_properties_invoke_velnor(
            "{ path=/bin/sh ; argv[]=/bin/sh /opt/maintain.sh ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        ));
        let python_wrapper = format!(
            "{{ path=/usr/bin/python3.12 ; argv[]=/usr/bin/python3.12 {} ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }}",
            extensionless.display()
        );
        assert!(command_properties_invoke_velnor(&python_wrapper));
        fs::write(&extensionless, "print('unrelated service')\n").unwrap();
        assert!(!command_properties_invoke_velnor(&python_wrapper));
        assert!(!command_properties_invoke_velnor(
            "{ path=/usr/bin/python3.12 ; argv[]=/usr/bin/python3.12 -c pass ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        ));
        assert!(command_properties_invoke_velnor(
            "{ path=/usr/bin/python3.12 ; argv[]=/usr/bin/python3.12 -c __import__(chr(111)+chr(115)).system(chr(47)+chr(111)+chr(112)+chr(116)+chr(47)+chr(118)+chr(101)+chr(108)+chr(110)+chr(111)+chr(114)+chr(45)+chr(114)+chr(117)+chr(110)+chr(110)+chr(101)+chr(114)+chr(32)+chr(100)+chr(97)+chr(101)+chr(109)+chr(111)+chr(110)) ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        ));
        assert!(command_properties_invoke_velnor(
            "{ path=/usr/bin/python3.12 ; argv[]=/usr/bin/python3.12 -cpass ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        ));
        assert!(command_properties_invoke_velnor(
            "{ path=/bin/sh ; argv[]=/bin/sh -ec true ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        ));
        assert!(!command_properties_invoke_velnor(
            "{ path=/usr/bin/env ; argv[]=/usr/bin/env /usr/bin/env python3.12 -c pass ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        ));
        assert!(command_properties_invoke_velnor(
            "{ path=/usr/bin/env ; argv[]=/usr/bin/env /usr/bin/env python3.12 -c __import__(chr(111)+chr(115)).system(chr(47)+chr(111)+chr(112)+chr(116)+chr(47)+chr(118)+chr(101)+chr(108)+chr(110)+chr(111)+chr(114)+chr(45)+chr(114)+chr(117)+chr(110)+chr(110)+chr(101)+chr(114)+chr(32)+chr(100)+chr(97)+chr(101)+chr(109)+chr(111)+chr(110)) ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        ));
        assert!(command_properties_invoke_velnor(
            "{ path=/opt/runner-copy ; argv[]=/opt/runner-copy --name alpha daemon ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        ));
        assert!(!command_properties_invoke_velnor(
            "{ path=/usr/sbin/avahi-daemon ; argv[]=/usr/sbin/avahi-daemon --no-drop-root ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }"
        ));
        assert!(process_argv_invokes_velnor(
            b"/bin/sh\0-c\0exec /opt/runner-copy daemon --name alpha\0"
        ));
        assert!(process_argv_invokes_velnor(
            b"/opt/runner-copy\0--name\0alpha\0--labels\0test\0--slots\02\0daemon\0"
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn physical_overlap_uses_inode_identity_across_bind_mount_ids() {
        let physical = DirectoryIdentity {
            device: 7,
            inode: 11,
            mount: crate::leftover_disk::FilesystemMountIdentity::LinuxMountId(2),
        };
        let bind_alias = DirectoryIdentity {
            mount: crate::leftover_disk::FilesystemMountIdentity::LinuxMountId(9),
            ..physical.clone()
        };
        let left = PhysicalDirectorySnapshot {
            root: Some(physical.clone()),
            locations: vec![PhysicalPathLocation {
                identity: physical,
                suffix: PathBuf::new(),
            }],
        };
        let right = PhysicalDirectorySnapshot {
            root: Some(bind_alias.clone()),
            locations: vec![PhysicalPathLocation {
                identity: bind_alias,
                suffix: PathBuf::new(),
            }],
        };

        assert!(physical_locations_overlap(&left, &right));
    }

    #[test]
    fn rejects_effective_dropins_and_escaped_root_or_trust_values() {
        let temp = test_tempdir().unwrap();
        let cache = temp.path().join("storage/cache/velnor/v1");
        let work = temp.path().join("work");
        let mut instance = instance(
            "custom",
            cache,
            work.clone(),
            temp.path().join("run"),
            "pool/a",
            Some(2),
        );
        instance.env_file = temp.path().join("custom.env");
        instance.environment.extend([
            ("VELNOR_NAME".to_owned(), "custom-runner".to_owned()),
            ("VELNOR_LABELS".to_owned(), "velnor,test".to_owned()),
            (
                "VELNOR_STORAGE_ROOT".to_owned(),
                instance.storage_root.display().to_string(),
            ),
            ("VELNOR_WORK_DIR".to_owned(), work.display().to_string()),
            ("VELNOR_SLOTS".to_owned(), "2".to_owned()),
            ("VELNOR_TRUST_SCOPE".to_owned(), "pool/a".to_owned()),
        ]);
        let valid = format!(
            "VELNOR_NAME=custom-runner\nVELNOR_LABELS=velnor,test\nVELNOR_WORK_DIR={}\nVELNOR_SLOTS=2\nVELNOR_TRUST_SCOPE=pool/a\n",
            work.display()
        );
        fs::write(&instance.env_file, &valid).unwrap();
        assert!(verify_root_environment_file(&instance).is_ok());
        assert!(verify_dropin_paths_property(&instance, "").is_ok());
        assert!(verify_dropin_paths_property(
            &instance,
            "/run/systemd/system/velnor-daemon@.service.d/override.conf"
        )
        .is_err());
        assert!(verify_dropin_paths_property(
            &instance,
            "/etc/systemd/system/velnor-daemon@.service.d/20-roots.conf\n/run/systemd/system/velnor-daemon@.service.d/90-reset.conf"
        )
        .is_err());

        let path = work.to_str().unwrap();
        let split = path.len() / 2;
        let continued = format!(
            "VELNOR_NAME=custom-runner\nVELNOR_LABELS=velnor,test\nVELNOR_WORK_DIR={}\\\n{}\nVELNOR_SLOTS=2\nVELNOR_TRUST_SCOPE=pool/a\n",
            &path[..split],
            &path[split..]
        );
        fs::write(&instance.env_file, continued).unwrap();
        assert!(verify_root_environment_file(&instance).is_ok());

        fs::write(
            &instance.env_file,
            format!(
                "VELNOR_NAME=custom-runner\nVELNOR_LABELS=velnor,test\nVELNOR_WORK_DIR=\"{}\"\nVELNOR_SLOTS=2\nVELNOR_TRUST_SCOPE=pool/a\n",
                work.display()
            ),
        )
        .unwrap();
        assert!(verify_root_environment_file(&instance).is_ok());

        fs::write(
            &instance.env_file,
            format!(
                "VELNOR_NAME=custom-runner\nVELNOR_LABELS=velnor,test\nVELNOR_WORK_DIR={}\\\n  _alternate\nVELNOR_SLOTS=2\nVELNOR_TRUST_SCOPE=pool/a\n",
                work.display()
            ),
        )
        .unwrap();
        assert!(verify_root_environment_file(&instance).is_err());

        fs::write(
            &instance.env_file,
            format!(
                "VELNOR_NAME=custom-runner\nVELNOR_LABELS=velnor,test\nVELNOR_WORK_DIR={}\nVELNOR_SLOTS=2\nVELNOR_TRUST_SCOPE=pool\\_a\n",
                work.display()
            ),
        )
        .unwrap();
        assert!(verify_root_environment_file(&instance).is_err());

        for invalid in [
            format!(
                "VELNOR_NAME=custom-runner\nVELNOR_LABELS=velnor,test\nVELNOR_WORK_DIR={}\\\\\nVELNOR_SLOTS=2\nVELNOR_TRUST_SCOPE=pool/a\n",
                work.display()
            ),
            format!(
                "# operator note \\\nVELNOR_NAME=custom-runner\nVELNOR_LABELS=velnor,test\nVELNOR_WORK_DIR={}\nVELNOR_SLOTS=2\nVELNOR_TRUST_SCOPE=pool/a\n",
                work.display()
            ),
            format!(
                "VELNOR_NAME=custom-runner\nVELNOR_LABELS=velnor,test\nVELNOR_WORK_DIR={}\\x2fother\nVELNOR_SLOTS=2\nVELNOR_TRUST_SCOPE=pool/a\n",
                work.display()
            ),
            format!(
                "VELNOR_NAME=custom-runner\nVELNOR_LABELS=velnor,test\nVELNOR_WORK_\\\nDIR={}\nVELNOR_SLOTS=2\nVELNOR_TRUST_SCOPE=pool/a\n",
                work.display()
            ),
            format!(
                "VELNOR_NAME=custom-runner\nVELNOR_LABELS=velnor,test\nVELNOR_WORK_DIR={}\nVELNOR_WORK_DIR={}_alternate\nVELNOR_SLOTS=2\nVELNOR_TRUST_SCOPE=pool/a\n",
                work.display(),
                work.display()
            ),
        ] {
            fs::write(&instance.env_file, invalid).unwrap();
            assert!(verify_root_environment_file(&instance).is_err());
        }
    }

    #[test]
    fn dotted_instance_environment_is_inventoried_and_secret_siblings_are_excluded() {
        let temp = test_tempdir().unwrap();
        let etc = temp.path().join("etc/velnor");
        mkdir(&etc);
        fs::write(etc.join("alpha.env"), "VELNOR_NAME=alpha\n").unwrap();
        fs::write(etc.join("alpha.beta.env"), "VELNOR_NAME=alpha.beta\n").unwrap();
        fs::write(
            etc.join("alpha.beta.secrets.env"),
            "GITHUB_TOKEN=ghp_secret\n",
        )
        .unwrap();
        let cache = temp.path().join("storage/cache/velnor/v1");
        let legacy = cache.join("scope/cargo");
        mkdir(&legacy);
        let marker = legacy.join("preserve-until-inventory-is-complete");
        fs::write(&marker, b"still owned by the host").unwrap();
        let alpha = instance(
            "alpha",
            cache,
            temp.path().join("work"),
            temp.path().join("run"),
            "scope",
            Some(1),
        );
        let dotted = instance(
            "alpha.beta",
            temp.path().join("other/cache/velnor/v1"),
            temp.path().join("other/work"),
            temp.path().join("other/run"),
            "scope",
            Some(1),
        );
        assert!(
            verify_instance_environment_inventory(&etc, &[alpha.clone(), dotted.clone()]).is_ok()
        );
        fs::write(
            etc.join("alpha.beta.secrets.env"),
            "VELNOR_WORK_DIR=/tmp/hidden-root\\\n/continued\n",
        )
        .unwrap();
        assert!(
            verify_instance_environment_inventory(&etc, &[alpha.clone(), dotted.clone()]).is_err()
        );
        fs::write(
            etc.join("alpha.beta.secrets.env"),
            "GITHUB_TOKEN=ghp_secret\n",
        )
        .unwrap();
        let mut alpha_for_secrets = alpha.clone();
        alpha_for_secrets.env_file = etc.join("alpha.env");
        fs::write(etc.join("alpha.secrets.env"), "GITHUB_TOKEN=ghp_secret\n").unwrap();
        assert!(verify_secret_environment_file(&alpha_for_secrets).is_ok());
        fs::write(
            etc.join("alpha.secrets.env"),
            "VELNOR_WORK_DIR=/tmp/hidden-root\\\n/continued\n",
        )
        .unwrap();
        assert!(verify_secret_environment_file(&alpha_for_secrets).is_err());
        assert!(verify_instance_environment_inventory(&etc, &[alpha.clone()]).is_err());
        let purge = purge_instances_with(
            || Ok(vec![alpha.clone()]),
            |instances| verify_instance_environment_inventory(&etc, instances),
            |_| Ok(()),
        );
        assert!(purge.is_err());
        assert_eq!(fs::read(marker).unwrap(), b"still owned by the host");
        assert!(validate_instance_unit_name(&dotted).is_ok());
    }

    #[test]
    fn rejects_manager_level_root_and_trust_overrides_without_exposing_values() {
        assert!(reject_manager_environment_overrides("PATH=/usr/bin\n").is_ok());
        let error = reject_manager_environment_overrides("VELNOR_STORAGE_ROOT=/sensitive/value\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("root or trust input"));
        assert!(!error.contains("/sensitive/value"));
    }

    #[test]
    fn systemd_inventory_matches_literal_template_instances_and_rejects_missing_configs() {
        let expected = BTreeSet::from([
            "velnor-daemon@alpha.service".to_owned(),
            "velnor-daemon@alpha.beta.service".to_owned(),
            "velnor-daemon.service".to_owned(),
        ]);
        let loaded = "velnor-daemon@alpha.service loaded inactive dead\nvelnor-daemon@alpha.beta.service loaded inactive dead\n";
        let files = "velnor-daemon@.service static -\nvelnor-daemon.service enabled enabled\nvelnor-daemon@alpha.service enabled enabled\nvelnor-daemon@alpha.beta.service enabled enabled\n";
        assert!(validate_systemd_unit_inventory(&expected, loaded, files).is_ok());

        let dotted = "velnor-daemon@alpha.beta.service loaded inactive dead\n";
        let expected_without_dotted = BTreeSet::from([
            "velnor-daemon@alpha.service".to_owned(),
            "velnor-daemon.service".to_owned(),
        ]);
        assert!(validate_systemd_unit_inventory(&expected_without_dotted, dotted, files).is_err());
        let enabled_without_env = "velnor-daemon@unconfigured.service enabled enabled\n";
        assert!(validate_systemd_unit_inventory(&expected, loaded, enabled_without_env).is_err());
        assert!(validate_systemd_unit_inventory(&BTreeSet::new(), "", files).is_err());

        let reserved_collision = "velnor-daemon@fleet-policy-audit.service disabled disabled\n";
        assert!(validate_systemd_unit_inventory(&expected, "", reserved_collision).is_err());
        assert!(validate_systemd_unit_inventory(&expected, reserved_collision, "").is_err());
    }

    #[test]
    fn dotted_doctor_template_units_share_the_instance_name_grammar() {
        assert_eq!(
            transaction_timer_service("velnor-doctor@alpha.beta.timer").as_deref(),
            Some("velnor-doctor@alpha.beta.service")
        );
        assert!(expected_transaction_oneshot_argv("velnor-doctor@alpha.beta.service").is_some());
        assert!(transaction_timer_service("velnor-doctor@../alpha.timer").is_none());
        assert!(expected_transaction_oneshot_argv("velnor-doctor@alpha@beta.service").is_none());
    }

    #[test]
    fn proves_every_unit_before_removing_any_instance_root() {
        let temp = test_tempdir().unwrap();
        let first_cache = temp.path().join("first/cache/velnor/v1");
        let second_cache = temp.path().join("second/cache/velnor/v1");
        let first = instance(
            "first",
            first_cache.clone(),
            temp.path().join("first/work"),
            temp.path().join("first/run"),
            "old_a",
            Some(1),
        );
        let second = instance(
            "second",
            second_cache.clone(),
            temp.path().join("second/work"),
            temp.path().join("second/run"),
            "old_b",
            Some(1),
        );
        seed_old_roots(&first, &["old_a"]);
        seed_old_roots(&second, &["old_b"]);
        let instances = [first, second];
        let result = purge_instances_with(
            || Ok(instances.to_vec()),
            |_| Ok(()),
            |instance| {
                if instance.instance == "second" {
                    bail!("unsupported effective command");
                }
                Ok(())
            },
        );
        assert!(result.is_err());
        assert!(first_cache.join("old_a/cargo").is_dir());
        assert!(second_cache.join("old_b/cargo").is_dir());
    }

    #[test]
    fn candidate_identity_snapshot_rejects_replacement_after_inventory() {
        let temp = test_tempdir().unwrap();
        let cache = temp.path().join("custom/cache/velnor/v1");
        let work = temp.path().join("custom/work");
        let instance = instance(
            "custom",
            cache.clone(),
            work,
            temp.path().join("run"),
            "old",
            Some(1),
        );
        seed_old_roots(&instance, &["old"]);
        let initial = plan_for_instance(&instance).unwrap();
        let candidate = cache.join("old/cargo");
        fs::rename(&candidate, cache.join("old/cargo-retained")).unwrap();
        mkdir(&candidate);
        let current = plan_for_instance(&instance).unwrap();
        assert!(validate_candidate_snapshots(&[initial], &[current]).is_err());
    }
}
