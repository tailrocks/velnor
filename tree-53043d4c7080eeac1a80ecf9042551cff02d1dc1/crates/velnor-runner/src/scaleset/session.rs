//! Message-session client (`session_client.go` at [`crate::scaleset::UPSTREAM_COMMIT`]).
//!
//! Mirrors `MessageSessionClient`: session create/refresh/close, the long
//! poll (`GET {queue}?lastMessageId={n}` + `X-ScaleSetMaxCapacity`, 202 →
//! `None`), ACK (`DELETE {queue}/{id}`), and `AcquireJobs`. Every queue call
//! refreshes once on the 401 queue-token signal and retries exactly once;
//! refresh wins are short-circuited when another task already renewed.

use std::sync::Arc;

use reqwest::{Method, StatusCode};
use tokio::sync::{Mutex, RwLock};
use url::Url;
use velnor_model::{
    AcquireJobsResponse, RunnerScaleSetMessage, RunnerScaleSetMessageResponse, ScaleSetJobAssigned,
    ScaleSetJobAvailable, ScaleSetJobCompleted, ScaleSetJobStarted, ScaleSetSession,
    SCALESET_API_VERSION, SCALESET_ENDPOINT, SCALESET_MAX_CAPACITY_HEADER,
};

use crate::scaleset::client::ScaleSetClient;
use crate::scaleset::errors::{ScaleSetError, ScaleSetFault};

/// Dispatched poll result (`RunnerScaleSetMessage`).
pub type ParsedMessage = RunnerScaleSetMessage;

/// Envelope type the parser accepts (`RunnerScaleSetJobMessages`).
pub const JOB_MESSAGES_ENVELOPE: &str = "RunnerScaleSetJobMessages";

struct SessionInner {
    client: ScaleSetClient,
    scale_set_id: i32,
    owner: String,
    session: RwLock<ScaleSetSession>,
    refresh_lock: Mutex<()>,
}

impl std::fmt::Debug for SessionInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionInner")
            .field("scale_set_id", &self.scale_set_id)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// Message session client (`MessageSessionClient`). Cheap to clone.
#[derive(Debug, Clone)]
pub struct MessageSessionClient {
    inner: Arc<SessionInner>,
}

impl MessageSessionClient {
    /// Mirror of `Client.MessageSessionClient`: create the queue session
    /// (`POST .../sessions` with `{ownerName}`, expects 200).
    pub async fn create(
        client: &ScaleSetClient,
        scale_set_id: i32,
        owner: &str,
    ) -> Result<Self, ScaleSetError> {
        let session = ScaleSetSession {
            owner_name: owner.to_string(),
            ..ScaleSetSession::default()
        };
        let body = serde_json::to_vec(&session).map_err(|error| {
            ScaleSetError::Local(format!("failed to marshal new session: {error}"))
        })?;
        let response = client
            .actions_service_request(
                Method::POST,
                &format!("/{SCALESET_ENDPOINT}/{scale_set_id}/sessions"),
                &[],
                Some(body),
            )
            .await
            .map_err(|error| {
                ScaleSetError::Local(format!("failed to do the session request: {error}"))
            })?;
        if response.status != StatusCode::OK {
            return Err(ScaleSetError::Local(format!(
                "failed to do the session request: unexpected status code {}",
                response.status
            )));
        }
        let created: ScaleSetSession = response.decode("session response").map_err(|error| {
            ScaleSetError::Local(format!("failed to unmarshal response body: {error}"))
        })?;
        Ok(Self {
            inner: Arc::new(SessionInner {
                client: client.clone(),
                scale_set_id,
                owner: owner.to_string(),
                session: RwLock::new(created),
                refresh_lock: Mutex::new(()),
            }),
        })
    }

    /// Mirror of `Session()`: current session snapshot.
    pub async fn session(&self) -> ScaleSetSession {
        self.inner.session.read().await.clone()
    }

    /// Mirror of `Close` (`DELETE .../sessions/{id}`, expects 204).
    pub async fn close(&self) -> Result<(), ScaleSetError> {
        let session = self.session().await;
        let response = self
            .inner
            .client
            .actions_service_request(
                Method::DELETE,
                &format!(
                    "/{}/{}/sessions/{}",
                    SCALESET_ENDPOINT, self.inner.scale_set_id, session.session_id
                ),
                &[],
                None,
            )
            .await?;
        if response.status != StatusCode::NO_CONTENT {
            return Err(response.failed(
                None,
                &format!("unexpected status code: {}", response.status.as_u16()),
            ));
        }
        Ok(())
    }

    /// Mirror of `refreshMessageSession` (PATCH, expects 200): mutex-guarded
    /// with the stale-session short-circuit — when the stored session no
    /// longer matches the expired one, another task already renewed.
    async fn refresh_message_session(
        &self,
        expired: &ScaleSetSession,
    ) -> Result<(), ScaleSetError> {
        let _guard = self.inner.refresh_lock.lock().await;
        let current = self.inner.session.read().await.clone();
        if current.session_id != expired.session_id
            || current.message_queue_access_token != expired.message_queue_access_token
        {
            return Ok(());
        }
        let response = self
            .inner
            .client
            .actions_service_request(
                Method::PATCH,
                &format!(
                    "/{}/{}/sessions/{}",
                    SCALESET_ENDPOINT, self.inner.scale_set_id, current.session_id
                ),
                &[],
                None,
            )
            .await
            .map_err(|error| {
                ScaleSetError::Local(format!("failed to do the session request: {error}"))
            })?;
        if response.status != StatusCode::OK {
            return Err(ScaleSetError::Local(format!(
                "failed to do the session request: unexpected status code {}",
                response.status
            )));
        }
        let refreshed: ScaleSetSession = response.decode("session response").map_err(|error| {
            ScaleSetError::Local(format!("failed to unmarshal response body: {error}"))
        })?;
        *self.inner.session.write().await = refreshed;
        Ok(())
    }

    /// Mirror of `GetMessage`: long poll with `lastMessageId` (iff > 0) and
    /// `X-ScaleSetMaxCapacity`; 202 → `Ok(None)`; 401 → refresh + retry once.
    pub async fn get_message(
        &self,
        last_message_id: i32,
        max_capacity: i32,
    ) -> Result<Option<ParsedMessage>, ScaleSetError> {
        let session = self.session().await;
        match self
            .get_message_inner(&session, last_message_id, max_capacity)
            .await
        {
            Ok(message) => Ok(message),
            Err(error) if !error.is_token_expired() => Err(ScaleSetError::Local(format!(
                "failed to get next message: {error}"
            ))),
            Err(_) => {
                self.refresh_message_session(&session)
                    .await
                    .map_err(|error| {
                        ScaleSetError::Local(format!("failed to refresh message session: {error}"))
                    })?;
                let renewed = self.session().await;
                self.get_message_inner(&renewed, last_message_id, max_capacity)
                    .await
            }
        }
    }

    async fn get_message_inner(
        &self,
        session: &ScaleSetSession,
        last_message_id: i32,
        max_capacity: i32,
    ) -> Result<Option<ParsedMessage>, ScaleSetError> {
        let mut url = Url::parse(&session.message_queue_url).map_err(|error| {
            ScaleSetError::Local(format!("failed to parse message queue url: {error}"))
        })?;
        if last_message_id > 0 {
            url.query_pairs_mut()
                .append_pair("lastMessageId", &last_message_id.to_string());
        }
        let user_agent = self.inner.client.user_agent().await;
        let response = self
            .inner
            .client
            .execute_queue_request(
                Method::GET,
                url,
                vec![
                    (
                        "Accept".to_string(),
                        format!("application/json; api-version={SCALESET_API_VERSION}"),
                    ),
                    (
                        "Authorization".to_string(),
                        format!("Bearer {}", session.message_queue_access_token),
                    ),
                    ("User-Agent".to_string(), user_agent),
                    (
                        SCALESET_MAX_CAPACITY_HEADER.to_string(),
                        max_capacity.to_string(),
                    ),
                ],
            )
            .await
            .map_err(|error| {
                ScaleSetError::Local(format!("failed to issue the request: {error}"))
            })?;
        match response.status {
            StatusCode::ACCEPTED => Ok(None),
            StatusCode::OK => parse_message_response(&response.body)
                .map(Some)
                .map_err(|error| {
                    response.failed(None, &format!("failed to parse message response: {error}"))
                }),
            StatusCode::UNAUTHORIZED => Err(response.failed(
                Some(ScaleSetFault::MessageQueueTokenExpired),
                "token expired",
            )),
            unexpected => {
                Err(response.failed(None, &format!("unexpected status code {unexpected}")))
            }
        }
    }

    /// Mirror of `DeleteMessage`: ACK-after-processing (`DELETE
    /// {queue}/{id}`, expects 204); 401 → refresh + retry once.
    pub async fn delete_message(&self, message_id: i32) -> Result<(), ScaleSetError> {
        let session = self.session().await;
        match self.delete_message_inner(&session, message_id).await {
            Ok(()) => Ok(()),
            Err(error) if !error.is_token_expired() => Err(ScaleSetError::Local(format!(
                "failed to delete message: {error}"
            ))),
            Err(_) => {
                self.refresh_message_session(&session)
                    .await
                    .map_err(|error| {
                        ScaleSetError::Local(format!("failed to refresh message session: {error}"))
                    })?;
                let renewed = self.session().await;
                self.delete_message_inner(&renewed, message_id).await
            }
        }
    }

    async fn delete_message_inner(
        &self,
        session: &ScaleSetSession,
        message_id: i32,
    ) -> Result<(), ScaleSetError> {
        let mut url = Url::parse(&session.message_queue_url).map_err(|error| {
            ScaleSetError::Local(format!("failed to parse message queue url: {error}"))
        })?;
        // Mirror `u.Path = fmt.Sprintf("%s/%d", u.Path, messageID)`.
        url.set_path(&format!(
            "{}/{message_id}",
            url.path().trim_end_matches('/')
        ));
        let user_agent = self.inner.client.user_agent().await;
        let response = self
            .inner
            .client
            .execute_queue_request(
                Method::DELETE,
                url,
                vec![
                    ("Content-Type".to_string(), "application/json".to_string()),
                    (
                        "Authorization".to_string(),
                        format!("Bearer {}", session.message_queue_access_token),
                    ),
                    ("User-Agent".to_string(), user_agent),
                ],
            )
            .await
            .map_err(|error| {
                ScaleSetError::Local(format!("failed to issue the request: {error}"))
            })?;
        if response.status == StatusCode::NO_CONTENT {
            return Ok(());
        }
        if response.status != StatusCode::UNAUTHORIZED {
            return Err(
                response.failed(None, &format!("unexpected status code {}", response.status))
            );
        }
        Err(response.failed(
            Some(ScaleSetFault::MessageQueueTokenExpired),
            "token expired",
        ))
    }

    /// Mirror of `AcquireJobs`: `POST .../acquirejobs` with the queue token
    /// (not the admin token); body is the raw `requestIDs` array; returns
    /// the acquired subset. 401 → refresh + retry once.
    pub async fn acquire_jobs(&self, request_ids: &[i64]) -> Result<Vec<i64>, ScaleSetError> {
        let session = self.session().await;
        match self.acquire_jobs_inner(&session, request_ids).await {
            Ok(ids) => Ok(ids),
            Err(error) if !error.is_token_expired() => Err(ScaleSetError::Local(format!(
                "failed to acquire jobs: {error}"
            ))),
            Err(_) => {
                self.refresh_message_session(&session)
                    .await
                    .map_err(|error| {
                        ScaleSetError::Local(format!("failed to refresh message session: {error}"))
                    })?;
                let renewed = self.session().await;
                self.acquire_jobs_inner(&renewed, request_ids).await
            }
        }
    }

    async fn acquire_jobs_inner(
        &self,
        session: &ScaleSetSession,
        request_ids: &[i64],
    ) -> Result<Vec<i64>, ScaleSetError> {
        let body = serde_json::to_vec(request_ids).map_err(|error| {
            ScaleSetError::Local(format!("failed to marshal request ids: {error}"))
        })?;
        let response = self
            .inner
            .client
            .actions_service_request_with_bearer(
                Method::POST,
                &format!(
                    "/{}/{}/acquirejobs",
                    SCALESET_ENDPOINT, self.inner.scale_set_id
                ),
                &[],
                Some(body),
                &session.message_queue_access_token,
            )
            .await
            .map_err(|error| {
                ScaleSetError::Local(format!("failed to issue acquire jobs request: {error}"))
            })?;
        if response.status == StatusCode::UNAUTHORIZED {
            return Err(response.failed(
                Some(ScaleSetFault::MessageQueueTokenExpired),
                "token expired",
            ));
        }
        if response.status != StatusCode::OK {
            return Err(
                response.failed(None, &format!("unexpected status code {}", response.status))
            );
        }
        response
            .decode::<AcquireJobsResponse>("acquire jobs response")
            .map(|decoded| decoded.value)
    }

    /// Scale-set ID this session polls.
    #[must_use]
    pub fn scale_set_id(&self) -> i32 {
        self.inner.scale_set_id
    }

    /// Session owner name.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.inner.owner
    }
}

/// Mirror of `parseRunnerScaleSetMessageResponse`: reject foreign envelopes,
/// dispatch batched messages on `messageType`, ignore unknown batched types
/// (upstream `default:` is empty).
pub fn parse_message_response(body: &[u8]) -> Result<ParsedMessage, String> {
    let envelope: RunnerScaleSetMessageResponse = serde_json::from_slice(body)
        .map_err(|error| format!("failed to decode runner scale set message response: {error}"))?;
    if envelope.message_type != JOB_MESSAGES_ENVELOPE {
        return Err(format!(
            "unsupported message type: {}",
            envelope.message_type
        ));
    }
    let mut message = RunnerScaleSetMessage {
        message_id: envelope.message_id,
        statistics: envelope.statistics,
        ..RunnerScaleSetMessage::default()
    };
    if envelope.body.is_empty() {
        return Ok(message);
    }
    let batched: Vec<serde_json::Value> = serde_json::from_str(&envelope.body)
        .map_err(|error| format!("failed to unmarshal batched messages: {error}"))?;
    for raw in &batched {
        let kind: BatchedKind = serde_json::from_value(raw.clone())
            .map_err(|error| format!("failed to decode job message type: {error}"))?;
        match kind.message_type {
            BatchedMessageType::JobAvailable => {
                let job: ScaleSetJobAvailable = serde_json::from_value(raw.clone())
                    .map_err(|error| format!("failed to decode job available: {error}"))?;
                message.job_available_messages.push(job);
            }
            BatchedMessageType::JobAssigned => {
                let job: ScaleSetJobAssigned = serde_json::from_value(raw.clone())
                    .map_err(|error| format!("failed to decode job assigned: {error}"))?;
                message.job_assigned_messages.push(job);
            }
            BatchedMessageType::JobStarted => {
                let job: ScaleSetJobStarted = serde_json::from_value(raw.clone())
                    .map_err(|error| format!("could not decode job started message. {error}"))?;
                message.job_started_messages.push(job);
            }
            BatchedMessageType::JobCompleted => {
                let job: ScaleSetJobCompleted = serde_json::from_value(raw.clone())
                    .map_err(|error| format!("failed to decode job completed: {error}"))?;
                message.job_completed_messages.push(job);
            }
            // Upstream `default:` is empty: future message types are ignored
            // here and surfaced through the unknown-event reconcile path.
            BatchedMessageType::Unknown => {}
        }
    }
    Ok(message)
}

#[derive(Debug, serde::Deserialize)]
struct BatchedKind {
    #[serde(rename = "messageType")]
    message_type: BatchedMessageType,
}

/// Batched dispatch lenient on unknown types (upstream ignores them).
#[derive(Debug, serde::Deserialize)]
enum BatchedMessageType {
    JobAvailable,
    JobAssigned,
    JobStarted,
    JobCompleted,
    #[serde(other)]
    Unknown,
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

    fn job_base(message_type: &str, request_id: i64) -> serde_json::Value {
        serde_json::json!({
            "messageType": message_type,
            "runnerRequestId": request_id,
            "repositoryName": "velnor",
            "ownerName": "tailrocks",
            "jobId": "job-id",
            "jobWorkflowRef": "ref",
            "jobDisplayName": "build",
            "workflowRunId": 9001,
            "eventName": "push",
            "requestLabels": ["velnor"],
            "queueTime": "2026-09-17T00:00:01Z"
        })
    }

    #[test]
    fn empty_body_parses_to_stats_only_message() {
        let parsed =
            parse_message_response(br#"{"messageId":1,"messageType":"RunnerScaleSetJobMessages"}"#)
                .unwrap();
        assert_eq!(parsed.message_id, 1);
        assert!(parsed.statistics.is_none());
        assert!(parsed.job_available_messages.is_empty());
    }

    #[test]
    fn foreign_envelope_is_rejected() {
        let error = parse_message_response(br#"{"messageId":1,"messageType":"RunnerJobRequest"}"#)
            .unwrap_err();
        assert!(error.contains("unsupported message type"), "{error}");
    }

    #[test]
    fn batch_dispatches_all_four_kinds() {
        let mut available = job_base("JobAvailable", 1);
        available["acquireJobUrl"] = serde_json::json!("https://a.example/acquire");
        let mut started = job_base("JobStarted", 3);
        started["runnerId"] = serde_json::json!(11);
        started["runnerName"] = serde_json::json!("velnor-set-0001");
        let mut completed = job_base("JobCompleted", 4);
        completed["runnerId"] = serde_json::json!(12);
        completed["runnerName"] = serde_json::json!("velnor-set-0002");
        completed["result"] = serde_json::json!("succeeded");
        let envelope = serde_json::json!({
            "messageId": 9,
            "messageType": "RunnerScaleSetJobMessages",
            "body": serde_json::to_string(&vec![
                available,
                job_base("JobAssigned", 2),
                started,
                completed,
                serde_json::json!({"messageType": "JobFuture", "runnerRequestId": 5}),
            ]).unwrap(),
            "statistics": {
                "totalAvailableJobs": 1,
                "totalAcquiredJobs": 0,
                "totalAssignedJobs": 2,
                "totalRunningJobs": 1,
                "totalRegisteredRunners": 2,
                "totalBusyRunners": 1,
                "totalIdleRunners": 1
            }
        });
        let parsed = parse_message_response(&serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert_eq!(parsed.message_id, 9);
        assert_eq!(parsed.job_available_messages.len(), 1);
        assert_eq!(parsed.job_available_messages[0].base.runner_request_id, 1);
        assert_eq!(parsed.job_assigned_messages.len(), 1);
        assert_eq!(parsed.job_started_messages.len(), 1);
        assert_eq!(parsed.job_completed_messages.len(), 1);
        assert_eq!(parsed.job_completed_messages[0].result, "succeeded");
        assert_eq!(parsed.statistics.unwrap().desired_runners(), 2);
    }
}
