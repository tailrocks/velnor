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
        crate::platform::validate_self_hosted_runner_labels(&self.labels)?;
        if self.group_id.is_none() && self.group_name.as_ref().is_none_or(String::is_empty) {
            anyhow::bail!("scale-set registration needs a runner group ID or name");
        }
        if let Some(id) = self.group_id
            && id <= 0
        {
            anyhow::bail!("scale-set runner group ID must be positive, got {id}");
        }
        if let Some(id) = self.set_id
            && id <= 0
        {
            anyhow::bail!("scale-set ID must be positive, got {id}");
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
    /// The live set's labels drifted and were patched back. The immutable
    /// runner policy is reported separately by the returned set itself.
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
    let (set, labels_updated) = reconcile_live_set(
        client,
        plan,
        live,
        group_id,
        group_name,
        Some(set_id),
        None,
        &format!("reconcile scale-set {set_id}"),
    )
    .await?;
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
        Some(live) => {
            let (live, labels_updated) = reconcile_live_set(
                client,
                plan,
                live,
                group_id,
                group_name,
                None,
                Some(name),
                &format!("reconcile scale-set {name:?}"),
            )
            .await?;
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
                runner_setting: pinned_runner_setting(),
                created_on: "0001-01-01T00:00:00Z".to_string(),
                runner_jit_config_url: String::new(),
                statistics: None,
            };
            match client.create_runner_scale_set(&fresh).await {
                Ok(set) => {
                    let (set, labels_updated) = reconcile_live_set(
                        client,
                        plan,
                        set,
                        group_id,
                        group_name,
                        None,
                        Some(name),
                        &format!("verify created scale-set {name:?}"),
                    )
                    .await?;
                    Ok(ReconciledSet {
                        group_id,
                        group_name: group_name.to_owned(),
                        set,
                        created: true,
                        labels_updated,
                    })
                }
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
                    let (live, labels_updated) = reconcile_live_set(
                        client,
                        plan,
                        live,
                        group_id,
                        group_name,
                        None,
                        Some(name),
                        &format!("reconcile raced scale-set {name:?}"),
                    )
                    .await?;
                    Ok(ReconciledSet {
                        group_id,
                        group_name: group_name.to_owned(),
                        set: live,
                        created: false,
                        labels_updated,
                    })
                }
                Err(error) => Err(anyhow::Error::new(error)
                    .context(format!("create scale set {name:?} in group {group_id}"))),
            }
        }
    }
}

/// The official runner image is immutable: a scale set must not allow the
/// runner process to self-update away from the digest Velnor provisioned.
/// This is a registration invariant, not a best-effort image hint.
fn pinned_runner_setting() -> RunnerSetting {
    RunnerSetting {
        disable_update: true,
    }
}

/// Validate and prepare one live set for adoption or post-create verification.
/// The same path is used for ID adoption, name adoption, and a create race so
/// no registration route can bypass the immutable-runner policy.
fn prepare_live_set(
    mut set: RunnerScaleSet,
    plan: &RegistrationPlan,
    expected_group_id: i32,
    expected_group_name: &str,
    expected_set_id: Option<i32>,
    expected_set_name: Option<&str>,
) -> Result<(RunnerScaleSet, bool, bool)> {
    validate_live_identity(
        &set,
        expected_group_id,
        expected_group_name,
        expected_set_id,
        expected_set_name,
    )?;

    let labels_updated = labels_drifted(&set.labels, &plan.labels);
    let group_name_updated =
        !expected_group_name.is_empty() && set.runner_group_name != expected_group_name;
    let runner_policy_updated = !set.runner_setting.disable_update;

    if labels_updated {
        set.labels = desired_labels(&plan.labels);
    }
    if group_name_updated {
        set.runner_group_name = expected_group_name.to_owned();
    }
    if runner_policy_updated {
        set.runner_setting = pinned_runner_setting();
    }

    Ok((
        set,
        labels_updated,
        labels_updated || group_name_updated || runner_policy_updated,
    ))
}

/// Apply any required PATCH and fail closed if GitHub does not return the
/// requested identity and immutable runner setting.
async fn reconcile_live_set(
    client: &ScaleSetClient,
    plan: &RegistrationPlan,
    live: RunnerScaleSet,
    expected_group_id: i32,
    expected_group_name: &str,
    expected_set_id: Option<i32>,
    expected_set_name: Option<&str>,
    operation: &str,
) -> Result<(RunnerScaleSet, bool)> {
    let (prepared, labels_updated, needs_update) = prepare_live_set(
        live,
        plan,
        expected_group_id,
        expected_group_name,
        expected_set_id,
        expected_set_name,
    )?;
    let set = if needs_update {
        client
            .update_runner_scale_set(prepared.id, &prepared)
            .await
            .with_context(|| format!("{operation}: update scale-set registration policy"))?
    } else {
        prepared
    };
    validate_reconciled_set(
        &set,
        plan,
        expected_group_id,
        expected_group_name,
        expected_set_id,
        expected_set_name,
    )
    .with_context(|| format!("{operation}: server returned an invalid scale-set identity"))?;
    Ok((set, labels_updated))
}

fn validate_live_identity(
    set: &RunnerScaleSet,
    expected_group_id: i32,
    expected_group_name: &str,
    expected_set_id: Option<i32>,
    expected_set_name: Option<&str>,
) -> Result<()> {
    if set.id <= 0 {
        anyhow::bail!(
            "scale-set registration returned a non-positive set ID {}",
            set.id
        );
    }
    if let Some(expected_id) = expected_set_id
        && set.id != expected_id
    {
        anyhow::bail!(
            "scale-set registration returned ID {}, not the pinned ID {}: refusing to adopt it",
            set.id,
            expected_id,
        );
    }
    if set.name.is_empty() {
        anyhow::bail!("scale-set registration returned an empty set name");
    }
    if set.runner_group_id != expected_group_id {
        anyhow::bail!(
            "scale set {} lives in runner group {} ({:?}), not the configured group {} ({:?}): refusing to move it",
            set.id,
            set.runner_group_id,
            set.runner_group_name,
            expected_group_id,
            expected_group_name,
        );
    }
    if let Some(expected_name) = expected_set_name
        && set.name != expected_name
    {
        anyhow::bail!(
            "scale set {} has name {:?}, not the configured name {:?}: refusing to adopt it",
            set.id,
            set.name,
            expected_name,
        );
    }
    Ok(())
}

fn validate_reconciled_set(
    set: &RunnerScaleSet,
    plan: &RegistrationPlan,
    expected_group_id: i32,
    expected_group_name: &str,
    expected_set_id: Option<i32>,
    expected_set_name: Option<&str>,
) -> Result<()> {
    validate_live_identity(
        set,
        expected_group_id,
        expected_group_name,
        expected_set_id,
        expected_set_name,
    )?;
    if !set.runner_setting.disable_update {
        anyhow::bail!(
            "scale set {} did not retain RunnerSetting.disableUpdate=true; refusing an auto-updating official runner",
            set.id
        );
    }
    if !plan.labels.is_empty() && labels_drifted(&set.labels, &plan.labels) {
        anyhow::bail!(
            "scale set {} did not retain the configured labels after reconciliation",
            set.id
        );
    }
    Ok(())
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
        let non_positive_set_id = RegistrationPlan {
            set_id: Some(0),
            set_name: None,
            ..plan()
        };
        assert!(non_positive_set_id.validate().is_err());
        let reserved_label = RegistrationPlan {
            labels: vec!["uBuNtU-24.04".into()],
            ..plan()
        };
        assert!(reserved_label
            .validate()
            .unwrap_err()
            .to_string()
            .contains("reserved for GitHub-hosted runner selection"));
        assert!(plan().validate().is_ok());
    }

    fn live_set(disable_update: bool) -> RunnerScaleSet {
        RunnerScaleSet {
            id: 7,
            name: "velnor-set".into(),
            runner_group_id: 3,
            runner_group_name: "velnor".into(),
            labels: desired_labels(&["velnor".into(), "linux".into()]),
            runner_setting: RunnerSetting { disable_update },
            created_on: "2026-09-17T00:00:00Z".into(),
            runner_jit_config_url: String::new(),
            statistics: None,
        }
    }

    #[test]
    fn pinned_runner_policy_disables_official_self_update() {
        assert!(pinned_runner_setting().disable_update);

        let (prepared, labels_updated, needs_update) = prepare_live_set(
            live_set(false),
            &plan(),
            3,
            "velnor",
            None,
            Some("velnor-set"),
        )
        .unwrap();
        assert!(!labels_updated);
        assert!(needs_update);
        assert!(prepared.runner_setting.disable_update);
    }

    #[test]
    fn live_identity_validation_rejects_invalid_upstream_objects() {
        let mut missing_id = live_set(true);
        missing_id.id = 0;
        assert!(validate_live_identity(&missing_id, 3, "velnor", None, None).is_err());

        let mut wrong_group = live_set(true);
        wrong_group.runner_group_id = 9;
        let error = validate_live_identity(&wrong_group, 3, "velnor", None, None).unwrap_err();
        assert!(error.to_string().contains("not the configured group"));

        let error =
            validate_live_identity(&live_set(true), 3, "velnor", None, Some("other")).unwrap_err();
        assert!(error.to_string().contains("refusing to adopt"));

        let error =
            validate_live_identity(&live_set(true), 3, "velnor", Some(8), None).unwrap_err();
        assert!(error.to_string().contains("pinned ID"));
    }

    #[test]
    fn reconciled_set_rejects_server_that_keeps_updates_enabled() {
        let error = validate_reconciled_set(
            &live_set(false),
            &plan(),
            3,
            "velnor",
            None,
            Some("velnor-set"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("disableUpdate=true"));
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
            status_fault: None,
            api_exception: None,
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
            status_fault: None,
            api_exception: None,
        }));
        assert!(!is_create_race(&missing));
        assert!(!is_create_race(&ScaleSetError::Transport("down".into())));
    }
}
