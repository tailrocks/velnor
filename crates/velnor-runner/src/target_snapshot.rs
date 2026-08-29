//! Automatic, compatible Cargo target snapshots (acceleration workstream 2).
//!
//! A *compatibility class* is the set of Rust builds whose incremental target
//! trees are interchangeable: same repository identity, trust scope, store
//! generation, host triple, toolchain, and workspace `RUSTFLAGS`. Within one
//! class every generation is a valid restore source for every job — Cargo's
//! own fingerprints invalidate stale units, so the store must not bucket by
//! job display name (identical jobs rebuilt everything from scratch), workflow
//! (cross-workflow reuse was forbidden for no compatibility reason), or exact
//! commit (an exact-revision match wiped a job-local target that carried
//! reusable dependency artifacts).
//!
//! Layout below the daemon targets store root:
//!
//! ```text
//! <targets>/<trust>/workspace-v5-compat/<sanitized-repo>/<compat-class>/<gen-id>/
//!     data/                          # the published target tree
//!     manifest.json                  # schema, branch/commit, provenance
//!     .velnor-target-complete-v1     # written before the generation is named
//! <compat-class>/current             # pointer to the newest successful gen-id
//! ```
//!
//! Provenance (workflow, job display name) is recorded in the manifest but is
//! never part of the class identity.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::acceleration::{AccelerationPolicy, DegradationLog, DegradationRecord};
use crate::job_message::AgentJobRequestMessage;

/// Bump whenever target-store identity semantics change. `workspace-v5-compat`
/// introduced compatibility-class bucketing; `workspace-v4-success-only` and
/// older stores used job-name bucketing with exact-revision matching — a
/// different identity model, so they are never read or migrated. GC reclaims
/// them as orphans.
pub const CARGO_TARGET_GENERATION: &str = "workspace-v5-compat";

/// Rust toolchain the job image bakes via mise (`docker/job-mise.toml`). Part
/// of the toolchain digest so an image bump that changes rustc/cargo yields a
/// new compatibility class instead of restoring foreign fingerprints.
/// MUST match the `rust` pin in docker/job-mise.toml (test-enforced).
pub const JOB_IMAGE_RUST_VERSION: &str = "1.97.1";

/// Marker proving a generation directory holds a fully published target tree.
pub const TARGET_COMPLETE_MARKER: &str = ".velnor-target-complete-v1";

/// Pointer file inside a class directory naming the newest successful gen-id.
pub const CURRENT_POINTER_FILE: &str = "current";

/// Per-generation manifest file.
pub const MANIFEST_FILE: &str = "manifest.json";

/// Default branch of the fallback dimension in candidate selection.
pub const DEFAULT_BRANCH: &str = "main";

/// Manifest schema version. Readers reject other values as foreign stores.
pub const MANIFEST_SCHEMA: u32 = 1;

/// The identity a compatibility class is derived from: repository, trust
/// scope, store generation, and the runner's host triple. The toolchain and
/// flag dimensions come from probing the checked-out workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetCompatIdentity {
    pub repository: String,
    pub trust_scope: String,
    pub host_triple: String,
}

/// Runner (host) Rust target triple. Snapshots are host-specific: a target
/// tree built for one triple must never restore on another.
#[must_use]
pub fn runner_host_triple() -> String {
    if cfg!(target_os = "linux") {
        if cfg!(target_arch = "x86_64") {
            "x86_64-unknown-linux-gnu".to_string()
        } else {
            "aarch64-unknown-linux-gnu".to_string()
        }
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "x86_64") {
            "x86_64-apple-darwin".to_string()
        } else {
            "aarch64-apple-darwin".to_string()
        }
    } else {
        format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
    }
}

/// The compatibility class of one job's target tree: a SHA-256 digest over
/// repository identity, trust scope, store generation, host triple, the
/// workspace toolchain digest (rust-toolchain files, mise.lock rust/cargo
/// pins, image rust version — which pins the cargo version), and the
/// workspace `RUSTFLAGS`/`RUSTDOCFLAGS`. Deliberately NOT over: commit SHA,
/// job display name, workflow name, profile, or features — a snapshot may
/// legitimately hold several profiles; Cargo invalidates units itself.
#[must_use]
pub fn cargo_target_compat_class(identity: &TargetCompatIdentity, workspace: &Path) -> String {
    let (rustflags, rustdocflags) = workspace_cargo_flags(workspace);
    let mut digest = Sha256::new();
    digest.update(b"velnor-target-compat-v1\n");
    digest.update(format!("repository={}\n", identity.repository));
    digest.update(format!("trust={}\n", identity.trust_scope));
    digest.update(format!("generation={CARGO_TARGET_GENERATION}\n"));
    digest.update(format!("host={}\n", identity.host_triple));
    digest.update(format!("toolchain={}\n", toolchain_digest(workspace)));
    digest.update(format!("rustflags={}\n", rustflags.unwrap_or_default()));
    digest.update(format!(
        "rustdocflags={}\n",
        rustdocflags.unwrap_or_default()
    ));
    hex(&digest.finalize())
}

/// Digest over every input that changes what rustc/cargo produce: the
/// workspace rust-toolchain files, the mise.lock rust/cargo pins, and the
/// job image's baked rust version (the cargo version comes from the same
/// sources, so it is covered by the same digest).
fn toolchain_digest(workspace: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(b"velnor-toolchain-v1\n");
    for name in ["rust-toolchain.toml", "rust-toolchain"] {
        match fs::read(workspace.join(name)) {
            Ok(bytes) => {
                digest.update(name);
                digest.update(b"=");
                digest.update(&bytes);
                digest.update(b"\n");
            }
            Err(_) => {
                digest.update(name);
                digest.update(b"=absent\n");
            }
        }
    }
    match mise_lock_rust_pins(workspace) {
        Some(pins) => digest.update(format!("mise={pins}\n")),
        None => digest.update("mise=absent\n"),
    }
    digest.update(format!("image-rust={JOB_IMAGE_RUST_VERSION}\n"));
    hex(&digest.finalize())
}

/// `rust`/`cargo` pins from the workspace `mise.lock`, when parseable.
/// Both the array-of-tables lock form (`[[tools.rust]] version = "..."`) and
/// the plain string form are accepted; anything else reads as absent.
fn mise_lock_rust_pins(workspace: &Path) -> Option<String> {
    let content = fs::read_to_string(workspace.join("mise.lock")).ok()?;
    let value = toml::from_str::<toml::Value>(&content).ok()?;
    let tools = value.get("tools")?;
    let mut pins = String::new();
    for key in ["rust", "cargo"] {
        let Some(tool) = tools.get(key) else {
            continue;
        };
        if let Some(entries) = tool.as_array() {
            for entry in entries {
                if let Some(version) = entry.get("version").and_then(toml::Value::as_str) {
                    pins.push_str(&format!("{key}={version};"));
                }
            }
        } else if let Some(version) = tool.as_str() {
            pins.push_str(&format!("{key}={version};"));
        }
    }
    if pins.is_empty() {
        None
    } else {
        Some(pins)
    }
}

/// `RUSTFLAGS` / `RUSTDOCFLAGS` from the workspace `.cargo/config.toml`.
/// Array values are joined with spaces the way cargo renders them.
fn workspace_cargo_flags(workspace: &Path) -> (Option<String>, Option<String>) {
    let Ok(content) = fs::read_to_string(workspace.join(".cargo").join("config.toml")) else {
        return (None, None);
    };
    let Ok(value) = toml::from_str::<toml::Value>(&content) else {
        return (None, None);
    };
    let build = value.get("build");
    let render = |name: &str| -> Option<String> {
        let raw = build?.get(name)?;
        if let Some(flags) = raw.as_str() {
            return Some(flags.trim().to_string()).filter(|flags| !flags.is_empty());
        }
        let joined = raw
            .as_array()?
            .iter()
            .filter_map(toml::Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        Some(joined).filter(|flags| !flags.is_empty())
    };
    (render("rustflags"), render("rustdocflags"))
}

/// Monotonic generation id: millisecond timestamp, then pid and an in-process
/// counter so two publishes in one millisecond stay distinct and ordered.
fn next_generation_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!(
        "g{millis:019}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

/// Provenance recorded per generation. Interesting for operators, irrelevant
/// to identity: two jobs with different display names share a class.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSnapshotProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_display_name: Option<String>,
}

/// One generation's manifest: what it is, where it came from, when it was
/// published. `schema` rejects foreign manifests; `compat_class` lets a
/// restored generation prove it landed in the right class directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetGenerationManifest {
    pub schema: u32,
    pub compat_class: String,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub created_unix: u64,
    #[serde(default)]
    pub provenance: TargetSnapshotProvenance,
}

impl TargetGenerationManifest {
    fn write_to(&self, directory: &Path) -> Result<()> {
        let body = serde_json::to_string(self).context("serialize target generation manifest")?;
        fs::write(directory.join(MANIFEST_FILE), format!("{body}\n"))
            .with_context(|| format!("write {}", directory.join(MANIFEST_FILE).display()))
    }

    fn read_from(directory: &Path) -> Option<Self> {
        let body = fs::read_to_string(directory.join(MANIFEST_FILE)).ok()?;
        let manifest: Self = serde_json::from_str(body.trim()).ok()?;
        (manifest.schema == MANIFEST_SCHEMA).then_some(manifest)
    }
}

/// One generation considered during candidate selection, with the dimension
/// reason it was accepted or rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TargetSnapshotCandidate {
    pub generation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub created_unix: u64,
    pub accepted: bool,
    pub reason: String,
}

/// How the job's target directory started life.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TargetMaterialization {
    Restored { generation: String },
    Cold { reason: String },
}

/// The full snapshot decision for one job: which compatibility class was
/// computed, which generation was restored (or why the job ran cold), and
/// every candidate examined with its dimension reason. Attached to the job
/// execution state; the acceleration report serializes it verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TargetSnapshotDecision {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_generation: Option<String>,
    pub class_digest: String,
    pub materialization: TargetMaterialization,
    pub candidates: Vec<TargetSnapshotCandidate>,
}

impl TargetSnapshotDecision {
    /// Cold decision after an unusable store: the digest still names the
    /// class the job would have joined.
    #[must_use]
    pub fn cold(class_digest: String, reason: String) -> Self {
        Self {
            selected_generation: None,
            class_digest,
            materialization: TargetMaterialization::Cold { reason },
            candidates: Vec::new(),
        }
    }

    /// JSON body for the job acceleration report. Infallible.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Whether the acceleration policy activates target snapshots for one job,
/// plus the degradation a disabled policy owes (Rust jobs only: a job that
/// never compiles loses nothing).
pub struct TargetSnapshotActivation {
    pub active: bool,
    pub degradation: DegradationLog,
}

#[must_use]
pub fn resolve_activation(
    policy: &AccelerationPolicy,
    job: &AgentJobRequestMessage,
) -> TargetSnapshotActivation {
    use velnor_model::TargetPersistenceChoice;
    match policy.target_persistence {
        TargetPersistenceChoice::Auto | TargetPersistenceChoice::On => TargetSnapshotActivation {
            active: true,
            degradation: DegradationLog::default(),
        },
        TargetPersistenceChoice::Off => {
            let mut degradation = DegradationLog::default();
            if crate::compiler_cache::job_compiles_rust(job) {
                degradation.record(DegradationRecord::target_persistence_disabled(
                    "policy [acceleration] target_persistence = \"off\"",
                ));
            }
            TargetSnapshotActivation {
                active: false,
                degradation,
            }
        }
    }
}

/// Class directory of a store parent: `<store>/<compat-class>`.
#[must_use]
pub fn class_directory(store: &Path, class_digest: &str) -> PathBuf {
    store.join(class_digest)
}

/// Publish one generation: stage the completed target tree under a hidden
/// staging directory inside the class dir, name it, then flip the `current`
/// pointer atomically (tmp + rename). The pointer flip is the publish point:
/// callers invoke this only after every step succeeded, so failed or
/// cancelled jobs never flip it.
pub fn publish_generation(
    class_dir: &Path,
    target: &Path,
    manifest: &TargetGenerationManifest,
) -> Result<String> {
    fs::create_dir_all(class_dir)
        .with_context(|| format!("create target class dir {}", class_dir.display()))?;
    let generation = next_generation_id();
    let staging = class_dir.join(format!(".staging-{generation}"));
    fs::remove_dir_all(&staging).ok();
    let payload = staging.join("data");
    fs::create_dir_all(&payload)
        .with_context(|| format!("create target staging dir {}", payload.display()))?;
    if let Err(error) = crate::executor::copy_dir_contents(target, &payload) {
        fs::remove_dir_all(&staging).ok();
        return Err(error).context("stage target generation");
    }
    fs::write(staging.join(TARGET_COMPLETE_MARKER), b"complete\n")
        .with_context(|| format!("write {}", staging.join(TARGET_COMPLETE_MARKER).display()))?;
    if let Err(error) = manifest.write_to(&staging) {
        fs::remove_dir_all(&staging).ok();
        return Err(error);
    }

    let _lock = crate::cache::CacheEntryLock::exclusive(class_dir)?;
    let generation_dir = class_dir.join(&generation);
    if let Err(error) = fs::rename(&staging, &generation_dir) {
        fs::remove_dir_all(&staging).ok();
        return Err(error)
            .with_context(|| format!("publish target generation {}", generation_dir.display()));
    }
    write_current_pointer(class_dir, &generation)?;
    Ok(generation)
}

/// Atomically point `current` at a generation (tmp file + rename).
fn write_current_pointer(class_dir: &Path, generation: &str) -> Result<()> {
    let temporary = class_dir.join(format!(".current.{}.tmp", std::process::id()));
    fs::write(&temporary, format!("{generation}\n"))
        .with_context(|| format!("write pointer temp {}", temporary.display()))?;
    if let Err(error) = fs::rename(&temporary, class_dir.join(CURRENT_POINTER_FILE)) {
        fs::remove_file(&temporary).ok();
        return Err(error).with_context(|| {
            format!(
                "flip current pointer {}",
                class_dir.join(CURRENT_POINTER_FILE).display()
            )
        });
    }
    Ok(())
}

/// The generation directory a class's `current` pointer names, when that
/// directory exists. A stale pointer (generation already evicted) is `None`.
#[must_use]
pub fn pointer_referenced_generation(class_dir: &Path) -> Option<PathBuf> {
    let pointer = fs::read_to_string(class_dir.join(CURRENT_POINTER_FILE)).ok()?;
    let generation = pointer.trim();
    if generation.is_empty() {
        return None;
    }
    let directory = class_dir.join(generation);
    directory.is_dir().then_some(directory)
}

/// Select the generation to restore. Preference chain: newest successful
/// generation from the job's own branch, then the newest from the default
/// branch, then the newest from any branch, else cold. Every generation
/// directory in the class is recorded as a candidate with its dimension
/// reason; the `current` pointer breaks ties in favour of the generation the
/// last successful job published.
#[must_use]
pub fn select_snapshot(
    class_dir: &Path,
    class_digest: &str,
    job_branch: Option<&str>,
) -> TargetSnapshotDecision {
    let pointer = fs::read_to_string(class_dir.join(CURRENT_POINTER_FILE))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let is_pointer = |name: &str| pointer.as_deref() == Some(name);

    let mut complete: Vec<(String, TargetGenerationManifest)> = Vec::new();
    let mut broken: Vec<TargetSnapshotCandidate> = Vec::new();
    if let Ok(entries) = fs::read_dir(class_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let directory = class_dir.join(&name);
            match TargetGenerationManifest::read_from(&directory) {
                Some(manifest)
                    if manifest.compat_class == class_digest && generation_complete(&directory) =>
                {
                    complete.push((name, manifest));
                }
                _ => broken.push(TargetSnapshotCandidate {
                    generation: name,
                    branch: None,
                    commit: None,
                    created_unix: 0,
                    accepted: false,
                    reason: "incomplete generation (missing manifest, marker, or data)".to_string(),
                }),
            }
        }
    }
    complete.sort_by(|(left_name, left), (right_name, right)| {
        right
            .created_unix
            .cmp(&left.created_unix)
            .then_with(|| is_pointer(right_name).cmp(&is_pointer(left_name)))
            .then_with(|| right_name.cmp(left_name))
    });

    // Preference chain dimensions: the job's own branch first, then the
    // default branch (skipped when they are the same branch).
    let mut dimensions: Vec<(String, &'static str)> = Vec::new();
    if let Some(branch) = job_branch.filter(|branch| !branch.is_empty()) {
        dimensions.push((branch.to_string(), "same-branch newest"));
    }
    if job_branch != Some(DEFAULT_BRANCH) {
        dimensions.push((DEFAULT_BRANCH.to_string(), "default-branch newest fallback"));
    }
    let mut selected: Option<(String, TargetGenerationManifest, &'static str)> = None;
    for (branch, label) in &dimensions {
        if let Some(found) = complete
            .iter()
            .find(|(_, manifest)| manifest.branch.as_deref() == Some(branch.as_str()))
        {
            selected = Some((found.0.clone(), found.1.clone(), label));
            break;
        }
    }
    if selected.is_none() {
        if let Some(found) = complete.first() {
            selected = Some((
                found.0.clone(),
                found.1.clone(),
                "newest any-branch fallback",
            ));
        }
    }

    let chosen_branch = selected
        .as_ref()
        .and_then(|(_, manifest, _)| manifest.branch.clone());
    let mut candidates: Vec<TargetSnapshotCandidate> = Vec::new();
    for (generation, manifest) in &complete {
        let accepted = selected
            .as_ref()
            .is_some_and(|(name, _, _)| name == generation);
        let reason = if accepted {
            selected
                .as_ref()
                .map(|(_, _, label)| label.to_string())
                .unwrap_or_default()
        } else if manifest.branch.is_some() && manifest.branch == chosen_branch {
            format!(
                "superseded by newer generation of branch '{}'",
                chosen_branch.as_deref().unwrap_or_default()
            )
        } else if let Some(branch) = &manifest.branch {
            format!(
                "branch '{branch}' outside preference chain for job branch '{}'",
                job_branch.unwrap_or("unknown")
            )
        } else {
            "generation without branch metadata lost the chain".to_string()
        };
        candidates.push(TargetSnapshotCandidate {
            generation: generation.clone(),
            branch: manifest.branch.clone(),
            commit: manifest.commit.clone(),
            created_unix: manifest.created_unix,
            accepted,
            reason,
        });
    }
    let broken_count = broken.len();
    candidates.extend(broken);

    let selected_generation = selected.as_ref().map(|(name, _, _)| name.clone());
    let materialization = match &selected_generation {
        Some(generation) => TargetMaterialization::Restored {
            generation: generation.clone(),
        },
        None => TargetMaterialization::Cold {
            reason: if complete.is_empty() && broken_count == 0 {
                "no compatible generation published yet".to_string()
            } else if complete.is_empty() {
                "no complete compatible generation".to_string()
            } else {
                "no candidate matched the preference chain".to_string()
            },
        },
    };
    TargetSnapshotDecision {
        selected_generation,
        class_digest: class_digest.to_string(),
        materialization,
        candidates,
    }
}

/// A generation directory is restorable when it carries the completion
/// marker and a data payload.
fn generation_complete(directory: &Path) -> bool {
    directory.join(TARGET_COMPLETE_MARKER).is_file() && directory.join("data").is_dir()
}

/// Normalize a GitHub ref to a branch label: `refs/heads/x` → `x`,
/// `refs/pull/N/merge` → `pull/N/merge`, `refs/tags/v1` → `tags/v1`.
#[must_use]
pub fn normalize_branch(reference: &str) -> Option<String> {
    let reference = reference.trim();
    let reference = reference.strip_prefix("refs/").unwrap_or(reference);
    let reference = reference.strip_prefix("heads/").unwrap_or(reference);
    Some(reference.to_string()).filter(|reference| !reference.is_empty())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> TargetCompatIdentity {
        TargetCompatIdentity {
            repository: "tailrocks/some-repo".to_string(),
            trust_scope: "trusted".to_string(),
            host_triple: runner_host_triple(),
        }
    }

    fn workspace(root: &Path) -> PathBuf {
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        workspace
    }

    fn publish(class_dir: &Path, target: &Path, branch: Option<&str>, created_unix: u64) -> String {
        let manifest = TargetGenerationManifest {
            schema: MANIFEST_SCHEMA,
            compat_class: "class".to_string(),
            repository: "tailrocks/some-repo".to_string(),
            branch: branch.map(ToOwned::to_owned),
            commit: Some("deadbeef".to_string()),
            created_unix,
            provenance: TargetSnapshotProvenance {
                workflow: Some("CI".to_string()),
                job_display_name: Some("Rust / test (ubuntu)".to_string()),
            },
        };
        publish_generation(class_dir, target, &manifest).unwrap()
    }

    #[test]
    fn class_is_stable_across_commits_job_names_and_workflows() {
        let root = std::env::temp_dir().join(format!("velnor-class-{}", uuid::Uuid::new_v4()));
        let workspace = workspace(&root);
        fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();

        let first = cargo_target_compat_class(&identity(), &workspace);
        let second = cargo_target_compat_class(&identity(), &workspace);
        assert_eq!(first, second);
        assert_eq!(first.len(), 64, "sha-256 hex digest");

        // Same workspace under a different commit/job/workflow: identity
        // inputs unchanged, class unchanged.
        fs::write(workspace.join("src"), "new commit content\n").unwrap();
        assert_eq!(first, cargo_target_compat_class(&identity(), &workspace));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn toolchain_file_change_selects_a_new_class() {
        let root = std::env::temp_dir().join(format!("velnor-class-{}", uuid::Uuid::new_v4()));
        let workspace = workspace(&root);
        fs::write(
            workspace.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.98.0\"\n",
        )
        .unwrap();
        let before = cargo_target_compat_class(&identity(), &workspace);
        fs::write(
            workspace.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.99.0\"\n",
        )
        .unwrap();
        let after = cargo_target_compat_class(&identity(), &workspace);
        assert_ne!(before, after);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mise_lock_rust_pin_and_flags_change_the_class() {
        let root = std::env::temp_dir().join(format!("velnor-class-{}", uuid::Uuid::new_v4()));
        let workspace = workspace(&root);
        let before = cargo_target_compat_class(&identity(), &workspace);

        fs::write(
            workspace.join("mise.lock"),
            "[[tools.rust]]\nversion = \"1.98.0\"\nbackend = \"core:rust\"\n",
        )
        .unwrap();
        let mise = cargo_target_compat_class(&identity(), &workspace);
        assert_ne!(before, mise);

        fs::create_dir_all(workspace.join(".cargo")).unwrap();
        fs::write(
            workspace.join(".cargo/config.toml"),
            "[build]\nrustflags = [\"-D\", \"warnings\"]\n",
        )
        .unwrap();
        assert_ne!(mise, cargo_target_compat_class(&identity(), &workspace));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn job_image_rust_version_matches_the_baked_mise_pin() {
        let manifest_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docker");
        let mise = fs::read_to_string(format!("{manifest_dir}/job-mise.toml")).unwrap();
        assert!(
            mise.contains(&format!("rust = {{ version = \"{JOB_IMAGE_RUST_VERSION}\"")),
            "JOB_IMAGE_RUST_VERSION must match docker/job-mise.toml"
        );
    }

    #[test]
    fn publish_flips_the_pointer_and_selection_prefers_same_branch() {
        let root = std::env::temp_dir().join(format!("velnor-snap-{}", uuid::Uuid::new_v4()));
        let class_dir = root.join("class");
        let target = root.join("target");
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::write(target.join("debug/seed"), "warm\n").unwrap();

        let main_new = publish(&class_dir, &target, Some("main"), 200);
        let main_old = publish(&class_dir, &target, Some("main"), 100);
        let feature = publish(&class_dir, &target, Some("feature/x"), 150);
        assert_eq!(
            pointer_referenced_generation(&class_dir),
            Some(class_dir.join(&feature)),
            "pointer names the newest successful generation"
        );

        let decision = select_snapshot(&class_dir, "class", Some("feature/x"));
        assert_eq!(
            decision.selected_generation.as_deref(),
            Some(feature.as_str())
        );
        assert_eq!(decision.candidates.len(), 3, "{:?}", decision.candidates);
        assert!(decision
            .candidates
            .iter()
            .any(|candidate| candidate.accepted && candidate.reason == "same-branch newest"));

        let decision = select_snapshot(&class_dir, "class", Some("main"));
        assert_eq!(
            decision.selected_generation.as_deref(),
            Some(main_new.as_str())
        );
        assert!(decision
            .candidates
            .iter()
            .any(|candidate| !candidate.accepted
                && candidate.generation == main_old
                && candidate.reason.contains("superseded")));

        // Branch with no snapshot of its own falls back to the default branch.
        let decision = select_snapshot(&class_dir, "class", Some("other"));
        assert_eq!(
            decision.selected_generation.as_deref(),
            Some(main_new.as_str())
        );
        assert!(decision
            .candidates
            .iter()
            .any(|candidate| candidate.accepted
                && candidate.reason == "default-branch newest fallback"));

        // Unknown job branch still restores something: newest any branch.
        let decision = select_snapshot(&class_dir, "class", None);
        assert!(matches!(
            decision.materialization,
            TargetMaterialization::Restored { .. }
        ));
        assert!(decision.to_json().contains("class_digest"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incomplete_generations_are_rejected_but_recorded() {
        let root = std::env::temp_dir().join(format!("velnor-snap-{}", uuid::Uuid::new_v4()));
        let class_dir = root.join("class");
        let staging = class_dir.join(".staging-partial");
        fs::create_dir_all(staging.join("data")).unwrap();
        let orphan = class_dir.join("g0001-crashed");
        fs::create_dir_all(orphan.join("data")).unwrap();

        let decision = select_snapshot(&class_dir, "class", Some("main"));
        assert_eq!(decision.selected_generation, None);
        assert!(matches!(
            decision.materialization,
            TargetMaterialization::Cold { .. }
        ));
        assert_eq!(decision.candidates.len(), 1, "{:?}", decision.candidates);
        assert_eq!(decision.candidates[0].generation, "g0001-crashed");
        assert!(decision.candidates[0].reason.contains("incomplete"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_pointer_reads_as_none() {
        let root = std::env::temp_dir().join(format!("velnor-snap-{}", uuid::Uuid::new_v4()));
        let class_dir = root.join("class");
        fs::create_dir_all(&class_dir).unwrap();
        fs::write(class_dir.join(CURRENT_POINTER_FILE), "g0000-evicted\n").unwrap();
        assert_eq!(pointer_referenced_generation(&class_dir), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalize_branch_strips_known_ref_prefixes() {
        assert_eq!(
            normalize_branch("refs/heads/feat/x").as_deref(),
            Some("feat/x")
        );
        assert_eq!(
            normalize_branch("refs/pull/42/merge").as_deref(),
            Some("pull/42/merge")
        );
        assert_eq!(normalize_branch("refs/tags/v1").as_deref(), Some("tags/v1"));
        assert_eq!(normalize_branch("main").as_deref(), Some("main"));
        assert_eq!(normalize_branch(""), None);
    }

    #[test]
    fn generation_ids_are_monotonic_and_unique() {
        let first = next_generation_id();
        let second = next_generation_id();
        assert_ne!(first, second);
        assert!(second > first, "ids sort monotonically: {first} {second}");
        assert!(first.starts_with('g'));
    }

    #[test]
    fn off_policy_degrades_only_rust_jobs() {
        let mut policy = AccelerationPolicy::maximum();
        policy.target_persistence = velnor_model::TargetPersistenceChoice::Off;
        let rust_job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "Rust",
            "requestId": 1,
            "steps": [{ "reference": { "type": "Script" }, "inputs": { "script": "cargo build" } }]
        }))
        .unwrap();
        let js_job: AgentJobRequestMessage = serde_json::from_value(serde_json::json!({
            "messageType": "PipelineAgentJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "JS",
            "requestId": 1,
            "steps": [{ "reference": { "type": "Script" }, "inputs": { "script": "npm test" } }]
        }))
        .unwrap();

        let rust = resolve_activation(&policy, &rust_job);
        assert!(!rust.active);
        let records = rust.degradation.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].feature, "acceleration.target_persistence");

        let js = resolve_activation(&policy, &js_job);
        assert!(!js.active);
        assert!(js.degradation.is_empty(), "nothing lost, nothing owed");

        let auto = resolve_activation(&AccelerationPolicy::maximum(), &rust_job);
        assert!(auto.active);
        assert!(auto.degradation.is_empty());
    }
}
