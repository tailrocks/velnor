//! Read-only resource projection and pagination service.
//!
//! Adapters publish already-authoritative, sanitized resources into this
//! service. Querying only filters those projections; it never reads files,
//! invokes subprocesses, or repairs state.

use std::sync::{Arc, RwLock};

use sha2::{Digest, Sha256};
use velnor_model::{AnyResource, Timestamp};

use crate::ports::{PortError, QueryPage, QueryPort, QueryRequest};

const MAX_PAGE_SIZE: u32 = 1_000;
const PAGE_PREFIX: &str = "v1:";

/// In-memory read projection with generation-safe opaque page tokens.
#[derive(Clone)]
pub struct QueryService {
    state: Arc<RwLock<QueryState>>,
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
        }
    }

    /// Replace the projection and advance its cursor generation.
    pub fn replace(&self, mut resources: Vec<AnyResource>) -> Result<(), PortError> {
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
        self.state
            .read()
            .map(|state| state.generation)
            .map_err(|_| PortError::Unavailable {
                resource: "query projection".to_owned(),
            })
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
            if !request.resource_kind.is_empty()
                && !resource.kind().eq_ignore_ascii_case(&request.resource_kind)
            {
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
mod tests {
    use super::*;
    use crate::ports::QueryPort;
    use velnor_model::{ResourceMeta, Source};

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
}
