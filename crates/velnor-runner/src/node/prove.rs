//! Observed Ready preconditions. Controller never stamps these true.
//!
//! Routing is valid only when on-disk **evidence** equals **desired policy**
//! for group, selected repositories, labels, and trust scope. A boolean
//! `{valid: true, group_valid: true}` file, a URL-only file, or any empty
//! field is invalid (August 24 class: registration without repo access).

use std::path::{Path, PathBuf};
use std::process::Child;

use serde::{Deserialize, Serialize};

/// On-disk routing observation. Missing or invalid file is not valid routing.
pub const ROUTING_FILE: &str = "routing.json";
/// Host-local executor proof written by a real preflight, never by daemon startup.
pub const EXECUTOR_OK: &str = "executor.ok";
/// Transitional host Docker socket. Presence is executor proof.
pub const HOST_DOCKER_SOCKET: &str = "/var/run/docker.sock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RoutingFields {
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub selected_repositories: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub trust_scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RoutingDocument {
    #[serde(default)]
    pub evidence: RoutingFields,
    #[serde(default)]
    pub policy: RoutingFields,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingObservation {
    pub valid: bool,
    pub group_valid: bool,
}

impl RoutingObservation {
    #[must_use]
    pub const fn invalid() -> Self {
        Self {
            valid: false,
            group_valid: false,
        }
    }
}

/// Read `routing.json`. Boolean stamps and URL-only files are invalid.
#[must_use]
pub fn observe_routing(state_dir: &Path) -> RoutingObservation {
    let Ok(bytes) = std::fs::read(state_dir.join(ROUTING_FILE)) else {
        return RoutingObservation::invalid();
    };
    let Ok(document) = serde_json::from_slice::<RoutingDocument>(&bytes) else {
        return RoutingObservation::invalid();
    };
    observe_document(&document)
}

#[must_use]
pub fn observe_document(document: &RoutingDocument) -> RoutingObservation {
    if !fields_complete(&document.evidence) || !fields_complete(&document.policy) {
        return RoutingObservation::invalid();
    }
    let group_valid = document.evidence.group == document.policy.group;
    let valid = group_valid && normalized(&document.evidence) == normalized(&document.policy);
    RoutingObservation { valid, group_valid }
}

/// Executor is proven by a preflight `executor.ok` file or a live host Docker socket.
#[must_use]
pub fn observe_executor(state_dir: &Path) -> bool {
    state_dir.join(EXECUTOR_OK).is_file() || Path::new(HOST_DOCKER_SOCKET).exists()
}

/// Session is live when the slot child is running or its journal pid still exists.
#[must_use]
pub fn observe_session(child: Option<&mut Child>, pid: Option<u32>) -> bool {
    if let Some(child) = child {
        if child.try_wait().ok().flatten().is_none() {
            return true;
        }
    }
    pid.is_some_and(pid_is_alive)
}

/// SIGNAL 0 existence check. Does not deliver a signal.
#[must_use]
pub fn pid_is_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) only tests whether `pid` exists.
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0
}

/// Persist evidence and desired policy. Never a boolean Ready stamp.
///
/// # Errors
/// Directory or write failures.
pub fn write_routing_document(
    state_dir: &Path,
    evidence: RoutingFields,
    policy: RoutingFields,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join(ROUTING_FILE);
    let body = serde_json::to_vec_pretty(&RoutingDocument { evidence, policy })?;
    std::fs::write(&path, body)?;
    Ok(path)
}

/// Record a preflight executor proof file. Daemon startup must not call this.
///
/// # Errors
/// Directory or write failures.
pub fn write_executor_ok(state_dir: &Path) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join(EXECUTOR_OK);
    std::fs::write(&path, b"ok\n")?;
    Ok(path)
}

fn fields_complete(fields: &RoutingFields) -> bool {
    !fields.group.is_empty()
        && !fields.selected_repositories.is_empty()
        && fields
            .selected_repositories
            .iter()
            .all(|repo| !repo.is_empty())
        && !fields.labels.is_empty()
        && fields.labels.iter().all(|label| !label.is_empty())
        && !fields.trust_scope.is_empty()
}

fn normalized(fields: &RoutingFields) -> RoutingFields {
    let mut selected_repositories = fields.selected_repositories.clone();
    selected_repositories.sort();
    let mut labels = fields.labels.clone();
    labels.sort();
    RoutingFields {
        group: fields.group.clone(),
        selected_repositories,
        labels,
        trust_scope: fields.trust_scope.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "velnor-prove-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn matching_fields() -> RoutingFields {
        RoutingFields {
            group: "velnor".into(),
            selected_repositories: vec!["tailrocks/velnor".into()],
            labels: vec!["velnor".into()],
            trust_scope: "trusted".into(),
        }
    }

    #[test]
    fn missing_routing_file_is_invalid() {
        let dir = tmp("missing");
        assert_eq!(observe_routing(&dir), RoutingObservation::invalid());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn boolean_stamp_is_invalid() {
        let dir = tmp("bool");
        std::fs::write(
            dir.join(ROUTING_FILE),
            br#"{"valid":true,"group_valid":true}"#,
        )
        .unwrap();
        assert_eq!(observe_routing(&dir), RoutingObservation::invalid());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn url_only_json_is_invalid() {
        let dir = tmp("url");
        std::fs::write(
            dir.join(ROUTING_FILE),
            br#"{"url":"https://github.com/o/r"}"#,
        )
        .unwrap();
        assert_eq!(observe_routing(&dir), RoutingObservation::invalid());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn matching_evidence_and_policy_is_valid() {
        let dir = tmp("match");
        let fields = matching_fields();
        write_routing_document(&dir, fields.clone(), fields).unwrap();
        assert_eq!(
            observe_routing(&dir),
            RoutingObservation {
                valid: true,
                group_valid: true
            }
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn repo_mismatch_is_invalid() {
        let dir = tmp("mismatch");
        let evidence = matching_fields();
        let mut policy = matching_fields();
        policy.selected_repositories = vec!["other/repo".into()];
        write_routing_document(&dir, evidence, policy).unwrap();
        let observed = observe_routing(&dir);
        assert!(!observed.valid);
        assert!(observed.group_valid);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn executor_ok_file_is_proof() {
        let dir = tmp("exec");
        assert!(!dir.join(EXECUTOR_OK).exists());
        write_executor_ok(&dir).unwrap();
        assert!(observe_executor(&dir));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn empty_labels_are_invalid() {
        let dir = tmp("labels");
        let mut fields = matching_fields();
        fields.labels.clear();
        write_routing_document(&dir, fields.clone(), fields).unwrap();
        assert_eq!(observe_routing(&dir), RoutingObservation::invalid());
        std::fs::remove_dir_all(dir).ok();
    }
}
