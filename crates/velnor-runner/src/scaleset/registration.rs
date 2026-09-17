//! GitHub registration adapters: scope/group/set registration + reconciliation.
//!
//! Before the first poll the daemon proves the scale set it will serve
//! exists and matches the declared shape: resolve the runner group (by
//! pinned ID or by name — never a silent default), then adopt the set by
//! pinned ID or get-or-create it by name, then reconcile label drift with
//! a bounded `PATCH`. Identity drift (wrong group, wrong name on a pinned
//! ID) fails closed: moving a set across groups or renaming it is an
//! operator decision, never an automatic one.
//!
//! This module never deletes a scale set. Decommission is explicit and
//! out of band; a normal restart only re-adopts.

use anyhow::{Context, Result};
use velnor_model::{RunnerScaleSet, RunnerSetting, ScaleSetLabel};

use crate::scaleset::{ScaleSetClient, ScaleSetError, ScaleSetFault};

/// Declared registration shape from the daemon's scale-set config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationPlan {
    /// Pin the runner group by ID (wins over `group_name` when both are set).
    pub group_id: Option<i32>,
    /// Resolve the runner group by name. Fails closed on zero/many matches.
    pub group_name: Option<String>,
    /// Adopt this set ID and verify it (never auto-created when missing).
    pub set_id: Option<i32>,
    /// Get-or-create the set under this name (needs `set_id == None`).
    pub set_name: Option<String>,
    /// Desired label names; reconciled onto the set when drifted.
    pub labels: Vec<String>,
}

impl RegistrationPlan {
    fn validate(&self) -> Result<()> {
        if self.group_id.is_none() && self.group_name.as_ref().is_none_or(String::is_empty) {
            anyhow::bail!("scale-set registration needs a runner group ID or name");
        }
        match (self.set_id, self.set_name.as_ref()) {
            (Some(_), Some(_)) => {
                anyhow::bail!(
                    "scale-set registration takes either a set ID or a set name, not both"
                );
            }
            (None, None) => {
                anyhow::bail!("scale-set registration needs a set ID or a set name");
            }
            (None, Some(name)) if name.is_empty() => {
                anyhow::bail!("scale-set registration set name is empty");
            }
            _ => Ok(()),
        }
    }
}

/// What [`reconcile_registration`] adopted or created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciledSet {
    pub group_id: i32,
    pub group_name: String,
    pub set: RunnerScaleSet,
    /// The set did not exist and was created by this call.
    pub created: bool,
    /// The live set's labels drifted and were patched back.
    pub labels_updated: bool,
}

/// Ensure the registered group + set exist and match the plan.
///
/// Order: group resolution → set adopt-or-create → label reconciliation.
/// Every network failure propagates to the daemon's supervised-retry loop;
/// identity mismatches fail closed with the expected-vs-live values.
pub async fn reconcile_registration(
    client: &ScaleSetClient,
    plan: &RegistrationPlan,
) -> Result<ReconciledSet> {
    plan.validate()?;
    let (group_id, group_name) = resolve_group(client, plan).await?;
    match plan.set_id {
        Some(id) => adopt_by_id(client, plan, id, group_id, &group_name).await,
        None => {
            let name = plan.set_name.clone().unwrap_or_default();
            get_or_create_by_name(client, plan, &name, group_id, &group_name).await
        }
    }
}

async fn resolve_group(client: &ScaleSetClient, plan: &RegistrationPlan) -> Result<(i32, String)> {
    if let Some(id) = plan.group_id {
        if id <= 0 {
            anyhow::bail!("scale-set runner group ID must be positive, got {id}");
        }
        let name = plan.group_name.clone().unwrap_or_default();
        return Ok((id, name));
    }
    let name = plan.group_name.clone().unwrap_or_default();
    let group = client
        .get_runner_group_by_name(&name)
        .await
        .with_context(|| format!("resolve scale-set runner group {name:?}"))?;
    Ok((group.id, group.name))
}

async fn adopt_by_id(
    client: &ScaleSetClient,
    plan: &RegistrationPlan,
    set_id: i32,
    group_id: i32,
    group_name: &str,
) -> Result<ReconciledSet> {
    let live = client
        .get_runner_scale_set_by_id(set_id)
        .await
        .with_context(|| format!("adopt scale set {set_id}"))?;
    if live.runner_group_id != group_id {
        anyhow::bail!(
            "scale set {set_id} lives in runner group {} ({:?}), not the configured group {group_id} ({group_name:?}): refusing to move it",
            live.runner_group_id,
            live.runner_group_name,
        );
    }
    let mut labels_updated = false;
    let mut set = live;
    if labels_drifted(&set.labels, &plan.labels) {
        set.labels = desired_labels(&plan.labels);
        set.runner_group_name = group_name.to_owned();
        let updated = client
            .update_runner_scale_set(set_id, &set)
            .await
            .with_context(|| format!("reconcile labels on scale set {set_id}"))?;
        set = updated;
        labels_updated = true;
    }
    Ok(ReconciledSet {
        group_id,
        group_name: group_name.to_owned(),
        set,
        created: false,
        labels_updated,
    })
}

async fn get_or_create_by_name(
    client: &ScaleSetClient,
    plan: &RegistrationPlan,
    name: &str,
    group_id: i32,
    group_name: &str,
) -> Result<ReconciledSet> {
    match client
        .get_runner_scale_set(group_id, name)
        .await
        .with_context(|| format!("look up scale set {name:?} in group {group_id}"))?
    {
        Some(mut live) => {
            let mut labels_updated = false;
            if labels_drifted(&live.labels, &plan.labels) {
                live.labels = desired_labels(&plan.labels);
                live.runner_group_name = group_name.to_owned();
                live = client
                    .update_runner_scale_set(live.id, &live)
                    .await
                    .with_context(|| format!("reconcile labels on scale set {name:?}"))?;
                labels_updated = true;
            }
            Ok(ReconciledSet {
                group_id,
                group_name: group_name.to_owned(),
                set: live,
                created: false,
                labels_updated,
            })
        }
        None => {
            let fresh = RunnerScaleSet {
                id: 0,
                name: name.to_owned(),
                runner_group_id: group_id,
                runner_group_name: group_name.to_owned(),
                labels: desired_labels(&plan.labels),
                runner_setting: RunnerSetting::default(),
                created_on: String::new(),
                runner_jit_config_url: String::new(),
                statistics: None,
            };
            match client.create_runner_scale_set(&fresh).await {
                Ok(set) => Ok(ReconciledSet {
                    group_id,
                    group_name: group_name.to_owned(),
                    set,
                    created: true,
                    labels_updated: false,
                }),
                Err(error) if is_create_race(&error) => {
                    // A concurrent creator won: adopt what is there now.
                    let live = client
                        .get_runner_scale_set(group_id, name)
                        .await
                        .with_context(|| {
                            format!("re-read scale set {name:?} after create conflict")
                        })?
                        .with_context(|| {
                            format!("scale set {name:?} still missing after create conflict")
                        })?;
                    Ok(ReconciledSet {
                        group_id,
                        group_name: group_name.to_owned(),
                        set: live,
                        created: false,
                        labels_updated: false,
                    })
                }
                Err(error) => Err(anyhow::Error::new(error)
                    .context(format!("create scale set {name:?} in group {group_id}"))),
            }
        }
    }
}

/// A create that lost a same-name race surfaces as 409/conflict (or
/// `RunnerExists`); both mean "adopt", never "fail".
fn is_create_race(error: &ScaleSetError) -> bool {
    matches!(
        error.fault(),
        Some(ScaleSetFault::Conflict | ScaleSetFault::RunnerExists)
    )
}

/// Desired label rows: names from the plan, types defaulted by the client
/// (`System`) on write.
fn desired_labels(names: &[String]) -> Vec<ScaleSetLabel> {
    names
        .iter()
        .map(|name| ScaleSetLabel {
            label_type: String::new(),
            name: name.clone(),
        })
        .collect()
}

/// Label drift compares names only: the server owns type spellings and
/// ordering.
fn labels_drifted(live: &[ScaleSetLabel], desired: &[String]) -> bool {
    let mut live_names: Vec<&str> = live.iter().map(|label| label.name.as_str()).collect();
    live_names.sort_unstable();
    let mut desired_names: Vec<&str> = desired.iter().map(String::as_str).collect();
    desired_names.sort_unstable();
    // An empty plan reconciles nothing: creating with zero labels lets the
    // server derive the name label, and adopting must not strip live labels
    // the operator manages elsewhere.
    if desired_names.is_empty() {
        return false;
    }
    live_names != desired_names
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

    fn plan() -> RegistrationPlan {
        RegistrationPlan {
            group_id: Some(3),
            group_name: None,
            set_id: None,
            set_name: Some("velnor-set".into()),
            labels: vec!["velnor".into(), "linux".into()],
        }
    }

    #[test]
    fn plan_validation_fails_closed() {
        let no_group = RegistrationPlan {
            group_id: None,
            group_name: None,
            ..plan()
        };
        assert!(no_group.validate().is_err());
        let both_ids = RegistrationPlan {
            set_id: Some(7),
            ..plan()
        };
        assert!(both_ids.validate().is_err());
        let neither = RegistrationPlan {
            set_id: None,
            set_name: None,
            ..plan()
        };
        assert!(neither.validate().is_err());
        let empty_name = RegistrationPlan {
            set_name: Some(String::new()),
            ..plan()
        };
        assert!(empty_name.validate().is_err());
        assert!(plan().validate().is_ok());
    }

    #[test]
    fn label_drift_ignores_order_and_type() {
        let live = vec![
            ScaleSetLabel {
                label_type: "System".into(),
                name: "linux".into(),
            },
            ScaleSetLabel {
                label_type: "User".into(),
                name: "velnor".into(),
            },
        ];
        assert!(!labels_drifted(&live, &["velnor".into(), "linux".into()]));
        assert!(labels_drifted(&live, &["velnor".into()]));
        assert!(labels_drifted(
            &live,
            &["velnor".into(), "linux".into(), "extra".into()]
        ));
        // Empty plan never strips operator-managed labels.
        assert!(!labels_drifted(&live, &[]));
    }

    #[test]
    fn create_race_detects_conflict_and_exists() {
        let conflict = ScaleSetError::RequestFailed(Box::new(crate::scaleset::RequestFailure {
            method: "POST".into(),
            url: "https://actions.invalid/sets".into(),
            status: "409".into(),
            activity: String::new(),
            request_id: String::new(),
            message: "exists".into(),
            fault: Some(ScaleSetFault::Conflict),
        }));
        assert!(is_create_race(&conflict));
        let missing = ScaleSetError::RequestFailed(Box::new(crate::scaleset::RequestFailure {
            method: "GET".into(),
            url: "https://actions.invalid/sets/7".into(),
            status: "404".into(),
            activity: String::new(),
            request_id: String::new(),
            message: "nope".into(),
            fault: Some(ScaleSetFault::NotFound),
        }));
        assert!(!is_create_race(&missing));
        assert!(!is_create_race(&ScaleSetError::Transport("down".into())));
    }
}
