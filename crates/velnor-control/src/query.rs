//! Read-only resource projection and pagination service.
//!
//! Adapters publish already-authoritative, sanitized resources into this
//! service. Querying only filters those projections; it never reads files,
//! invokes subprocesses, or repairs state.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};
use velnor_model::{
    AnyResource, Event, ExecutionBackendKind, Host, Instance, Job, QueueEntry, RepositoryRef,
    ResourceMeta, Run, RunnerRegistration, Slot, Source, Timestamp,
};

use crate::ports::{PortError, QueryPage, QueryPort, QueryRequest};
use crate::store::{JobSummary, Store};

const MAX_PAGE_SIZE: u32 = 1_000;
const PAGE_PREFIX: &str = "v1:";

/// In-memory read projection with generation-safe opaque page tokens.
#[derive(Clone)]
pub struct QueryService {
    state: Arc<RwLock<QueryState>>,
    durable: Option<DurableProjection>,
}

#[derive(Clone)]
struct DurableProjection {
    store: Arc<Store>,
    instance_slug: String,
}

impl Default for QueryService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
struct QueryState {
    generation: u64,
    resources: Vec<AnyResource>,
}

impl QueryService {
    /// Create an empty projection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(QueryState::default())),
            durable: None,
        }
    }

    /// Create a projection backed by the host-shared operational store.
    #[must_use]
    pub fn with_store(store: Arc<Store>, instance_slug: impl Into<String>) -> Self {
        Self {
            state: Arc::new(RwLock::new(QueryState::default())),
            durable: Some(DurableProjection {
                store,
                instance_slug: instance_slug.into(),
            }),
        }
    }

    /// Replace the projection and advance its cursor generation.
    pub fn replace(&self, mut resources: Vec<AnyResource>) -> Result<(), PortError> {
        if self.durable.is_some() {
            return Err(PortError::Unsupported {
                operation: "replace durable query projection".to_owned(),
            });
        }
        if resources.iter().any(|resource| {
            resource.meta().name.trim().is_empty() || resource.meta().name.len() > 512
        }) {
            return Err(PortError::Invalid {
                field: "resource.meta.name".to_owned(),
                message: "resource names must be 1..512 bytes".to_owned(),
            });
        }
        resources.sort_by(|left, right| {
            (left.kind(), left.meta().name.as_str())
                .cmp(&(right.kind(), right.meta().name.as_str()))
        });
        let mut state = self.state.write().map_err(|_| PortError::Unavailable {
            resource: "query projection".to_owned(),
        })?;
        let generation = state.generation.saturating_add(1);
        state.generation = generation;
        state.resources = resources;
        Ok(())
    }

    /// Current projection generation used by watch/read consumers.
    pub fn generation(&self) -> Result<u64, PortError> {
        self.refresh_durable()?;
        self.state
            .read()
            .map(|state| state.generation)
            .map_err(|_| PortError::Unavailable {
                resource: "query projection".to_owned(),
            })
    }

    fn refresh_durable(&self) -> Result<(), PortError> {
        let Some(durable) = &self.durable else {
            return Ok(());
        };
        let resources = load_durable_resources(&durable.store, &durable.instance_slug)?;
        let mut state = self.state.write().map_err(|_| PortError::Unavailable {
            resource: "query projection".to_owned(),
        })?;
        if state.resources != resources {
            state.generation = state.generation.saturating_add(1);
            state.resources = resources;
        }
        Ok(())
    }
}

impl QueryPort for QueryService {
    fn query(&self, request: QueryRequest) -> Result<QueryPage, PortError> {
        if request.limit == 0 || request.limit > MAX_PAGE_SIZE {
            return Err(PortError::Invalid {
                field: "limit".to_owned(),
                message: format!("must be between 1 and {MAX_PAGE_SIZE}"),
            });
        }
        let since = if let Some(raw) = request.since.as_deref() {
            Some(Timestamp::parse(raw).map_err(|_| PortError::Invalid {
                field: "since".to_owned(),
                message: "must be RFC 3339".to_owned(),
            })?)
        } else {
            None
        };

        validate_selector(request.selector.as_deref(), "selector")?;
        validate_selector(request.field_selector.as_deref(), "field_selector")?;
        let fingerprint = query_fingerprint(&request);
        self.refresh_durable()?;
        let (generation, token_fingerprint, offset) = request
            .page_token
            .as_deref()
            .map(parse_page_token)
            .transpose()?
            .unwrap_or((0, String::new(), 0));
        let state = self.state.read().map_err(|_| PortError::Unavailable {
            resource: "query projection".to_owned(),
        })?;
        if generation != 0 && generation != state.generation {
            return Err(PortError::Conflict {
                operation: "page token generation expired".to_owned(),
            });
        }
        if generation != 0 && token_fingerprint != fingerprint {
            return Err(PortError::Conflict {
                operation: "page token does not match query".to_owned(),
            });
        }

        let page_end = offset.saturating_add(request.limit as usize);
        let mut total = 0_usize;
        let mut page = Vec::with_capacity(request.limit as usize);
        for resource in &state.resources {
            if !resource_kind_matches(resource, &request.resource_kind) {
                continue;
            }
            if since.is_some_and(|at| resource.meta().last_transition_time < at) {
                continue;
            }
            if !matches_selector(resource, request.selector.as_deref())? {
                continue;
            }
            if !matches_field_selector(resource, request.field_selector.as_deref())? {
                continue;
            }
            if total >= offset && total < page_end {
                page.push(resource.clone());
            }
            total = total.saturating_add(1);
            // The page contract needs only one look-ahead match. Avoid
            // rescanning the remainder of a large projection to discover
            // whether a continuation token is needed.
            if total > page_end {
                break;
            }
        }
        let start = offset.min(total);
        let end = start.saturating_add(request.limit as usize).min(total);
        let next_page_token =
            (end < total).then(|| format!("{PAGE_PREFIX}{}:{fingerprint}:{end}", state.generation));
        Ok(QueryPage {
            resources: page,
            next_page_token,
        })
    }
}

fn resource_kind_matches(resource: &AnyResource, requested: &str) -> bool {
    if requested.is_empty() {
        return true;
    }
    resource
        .kind()
        .eq_ignore_ascii_case(canonical_kind(requested))
}

fn canonical_kind(requested: &str) -> &str {
    let normalized = requested.to_ascii_lowercase();
    match normalized.as_str() {
        "host" | "hosts" => "Host",
        "instance" | "instances" => "Instance",
        "slot" | "slots" => "Slot",
        "runner" | "runners" | "runnerregistration" | "runnerregistrations" => "RunnerRegistration",
        "job" | "jobs" => "Job",
        "run" | "runs" => "Run",
        "queue" | "queues" | "queueentry" | "queueentries" => "QueueEntry",
        "event" | "events" => "Event",
        "reservation" | "reservations" => "Reservation",
        "lease" | "leases" => "Lease",
        "capability" | "capabilities" => "Capability",
        "adapter" | "adapters" => "Adapter",
        _ => requested,
    }
}

fn load_durable_resources(
    store: &Store,
    instance_slug: &str,
) -> Result<Vec<AnyResource>, PortError> {
    let mut resources = Vec::new();

    let instance_host = if let Some(row) = store
        .instance_row(instance_slug)
        .map_err(store_query_error)?
    {
        let host = row.host.clone();
        resources.push(AnyResource::Host(Host {
            meta: ResourceMeta::new(&row.host, Source::Local, row.updated_at),
            hostname: row.host.clone(),
            agent_version: (!row.daemon_version.is_empty()).then_some(row.daemon_version.clone()),
            labels: BTreeMap::new(),
        }));
        resources.push(AnyResource::Instance(Instance {
            meta: ResourceMeta::new(&row.instance_slug, Source::Local, row.updated_at),
            host: row.host,
            version: row.daemon_version,
            uptime_ms: None,
            slots_configured: row.slots_configured,
            slots_busy: row.slots_busy,
        }));
        Some(host)
    } else {
        None
    };

    for row in store.slot_rows(instance_slug).map_err(store_query_error)? {
        resources.push(AnyResource::Slot(Slot {
            meta: ResourceMeta::new(&row.identity.slot_id.0, Source::Local, row.updated_at),
            host: row.identity.host,
            index: row.identity.slot_index,
            slot_kind: row.identity.slot_kind,
            phase: row.phase,
            job: row.job_name,
        }));
    }

    for row in store
        .runner_registration_rows(instance_slug)
        .map_err(store_query_error)?
    {
        let labels = serde_json::from_str::<BTreeMap<String, String>>(&row.labels_json)
            .map_err(|_| durable_projection_error("runner registration labels"))?;
        resources.push(AnyResource::RunnerRegistration(RunnerRegistration {
            meta: ResourceMeta::new(&row.name, Source::Github, row.updated_at)
                .with_uid(format!("runner-{}", row.runner_id)),
            labels,
            ephemeral: row.ephemeral,
            online: row.online,
        }));
    }

    let summaries = store
        .job_summaries(instance_slug)
        .map_err(store_query_error)?;
    let mut runs = BTreeMap::new();
    let mut queue_position = 0_u64;
    for summary in &summaries {
        if let Some(run_id) = summary.run_id {
            runs.entry(run_id).or_insert_with(|| summary.clone());
        }
        let job = job_resource(summary, instance_host.as_deref())?;
        if matches!(summary.phase.as_str(), "queued" | "waiting") {
            let last_transition_time = job.meta.last_transition_time;
            resources.push(AnyResource::QueueEntry(QueueEntry {
                meta: ResourceMeta::new(
                    &format!("queue-{}", summary.job_uid),
                    Source::Local,
                    last_transition_time,
                ),
                position: queue_position,
                job: summary.job_uid.clone(),
                wait_ms: None,
            }));
            queue_position = queue_position.saturating_add(1);
        }
        resources.push(AnyResource::Job(job));
    }
    for (run_id, summary) in runs {
        resources.push(AnyResource::Run(run_resource(run_id, &summary)?));
    }

    let event_window = store
        .event_window(instance_slug, 0, None, 4_096)
        .map_err(store_query_error)?;
    for stored in event_window.events {
        let row = stored.row;
        resources.push(AnyResource::Event(Event {
            meta: ResourceMeta::new(
                &format!("event-{}", stored.id),
                Source::Local,
                row.occurred_at,
            ),
            sequence: stored.id,
            occurred_at: row.occurred_at,
            event_kind: row.event_kind,
            subject: row.subject,
            detail: row.detail,
        }));
    }

    resources.sort_by(|left, right| {
        (left.kind(), left.meta().name.as_str()).cmp(&(right.kind(), right.meta().name.as_str()))
    });
    Ok(resources)
}

fn identity_slug(raw: &str) -> String {
    let slug: String = raw
        .chars()
        .flat_map(char::to_lowercase)
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    slug.trim_matches('-').to_owned()
}

fn job_resource(summary: &JobSummary, instance_host: Option<&str>) -> Result<Job, PortError> {
    let repository = repository_ref(&summary.repository)?;
    let last_transition_time =
        parse_optional_timestamp(summary.acquired_at.as_deref(), "acquired_at")?
            .or(parse_optional_timestamp(
                summary.queued_at.as_deref(),
                "queued_at",
            )?)
            .unwrap_or(Timestamp::UNIX_EPOCH);
    let host = identity_slug(
        instance_host
            .filter(|value| !value.is_empty())
            .unwrap_or(&summary.instance_slug),
    );
    Ok(Job {
        meta: ResourceMeta::new(&summary.job_uid, Source::Merged, last_transition_time),
        repository,
        run: summary.run_id.map(|run_id| format!("run-{run_id}")),
        workflow: summary.workflow.clone(),
        head_branch: summary.head_ref.clone(),
        queued_ms: None,
        duration_ms: None,
        conclusion: summary.conclusion.clone(),
        host: Some(host),
        instance: Some(summary.instance_slug.clone()),
        slot: summary.slot_name.clone(),
        runner: summary.runner_name.clone(),
        execution_backend: summary
            .execution_backend
            .as_deref()
            .and_then(|raw| ExecutionBackendKind::parse_value(raw).ok()),
    })
}

fn run_resource(run_id: i64, summary: &JobSummary) -> Result<Run, PortError> {
    let repository = repository_ref(&summary.repository)?;
    let last_transition_time =
        parse_optional_timestamp(summary.acquired_at.as_deref(), "acquired_at")?
            .or(parse_optional_timestamp(
                summary.queued_at.as_deref(),
                "queued_at",
            )?)
            .unwrap_or(Timestamp::UNIX_EPOCH);
    let number =
        u64::try_from(run_id).map_err(|_| durable_projection_error("negative run identity"))?;
    Ok(Run {
        meta: ResourceMeta::new(
            &format!("run-{run_id}"),
            Source::Merged,
            last_transition_time,
        )
        .with_uid(format!("run-{run_id}")),
        repository,
        number,
        head_sha: summary.head_sha.clone().unwrap_or_default(),
        head_branch: summary.head_ref.clone().unwrap_or_default(),
        event: summary.trigger_event.clone().unwrap_or_default(),
        status: run_status(&summary.phase).to_owned(),
        conclusion: summary.conclusion.clone(),
        url: None,
    })
}

fn run_status(phase: &str) -> &'static str {
    match phase {
        "queued" | "waiting" => "queued",
        "completed" | "canceled" | "cancelled" | "rejected" | "terminal" => "completed",
        _ => "in_progress",
    }
}

fn repository_ref(raw: &str) -> Result<RepositoryRef, PortError> {
    let Some((owner, name)) = raw.split_once('/') else {
        return Err(durable_projection_error("job repository identity"));
    };
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(durable_projection_error("job repository identity"));
    }
    Ok(RepositoryRef::new(owner, name))
}

fn parse_optional_timestamp(
    raw: Option<&str>,
    field: &str,
) -> Result<Option<Timestamp>, PortError> {
    raw.map(|value| Timestamp::parse(value).map_err(|_| durable_projection_error(field)))
        .transpose()
}

fn store_query_error(error: crate::store::StoreError) -> PortError {
    PortError::Operation {
        operation: format!("durable query failed ({})", error.envelope.reason),
    }
}

fn durable_projection_error(field: &str) -> PortError {
    PortError::Operation {
        operation: format!("durable query projection: {field}"),
    }
}

fn parse_page_token(raw: &str) -> Result<(u64, String, usize), PortError> {
    let Some(raw) = raw.strip_prefix(PAGE_PREFIX) else {
        return Err(PortError::Invalid {
            field: "page_token".to_owned(),
            message: "malformed continuation token".to_owned(),
        });
    };
    let mut parts = raw.split(':');
    let generation = parts.next().ok_or_else(|| PortError::Invalid {
        field: "page_token".to_owned(),
        message: "malformed continuation token".to_owned(),
    })?;
    let fingerprint = parts.next().ok_or_else(|| PortError::Invalid {
        field: "page_token".to_owned(),
        message: "malformed continuation token".to_owned(),
    })?;
    let offset = parts.next().ok_or_else(|| PortError::Invalid {
        field: "page_token".to_owned(),
        message: "malformed continuation token".to_owned(),
    })?;
    if parts.next().is_some() || fingerprint.len() != 64 {
        return Err(PortError::Invalid {
            field: "page_token".to_owned(),
            message: "malformed continuation token".to_owned(),
        });
    }
    let generation = generation.parse::<u64>().map_err(|_| PortError::Invalid {
        field: "page_token".to_owned(),
        message: "malformed continuation token".to_owned(),
    })?;
    if generation == 0 {
        return Err(PortError::Invalid {
            field: "page_token".to_owned(),
            message: "malformed continuation token".to_owned(),
        });
    }
    let offset = offset.parse::<usize>().map_err(|_| PortError::Invalid {
        field: "page_token".to_owned(),
        message: "malformed continuation token".to_owned(),
    })?;
    Ok((generation, fingerprint.to_owned(), offset))
}

fn query_fingerprint(request: &QueryRequest) -> String {
    let mut hasher = Sha256::new();
    let limit = request.limit.to_string();
    for value in [
        request.resource_kind.as_str(),
        request.selector.as_deref().unwrap_or(""),
        request.field_selector.as_deref().unwrap_or(""),
        request.since.as_deref().unwrap_or(""),
        limit.as_str(),
    ] {
        hasher.update(value.len().to_string().as_bytes());
        hasher.update(b":");
        hasher.update(value.as_bytes());
        hasher.update(b";");
    }
    hex_digest(&hasher.finalize())
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn validate_selector(selector: Option<&str>, field: &str) -> Result<(), PortError> {
    let Some(selector) = selector else {
        return Ok(());
    };
    if selector.is_empty() {
        return Err(PortError::Invalid {
            field: field.to_owned(),
            message: "selector must not be empty".to_owned(),
        });
    }
    for term in selector.split(',') {
        let Some((selector_field, _)) = term.split_once('=') else {
            return Err(invalid_selector(field));
        };
        let selector_field = selector_field.trim();
        if !matches!(
            selector_field,
            "name" | "metadata.name" | "kind" | "resourceKind" | "source"
        ) {
            return Err(invalid_selector(selector_field));
        }
    }
    Ok(())
}

fn matches_selector(resource: &AnyResource, selector: Option<&str>) -> Result<bool, PortError> {
    let Some(selector) = selector else {
        return Ok(true);
    };
    selector.split(',').try_fold(true, |matched, term| {
        let Some((field, expected)) = term.split_once('=') else {
            return Err(invalid_selector("selector"));
        };
        let term_matches = match field.trim() {
            "name" | "metadata.name" => resource.meta().name == expected.trim(),
            "kind" | "resourceKind" => resource.kind().eq_ignore_ascii_case(expected.trim()),
            "source" => resource
                .meta()
                .source
                .as_str()
                .eq_ignore_ascii_case(expected.trim()),
            _ => return Err(invalid_selector(field.trim())),
        };
        Ok(matched && term_matches)
    })
}

fn matches_field_selector(
    resource: &AnyResource,
    selector: Option<&str>,
) -> Result<bool, PortError> {
    let Some(selector) = selector else {
        return Ok(true);
    };
    selector.split(',').try_fold(true, |matched, term| {
        let Some((field, expected)) = term.split_once('=') else {
            return Err(invalid_selector("field_selector"));
        };
        let term_matches = match field.trim() {
            "name" | "metadata.name" => resource.meta().name == expected.trim(),
            "kind" | "resourceKind" => resource.kind().eq_ignore_ascii_case(expected.trim()),
            "source" => resource
                .meta()
                .source
                .as_str()
                .eq_ignore_ascii_case(expected.trim()),
            _ => return Err(invalid_selector(field.trim())),
        };
        Ok(matched && term_matches)
    })
}

fn invalid_selector(field: &str) -> PortError {
    PortError::Invalid {
        field: field.to_owned(),
        message: "selector field is not supported".to_owned(),
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
    use crate::ports::QueryPort;
    use crate::store::{
        InstanceRow, JobRow, RunnerRegistrationRow, SlotIdentity, SlotTransitionRequest, Store,
    };
    use velnor_model::{Generation, ResourceMeta, SlotId, SlotKind, SlotPhase, Source};

    fn resource(name: &str) -> AnyResource {
        resource_at(name, Timestamp::UNIX_EPOCH)
    }

    fn resource_at(name: &str, at: Timestamp) -> AnyResource {
        AnyResource::Host(velnor_model::Host {
            meta: ResourceMeta::new(name, Source::Local, at),
            hostname: name.to_owned(),
            agent_version: None,
            labels: Default::default(),
        })
    }

    #[test]
    fn query_is_sorted_and_page_token_is_generation_bound() {
        let service = QueryService::new();
        service
            .replace(vec![resource("b"), resource("a")])
            .expect("replace");
        let first = service
            .query(QueryRequest {
                resource_kind: "Host".to_owned(),
                limit: 1,
                ..QueryRequest::default()
            })
            .expect("query");
        assert_eq!(first.resources[0].meta().name, "a");
        let token = first.next_page_token.expect("next page");
        service
            .replace(vec![resource("c")])
            .expect("new generation");
        let error = service
            .query(QueryRequest {
                resource_kind: "Host".to_owned(),
                page_token: Some(token),
                ..QueryRequest::default()
            })
            .unwrap_err();
        assert!(matches!(error, PortError::Conflict { .. }));
    }

    #[test]
    fn page_token_generation_zero_is_rejected() {
        let service = QueryService::new();
        service
            .replace(vec![resource("a"), resource("b")])
            .expect("replace");
        let error = service
            .query(QueryRequest {
                page_token: Some(format!("v1:0:{}:1", "0".repeat(64))),
                ..QueryRequest::default()
            })
            .unwrap_err();
        assert!(matches!(error, PortError::Invalid { field, .. } if field == "page_token"));
    }

    #[test]
    fn invalid_selectors_fail_even_for_empty_projection() {
        let service = QueryService::new();
        let error = service
            .query(QueryRequest {
                selector: Some("unsupported=value".to_owned()),
                ..QueryRequest::default()
            })
            .unwrap_err();
        assert!(matches!(error, PortError::Invalid { field, .. } if field == "unsupported"));
    }

    #[test]
    fn field_selector_errors_are_not_silent_empty_pages() {
        let service = QueryService::new();
        service
            .replace(vec![resource("a").clone()])
            .expect("replace");
        let error = service
            .query(QueryRequest {
                field_selector: Some("unsupported=value".to_owned()),
                ..QueryRequest::default()
            })
            .unwrap_err();
        assert!(matches!(error, PortError::Invalid { field, .. } if field == "unsupported"));
    }

    #[test]
    fn since_excludes_older_resources() {
        let service = QueryService::new();
        service
            .replace(vec![
                resource_at("old", Timestamp::UNIX_EPOCH),
                resource_at(
                    "new",
                    Timestamp::parse("2026-08-24T00:00:00Z").expect("fixed timestamp"),
                ),
            ])
            .expect("replace");
        let page = service
            .query(QueryRequest {
                since: Some("2026-08-24T00:00:00Z".to_owned()),
                ..QueryRequest::default()
            })
            .expect("query");
        assert_eq!(page.resources.len(), 1);
        assert_eq!(page.resources[0].meta().name, "new");
    }

    #[test]
    fn page_token_is_bound_to_filters_and_limit() {
        let service = QueryService::new();
        service
            .replace(vec![resource("a"), resource("b")])
            .expect("replace");
        let first = service
            .query(QueryRequest {
                resource_kind: "Host".to_owned(),
                limit: 1,
                ..QueryRequest::default()
            })
            .expect("query");
        let token = first.next_page_token.expect("next page");
        let error = service
            .query(QueryRequest {
                resource_kind: "Host".to_owned(),
                limit: 2,
                page_token: Some(token),
                ..QueryRequest::default()
            })
            .unwrap_err();
        assert!(matches!(error, PortError::Conflict { .. }));
    }

    #[test]
    fn durable_projection_reads_rows_and_normalizes_plural_nouns() {
        let directory = std::env::temp_dir().join(format!(
            "velnor-query-durable-{}-{}",
            std::process::id(),
            Timestamp::now().as_offset_datetime().unix_timestamp_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("query test directory");
        let path = directory.join("state.db");
        let store = Arc::new(Store::open(&path).expect("query test store"));
        let now = Timestamp::now();
        store
            .upsert_instance(&InstanceRow {
                instance_slug: "primary".to_owned(),
                host: "macbook.local".to_owned(),
                daemon_version: "0.1.0".to_owned(),
                slots_configured: 1,
                slots_busy: 0,
                updated_at: now,
            })
            .expect("instance row");
        store
            .record_next_slot_transition(
                &SlotIdentity {
                    instance_slug: "primary".to_owned(),
                    slot_id: SlotId("slot-0".to_owned()),
                    host: "macbook.local".to_owned(),
                    slot_index: 0,
                    slot_kind: SlotKind::Stable,
                },
                &SlotTransitionRequest {
                    request_key: "query-slot-1".to_owned(),
                    generation: Generation(1),
                    target: SlotPhase::Idle,
                    job_name: None,
                    message: None,
                    transition_time: now,
                },
            )
            .expect("slot row");
        store
            .upsert_runner_registration(&RunnerRegistrationRow {
                instance_slug: "primary".to_owned(),
                runner_id: 7,
                name: "runner-7".to_owned(),
                ephemeral: false,
                online: true,
                labels_json: r#"{"self-hosted":"true"}"#.to_owned(),
                registered_at: now,
                updated_at: now,
            })
            .expect("runner row");
        store
            .record_job(&JobRow {
                instance_slug: "primary".to_owned(),
                job_uid: "job-7".to_owned(),
                repository: "tailrocks/velnor".to_owned(),
                workflow: ".github/workflows/ci.yml".to_owned(),
                job_name: "build".to_owned(),
                run_id: Some(7),
                attempt: Some(1),
                head_ref: Some("main".to_owned()),
                head_sha: Some("abc123".to_owned()),
                trigger_event: Some("push".to_owned()),
                queued_at: Some(now),
                acquired_at: None,
                slot_name: Some("slot-0".to_owned()),
                runner_name: Some("runner-7".to_owned()),
                execution_backend: Some("docker".to_owned()),
                trust_scope: Some("trusted".to_owned()),
                trust_class: Some("trusted".to_owned()),
                resource_policy: Some("standard".to_owned()),
                phase: "queued".to_owned(),
                conclusion: None,
                infrastructure_category: None,
                updated_at: now,
            })
            .expect("job row");

        let service = QueryService::with_store(Arc::clone(&store), "primary");
        let query = |resource_kind: &str| {
            service
                .query(QueryRequest {
                    resource_kind: resource_kind.to_owned(),
                    ..QueryRequest::default()
                })
                .expect("durable query")
                .resources
        };
        assert_eq!(query("instances")[0].kind(), "Instance");
        assert_eq!(query("slots")[0].kind(), "Slot");
        assert_eq!(query("runners")[0].kind(), "RunnerRegistration");
        let jobs = query("jobs");
        assert_eq!(jobs[0].kind(), "Job");
        let AnyResource::Job(job) = &jobs[0] else {
            panic!("expected Job");
        };
        assert_eq!(job.host.as_deref(), Some("macbook-local"));
        assert_eq!(job.instance.as_deref(), Some("primary"));
        assert_eq!(job.slot.as_deref(), Some("slot-0"));
        assert_eq!(job.runner.as_deref(), Some("runner-7"));
        assert_eq!(job.execution_backend, Some(ExecutionBackendKind::Docker));
        assert_eq!(query("runs")[0].kind(), "Run");
        assert_eq!(query("queues")[0].kind(), "QueueEntry");

        drop(store);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn job_resource_ignores_non_closed_execution_backend() {
        let summary = crate::store::JobSummary {
            instance_slug: "primary".to_owned(),
            job_uid: "job-7".to_owned(),
            repository: "tailrocks/velnor".to_owned(),
            workflow: ".github/workflows/ci.yml".to_owned(),
            job_name: "build".to_owned(),
            run_id: Some(7),
            attempt: Some(1),
            head_ref: Some("main".to_owned()),
            head_sha: Some("abc123".to_owned()),
            trigger_event: Some("push".to_owned()),
            queued_at: None,
            acquired_at: None,
            slot_name: Some("slot-0".to_owned()),
            runner_name: Some("runner-7".to_owned()),
            execution_backend: Some("self-hosted".to_owned()),
            trust_scope: Some("trusted".to_owned()),
            trust_class: Some("trusted".to_owned()),
            resource_policy: Some("standard".to_owned()),
            phase: "queued".to_owned(),
            conclusion: None,
            infrastructure_category: None,
        };
        let job = job_resource(&summary, Some("macbook-local")).expect("job");
        assert_eq!(job.execution_backend, None);

        let mut docker = summary.clone();
        docker.execution_backend = Some("docker".to_owned());
        let job = job_resource(&docker, Some("macbook-local")).expect("job");
        assert_eq!(job.execution_backend, Some(ExecutionBackendKind::Docker));
    }
}
