//! Read-only GitHub collection for the evidence checker.
//!
//! This module intentionally has no result-ledger inputs. It obtains current
//! repository, PR, ruleset, workflow, run, check, and job facts through the
//! existing `FleetHttp` transport. Any inaccessible or malformed endpoint is
//! an error; an empty successful-looking fallback is never synthesized.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::evidence_check::{
    CheckObservation, ChildRunObservation, ExecutionObservation, JobObservation, ManifestDocument,
    ManifestRepository, RequiredContext, RulesetObservation, SnapshotDocument, SnapshotPullRequest,
    SnapshotRepository, SnapshotSource, WorkflowObservation,
};
use crate::fleet_policy_client::{FleetHttp, FleetHttpMethod, FleetHttpRequest};

const PER_PAGE: usize = 100;
const MAX_PAGES: u32 = 1000;

pub(crate) async fn collect_live_snapshot<H: FleetHttp>(
    http: &H,
    manifest: &ManifestDocument,
    snapshot_id: String,
) -> Result<SnapshotDocument> {
    let captured_at_utc = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("format live snapshot time")?;
    let mut page_count = 0_u32;
    let mut repositories = Vec::with_capacity(manifest.repositories.len());
    for manifest_repo in &manifest.repositories {
        repositories.push(
            collect_repository(http, manifest_repo, &mut page_count)
                .await
                .with_context(|| format!("collect live facts for {}", manifest_repo.repository))?,
        );
    }
    Ok(SnapshotDocument {
        schema_version: 2,
        snapshot_id,
        manifest_id: manifest.manifest_id.clone(),
        observed_at_utc: captured_at_utc.clone(),
        source: SnapshotSource {
            collector: "velnor-tools/evidence-live".to_owned(),
            collector_revision: env!("CARGO_PKG_VERSION").to_owned(),
            api_base: crate::fleet_policy_client::DEFAULT_GITHUB_API_URL.to_owned(),
            captured_at_utc,
            read_only: true,
            page_count,
            // These are non-secret scope names only. The transport never puts
            // the bearer token into this snapshot.
            permission_scopes: vec![
                "metadata:read".to_owned(),
                "contents:read".to_owned(),
                "actions:read".to_owned(),
                "checks:read".to_owned(),
                "pull_requests:read".to_owned(),
            ],
        },
        repositories,
    })
}

async fn collect_repository<H: FleetHttp>(
    http: &H,
    manifest: &ManifestRepository,
    page_count: &mut u32,
) -> Result<SnapshotRepository> {
    let repository_path = format!("/repos/{}", manifest.repository);
    let repository = get_json(http, &repository_path, &Vec::new(), page_count).await?;
    let repository_id = positive_u64(&repository, "id")?;
    let default_branch = string_field(&repository, "default_branch")?;
    let commit = get_json(
        http,
        &format!("/repos/{}/commits/{default_branch}", manifest.repository),
        &Vec::new(),
        page_count,
    )
    .await?;
    let default_branch_sha = nested_string(&commit, &["sha"])?;
    let workflows = collect_workflows(http, manifest, &default_branch_sha, page_count).await?;
    let ruleset = collect_rulesets(http, manifest, page_count).await?;
    let open_pr_values = paginate(
        http,
        &format!("/repos/{}/pulls", manifest.repository),
        &[("state", "open"), ("sort", "updated")],
        "",
        page_count,
    )
    .await?;
    let mut open_prs = Vec::with_capacity(open_pr_values.len());
    for value in open_pr_values {
        let number = positive_u64(&value, "number")?;
        let draft = value
            .get("draft")
            .and_then(Value::as_bool)
            .ok_or_else(|| anyhow!("PR #{number} lacks draft state"))?;
        let author = nested_string(&value, &["user", "login"])?;
        let author_association = string_field(&value, "author_association")?;
        let head_repository = nested_string(&value, &["head", "repo", "full_name"])?;
        let head_sha = nested_string(&value, &["head", "sha"])?;
        let base_sha = nested_string(&value, &["base", "sha"])?;
        let merge_sha = value
            .get("merge_commit_sha")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let merge_group_sha = value
            .get("merge_group")
            .and_then(|group| group.get("sha"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let source_url = string_field(&value, "html_url")?;
        let executions =
            collect_executions(http, manifest, &head_sha, &ruleset, &workflows, page_count).await?;
        open_prs.push(SnapshotPullRequest {
            number,
            state: "open".to_owned(),
            draft,
            author,
            author_association,
            head_repository,
            head_sha,
            base_sha,
            merge_sha,
            merge_group_sha,
            source_url,
            executions,
        });
    }
    let main_executions = collect_executions(
        http,
        manifest,
        &default_branch_sha,
        &ruleset,
        &workflows,
        page_count,
    )
    .await?;
    Ok(SnapshotRepository {
        repository: manifest.repository.clone(),
        repository_id,
        default_branch,
        default_branch_sha,
        ruleset,
        workflows,
        main_executions,
        open_prs,
    })
}

async fn collect_workflows<H: FleetHttp>(
    http: &H,
    manifest: &ManifestRepository,
    source_sha: &str,
    page_count: &mut u32,
) -> Result<Vec<WorkflowObservation>> {
    let values = paginate(
        http,
        &format!("/repos/{}/actions/workflows", manifest.repository),
        &[],
        "workflows",
        page_count,
    )
    .await?;
    let mut workflows = Vec::new();
    for value in values {
        let path = string_field(&value, "path")?;
        let source_url = string_field(&value, "html_url")?;
        let content = get_json(
            http,
            &format!(
                "/repos/{}/contents/{}",
                manifest.repository,
                path.trim_start_matches('/')
            ),
            &[("ref".to_owned(), source_sha.to_owned())],
            page_count,
        )
        .await?;
        let revision = content
            .get("sha")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("workflow {path} content response lacks immutable sha"))?
            .to_owned();
        workflows.push(WorkflowObservation {
            path,
            revision,
            source_sha: source_sha.to_owned(),
            event: "unknown".to_owned(),
            source_url,
        });
    }
    if workflows.is_empty() {
        bail!(
            "GitHub returned no workflow inventory for {}",
            manifest.repository
        );
    }
    Ok(workflows)
}

async fn collect_rulesets<H: FleetHttp>(
    http: &H,
    manifest: &ManifestRepository,
    page_count: &mut u32,
) -> Result<RulesetObservation> {
    let values = paginate(
        http,
        &format!("/repos/{}/rulesets", manifest.repository),
        &[("includes_parents", "true")],
        "",
        page_count,
    )
    .await?;
    let mut required = BTreeMap::<(String, String), RequiredContext>::new();
    for value in values {
        let id = positive_u64(&value, "id")?;
        let detail = get_json(
            http,
            &format!("/repos/{}/rulesets/{id}", manifest.repository),
            &Vec::new(),
            page_count,
        )
        .await?;
        let Some(rules) = detail.get("rules").and_then(Value::as_array) else {
            bail!("ruleset {id} for {} lacks rules array", manifest.repository);
        };
        for rule in rules {
            let Some(parameters) = rule.get("parameters") else {
                continue;
            };
            let Some(checks) = parameters
                .get("required_status_checks")
                .or_else(|| parameters.get("required_status_checks_and_apps"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for check in checks {
                let context = check
                    .get("context")
                    .or_else(|| check.get("name"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("ruleset {id} has required check without context"))?;
                let app_id = check
                    .get("integration_id")
                    .or_else(|| check.get("app_id"))
                    .and_then(Value::as_i64)
                    .map(|value| value.to_string())
                    .or_else(|| check.get("app").and_then(Value::as_str).map(str::to_owned))
                    .ok_or_else(|| anyhow!("ruleset {id} check {context} lacks app identity"))?;
                required.insert(
                    (context.to_owned(), app_id.clone()),
                    RequiredContext {
                        context: context.to_owned(),
                        app_id,
                    },
                );
            }
        }
    }
    let source_url = format!("https://github.com/{}/settings/rules", manifest.repository);
    Ok(RulesetObservation {
        required_checks: required.into_values().collect(),
        source_url,
        pages_complete: true,
    })
}

async fn collect_executions<H: FleetHttp>(
    http: &H,
    manifest: &ManifestRepository,
    source_sha: &str,
    ruleset: &RulesetObservation,
    workflows: &[WorkflowObservation],
    page_count: &mut u32,
) -> Result<Vec<ExecutionObservation>> {
    let values = paginate(
        http,
        &format!("/repos/{}/actions/runs", manifest.repository),
        &[("head_sha", source_sha)],
        "workflow_runs",
        page_count,
    )
    .await?;
    let mut executions = Vec::new();
    for value in values {
        let run_sha = string_field(&value, "head_sha")?;
        if run_sha != source_sha {
            continue;
        }
        let run_id = positive_u64(&value, "id")?;
        let run_attempt = value
            .get("run_attempt")
            .and_then(Value::as_u64)
            .unwrap_or(1) as u32;
        let run_url = string_field(&value, "html_url")?;
        let workflow_path = value
            .get("path")
            .and_then(Value::as_str)
            .or_else(|| value.get("workflow_name").and_then(Value::as_str))
            .ok_or_else(|| anyhow!("run {run_id} lacks workflow path"))?
            .to_owned();
        let workflow_revision = workflows
            .iter()
            .find(|workflow| workflow.path == workflow_path)
            .map(|workflow| workflow.revision.clone())
            .ok_or_else(|| {
                anyhow!("run {run_id} workflow {workflow_path} absent from source inventory")
            })?;
        let event = string_field(&value, "event")?;
        let status = string_field(&value, "status")?;
        let conclusion = value
            .get("conclusion")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let jobs = collect_jobs(http, manifest, run_id, &event, page_count).await?;
        let provider =
            if !jobs.is_empty() && jobs.iter().all(|job| job.runner_kind == "velnor-managed") {
                "velnor".to_owned()
            } else {
                "github".to_owned()
            };
        let (runner_name, host_id, runner_kind, runner_labels) = jobs
            .first()
            .map(|job| {
                (
                    job.runner_name.clone(),
                    job.host_id.clone(),
                    job.runner_kind.clone(),
                    job.runner_labels.clone(),
                )
            })
            .unwrap_or_else(|| (String::new(), String::new(), String::new(), Vec::new()));
        let required_checks = collect_checks(
            http, manifest, source_sha, run_id, &event, ruleset, page_count,
        )
        .await?;
        let child_runs = collect_child_runs(http, manifest, source_sha, run_id, page_count).await?;
        executions.push(ExecutionObservation {
            run_id,
            run_attempt,
            run_url,
            workflow_path,
            workflow_revision,
            event,
            trigger_source_sha: run_sha.clone(),
            actual_checkout_sha: run_sha,
            status,
            conclusion,
            provider,
            runner_name,
            host_id,
            runner_kind,
            runner_labels,
            jobs,
            required_checks,
            child_runs,
        });
    }
    Ok(executions)
}

async fn collect_child_runs<H: FleetHttp>(
    http: &H,
    manifest: &ManifestRepository,
    source_sha: &str,
    parent_run_id: u64,
    page_count: &mut u32,
) -> Result<Vec<ChildRunObservation>> {
    let child_specs = manifest
        .expected_jobs
        .iter()
        .filter_map(|job| job.child_workflow.as_ref())
        .collect::<Vec<_>>();
    if child_specs.is_empty() {
        return Ok(Vec::new());
    }
    let values = paginate(
        http,
        &format!("/repos/{}/actions/runs", manifest.repository),
        &[("head_sha", source_sha), ("event", "workflow_run")],
        "workflow_runs",
        page_count,
    )
    .await?;
    let mut children = Vec::new();
    for value in values {
        let workflow_path = value
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let Some(spec) = child_specs
            .iter()
            .find(|spec| spec.workflow_path == workflow_path)
        else {
            continue;
        };
        let Some(observed_parent_run_id) = value.get("parent_run_id").and_then(Value::as_u64)
        else {
            // GitHub does not expose a parent relation for this response. Do
            // not infer one from a matching SHA; missing association is a
            // failed child graph, not successful evidence.
            continue;
        };
        if observed_parent_run_id != parent_run_id {
            continue;
        }
        let run_id = positive_u64(&value, "id")?;
        let child_source_sha = value
            .get("head_sha")
            .and_then(Value::as_str)
            .unwrap_or(source_sha)
            .to_owned();
        let provider = manifest
            .expected_jobs
            .iter()
            .find(|job| {
                job.child_workflow
                    .as_ref()
                    .is_some_and(|child| child.workflow_path == spec.workflow_path)
            })
            .map(|job| job.provider.clone())
            .unwrap_or_else(|| "github".to_owned());
        children.push(ChildRunObservation {
            parent_run_id,
            run_id,
            run_attempt: value
                .get("run_attempt")
                .and_then(Value::as_u64)
                .unwrap_or(1) as u32,
            repository: spec.repository.clone(),
            workflow_path,
            event: "workflow_run".to_owned(),
            source_sha: child_source_sha,
            provider,
            status: string_field(&value, "status")?,
            conclusion: value
                .get("conclusion")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            source_url: string_field(&value, "html_url")?,
        });
    }
    Ok(children)
}

async fn collect_jobs<H: FleetHttp>(
    http: &H,
    manifest: &ManifestRepository,
    run_id: u64,
    event: &str,
    page_count: &mut u32,
) -> Result<Vec<JobObservation>> {
    let values = paginate(
        http,
        &format!("/repos/{}/actions/runs/{run_id}/jobs", manifest.repository),
        &[],
        "jobs",
        page_count,
    )
    .await?;
    let mut jobs = Vec::new();
    for value in values {
        let job_id = positive_u64(&value, "id")?.to_string();
        let runner_name = value
            .get("runner_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let labels = value
            .get("labels")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("job {job_id} lacks runner labels"))?
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("job {job_id} has a non-string runner label"))?;
        let runner_kind = classify_runner(&runner_name, &labels);
        let host_id = value
            .get("runner_id")
            .and_then(Value::as_u64)
            .map(|value| value.to_string())
            .or_else(|| (!runner_name.is_empty()).then_some(runner_name.clone()))
            .unwrap_or_default();
        let name = string_field(&value, "name")?;
        let expected = manifest
            .expected_jobs
            .iter()
            .find(|job| job.job_id == name || job.job_id == job_id);
        let (workload_id, provider, platform, architecture) = expected
            .map(|job| {
                (
                    job.workload_id.clone(),
                    job.provider.clone(),
                    job.platform.clone(),
                    job.architecture.clone(),
                )
            })
            .unwrap_or_else(|| {
                (
                    name.clone(),
                    if runner_kind == "velnor-managed" {
                        "velnor".to_owned()
                    } else {
                        "github".to_owned()
                    },
                    "unknown".to_owned(),
                    "unknown".to_owned(),
                )
            });
        jobs.push(JobObservation {
            job_id,
            job_name: name,
            workload_id,
            provider,
            platform,
            architecture,
            status: string_field(&value, "status")?,
            conclusion: value
                .get("conclusion")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            event: event.to_owned(),
            runner_name,
            host_id,
            runner_kind,
            runner_labels: labels,
            source_url: string_field(&value, "html_url")?,
        });
    }
    Ok(jobs)
}

async fn collect_checks<H: FleetHttp>(
    http: &H,
    manifest: &ManifestRepository,
    source_sha: &str,
    run_id: u64,
    event: &str,
    ruleset: &RulesetObservation,
    page_count: &mut u32,
) -> Result<Vec<CheckObservation>> {
    let values = paginate(
        http,
        &format!(
            "/repos/{}/commits/{source_sha}/check-runs",
            manifest.repository
        ),
        &[],
        "check_runs",
        page_count,
    )
    .await?;
    let mut checks = Vec::new();
    for required in &ruleset.required_checks {
        if let Some(value) = values.iter().find(|value| {
            value.get("name").and_then(Value::as_str) == Some(required.context.as_str())
        }) {
            let actual_app_id = value
                .get("app")
                .and_then(|app| {
                    app.get("id")
                        .and_then(Value::as_i64)
                        .map(|id| id.to_string())
                        .or_else(|| app.get("slug").and_then(Value::as_str).map(str::to_owned))
                })
                .ok_or_else(|| anyhow!("check {} lacks actual app identity", required.context))?;
            checks.push(CheckObservation {
                context: required.context.clone(),
                app_id: actual_app_id,
                status: string_field(value, "status")?,
                conclusion: value
                    .get("conclusion")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                run_id,
                job_id: value
                    .get("external_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                source_url: value
                    .get("details_url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                event: event.to_owned(),
            });
        }
    }
    Ok(checks)
}

fn classify_runner(name: &str, labels: &[String]) -> String {
    let lower_name = name.to_ascii_lowercase();
    if labels.iter().any(|label| {
        let label = label.to_ascii_lowercase();
        label == "self-hosted" || label.contains("velnor")
    }) || lower_name.contains("velnor")
    {
        "velnor-managed".to_owned()
    } else {
        "github-hosted".to_owned()
    }
}

async fn paginate<H: FleetHttp>(
    http: &H,
    path: &str,
    initial_query: &[(&str, &str)],
    array_field: &str,
    page_count: &mut u32,
) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    for page in 1..=MAX_PAGES {
        let mut query = initial_query
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect::<Vec<_>>();
        query.push(("per_page".to_owned(), PER_PAGE.to_string()));
        query.push(("page".to_owned(), page.to_string()));
        let body = get_json(http, path, &query, page_count).await?;
        let values = if array_field.is_empty() {
            body.as_array()
                .cloned()
                .ok_or_else(|| anyhow!("{path} page {page} is not an array"))?
        } else {
            body.get(array_field)
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| anyhow!("{path} page {page} lacks {array_field} array"))?
        };
        let count = values.len();
        result.extend(values);
        if count < PER_PAGE {
            return Ok(result);
        }
    }
    bail!("{path} exceeded pagination cap; refusing truncated evidence")
}

async fn get_json<H: FleetHttp>(
    http: &H,
    path: &str,
    query: &[(String, String)],
    page_count: &mut u32,
) -> Result<Value> {
    let request = FleetHttpRequest {
        method: FleetHttpMethod::Get,
        url_path: path.to_owned(),
        query: query
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        json_body: None,
    };
    let response = http
        .execute(request)
        .await
        .map_err(|error| anyhow!("GitHub read transport failed: {}", error.message))?;
    *page_count = page_count.saturating_add(1);
    if response.status == 403
        || response.status == 404
        || response.status == 429
        || response.status == 500
        || response.status == 502
        || response.status == 503
        || response.status == 504
    {
        bail!(
            "GitHub read endpoint {path} returned status {}",
            response.status
        );
    }
    if !(200..300).contains(&response.status) {
        bail!(
            "GitHub read endpoint {path} returned unexpected status {}",
            response.status
        );
    }
    if response.body.is_null() {
        bail!("GitHub read endpoint {path} returned an empty body");
    }
    Ok(response.body)
}

fn string_field(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("GitHub response lacks non-empty {field}"))
}

fn nested_string(value: &Value, fields: &[&str]) -> Result<String> {
    let mut current = value;
    for field in fields {
        current = current
            .get(*field)
            .ok_or_else(|| anyhow!("GitHub response lacks {}", fields.join(".")))?;
    }
    current
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!(
                "GitHub response field {} is not a non-empty string",
                fields.join(".")
            )
        })
}

fn positive_u64(value: &Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("GitHub response lacks positive {field}"))
}
