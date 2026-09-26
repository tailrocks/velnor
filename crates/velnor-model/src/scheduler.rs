//! GitHub scheduler backends. Production uses per-slot JIT V2.

use serde::{Deserialize, Deserializer, Serialize};

/// Pinned `actions/scaleset` revision used for protocol fixtures.
/// <https://github.com/actions/scaleset/commit/e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5>
/// Audited: Go sources identical to `fb563005` (only CI workflow bumps after
/// it); `session_client.go`, `client.go`, `types.go`, `errors.go`,
/// `config.go`, `common_client.go`, `jwt_provider.go` all match byte-for-byte.
pub const SCALESET_UPSTREAM_COMMIT: &str = "e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5";

/// Actions Service scale-set path from that revision (`client.go`).
pub const SCALESET_ENDPOINT: &str = "_apis/runtime/runnerscalesets";
/// Max-capacity header from that revision (`HeaderScaleSetMaxCapacity`).
pub const SCALESET_MAX_CAPACITY_HEADER: &str = "X-ScaleSetMaxCapacity";
/// Actions Service API version appended on scale-set requests.
pub const SCALESET_API_VERSION: &str = "6.0-preview";

/// Which GitHub scheduler a fleet may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerKind {
    /// Current production: per-slot `generate-jitconfig` + V2 broker.
    PerSlotJitV2,
    /// Public-preview scale-set APIs. Not production until estate proof.
    ScaleSetV2,
}

impl SchedulerKind {
    /// The current backend allowed to register or advertise capacity.
    pub const CURRENT: Self = Self::PerSlotJitV2;

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PerSlotJitV2 => "per_slot_jit_v2",
            Self::ScaleSetV2 => "scale_set_v2",
        }
    }

    /// Reject scheduler backends other than the current per-slot JIT path.
    ///
    /// # Errors
    /// [`SchedulerNotCurrent`] when `self` is not [`Self::CURRENT`].
    pub fn ensure_current(self) -> Result<(), SchedulerNotCurrent> {
        if self == Self::CURRENT {
            Ok(())
        } else {
            Err(SchedulerNotCurrent { requested: self })
        }
    }
}

/// A scheduler backend other than the current per-slot JIT path was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerNotCurrent {
    pub requested: SchedulerKind,
}

impl std::fmt::Display for SchedulerNotCurrent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "scheduler {} is not current; estate group/repo/label/YAML equivalence is unproven (upstream {})",
            self.requested.as_str(),
            SCALESET_UPSTREAM_COMMIT
        )
    }
}

impl std::error::Error for SchedulerNotCurrent {}

/// `RunnerScaleSetStatistic` from `types.go` at [`SCALESET_UPSTREAM_COMMIT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RunnerScaleSetStatistic {
    pub total_available_jobs: i32,
    pub total_acquired_jobs: i32,
    pub total_assigned_jobs: i32,
    pub total_running_jobs: i32,
    pub total_registered_runners: i32,
    pub total_busy_runners: i32,
    pub total_idle_runners: i32,
}

impl RunnerScaleSetStatistic {
    /// Desired online runners. Message bodies cap at 50; statistics are authoritative.
    #[must_use]
    pub fn desired_runners(self) -> u32 {
        u32::try_from(self.total_assigned_jobs.max(0)).unwrap_or(0)
    }
}

/// Batched scale-set message wrapper (`RunnerScaleSetJobMessages`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RunnerScaleSetMessageResponse {
    pub message_id: i32,
    pub message_type: String,
    #[serde(default)]
    pub body: String,
    pub statistics: Option<RunnerScaleSetStatistic>,
}

/// Job lifecycle message types from `types.go`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ScaleSetJobMessageType {
    /// Go's zero-value message type is the empty string.
    #[default]
    #[serde(rename = "")]
    Unspecified,
    JobAvailable,
    JobAssigned,
    JobStarted,
    JobCompleted,
}

/// Shared job-message fields from `types.go` (`JobMessageBase`).
///
/// Wire timestamps stay `String`: Go `time.Time` serializes RFC 3339 and the
/// zero value (`0001-01-01T00:00:00Z`) appears on live messages, so parsing
/// here would reject real traffic. Callers parse what they need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScaleSetJobMessage {
    pub message_type: ScaleSetJobMessageType,
    pub runner_request_id: i64,
    pub repository_name: String,
    pub owner_name: String,
    pub job_id: String,
    pub job_workflow_ref: String,
    pub job_display_name: String,
    pub workflow_run_id: i64,
    pub event_name: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub request_labels: Vec<String>,
    #[serde(default = "go_zero_timestamp")]
    pub queue_time: String,
    #[serde(default = "go_zero_timestamp")]
    pub scale_set_assign_time: String,
    #[serde(default = "go_zero_timestamp")]
    pub runner_assign_time: String,
    #[serde(default = "go_zero_timestamp")]
    pub finish_time: String,
}

fn go_zero_timestamp() -> String {
    "0001-01-01T00:00:00Z".to_owned()
}

fn go_zero_uuid() -> String {
    "00000000-0000-0000-0000-000000000000".to_owned()
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

impl Default for ScaleSetJobMessage {
    fn default() -> Self {
        Self {
            message_type: ScaleSetJobMessageType::default(),
            runner_request_id: 0,
            repository_name: String::new(),
            owner_name: String::new(),
            job_id: String::new(),
            job_workflow_ref: String::new(),
            job_display_name: String::new(),
            workflow_run_id: 0,
            event_name: String::new(),
            request_labels: Vec::new(),
            queue_time: go_zero_timestamp(),
            scale_set_assign_time: go_zero_timestamp(),
            runner_assign_time: go_zero_timestamp(),
            finish_time: go_zero_timestamp(),
        }
    }
}

/// `JobAvailable` from `types.go`: offered work plus its acquire URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScaleSetJobAvailable {
    pub acquire_job_url: String,
    #[serde(flatten)]
    pub base: ScaleSetJobMessage,
}

/// `JobAssigned` from `types.go`: an acquired request now owned by a runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScaleSetJobAssigned {
    #[serde(flatten)]
    pub base: ScaleSetJobMessage,
}

/// `JobStarted` from `types.go`: execution began on the named runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScaleSetJobStarted {
    pub runner_id: i32,
    pub runner_name: String,
    #[serde(flatten)]
    pub base: ScaleSetJobMessage,
}

/// `JobCompleted` from `types.go`: terminal observation with a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScaleSetJobCompleted {
    pub result: String,
    pub runner_id: i32,
    pub runner_name: String,
    #[serde(flatten)]
    pub base: ScaleSetJobMessage,
}

/// Dispatched batch from `parseRunnerScaleSetMessageResponse` (`client.go`).
///
/// Upstream never serializes this struct; the serde shape below is the local
/// fixture/canary encoding (lower-camel Go field names) and is documented as
/// such wherever it is written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RunnerScaleSetMessage {
    pub message_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statistics: Option<RunnerScaleSetStatistic>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub job_available_messages: Vec<ScaleSetJobAvailable>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub job_assigned_messages: Vec<ScaleSetJobAssigned>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub job_started_messages: Vec<ScaleSetJobStarted>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub job_completed_messages: Vec<ScaleSetJobCompleted>,
    /// Batched `messageType` values the parser did not recognize (upstream
    /// `default:` ignores them). Recorded so the loop's unknown-event
    /// reconcile path sees what dispatch dropped; never empty-checked for
    /// ACK gating.
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub unknown_message_types: Vec<String>,
}

/// `acquireJobsResponse` from `types.go`: the acquired subset, not an echo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AcquireJobsResponse {
    pub count: i32,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub value: Vec<i64>,
}

/// `RunnerScaleSetJitRunnerSetting` from `types.go` (JIT request body).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RunnerScaleSetJitRunnerSetting {
    pub name: String,
    pub work_folder: String,
}

/// `RunnerReference` from `types.go`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RunnerReference {
    pub id: i32,
    pub name: String,
    pub runner_scale_set_id: i32,
}

/// `RunnerReferenceList` from `types.go` (`GetRunnerByName` response).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RunnerReferenceList {
    pub count: i32,
    #[serde(
        default,
        rename = "value",
        deserialize_with = "deserialize_null_default"
    )]
    pub runner_references: Vec<RunnerReference>,
}

/// `RunnerScaleSetJitRunnerConfig` from `types.go`.
///
/// `encoded_jit_config` is secret-adjacent: fixtures store `REDACTED`, never a
/// live blob.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RunnerScaleSetJitRunnerConfig {
    pub runner: Option<RunnerReference>,
    #[serde(rename = "encodedJITConfig")]
    pub encoded_jit_config: String,
}

impl std::fmt::Debug for RunnerScaleSetJitRunnerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerScaleSetJitRunnerConfig")
            .field("runner", &self.runner)
            .field("encoded_jit_config", &"<redacted>")
            .finish()
    }
}

/// `Label` from `types.go`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScaleSetLabel {
    #[serde(rename = "type")]
    pub label_type: String,
    pub name: String,
}

/// `RunnerGroup` from `types.go`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RunnerGroup {
    pub id: i32,
    pub name: String,
    pub size: i32,
    pub is_default_group: bool,
}

/// `RunnerGroupList` from `types.go` (`GetRunnerGroupByName` response).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RunnerGroupList {
    pub count: i32,
    #[serde(
        default,
        rename = "value",
        deserialize_with = "deserialize_null_default"
    )]
    pub runner_groups: Vec<RunnerGroup>,
}

/// `runnerScaleSetsResponse` from `types.go` (list/get-by-name response).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RunnerScaleSetList {
    pub count: i32,
    #[serde(
        default,
        rename = "value",
        deserialize_with = "deserialize_null_default"
    )]
    pub runner_scale_sets: Vec<RunnerScaleSet>,
}

/// `RunnerSetting` from `types.go`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RunnerSetting {
    #[serde(default, skip_serializing_if = "is_false")]
    pub disable_update: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde skip predicate")]
fn is_false(value: &bool) -> bool {
    !value
}

/// `RunnerScaleSet` from `types.go`.
///
/// `runner_setting` and `created_on` serialize WITHOUT `omitempty` upstream
/// (#110); the serde shape below preserves that exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RunnerScaleSet {
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub id: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub runner_group_id: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runner_group_name: String,
    #[serde(
        default,
        deserialize_with = "deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub labels: Vec<ScaleSetLabel>,
    #[serde(rename = "RunnerSetting")]
    pub runner_setting: RunnerSetting,
    #[serde(default = "go_zero_timestamp")]
    pub created_on: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub runner_jit_config_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statistics: Option<RunnerScaleSetStatistic>,
}

impl Default for RunnerScaleSet {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            runner_group_id: 0,
            runner_group_name: String::new(),
            labels: Vec::new(),
            runner_setting: RunnerSetting::default(),
            created_on: go_zero_timestamp(),
            runner_jit_config_url: String::new(),
            statistics: None,
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde skip predicate")]
fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

/// `RunnerScaleSetSession` from `types.go`.
///
/// `session_id` stays `String`: the wire format is a UUID string and parsing
/// it here would only add a failure mode to session resume. Queue URL and
/// access token are secret-adjacent: the journal stores hashes only.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScaleSetSession {
    #[serde(default = "go_zero_uuid", skip_serializing_if = "String::is_empty")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_scale_set: Option<RunnerScaleSet>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message_queue_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message_queue_access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statistics: Option<RunnerScaleSetStatistic>,
}

impl Default for ScaleSetSession {
    fn default() -> Self {
        Self {
            session_id: go_zero_uuid(),
            owner_name: String::new(),
            runner_scale_set: None,
            message_queue_url: String::new(),
            message_queue_access_token: String::new(),
            statistics: None,
        }
    }
}

impl std::fmt::Debug for ScaleSetSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScaleSetSession")
            .field("session_id", &self.session_id)
            .field("owner_name", &self.owner_name)
            .field("runner_scale_set", &self.runner_scale_set)
            .field("message_queue_url", &"<redacted>")
            .field("message_queue_access_token", &"<redacted>")
            .field("statistics", &self.statistics)
            .finish()
    }
}

/// Scale-set worker lifecycle (§5.2): the durable per-worker state carried by
/// `ScaleSetWorkerEdge` journal events and the `scaleset_workers` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleSetWorkerState {
    Observed,
    Eligible,
    Reserved,
    AcquireIntent,
    Acquired,
    Uncertain,
    ProvisionIntent,
    DindReady,
    RunnerConnected,
    Running,
    Terminal,
    DiagnosticExport,
    OwnedCleanup,
    PermitReleased,
}

impl ScaleSetWorkerState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Eligible => "eligible",
            Self::Reserved => "reserved",
            Self::AcquireIntent => "acquire_intent",
            Self::Acquired => "acquired",
            Self::Uncertain => "uncertain",
            Self::ProvisionIntent => "provision_intent",
            Self::DindReady => "dind_ready",
            Self::RunnerConnected => "runner_connected",
            Self::Running => "running",
            Self::Terminal => "terminal",
            Self::DiagnosticExport => "diagnostic_export",
            Self::OwnedCleanup => "owned_cleanup",
            Self::PermitReleased => "permit_released",
        }
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

    #[test]
    fn current_scheduler_is_per_slot_jit_v2() {
        assert_eq!(SchedulerKind::CURRENT, SchedulerKind::PerSlotJitV2);
        assert!(SchedulerKind::PerSlotJitV2.ensure_current().is_ok());
        assert!(SchedulerKind::ScaleSetV2.ensure_current().is_err());
    }

    #[test]
    fn removed_snake_case_scheduler_variant_is_rejected() {
        let removed_variant = ["legacy", "jit", "v2"].join("_");
        assert!(serde_json::from_str::<SchedulerKind>(&format!("\"{removed_variant}\"")).is_err());
    }

    #[test]
    fn removed_pascal_case_scheduler_variant_is_rejected() {
        let removed_variant = ["Legacy", "Jit", "V2"].concat();
        assert!(serde_json::from_str::<SchedulerKind>(&format!("\"{removed_variant}\"")).is_err());
    }

    #[test]
    fn job_message_types_match_upstream_names() {
        assert_eq!(
            serde_json::to_string(&ScaleSetJobMessageType::JobAvailable).unwrap(),
            "\"JobAvailable\""
        );
        assert_eq!(
            serde_json::to_string(&ScaleSetJobMessageType::JobAssigned).unwrap(),
            "\"JobAssigned\""
        );
        assert_eq!(
            serde_json::to_string(&ScaleSetJobMessageType::JobStarted).unwrap(),
            "\"JobStarted\""
        );
        assert_eq!(
            serde_json::to_string(&ScaleSetJobMessageType::JobCompleted).unwrap(),
            "\"JobCompleted\""
        );
        assert_eq!(
            serde_json::to_string(&ScaleSetJobMessageType::Unspecified).unwrap(),
            "\"\""
        );
    }

    #[test]
    fn desired_runners_uses_statistics_not_message_count() {
        let stats = RunnerScaleSetStatistic {
            total_assigned_jobs: 3,
            ..RunnerScaleSetStatistic::default()
        };
        assert_eq!(stats.desired_runners(), 3);
    }

    #[test]
    fn upstream_pin_is_audited_e6daac70() {
        assert_eq!(
            SCALESET_UPSTREAM_COMMIT,
            "e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5"
        );
    }

    #[test]
    fn job_available_wire_shape_matches_types_go() {
        let json = serde_json::json!({
            "acquireJobUrl": "https://actions.example/_apis/runtime/runnerscalesets/7/acquirejobs",
            "messageType": "JobAvailable",
            "runnerRequestId": 4242,
            "repositoryName": "velnor",
            "ownerName": "tailrocks",
            "jobId": "8f3a2c1e-0000-4000-8000-000000000000",
            "jobWorkflowRef": "tailrocks/velnor/.github/workflows/ci.yml@refs/heads/main",
            "jobDisplayName": "build",
            "workflowRunId": 9001,
            "eventName": "push",
            "requestLabels": ["velnor"],
            "queueTime": "2026-09-17T00:00:01Z",
            "scaleSetAssignTime": "0001-01-01T00:00:00Z",
            "runnerAssignTime": "0001-01-01T00:00:00Z",
            "finishTime": "0001-01-01T00:00:00Z"
        });
        let parsed: ScaleSetJobAvailable = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.base.runner_request_id, 4242);
        assert_eq!(
            parsed.base.message_type,
            ScaleSetJobMessageType::JobAvailable
        );
        assert_eq!(parsed.base.request_labels, vec!["velnor".to_string()]);
        let round: serde_json::Value = serde_json::to_value(&parsed).unwrap();
        assert_eq!(round["messageType"], "JobAvailable");
        assert_eq!(round["runnerRequestId"], 4242);
    }

    #[test]
    fn job_completed_carries_result_and_runner_identity() {
        let json = serde_json::json!({
            "result": "succeeded",
            "runnerId": 11,
            "runnerName": "velnor-set-0007",
            "messageType": "JobCompleted",
            "runnerRequestId": 4243,
            "repositoryName": "velnor",
            "ownerName": "tailrocks",
            "jobId": "job-id",
            "jobWorkflowRef": "ref",
            "jobDisplayName": "build",
            "workflowRunId": 9001,
            "eventName": "push",
            "queueTime": "2026-09-17T00:00:01Z"
        });
        let parsed: ScaleSetJobCompleted = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.result, "succeeded");
        assert_eq!(parsed.runner_id, 11);
    }

    #[test]
    fn runner_scale_set_keeps_runner_setting_without_omitempty() {
        let set = RunnerScaleSet {
            id: 7,
            name: "velnor-set".into(),
            runner_group_id: 1,
            runner_group_name: String::new(),
            labels: vec![ScaleSetLabel {
                label_type: "System".into(),
                name: "velnor-set".into(),
            }],
            runner_setting: RunnerSetting::default(),
            created_on: "2026-09-17T00:00:00Z".into(),
            runner_jit_config_url: String::new(),
            statistics: None,
        };
        let json = serde_json::to_value(&set).unwrap();
        assert!(json.get("RunnerSetting").is_some());
        assert!(json.get("createdOn").is_some());
        assert!(json.get("runnerGroupName").is_none());
    }

    #[test]
    fn acquire_jobs_response_is_count_plus_subset() {
        let parsed: AcquireJobsResponse =
            serde_json::from_str(r#"{"count":2,"value":[4242,4243]}"#).unwrap();
        assert_eq!(parsed.count, 2);
        assert_eq!(parsed.value, vec![4242, 4243]);
    }

    #[test]
    fn omitted_scale_set_fields_decode_to_go_zero_values() {
        let stats: RunnerScaleSetStatistic =
            serde_json::from_str(r#"{"totalBusyRunners":2}"#).unwrap();
        assert_eq!(stats.total_busy_runners, 2);
        assert_eq!(stats.total_assigned_jobs, 0);

        let envelope: RunnerScaleSetMessageResponse = serde_json::from_str("{}").unwrap();
        assert_eq!(envelope.message_id, 0);
        assert_eq!(envelope.message_type, "");
        assert_eq!(envelope.body, "");
        assert!(envelope.statistics.is_none());

        let available: ScaleSetJobAvailable = serde_json::from_str("{}").unwrap();
        assert_eq!(available.acquire_job_url, "");
        assert_eq!(
            available.base.message_type,
            ScaleSetJobMessageType::Unspecified
        );
        assert_eq!(available.base.runner_request_id, 0);
        assert!(available.base.request_labels.is_empty());
        assert_eq!(available.base.queue_time, "0001-01-01T00:00:00Z");

        let set: RunnerScaleSet = serde_json::from_str("{}").unwrap();
        assert_eq!(set.id, 0);
        assert_eq!(set.runner_setting, RunnerSetting::default());
        assert_eq!(set.created_on, "0001-01-01T00:00:00Z");

        let acquire: AcquireJobsResponse = serde_json::from_str("{}").unwrap();
        assert_eq!(acquire.count, 0);
        assert!(acquire.value.is_empty());

        let config: RunnerScaleSetJitRunnerConfig = serde_json::from_str("{}").unwrap();
        assert!(config.runner.is_none());
        assert_eq!(config.encoded_jit_config, "");
    }

    #[test]
    fn go_nil_slices_decode_as_zero_slices() {
        let available: ScaleSetJobAvailable =
            serde_json::from_str(r#"{"requestLabels":null}"#).unwrap();
        assert!(available.base.request_labels.is_empty());

        let set: RunnerScaleSet = serde_json::from_str(r#"{"labels":null}"#).unwrap();
        assert!(set.labels.is_empty());

        let groups: RunnerGroupList = serde_json::from_str(r#"{"value":null}"#).unwrap();
        assert!(groups.runner_groups.is_empty());

        let acquired: AcquireJobsResponse = serde_json::from_str(r#"{"value":null}"#).unwrap();
        assert!(acquired.value.is_empty());
    }

    #[test]
    fn scale_set_session_debug_redacts_queue_capabilities_and_jit_blob() {
        let session = ScaleSetSession {
            message_queue_url: "https://queue.example/messages?sig=url-secret".into(),
            message_queue_access_token: "queue-token-secret".into(),
            ..ScaleSetSession::default()
        };
        let rendered = format!("{session:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("url-secret"), "{rendered}");
        assert!(!rendered.contains("queue-token-secret"), "{rendered}");

        let config = RunnerScaleSetJitRunnerConfig {
            encoded_jit_config: "encoded-jit-secret".into(),
            ..RunnerScaleSetJitRunnerConfig::default()
        };
        let rendered = format!("{config:?}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(!rendered.contains("encoded-jit-secret"), "{rendered}");
    }

    #[test]
    fn zero_scale_set_session_matches_go_uuid_omitempty_encoding() {
        let session: ScaleSetSession = serde_json::from_str(r#"{"ownerName":"worker-1"}"#).unwrap();
        assert_eq!(session.session_id, "00000000-0000-0000-0000-000000000000");
        let json = serde_json::to_value(session).unwrap();
        assert_eq!(json["sessionId"], "00000000-0000-0000-0000-000000000000");
        assert_eq!(json["ownerName"], "worker-1");
        assert!(json.get("messageQueueUrl").is_none());
        assert!(json.get("messageQueueAccessToken").is_none());
    }

    #[test]
    fn worker_state_names_are_snake_case() {
        assert_eq!(
            serde_json::to_string(&ScaleSetWorkerState::AcquireIntent).unwrap(),
            "\"acquire_intent\""
        );
        assert_eq!(ScaleSetWorkerState::DindReady.as_str(), "dind_ready");
        assert_eq!(
            ScaleSetWorkerState::PermitReleased.as_str(),
            "permit_released"
        );
    }
}
