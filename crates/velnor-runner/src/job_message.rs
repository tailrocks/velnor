#![allow(dead_code)]

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const PIPELINE_AGENT_JOB_REQUEST: &str = "PipelineAgentJobRequest";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentJobRequestMessage {
    #[serde(rename = "MessageType", alias = "messageType")]
    pub message_type: String,
    #[serde(rename = "Plan", alias = "plan")]
    pub plan: TaskOrchestrationPlanReference,
    #[serde(rename = "Timeline", alias = "timeline")]
    pub timeline: TimelineReference,
    #[serde(rename = "JobId", alias = "jobId")]
    pub job_id: String,
    #[serde(rename = "JobDisplayName", alias = "jobDisplayName")]
    pub job_display_name: String,
    #[serde(rename = "JobName", alias = "jobName")]
    pub job_name: Option<String>,
    #[serde(rename = "RequestId", alias = "requestId")]
    pub request_id: i64,
    #[serde(default, rename = "LockedUntil", alias = "lockedUntil")]
    pub locked_until: Option<String>,
    #[serde(default, rename = "Variables", alias = "variables")]
    pub variables: BTreeMap<String, VariableValue>,
    #[serde(default, rename = "Mask", alias = "mask")]
    pub mask: Vec<MaskHint>,
    #[serde(default, rename = "Resources", alias = "resources")]
    pub resources: JobResources,
    #[serde(default, rename = "Steps", alias = "steps")]
    pub steps: Vec<ActionStep>,
    #[serde(
        default,
        rename = "EnvironmentVariables",
        alias = "environmentVariables"
    )]
    pub environment_variables: Vec<Value>,
    #[serde(default, rename = "Defaults", alias = "defaults")]
    pub defaults: Vec<Value>,
    #[serde(default, rename = "JobContainer", alias = "jobContainer")]
    pub job_container: Option<Value>,
    #[serde(
        default,
        rename = "JobServiceContainers",
        alias = "jobServiceContainers"
    )]
    pub job_service_containers: Option<Value>,
    #[serde(default, rename = "JobOutputs", alias = "jobOutputs")]
    pub job_outputs: Option<Value>,
    #[serde(default, rename = "Workspace", alias = "workspace")]
    pub workspace: Option<Value>,
    #[serde(default, rename = "ContextData", alias = "contextData")]
    pub context_data: BTreeMap<String, Value>,
    #[serde(default, rename = "ActionsEnvironment", alias = "actionsEnvironment")]
    pub actions_environment: Option<Value>,
    #[serde(default, rename = "BillingOwnerId", alias = "billingOwnerId")]
    pub billing_owner_id: Option<String>,
    #[serde(default, rename = "dependencies")]
    pub actions_dependencies: Vec<String>,
}

impl AgentJobRequestMessage {
    pub fn parse_json(body: &str) -> Result<Self> {
        serde_json::from_str(body).context("parse AgentJobRequestMessage")
    }

    pub fn system_connection(&self) -> Option<&ServiceEndpoint> {
        self.resources
            .endpoints
            .iter()
            .find(|endpoint| endpoint.name.eq_ignore_ascii_case("SystemVssConnection"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskOrchestrationPlanReference {
    #[serde(default, rename = "ScopeIdentifier", alias = "scopeIdentifier")]
    pub scope_identifier: Option<String>,
    #[serde(default, rename = "PlanType", alias = "planType")]
    pub plan_type: Option<String>,
    #[serde(default, rename = "Version", alias = "version")]
    pub version: Option<i32>,
    #[serde(rename = "PlanId", alias = "planId")]
    pub plan_id: String,
    #[serde(default, rename = "PlanGroup", alias = "planGroup")]
    pub plan_group: Option<String>,
    #[serde(default, rename = "ArtifactUri", alias = "artifactUri")]
    pub artifact_uri: Option<String>,
    #[serde(default, rename = "ArtifactLocation", alias = "artifactLocation")]
    pub artifact_location: Option<String>,
    #[serde(default, rename = "Definition", alias = "definition")]
    pub definition: Option<Value>,
    #[serde(default, rename = "Owner", alias = "owner")]
    pub owner: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineReference {
    #[serde(rename = "Id", alias = "id")]
    pub id: String,
    #[serde(default, rename = "ChangeId", alias = "changeId")]
    pub change_id: Option<i32>,
    #[serde(default, rename = "Location", alias = "location")]
    pub location: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobResources {
    #[serde(default, rename = "Endpoints", alias = "endpoints")]
    pub endpoints: Vec<ServiceEndpoint>,
    #[serde(default, rename = "Repositories", alias = "repositories")]
    pub repositories: Vec<RepositoryResource>,
    #[serde(default, rename = "Containers", alias = "containers")]
    pub containers: Vec<ContainerResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceEndpoint {
    #[serde(rename = "Name", alias = "name")]
    pub name: String,
    #[serde(default, rename = "Url", alias = "url")]
    pub url: Option<String>,
    #[serde(default, rename = "Authorization", alias = "authorization")]
    pub authorization: Option<EndpointAuthorization>,
    #[serde(default, rename = "Data", alias = "data")]
    pub data: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointAuthorization {
    #[serde(default, rename = "Scheme", alias = "scheme")]
    pub scheme: Option<String>,
    #[serde(default, rename = "Parameters", alias = "parameters")]
    pub parameters: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryResource {
    #[serde(default, rename = "Alias", alias = "alias")]
    pub alias: Option<String>,
    #[serde(default, rename = "Name", alias = "name")]
    pub name: Option<String>,
    #[serde(default, rename = "Ref", alias = "ref")]
    pub git_ref: Option<String>,
    #[serde(default, rename = "Version", alias = "version")]
    pub version: Option<String>,
    #[serde(default, rename = "Url", alias = "url")]
    pub url: Option<String>,
    #[serde(default, rename = "Properties", alias = "properties")]
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerResource {
    #[serde(default, rename = "Alias", alias = "alias")]
    pub alias: Option<String>,
    #[serde(default, rename = "Image", alias = "image")]
    pub image: Option<String>,
    #[serde(default, rename = "Options", alias = "options")]
    pub options: Option<String>,
    #[serde(default, rename = "Ports", alias = "ports")]
    pub ports: BTreeMap<String, String>,
    #[serde(
        default,
        rename = "EnvironmentVariables",
        alias = "environmentVariables"
    )]
    pub environment_variables: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableValue {
    #[serde(default, rename = "Value", alias = "value")]
    pub value: Option<String>,
    #[serde(default, rename = "IsSecret", alias = "isSecret")]
    pub is_secret: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaskHint {
    #[serde(default, rename = "Type", alias = "type")]
    pub r#type: Option<String>,
    #[serde(default, rename = "Value", alias = "value")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionStep {
    #[serde(default, rename = "Type", alias = "type")]
    pub r#type: Option<Value>,
    #[serde(default, rename = "Id", alias = "id")]
    pub id: Option<String>,
    #[serde(default, rename = "Name", alias = "name")]
    pub name: Option<String>,
    #[serde(
        default,
        rename = "DisplayName",
        alias = "displayName",
        alias = "display_name"
    )]
    pub display_name: Option<String>,
    /// Template token form of the display name — the broker sends names with
    /// (or without) expressions here and leaves DisplayName null; the runner
    /// evaluates it at runtime (actions/runner GenerateDisplayName).
    #[serde(
        default,
        rename = "DisplayNameToken",
        alias = "displayNameToken",
        alias = "display_name_token"
    )]
    pub display_name_token: Option<Value>,
    #[serde(default = "default_true", rename = "Enabled", alias = "enabled")]
    pub enabled: bool,
    #[serde(default, rename = "Condition", alias = "condition")]
    pub condition: Option<String>,
    #[serde(default, rename = "ContinueOnError", alias = "continueOnError")]
    pub continue_on_error: Option<Value>,
    #[serde(default, rename = "TimeoutInMinutes", alias = "timeoutInMinutes")]
    pub timeout_in_minutes: Option<Value>,
    #[serde(
        default,
        rename = "ContextName",
        alias = "contextName",
        alias = "context_name"
    )]
    pub context_name: Option<String>,
    #[serde(default, rename = "Reference", alias = "reference")]
    pub reference: Option<ActionStepDefinitionReference>,
    #[serde(default, rename = "Environment", alias = "environment")]
    pub environment: Option<Value>,
    #[serde(default, rename = "Inputs", alias = "inputs")]
    pub inputs: Option<Value>,
}

impl ActionStep {
    /// The display name template: the plain DisplayName when the server sent
    /// one (legacy/back-compat), else the DisplayNameToken rendered to a
    /// `${{ ... }}` template string for runtime evaluation.
    pub fn display_name_template(&self) -> Option<String> {
        if let Some(name) = self
            .display_name
            .as_deref()
            .filter(|name| !name.is_empty() && !name.starts_with("__"))
        {
            return Some(name.to_string());
        }
        self.display_name_token
            .as_ref()
            .and_then(display_token_template)
            .filter(|name| !name.is_empty() && !name.starts_with("__"))
    }

    pub fn reference_type(&self) -> Option<ActionReferenceType> {
        self.reference
            .as_ref()
            .and_then(|reference| reference.r#type)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionStepDefinitionReference {
    #[serde(
        default,
        rename = "Type",
        alias = "type",
        deserialize_with = "deserialize_action_reference_type"
    )]
    pub r#type: Option<ActionReferenceType>,
    #[serde(default, rename = "Name", alias = "name")]
    pub name: Option<String>,
    #[serde(default, rename = "Ref", alias = "ref")]
    pub git_ref: Option<String>,
    #[serde(default, rename = "RepositoryType", alias = "repositoryType")]
    pub repository_type: Option<String>,
    #[serde(default, rename = "Path", alias = "path")]
    pub path: Option<String>,
    #[serde(default, rename = "Image", alias = "image")]
    pub image: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ActionReferenceType {
    Repository,
    ContainerRegistry,
    Script,
}

/// Render a scalar template token to display text: literals verbatim,
/// expression tokens back to `${{ ... }}` so the executor's resolver
/// evaluates them with the job contexts.
fn display_token_template(token: &Value) -> Option<String> {
    match token {
        Value::String(value) => Some(value.clone()),
        Value::Object(object) => {
            if let Some(expr) = object
                .get("expr")
                .or_else(|| object.get("Expr"))
                .and_then(Value::as_str)
            {
                return Some(format!("${{{{ {expr} }}}}"));
            }
            object
                .get("lit")
                .or_else(|| object.get("Lit"))
                .or_else(|| object.get("value"))
                .or_else(|| object.get("Value"))
                .and_then(display_token_template)
        }
        _ => None,
    }
}

fn default_true() -> bool {
    true
}

fn deserialize_action_reference_type<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ActionReferenceType>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };

    match value {
        Value::Number(number) => match number.as_i64() {
            Some(1) => Ok(Some(ActionReferenceType::Repository)),
            Some(2) => Ok(Some(ActionReferenceType::ContainerRegistry)),
            Some(3) => Ok(Some(ActionReferenceType::Script)),
            _ => Ok(None),
        },
        Value::String(value) => {
            if value.eq_ignore_ascii_case("repository") {
                Ok(Some(ActionReferenceType::Repository))
            } else if value.eq_ignore_ascii_case("containerregistry")
                || value.eq_ignore_ascii_case("container_registry")
                || value.eq_ignore_ascii_case("container-registry")
            {
                Ok(Some(ActionReferenceType::ContainerRegistry))
            } else if value.eq_ignore_ascii_case("script") {
                Ok(Some(ActionReferenceType::Script))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_template_prefers_plain_then_token() {
        // Broker reality (jackin-agent-brown dump 2026-06-11): DisplayName is
        // null and the explicit `name:` arrives only as DisplayNameToken.
        let step: ActionStep = serde_json::from_value(serde_json::json!({
            "id": "s1",
            "name": "__docker_login-action",
            "displayName": null,
            "displayNameToken": { "type": 0, "lit": "Login to Docker Hub for base image pulls" },
            "reference": { "type": "Repository", "name": "docker/login-action" }
        }))
        .unwrap();
        assert_eq!(
            step.display_name_template().as_deref(),
            Some("Login to Docker Hub for base image pulls")
        );

        // Expression tokens render back to ${{ }} for runtime evaluation.
        let step: ActionStep = serde_json::from_value(serde_json::json!({
            "id": "s2",
            "name": "__run",
            "displayNameToken": { "type": 3, "expr": "format('Deploy {0}', inputs.env)" },
            "reference": { "type": "Script" }
        }))
        .unwrap();
        assert_eq!(
            step.display_name_template().as_deref(),
            Some("${{ format('Deploy {0}', inputs.env) }}")
        );

        // Plain DisplayName (legacy servers) wins over the token.
        let step: ActionStep = serde_json::from_value(serde_json::json!({
            "id": "s3",
            "displayName": "Plain name",
            "displayNameToken": { "type": 0, "lit": "Token name" },
            "reference": { "type": "Script" }
        }))
        .unwrap();
        assert_eq!(step.display_name_template().as_deref(), Some("Plain name"));

        // Internal placeholders never become display names.
        let step: ActionStep = serde_json::from_value(serde_json::json!({
            "id": "s4",
            "displayName": "__run_2",
            "reference": { "type": "Script" }
        }))
        .unwrap();
        assert_eq!(step.display_name_template(), None);
    }

    const JOB_ID: &str = "11111111-1111-1111-1111-111111111111";
    const PLAN_ID: &str = "22222222-2222-2222-2222-222222222222";
    const TIMELINE_ID: &str = "33333333-3333-3333-3333-333333333333";
    const STEP_ID: &str = "44444444-4444-4444-4444-444444444444";

    #[test]
    fn parses_pipeline_agent_job_request_subset() {
        let body = format!(
            r#"{{
                "MessageType": "PipelineAgentJobRequest",
                "Plan": {{
                    "PlanId": "{PLAN_ID}",
                    "PlanType": "Build",
                    "ScopeIdentifier": "55555555-5555-5555-5555-555555555555",
                    "Version": 8
                }},
                "Timeline": {{
                    "Id": "{TIMELINE_ID}",
                    "ChangeId": 1
                }},
                "JobId": "{JOB_ID}",
                "JobDisplayName": "Check",
                "JobName": "check",
                "RequestId": 123,
                "LockedUntil": "2026-05-31T12:10:00Z",
                "Variables": {{
                    "system.github.token": {{
                        "Value": "secret-token",
                        "IsSecret": true
                    }},
                    "github.repository": {{
                        "Value": "ChainArgos/java-monorepo"
                    }}
                }},
                "Resources": {{
                    "Endpoints": [{{
                        "Name": "SystemVssConnection",
                        "Url": "https://pipelines.actions.githubusercontent.com/abc",
                        "Authorization": {{
                            "Scheme": "OAuth",
                            "Parameters": {{
                                "AccessToken": "job-token"
                            }}
                        }},
                        "Data": {{
                            "GenerateIdTokenUrl": "https://token.actions.githubusercontent.com"
                        }}
                    }}],
                    "Repositories": [{{
                        "Alias": "self",
                        "Name": "ChainArgos/java-monorepo",
                        "Ref": "refs/heads/main",
                        "Version": "abc123",
                        "Properties": {{
                            "cloneUrl": "https://github.com/ChainArgos/java-monorepo.git"
                        }}
                    }}]
                }},
                "Steps": [{{
                    "Type": "Action",
                    "Id": "{STEP_ID}",
                    "Name": "__run",
                    "DisplayName": "Run tests",
                    "Enabled": true,
                    "Condition": "success()",
                    "Reference": {{
                        "Type": "Script"
                    }},
                    "Inputs": {{
                        "script": "cargo test",
                        "shell": "bash",
                        "workingDirectory": "./crates"
                    }}
                }}]
            }}"#
        );

        let message = AgentJobRequestMessage::parse_json(&body).unwrap();

        assert_eq!(message.message_type, PIPELINE_AGENT_JOB_REQUEST);
        assert_eq!(message.request_id, 123);
        assert_eq!(message.job_id, JOB_ID);
        assert_eq!(message.plan.plan_id, PLAN_ID);
        assert_eq!(message.timeline.id, TIMELINE_ID);
        assert!(message.variables["system.github.token"].is_secret);
        assert_eq!(
            message
                .system_connection()
                .unwrap()
                .authorization
                .as_ref()
                .unwrap()
                .parameters["AccessToken"],
            "job-token"
        );
        assert_eq!(message.steps.len(), 1);
        assert_eq!(
            message.steps[0].reference_type(),
            Some(ActionReferenceType::Script)
        );
        assert_eq!(message.steps[0].display_name.as_deref(), Some("Run tests"));
    }

    #[test]
    fn accepts_lower_camel_case_message_fields() {
        let body = format!(
            r#"{{
                "messageType": "PipelineAgentJobRequest",
                "plan": {{ "planId": "{PLAN_ID}" }},
                "timeline": {{ "id": "{TIMELINE_ID}" }},
                "jobId": "{JOB_ID}",
                "jobDisplayName": "Check",
                "requestId": 123,
                "steps": [{{
                    "reference": {{ "type": "Repository", "name": "actions/checkout", "ref": "v4" }}
                }}]
            }}"#
        );

        let message = AgentJobRequestMessage::parse_json(&body).unwrap();

        assert_eq!(message.message_type, PIPELINE_AGENT_JOB_REQUEST);
        assert_eq!(
            message.steps[0].reference.as_ref().unwrap().name.as_deref(),
            Some("actions/checkout")
        );
        assert_eq!(
            message.steps[0].reference_type(),
            Some(ActionReferenceType::Repository)
        );
    }

    #[test]
    fn accepts_snake_case_step_name_fields() {
        let body = format!(
            r#"{{
                "messageType": "PipelineAgentJobRequest",
                "plan": {{ "planId": "{PLAN_ID}" }},
                "timeline": {{ "id": "{TIMELINE_ID}" }},
                "jobId": "{JOB_ID}",
                "jobDisplayName": "Check",
                "requestId": 123,
                "steps": [{{
                    "name": "__run",
                    "display_name": "Install Ansible",
                    "context_name": "install",
                    "reference": {{ "type": "Script" }},
                    "inputs": {{ "script": "pip install ansible-core" }}
                }}]
            }}"#
        );

        let message = AgentJobRequestMessage::parse_json(&body).unwrap();

        assert_eq!(
            message.steps[0].display_name.as_deref(),
            Some("Install Ansible")
        );
        assert_eq!(message.steps[0].context_name.as_deref(), Some("install"));
    }

    #[test]
    fn accepts_numeric_action_reference_type() {
        let body = format!(
            r#"{{
                "messageType": "PipelineAgentJobRequest",
                "plan": {{ "planId": "{PLAN_ID}" }},
                "timeline": {{ "id": "{TIMELINE_ID}" }},
                "jobId": "{JOB_ID}",
                "jobDisplayName": "Check",
                "requestId": 123,
                "steps": [{{
                    "reference": {{ "type": 3 }}
                }}]
            }}"#
        );

        let message = AgentJobRequestMessage::parse_json(&body).unwrap();

        assert_eq!(
            message.steps[0].reference_type(),
            Some(ActionReferenceType::Script)
        );
    }
}
