//! Explicit GitHub scheduler boundary. Production is Legacy JIT V2.

use velnor_model::{SchedulerKind, SCALESET_ENDPOINT, SCALESET_MAX_CAPACITY_HEADER};

use crate::protocol::{GitHubJitConfigRequest, GitHubScope};

/// Production scheduler selection. ScaleSetV2 is constructible for fixtures
/// but cannot activate on the live register path.
#[must_use]
pub fn production_scheduler() -> SchedulerKind {
    SchedulerKind::PRODUCTION
}

/// Exact JIT register URL used by LegacyJitV2 (shipped `GitHubScope`).
///
/// # Errors
/// Invalid GitHub URL.
pub fn legacy_jit_v2_register_url(github_url: &str) -> anyhow::Result<String> {
    Ok(GitHubScope::parse(github_url)?.jit_config_url.to_string())
}

/// Exact JIT request body used by LegacyJitV2 (shipped `GitHubJitConfigRequest`).
#[must_use]
pub fn legacy_jit_v2_request(
    name: String,
    runner_group_id: i64,
    labels: Vec<String>,
) -> GitHubJitConfigRequest {
    GitHubJitConfigRequest {
        name,
        runner_group_id,
        labels,
        work_folder: None,
    }
}

/// Scale-set JIT path from pinned `actions/scaleset` `GenerateJitRunnerConfig`.
#[must_use]
pub fn scaleset_v2_generate_jit_path(scale_set_id: i32) -> String {
    format!("/{SCALESET_ENDPOINT}/{scale_set_id}/generatejitconfig")
}

/// Scale-set poll header from pinned `HeaderScaleSetMaxCapacity`.
#[must_use]
pub fn scaleset_v2_max_capacity_header() -> &'static str {
    SCALESET_MAX_CAPACITY_HEADER
}

#[cfg(test)]
mod tests {
    use super::*;
    use velnor_model::{
        RunnerScaleSetMessageResponse, RunnerScaleSetStatistic, SCALESET_API_VERSION,
        SCALESET_UPSTREAM_COMMIT,
    };

    #[test]
    fn production_scheduler_is_legacy_jit_v2() {
        assert_eq!(production_scheduler(), SchedulerKind::LegacyJitV2);
        assert!(production_scheduler().activate_production().is_ok());
        assert!(SchedulerKind::ScaleSetV2.activate_production().is_err());
    }

    #[test]
    fn legacy_jit_v2_url_is_generate_jitconfig() {
        let url = legacy_jit_v2_register_url("https://github.com/tailrocks/velnor").unwrap();
        assert!(
            url.ends_with("/repos/tailrocks/velnor/actions/runners/generate-jitconfig"),
            "{url}"
        );
        let org = legacy_jit_v2_register_url("https://github.com/tailrocks").unwrap();
        assert!(
            org.ends_with("/orgs/tailrocks/actions/runners/generate-jitconfig"),
            "{org}"
        );
    }

    #[test]
    fn legacy_jit_v2_request_fields_are_name_group_labels() {
        let request = legacy_jit_v2_request("velnor-slot-1".into(), 7, vec!["velnor".into()]);
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["name"], "velnor-slot-1");
        assert_eq!(json["runner_group_id"], 7);
        assert_eq!(json["labels"][0], "velnor");
        assert!(json.get("work_folder").is_none());
    }

    #[test]
    fn scaleset_v2_paths_match_pinned_upstream() {
        assert_eq!(SCALESET_UPSTREAM_COMMIT.len(), 40);
        assert_eq!(
            scaleset_v2_generate_jit_path(42),
            "/_apis/runtime/runnerscalesets/42/generatejitconfig"
        );
        assert_eq!(scaleset_v2_max_capacity_header(), "X-ScaleSetMaxCapacity");
        assert_eq!(SCALESET_API_VERSION, "6.0-preview");
    }

    #[test]
    fn scaleset_v2_desired_runners_ignore_truncated_message_body() {
        // Upstream: responses contain at most 50 messages; statistics are current.
        let body = serde_json::to_string(
            &(0..51)
                .map(|_| serde_json::json!({"messageType": "JobAssigned"}))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let fixture = serde_json::json!({
            "messageId": 9,
            "messageType": "RunnerScaleSetJobMessages",
            "body": body,
            "statistics": {
                "totalAvailableJobs": 0,
                "totalAcquiredJobs": 0,
                "totalAssignedJobs": 3,
                "totalRunningJobs": 1,
                "totalRegisteredRunners": 2,
                "totalBusyRunners": 1,
                "totalIdleRunners": 1
            }
        });
        let parsed: RunnerScaleSetMessageResponse = serde_json::from_value(fixture).unwrap();
        assert_eq!(parsed.message_type, "RunnerScaleSetJobMessages");
        let batched: Vec<serde_json::Value> = serde_json::from_str(&parsed.body).unwrap();
        assert_eq!(batched.len(), 51);
        let stats: RunnerScaleSetStatistic = parsed.statistics.unwrap();
        assert_eq!(stats.desired_runners(), 3);
        assert_ne!(stats.desired_runners() as usize, batched.len());
    }
}
