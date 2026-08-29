//! Versioned local HTTP adapters for the control-plane ports.
//!
//! Route handlers only extract/validate transport data, call one application
//! port, and map its result. Read and mutation routers are constructed
//! separately so the caller can bind them to distinct Unix sockets.

use std::path::Path as FsPath;
use std::sync::Arc;

use axum::{
    extract::{connect_info::ConnectInfo, Path as AxumPath, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use velnor_model::{ExitClass, MachineErrorEnvelope, SCHEMA_VERSION};

use velnor_control::ports::{
    LogPort, LogRequest, MutationKind, MutationPort, MutationRequest, QueryPage, QueryPort,
    QueryRequest, WatchPort, WatchRequest,
};

/// Shared application ports held by HTTP handlers.
#[derive(Clone)]
pub struct ApiState {
    /// Resource query implementation.
    pub query: Arc<dyn QueryPort>,
    /// Ordered event observation implementation.
    pub watch: Arc<dyn WatchPort>,
    /// Sanitized log access implementation.
    pub logs: Arc<dyn LogPort>,
    /// Lifecycle/reconciliation mutation implementation.
    pub mutation: Arc<dyn MutationPort>,
}

/// Stable `/v1/info` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InfoResponse {
    /// API path version.
    pub api_version: &'static str,
    /// Model schema version.
    pub schema_version: u32,
    /// Whether this socket accepts mutations.
    pub mutations: bool,
}

/// Read-only control socket router.
pub fn control_router(state: ApiState) -> Router {
    Router::new()
        .route("/v1/info", get(info_read))
        .route("/v1/{resource_kind}", get(query_resource))
        .route("/v1/watch", get(watch))
        .route("/v1/logs/{subject}", get(logs))
        .layer(middleware::from_fn(require_peer))
        .with_state(state)
}

/// Mutation-only admin socket router.
pub fn admin_router(state: ApiState) -> Router {
    Router::new()
        .route("/v1/info", get(info_admin))
        .route(
            "/v1/instances/{instance}/{operation}",
            post(mutate_instance),
        )
        .layer(middleware::from_fn(require_peer))
        .with_state(state)
}

/// Serve a router on an already-authorized Unix listener.
pub async fn serve_unix(
    listener: tokio::net::UnixListener,
    router: Router,
) -> Result<(), std::io::Error> {
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<PeerCredentials>(),
    )
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))
}

/// Read-socket mode: owner/group access only; package ownership supplies the
/// `velnor` group.
pub const CONTROL_SOCKET_MODE: u32 = 0o660;
/// Admin-socket mode: owner/group access only; package ownership supplies the
/// `velnor-admin` group.
pub const ADMIN_SOCKET_MODE: u32 = 0o660;

/// Bind one exact Unix socket path without following or deleting foreign
/// filesystem objects.
pub fn bind_unix(path: &FsPath, mode: u32) -> Result<tokio::net::UnixListener, std::io::Error> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "socket must have a parent",
        )
    })?;
    inspect_directory_chain(parent)?;
    if let Ok(existing) = std::fs::symlink_metadata(path) {
        if !existing.file_type().is_socket() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "refusing to replace non-socket control path",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owner = unsafe { libc::geteuid() } as u32;
            if existing.uid() != owner && owner != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "refusing to replace a foreign-owned control socket",
                ));
            }
        }
        std::fs::remove_file(path)?;
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    let bound = std::fs::symlink_metadata(path)?;
    if !bound.file_type().is_socket() {
        return Err(std::io::Error::other("bound control path is not a socket"));
    }
    if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
        drop(listener);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(listener)
}

fn inspect_directory_chain(path: &FsPath) -> Result<(), std::io::Error> {
    let mut current = FsPath::new("/").to_path_buf();
    for component in path.components() {
        if component == std::path::Component::RootDir {
            continue;
        }
        let name = match component {
            std::path::Component::Normal(name) => name,
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "socket parent contains a non-canonical component",
                ));
            }
        };
        current.push(name);
        let metadata = std::fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "socket parent contains a symlink or non-directory",
            ));
        }
    }
    Ok(())
}

/// Trusted Unix peer identity. A missing credential is never treated as an
/// anonymous local caller.
#[derive(Debug, Clone, Copy)]
pub struct PeerCredentials {
    /// Peer effective user id.
    pub uid: u32,
    /// Peer effective group id.
    pub gid: u32,
    /// Whether the kernel supplied a complete credential tuple.
    pub valid: bool,
}

impl
    axum::extract::connect_info::Connected<
        axum::serve::IncomingStream<'_, tokio::net::UnixListener>,
    > for PeerCredentials
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, tokio::net::UnixListener>) -> Self {
        peer_credentials(stream.io())
    }
}

#[cfg(target_os = "linux")]
fn peer_credentials(stream: &tokio::net::UnixStream) -> PeerCredentials {
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;

    let mut raw = MaybeUninit::<libc::ucred>::uninit();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `raw` points to enough writable storage and `length` describes
    // that storage. The file descriptor is borrowed for this call only.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            raw.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result == 0 {
        // SAFETY: getsockopt initialized `raw` when it returned success.
        let raw = unsafe { raw.assume_init() };
        PeerCredentials {
            uid: raw.uid,
            gid: raw.gid,
            valid: true,
        }
    } else {
        PeerCredentials {
            uid: 0,
            gid: 0,
            valid: false,
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn peer_credentials(_stream: &tokio::net::UnixStream) -> PeerCredentials {
    PeerCredentials {
        uid: 0,
        gid: 0,
        valid: false,
    }
}

async fn require_peer(
    ConnectInfo(peer): ConnectInfo<PeerCredentials>,
    request: Request,
    next: Next,
) -> Response {
    if peer.valid {
        next.run(request).await
    } else {
        ApiError::unauthorized().into_response()
    }
}

async fn info_read() -> Json<InfoResponse> {
    Json(InfoResponse {
        api_version: "v1",
        schema_version: SCHEMA_VERSION,
        mutations: false,
    })
}

async fn info_admin() -> Json<InfoResponse> {
    Json(InfoResponse {
        api_version: "v1",
        schema_version: SCHEMA_VERSION,
        mutations: true,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QueryParams {
    selector: Option<String>,
    field_selector: Option<String>,
    page_token: Option<String>,
    since: Option<String>,
    limit: Option<u32>,
}

async fn query_resource(
    State(state): State<ApiState>,
    AxumPath(resource_kind): AxumPath<String>,
    Query(params): Query<QueryParams>,
) -> Result<Json<QueryPage>, ApiError> {
    let page = state.query.query(QueryRequest {
        resource_kind,
        selector: params.selector,
        field_selector: params.field_selector,
        page_token: params.page_token,
        limit: params.limit.unwrap_or(100),
        since: params.since,
    })?;
    Ok(Json(page))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WatchParams {
    resource_kind: Option<String>,
    after_version: Option<u64>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LogParams {
    source: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
}

async fn logs(
    State(state): State<ApiState>,
    AxumPath(subject): AxumPath<String>,
    Query(params): Query<LogParams>,
) -> Result<Json<Vec<velnor_control::ports::LogItem>>, ApiError> {
    Ok(Json(state.logs.logs(LogRequest {
        subject,
        source: params.source,
        cursor: params.cursor,
        limit: params.limit.unwrap_or(100),
    })?))
}

async fn watch(
    State(state): State<ApiState>,
    Query(params): Query<WatchParams>,
) -> Result<Json<Vec<velnor_control::ports::WatchItem>>, ApiError> {
    Ok(Json(state.watch.watch(WatchRequest {
        resource_kind: params.resource_kind,
        after_version: params.after_version,
        limit: params.limit.unwrap_or(100),
    })?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MutationBody {
    operation: String,
    reason: String,
    idempotency_key: String,
    expected_version: Option<u64>,
    slots: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MutationResponse {
    operation_id: String,
    phase: String,
}

async fn mutate_instance(
    State(state): State<ApiState>,
    AxumPath((instance, operation)): AxumPath<(String, String)>,
    Json(body): Json<MutationBody>,
) -> Result<(StatusCode, Json<MutationResponse>), ApiError> {
    let kind = parse_mutation(&operation)?;
    if body.operation != operation {
        return Err(ApiError::bad_request(
            "mutation.operation",
            "body operation does not match path",
        ));
    }
    let result = state.mutation.mutate(MutationRequest {
        kind,
        target: instance,
        reason: body.reason,
        idempotency_key: body.idempotency_key,
        expected_version: body.expected_version,
        scale_to: body.slots,
    })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(MutationResponse {
            operation_id: result.operation_id,
            phase: result.phase,
        }),
    ))
}

fn parse_mutation(raw: &str) -> Result<MutationKind, ApiError> {
    match raw {
        "cordon" => Ok(MutationKind::Cordon),
        "uncordon" => Ok(MutationKind::Uncordon),
        "drain" => Ok(MutationKind::Drain),
        "resume" => Ok(MutationKind::Resume),
        "restart" => Ok(MutationKind::Restart),
        "scale" => Ok(MutationKind::Scale),
        "recycle" => Ok(MutationKind::Recycle),
        "reconcile" => Ok(MutationKind::Reconcile),
        _ => Err(ApiError::bad_request(
            "operation",
            "unsupported lifecycle operation",
        )),
    }
}

/// Safe transport error mapping for all handler/extractor failures.
pub struct ApiError {
    status: StatusCode,
    envelope: MachineErrorEnvelope,
}

impl ApiError {
    fn bad_request(reason: &str, message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            envelope: MachineErrorEnvelope::new("USAGE", 2, reason).with_remediation(message),
        }
    }

    fn unauthorized() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            envelope: MachineErrorEnvelope::new(
                ExitClass::Authorization.as_str(),
                ExitClass::Authorization.code(),
                "control.peer.unauthorized",
            )
            .with_remediation("connect through an authorized Velnor Unix socket"),
        }
    }
}

impl From<velnor_control::ports::PortError> for ApiError {
    fn from(error: velnor_control::ports::PortError) -> Self {
        let (status, class, reason, message) = match error {
            velnor_control::ports::PortError::Authorization { .. } => (
                StatusCode::FORBIDDEN,
                ExitClass::Authorization,
                "authorization.denied".to_owned(),
                "authorization denied".to_owned(),
            ),
            velnor_control::ports::PortError::Invalid { field, .. } => (
                StatusCode::BAD_REQUEST,
                ExitClass::Usage,
                format!("field.invalid.{field}"),
                "request validation failed".to_owned(),
            ),
            velnor_control::ports::PortError::Unavailable { .. } => (
                StatusCode::NOT_FOUND,
                ExitClass::Unavailable,
                "resource.unavailable".to_owned(),
                "resource unavailable".to_owned(),
            ),
            velnor_control::ports::PortError::Conflict { .. } => (
                StatusCode::CONFLICT,
                ExitClass::Conflict,
                "resource.version_conflict".to_owned(),
                "resource version conflict".to_owned(),
            ),
            velnor_control::ports::PortError::Unsupported { .. } => (
                StatusCode::NOT_IMPLEMENTED,
                ExitClass::Operation,
                "operation.unsupported".to_owned(),
                "operation is not implemented".to_owned(),
            ),
            velnor_control::ports::PortError::Operation { .. } => (
                StatusCode::INTERNAL_SERVER_ERROR,
                ExitClass::Operation,
                "operation.failed".to_owned(),
                "operation failed".to_owned(),
            ),
        };
        Self {
            status,
            envelope: MachineErrorEnvelope::new(class.as_str(), class.code(), &reason)
                .with_remediation(message),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.envelope)).into_response()
    }
}
