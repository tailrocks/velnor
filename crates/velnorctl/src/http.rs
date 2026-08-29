//! Versioned local HTTP adapters for the control-plane ports.
//!
//! Route handlers only extract/validate transport data, call one application
//! port, and map its result. Read and mutation routers are constructed
//! separately so the caller can bind them to distinct Unix sockets.

use std::ffi::CString;
use std::path::Path as FsPath;
use std::sync::Arc;

use axum::{
    extract::{
        connect_info::ConnectInfo,
        rejection::{JsonRejection, QueryRejection},
        DefaultBodyLimit, Extension, Path as AxumPath, Query, Request, State,
    },
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use velnor_model::{ExitClass, MachineErrorEnvelope, SCHEMA_VERSION};

use velnor_control::ports::{
    LogPort, LogRequest, MutationKind, MutationPort, QueryPage, QueryPort, QueryRequest, WatchPort,
    WatchRequest,
};

/// Shared application ports held by HTTP handlers.
#[derive(Clone)]
pub struct ApiState {
    /// Exact daemon instance authorized by this API state.
    instance: Arc<str>,
    /// Resource query implementation.
    pub query: Arc<dyn QueryPort>,
    /// Ordered event observation implementation.
    pub watch: Arc<dyn WatchPort>,
    /// Sanitized log access implementation.
    pub logs: Arc<dyn LogPort>,
    /// Lifecycle/reconciliation mutation implementation.
    pub mutation: Arc<dyn MutationPort>,
    /// Shared admission bound for synchronous application-port work.
    blocking: Arc<Semaphore>,
}

const APPLICATION_BLOCKING_CONCURRENCY: usize = 16;
const MAX_MUTATION_BODY_BYTES: usize = 64 * 1024;

impl ApiState {
    /// Build transport state bound to one daemon instance.
    ///
    /// The admin route compares its path instance with this identity before
    /// invoking the mutation port. Callers must construct production services
    /// with [`ApplicationServices::with_store`]; this constructor never creates
    /// a second projection, log buffer, or lifecycle ledger.
    #[must_use]
    pub fn from_services_for_instance(
        services: &velnor_control::application::ApplicationServices,
        instance: impl Into<String>,
    ) -> Self {
        Self {
            instance: Arc::from(instance.into()),
            query: services.query(),
            watch: services.events(),
            logs: services.logs(),
            mutation: services.lifecycle(),
            blocking: Arc::new(Semaphore::new(APPLICATION_BLOCKING_CONCURRENCY)),
        }
    }

    /// Build an in-memory state for unit tests using the default instance.
    #[cfg(test)]
    #[must_use]
    pub fn from_services(services: &velnor_control::application::ApplicationServices) -> Self {
        Self::from_services_for_instance(services, "default")
    }
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
        .layer(Extension(PeerPolicy::for_group(CONTROL_GROUP)))
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
        .layer(Extension(PeerPolicy::for_group(ADMIN_GROUP)))
        .layer(DefaultBodyLimit::max(MAX_MUTATION_BODY_BYTES))
        .with_state(state)
}

/// Serve a router on an already-authorized Unix listener.
pub async fn serve_unix(
    listener: tokio::net::UnixListener,
    router: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), std::io::Error> {
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<PeerCredentials>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))
}

/// Read-socket mode: owner/group access only; package ownership supplies the
/// `velnor` group.
pub const CONTROL_SOCKET_MODE: u32 = 0o660;
/// Admin-socket mode: owner/group access only; package ownership supplies the
/// `velnor-admin` group.
pub const ADMIN_SOCKET_MODE: u32 = 0o660;
/// Unix group owning the read-only control socket.
pub const CONTROL_GROUP: &str = "velnor";
/// Unix group owning the mutation-only admin socket.
pub const ADMIN_GROUP: &str = "velnor-admin";

/// Create and validate one daemon instance directory before any lock or
/// socket pathname is opened. The package owns `/run/velnor`; this helper
/// creates only the validated child and refuses symlink or writable-path
/// substitutions.
pub fn prepare_instance_dir(path: &FsPath) -> Result<(), std::io::Error> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "instance directory must have a parent",
        )
    })?;
    inspect_directory_chain(parent)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "instance directory is not a trusted directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path)?;
        }
        Err(error) => return Err(error),
    }
    inspect_directory_chain(path)
}

/// Verify that package-owned socket groups exist before daemon readiness.
pub fn validate_socket_groups() -> Result<(), std::io::Error> {
    for name in [CONTROL_GROUP, ADMIN_GROUP] {
        if group_id(name).is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "required Velnor socket group is unavailable",
            ));
        }
    }
    Ok(())
}

/// Bind one exact Unix socket path without following or deleting foreign
/// filesystem objects.
pub fn bind_unix(
    path: &FsPath,
    mode: u32,
    group_name: &str,
) -> Result<tokio::net::UnixListener, std::io::Error> {
    use std::os::unix::io::AsRawFd;

    let group = group_id(group_name).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "required Velnor socket group is unavailable",
        )
    })?;

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "socket must have a parent",
        )
    })?;
    inspect_directory_chain(parent)?;
    // `bind` is the atomic ownership claim. Never unlink an existing path:
    // check-then-remove would let a sibling replace the pathname between the
    // metadata check and the unlink, and could destroy an unrelated endpoint.
    let listener = tokio::net::UnixListener::bind(path)?;
    let bound_socket = match socket_identity(path) {
        Some(identity) => identity,
        None => {
            // The pathname identity could not be proven. Dropping the live
            // listener is safe; unlinking by type would reintroduce a
            // pathname TOCTOU race and could remove a replacement endpoint.
            drop(listener);
            return Err(std::io::Error::other(
                "bound control socket identity could not be verified",
            ));
        }
    };
    let fd = listener.as_raw_fd();
    // SAFETY: `fd` is borrowed from the live listener and the sentinel uid
    // preserves the kernel-assigned owner.
    let chown_result = unsafe { libc::fchown(fd, u32::MAX, group) };
    if chown_result != 0 {
        let error = std::io::Error::last_os_error();
        cleanup_failed_bind(path, listener, bound_socket);
        return Err(error);
    }
    // SAFETY: `fd` is a live Unix listener descriptor.
    if unsafe { libc::fchmod(fd, mode as libc::mode_t) } != 0 {
        let error = std::io::Error::last_os_error();
        cleanup_failed_bind(path, listener, bound_socket);
        return Err(error);
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` points to writable storage of the required size.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        let error = std::io::Error::last_os_error();
        cleanup_failed_bind(path, listener, bound_socket);
        return Err(error);
    }
    // SAFETY: fstat initialized `stat` on success.
    let stat = unsafe { stat.assume_init() };
    if stat.st_gid != group || u64::from(stat.st_mode) & 0o777 != u64::from(mode) {
        cleanup_failed_bind(path, listener, bound_socket);
        return Err(std::io::Error::other(
            "bound control socket ownership or mode was not enforced",
        ));
    }
    Ok(listener)
}

/// Reclaim a socket pathname left by a crashed daemon after the instance lock
/// has been acquired. A live endpoint or any non-socket filesystem object is
/// never removed; only an unconnected socket with the same identity observed
/// before unlink is eligible.
pub fn remove_stale_socket(path: &FsPath) -> Result<(), std::io::Error> {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::UnixStream;

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "existing socket path is not a socket",
        ));
    }
    let identity = socket_identity(path)
        .ok_or_else(|| std::io::Error::other("existing socket identity could not be verified"))?;
    match UnixStream::connect(path) {
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "daemon instance socket is still serving",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Err(error) => return Err(error),
    }
    if socket_identity(path) == Some(identity) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
}

fn socket_identity(path: &FsPath) -> Option<SocketIdentity> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let metadata = std::fs::symlink_metadata(path).ok()?;
    metadata.file_type().is_socket().then(|| SocketIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

/// Remove only the exact socket pathname captured just after bind when
/// post-bind setup fails. Dropping a Unix listener does not remove its
/// pathname, so leaving this cleanup to the next daemon start would create a
/// stale control endpoint. The metadata comparison prevents removing a
/// replacement socket if the pathname changed after bind.
fn cleanup_failed_bind(
    path: &FsPath,
    listener: tokio::net::UnixListener,
    identity: SocketIdentity,
) {
    drop(listener);

    if socket_identity(path) == Some(identity) {
        let _ = std::fs::remove_file(path);
    }
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owner = unsafe { libc::geteuid() } as u32;
            let mode = metadata.mode();
            if metadata.uid() != 0 && metadata.uid() != owner || mode & 0o022 != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "socket parent is not a trusted non-writable directory",
                ));
            }
        }
    }
    Ok(())
}

/// Trusted Unix peer identity. A missing credential is never treated as an
/// anonymous local caller.
#[derive(Debug, Clone)]
pub struct PeerCredentials {
    /// Peer effective user id.
    pub uid: u32,
    /// Peer effective group id.
    pub gid: u32,
    /// Whether the kernel supplied a complete credential tuple.
    pub valid: bool,
    /// Supplementary groups reported by the kernel for this peer.
    pub groups: Box<[u32]>,
    /// Whether the supplementary-group response was complete.
    pub groups_valid: bool,
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() as u32 }
}

#[derive(Debug, Clone, Copy)]
struct PeerPolicy {
    owner_uid: u32,
    group_gid: Option<u32>,
}

impl PeerPolicy {
    fn for_group(name: &str) -> Self {
        Self {
            owner_uid: current_uid(),
            group_gid: group_id(name),
        }
    }

    fn allows(self, peer: &PeerCredentials) -> bool {
        peer.valid
            && (peer.uid == 0
                || peer.uid == self.owner_uid
                || self.group_gid.is_some_and(|gid| {
                    peer.gid == gid || (peer.groups_valid && peer.groups.contains(&gid))
                }))
    }
}

fn group_id(name: &str) -> Option<u32> {
    let name = CString::new(name).ok()?;
    // SAFETY: `name` is NUL-terminated for the duration of this libc call.
    let group = unsafe { libc::getgrnam(name.as_ptr()) };
    (!group.is_null()).then(|| {
        // SAFETY: libc returned a non-null group entry.
        unsafe { (*group).gr_gid as u32 }
    })
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
        let (groups, groups_valid) = peer_groups(stream);
        PeerCredentials {
            uid: raw.uid,
            gid: raw.gid,
            valid: true,
            groups,
            groups_valid,
        }
    } else {
        PeerCredentials {
            uid: 0,
            gid: 0,
            valid: false,
            groups: Box::new([]),
            groups_valid: false,
        }
    }
}

#[cfg(target_os = "linux")]
fn peer_groups(stream: &tokio::net::UnixStream) -> (Box<[u32]>, bool) {
    use std::os::fd::AsRawFd;

    const MAX_SUPPLEMENTARY_GROUPS: usize = 1024;
    let mut groups = [0 as libc::gid_t; MAX_SUPPLEMENTARY_GROUPS];
    let mut length = std::mem::size_of_val(&groups) as libc::socklen_t;
    // SAFETY: the buffer is valid writable storage and its length is bounded.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERGROUPS,
            groups.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result != 0 || !(length as usize).is_multiple_of(std::mem::size_of::<libc::gid_t>()) {
        return (Box::new([]), false);
    }
    let byte_length = length as usize;
    let buffer_length = std::mem::size_of_val(&groups);
    if byte_length > buffer_length {
        return (Box::new([]), false);
    }
    let count = byte_length / std::mem::size_of::<libc::gid_t>();
    let complete = byte_length < buffer_length;
    let groups = groups[..count].to_vec().into_boxed_slice();
    (groups, complete)
}

#[cfg(target_os = "macos")]
fn peer_credentials(stream: &tokio::net::UnixStream) -> PeerCredentials {
    use std::os::fd::AsRawFd;

    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: both output pointers refer to live scalar storage and the
    // descriptor is borrowed only for this libc call.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result == 0 {
        PeerCredentials {
            uid,
            gid,
            valid: true,
            // macOS exposes the peer's effective uid/gid here, but not a
            // complete supplementary-group vector through this API. The
            // primary group remains enforceable; supplementary-only access
            // fails closed in PeerPolicy::allows.
            groups: Box::new([]),
            groups_valid: false,
        }
    } else {
        PeerCredentials {
            uid: 0,
            gid: 0,
            valid: false,
            groups: Box::new([]),
            groups_valid: false,
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_credentials(_stream: &tokio::net::UnixStream) -> PeerCredentials {
    PeerCredentials {
        uid: 0,
        gid: 0,
        valid: false,
        groups: Box::new([]),
        groups_valid: false,
    }
}

async fn require_peer(
    ConnectInfo(peer): ConnectInfo<PeerCredentials>,
    Extension(policy): Extension<PeerPolicy>,
    request: Request,
    next: Next,
) -> Response {
    if policy.allows(&peer) {
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
        mutations: false,
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
    query: Result<Query<QueryParams>, QueryRejection>,
) -> Result<Json<QueryPage>, ApiError> {
    let Query(params) = query.map_err(|_| {
        ApiError::bad_request("query", "query parameters are malformed or unsupported")
    })?;
    let query = Arc::clone(&state.query);
    let page = run_application_call(state.blocking.clone(), move || {
        query.query(QueryRequest {
            resource_kind,
            selector: params.selector,
            field_selector: params.field_selector,
            page_token: params.page_token,
            limit: params.limit.unwrap_or(100),
            since: params.since,
        })
    })
    .await?;
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
    query: Result<Query<LogParams>, QueryRejection>,
) -> Result<Json<Vec<velnor_control::ports::LogItem>>, ApiError> {
    let Query(params) = query.map_err(|_| {
        ApiError::bad_request("query", "query parameters are malformed or unsupported")
    })?;
    let logs = Arc::clone(&state.logs);
    let records = run_application_call(state.blocking.clone(), move || {
        logs.logs(LogRequest {
            subject,
            source: params.source,
            cursor: params.cursor,
            limit: params.limit.unwrap_or(100),
        })
    })
    .await?;
    Ok(Json(records))
}

async fn watch(
    State(state): State<ApiState>,
    query: Result<Query<WatchParams>, QueryRejection>,
) -> Result<Json<Vec<velnor_control::ports::WatchItem>>, ApiError> {
    let Query(params) = query.map_err(|_| {
        ApiError::bad_request("query", "query parameters are malformed or unsupported")
    })?;
    let watch = Arc::clone(&state.watch);
    let events = run_application_call(state.blocking.clone(), move || {
        watch.watch(WatchRequest {
            resource_kind: params.resource_kind,
            after_version: params.after_version,
            limit: params.limit.unwrap_or(100),
        })
    })
    .await?;
    Ok(Json(events))
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

async fn mutate_instance(
    State(state): State<ApiState>,
    AxumPath((instance, operation)): AxumPath<(String, String)>,
    body: Result<Json<MutationBody>, JsonRejection>,
) -> Result<StatusCode, ApiError> {
    let Json(body) = body.map_err(|_| {
        ApiError::bad_request(
            "body",
            "request body must be valid JSON with the documented fields",
        )
    })?;
    let MutationBody {
        operation: body_operation,
        reason,
        idempotency_key,
        expected_version,
        slots,
    } = body;
    if instance != state.instance.as_ref() {
        return Err(ApiError::bad_request(
            "mutation.instance",
            "path instance does not match daemon instance",
        ));
    }
    let _kind = parse_mutation(&operation)?;
    if body_operation != operation {
        return Err(ApiError::bad_request(
            "mutation.operation",
            "body operation does not match path",
        ));
    }
    let _ = (reason, idempotency_key, expected_version, slots);
    // The durable lifecycle ledger is not an actuator. Until the daemon
    // reconciler consumes these intents, accepting them as HTTP 202 would
    // claim an effect that the runtime cannot perform.
    Err(ApiError::from(
        velnor_control::ports::PortError::Unsupported {
            operation: "lifecycle reconciler is not installed".to_owned(),
        },
    ))
}

async fn run_application_call<T, F>(blocking: Arc<Semaphore>, call: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, velnor_control::ports::PortError> + Send + 'static,
{
    // Queueing unbounded application work lets a burst of local callers hold
    // open connections indefinitely and can starve shutdown. Shed excess
    // work at the transport boundary; the caller can retry with its own
    // deadline after the current bounded set drains.
    let permit = blocking
        .try_acquire_owned()
        .map_err(|_| ApiError::overloaded())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        call()
    })
    .await
    .map_err(|_| ApiError::operation_failed())?
    .map_err(ApiError::from)
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

    fn operation_failed() -> Self {
        Self::from(velnor_control::ports::PortError::Operation {
            operation: "application call failed".to_owned(),
        })
    }

    fn overloaded() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            envelope: MachineErrorEnvelope::new(
                ExitClass::Transport.as_str(),
                ExitClass::Transport.code(),
                "control.overloaded",
            )
            .with_remediation("retry after the current control requests drain"),
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use velnor_control::ports::{MutationRequest, MutationResult, PortError};
    use velnor_model::{AnyResource, Event, ResourceMeta, Source, Timestamp};

    #[derive(Default)]
    struct RecordingMutation {
        calls: AtomicUsize,
    }

    impl MutationPort for RecordingMutation {
        fn mutate(&self, _request: MutationRequest) -> Result<MutationResult, PortError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(MutationResult {
                operation_id: "operation-1".to_owned(),
                phase: "accepted".to_owned(),
                resource: None,
            })
        }
    }

    fn event(kind: &str) -> Event {
        Event {
            meta: ResourceMeta::new("instance/primary", Source::Local, Timestamp::UNIX_EPOCH),
            sequence: 1,
            occurred_at: Timestamp::UNIX_EPOCH,
            event_kind: kind.to_owned(),
            subject: "instance/primary".to_owned(),
            detail: None,
        }
    }

    #[test]
    fn api_state_reuses_one_application_service_bundle() {
        let services = velnor_control::application::ApplicationServices::in_memory_for_tests();
        let state = ApiState::from_services(&services);

        services
            .query()
            .replace(vec![AnyResource::Event(event("ready"))])
            .expect("replace");
        assert_eq!(
            state
                .query
                .query(QueryRequest::default())
                .expect("query")
                .resources
                .len(),
            1
        );

        services
            .events()
            .publish(event("degraded"))
            .expect("publish");
        assert_eq!(
            state
                .watch
                .watch(WatchRequest {
                    resource_kind: None,
                    after_version: None,
                    limit: 10
                })
                .expect("watch")
                .len(),
            1
        );

        services
            .logs()
            .append("job-1", "active", "safe", &[])
            .expect("append");
        assert_eq!(
            state
                .logs
                .logs(LogRequest {
                    subject: "job-1".to_owned(),
                    source: None,
                    cursor: None,
                    limit: 10
                })
                .expect("logs")
                .len(),
            1
        );

        services.lifecycle().register("primary").expect("register");
        let result = state
            .mutation
            .mutate(MutationRequest {
                kind: MutationKind::Cordon,
                target: "primary".to_owned(),
                reason: "maintenance".to_owned(),
                idempotency_key: "api-state-test".to_owned(),
                expected_version: Some(1),
                scale_to: None,
            })
            .expect("mutate");
        assert_eq!(result.phase, "accepted");
    }

    #[tokio::test]
    async fn admin_rejects_another_instance_before_mutation() {
        let services = velnor_control::application::ApplicationServices::in_memory_for_tests();
        let mutation = Arc::new(RecordingMutation::default());
        let state = ApiState {
            instance: Arc::from("primary"),
            query: services.query(),
            watch: services.events(),
            logs: services.logs(),
            mutation: Arc::clone(&mutation) as Arc<dyn MutationPort>,
            blocking: Arc::new(Semaphore::new(APPLICATION_BLOCKING_CONCURRENCY)),
        };

        let result = mutate_instance(
            State(state),
            AxumPath(("other".to_owned(), "cordon".to_owned())),
            Ok(Json(MutationBody {
                operation: "cordon".to_owned(),
                reason: "maintenance".to_owned(),
                idempotency_key: "request-1".to_owned(),
                expected_version: None,
                slots: None,
            })),
        )
        .await;

        let error = result.expect_err("foreign instance must be rejected");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(mutation.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn application_admission_sheds_when_the_bound_is_full() {
        let blocking = Arc::new(Semaphore::new(0));
        let result: Result<(), ApiError> =
            run_application_call(blocking, || Ok::<(), PortError>(())).await;

        let error = result.expect_err("a full application bound must shed work");
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.envelope.reason, "control.overloaded");
    }

    #[tokio::test]
    async fn prepare_instance_dir_creates_and_validates_the_child_directory() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let parent = std::env::current_dir()
            .expect("current directory")
            .join(format!(
                ".velnorctl-instance-dir-{}-{suffix}",
                std::process::id()
            ));
        std::fs::create_dir(&parent).expect("create test parent");
        let instance = parent.join("primary");

        prepare_instance_dir(&instance).expect("prepare instance directory");

        assert!(instance.is_dir());
        std::fs::remove_dir_all(parent).expect("remove test directory");
    }

    #[tokio::test]
    async fn failed_bind_cleanup_removes_only_the_bound_socket() {
        let path = test_socket_path("failed-bind-cleanup");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind test socket");
        let identity = socket_identity(&path).expect("socket identity");

        cleanup_failed_bind(&path, listener, identity);

        assert!(!path.exists());

        let listener = tokio::net::UnixListener::bind(&path).expect("rebind test socket");
        let identity = socket_identity(&path).expect("replacement identity");
        std::fs::remove_file(&path).expect("unlink original path");
        let replacement = tokio::net::UnixListener::bind(&path).expect("bind replacement socket");

        cleanup_failed_bind(&path, listener, identity);

        assert!(path.exists());
        drop(replacement);
        std::fs::remove_file(path).expect("remove replacement socket");
    }

    #[tokio::test]
    async fn stale_socket_recovery_removes_unconnected_socket_only() {
        let path = test_socket_path("stale-recovery");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind stale socket");
        drop(listener);

        remove_stale_socket(&path).expect("remove stale socket");

        assert!(!path.exists());
    }

    #[tokio::test]
    async fn real_control_socket_exposes_reads_but_no_mutations() {
        let services = velnor_control::application::ApplicationServices::in_memory_for_tests();
        let state = ApiState::from_services(&services);
        let path = test_socket_path("control");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind control socket");
        let (shutdown, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(serve_unix(listener, control_router(state), async move {
            let _ = shutdown_rx.changed().await;
        }));

        let info = socket_request(&path, "GET", "/v1/info", b"").await;
        assert_eq!(response_status(&info), 200);
        assert!(String::from_utf8_lossy(&info).contains("\"mutations\":false"));

        let mutation = socket_request(
            &path,
            "POST",
            "/v1/instances/default/cordon",
            br#"{"operation":"cordon","reason":"test","idempotencyKey":"test"}"#,
        )
        .await;
        assert!(matches!(response_status(&mutation), 404 | 405));

        shutdown.send(true).expect("signal control shutdown");
        server.await.expect("join control server").expect("serve");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn real_admin_socket_rejects_unimplemented_mutation_and_bad_json() {
        let services = velnor_control::application::ApplicationServices::in_memory_for_tests();
        let mutation = Arc::new(RecordingMutation::default());
        let state = ApiState {
            instance: Arc::from("primary"),
            query: services.query(),
            watch: services.events(),
            logs: services.logs(),
            mutation: Arc::clone(&mutation) as Arc<dyn MutationPort>,
            blocking: Arc::new(Semaphore::new(APPLICATION_BLOCKING_CONCURRENCY)),
        };
        let path = test_socket_path("admin");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind admin socket");
        let (shutdown, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(serve_unix(listener, admin_router(state), async move {
            let _ = shutdown_rx.changed().await;
        }));

        let accepted = socket_request(
            &path,
            "POST",
            "/v1/instances/primary/cordon",
            br#"{"operation":"cordon","reason":"test","idempotencyKey":"test"}"#,
        )
        .await;
        assert_eq!(response_status(&accepted), 501);
        assert_eq!(mutation.calls.load(Ordering::Relaxed), 0);

        let rejected = socket_request(
            &path,
            "POST",
            "/v1/instances/primary/cordon",
            br#"{"operation":"cordon","reason":"test","idempotencyKey":"test","unknown":true}"#,
        )
        .await;
        assert_eq!(response_status(&rejected), 400);
        let rejected_text = String::from_utf8_lossy(&rejected);
        assert!(rejected_text.contains("body"));
        assert!(!rejected_text.contains("unknown"));
        assert_eq!(mutation.calls.load(Ordering::Relaxed), 0);

        shutdown.send(true).expect("signal admin shutdown");
        server.await.expect("join admin server").expect("serve");
        let _ = std::fs::remove_file(path);
    }

    fn test_socket_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "velnorctl-http-{label}-{}-{}.sock",
            std::process::id(),
            NEXT_TEST_SOCKET.fetch_add(1, Ordering::Relaxed)
        ))
    }

    static NEXT_TEST_SOCKET: AtomicUsize = AtomicUsize::new(1);

    async fn socket_request(
        path: &std::path::Path,
        method: &str,
        target: &str,
        body: &[u8],
    ) -> Vec<u8> {
        let mut stream = tokio::net::UnixStream::connect(path)
            .await
            .expect("connect test socket");
        let request = format!(
            "{method} {target} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write test request");
        stream.write_all(body).await.expect("write test body");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("read test response");
        response
    }

    fn response_status(response: &[u8]) -> u16 {
        assert!(
            !response.is_empty(),
            "test server returned no HTTP response: {response:?}"
        );
        String::from_utf8_lossy(response)
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|status| status.parse().ok())
            .expect("HTTP status")
    }

    #[test]
    fn peer_policy_requires_valid_identity_and_route_group() {
        let policy = PeerPolicy {
            owner_uid: 4242,
            group_gid: Some(77),
        };
        let owner = PeerCredentials {
            uid: 4242,
            gid: 9,
            valid: true,
            groups: Box::new([]),
            groups_valid: true,
        };
        let supplementary_group = PeerCredentials {
            uid: 9,
            gid: 8,
            valid: true,
            groups: vec![77].into_boxed_slice(),
            groups_valid: true,
        };
        let wrong_group = PeerCredentials {
            uid: 9,
            gid: 8,
            valid: true,
            groups: vec![78].into_boxed_slice(),
            groups_valid: true,
        };
        let missing_credentials = PeerCredentials {
            uid: 4242,
            gid: 9,
            valid: false,
            groups: Box::new([]),
            groups_valid: false,
        };

        assert!(policy.allows(&owner));
        assert!(policy.allows(&supplementary_group));
        assert!(!policy.allows(&wrong_group));
        assert!(!policy.allows(&missing_credentials));
    }

    #[test]
    fn root_peer_is_allowed_but_group_policy_is_not_bypassable_by_gid_zero() {
        let policy = PeerPolicy {
            owner_uid: 4242,
            group_gid: Some(77),
        };
        let root = PeerCredentials {
            uid: 0,
            gid: 0,
            valid: true,
            groups: Box::new([]),
            groups_valid: true,
        };
        let gid_zero = PeerCredentials {
            uid: 9,
            gid: 0,
            valid: true,
            groups: Box::new([]),
            groups_valid: true,
        };

        assert!(policy.allows(&root));
        assert!(!policy.allows(&gid_zero));
    }

    #[test]
    fn primary_group_is_sufficient_when_supplementary_groups_are_unavailable() {
        let policy = PeerPolicy {
            owner_uid: 4242,
            group_gid: Some(77),
        };
        let primary_group = PeerCredentials {
            uid: 9,
            gid: 77,
            valid: true,
            groups: Box::new([]),
            groups_valid: false,
        };

        assert!(policy.allows(&primary_group));
    }

    #[test]
    fn incomplete_supplementary_groups_cannot_authorize_a_peer() {
        let policy = PeerPolicy {
            owner_uid: 4242,
            group_gid: Some(77),
        };
        let truncated_groups = PeerCredentials {
            uid: 9,
            gid: 8,
            valid: true,
            groups: vec![77].into_boxed_slice(),
            groups_valid: false,
        };

        assert!(!policy.allows(&truncated_groups));
    }
}
