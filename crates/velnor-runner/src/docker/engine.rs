//! Minimal async Docker Engine API client over the daemon Unix socket.
//!
//! GOAL 17, option C: a minimal direct Engine API implementation for the
//! read-only control-plane GETs, because the maintained general clients cost
//! more than six small GETs justify. What is already in the tree:
//!
//! * `tokio` (full: `net`, `time`, `rt-multi-thread`, `io-util`) for the
//!   `gha_cache` server — drives the socket I/O here with zero new crates;
//! * `serde_json` — parses the small JSON documents these endpoints return;
//! * `hyper`/`hyper-util` — present but server-featured only (`server`,
//!   `http1`, `tokio`, `service`). Enabling their client sides would pull the
//!   HTTP client machinery (`client`, `client-legacy`, `tower-service`) for
//!   six `Connection: close` GETs whose framing is twenty lines. The
//!   `execution/unix_api.rs` precedent shows hand-rolled socket HTTP is this
//!   codebase's established shape; this module is its async Engine sibling.
//!
//! Endpoint coverage is read-only control-plane only: `version`, `info`,
//! container inspect, image inspect, network inspect, and the filtered
//! container/network/volume lists. Paths are unversioned, which is the
//! negotiation: the daemon serves its native schema, nothing pins an old API
//! version, and any schema drift surfaces as an [`EngineError`] that the
//! facade answers with its CLI fallback — never as a misread value.
//!
//! Transport rules, all load-bearing for the "identical results" guarantee:
//!
//! * one fresh `Connection: close` connection per call (no reuse in this
//!   slice; a Unix-socket connect is microseconds, reuse is a follow-up);
//! * `Content-Length`, `Transfer-Encoding: chunked`, and close-delimited
//!   bodies. Chunked is required, not a fallback case: a live Engine 29.4.0
//!   chunks these very GETs, so declining it would leave the fast path
//!   permanently dark;
//! * response bodies are capped ([`MAX_BODY_BYTES`]); the cap is an API
//!   error, and the fallback serves the call identically;
//! * response bodies are NEVER logged: container inspect documents carry
//!   `Config.Env`, which carries secrets, and the `velnor.docker` sinks
//!   perform no redaction. [`EngineError`]'s `Display` carries status codes,
//!   byte counts, and I/O error strings only.
//!
//! The facade ([`super::client`]) owns routing: every migrated query tries
//! the API first under a capped budget and falls back to its historical CLI
//! query on ANY API failure, logging the fallback with telemetry. Engine
//! errors therefore never surface and never need a
//! `DockerErrorCategory` mapping of their own: whatever the caller sees is
//! either an API value
//! proven identical by test, or the CLI's own result with the CLI's own
//! category.

use super::deadline::DockerOp;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

/// Daemon all transports agree on: both CLI seams force
/// `DOCKER_HOST=unix:///var/run/docker.sock` (`executor.rs`
/// `configure_host_docker_command`), so this socket is the same daemon the
/// CLI fallback talks to on both the job and the host path.
pub(crate) fn socket_path() -> PathBuf {
    #[cfg(test)]
    if let Some(override_path) = test_socket_override() {
        return override_path;
    }
    PathBuf::from(crate::docker_lease::HOST_DOCKER_SOCKET)
}

/// Cap on one API attempt. The class deadline still bounds the CLI fallback,
/// so a degraded daemon costs at most this budget plus one class-bounded CLI
/// call; a healthy daemon answers in milliseconds, which makes five seconds
/// two orders of magnitude of headroom, not a behavior change.
const API_BUDGET_CAP: Duration = Duration::from_secs(5);

/// Largest response body accepted before the call becomes an API error (and
/// the CLI fallback serves it). `info` on a large host is tens of kilobytes;
/// eight megabytes is two orders of magnitude above that and still bounded,
/// which answers BC-7's unbounded-buffering finding for the new transport.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Largest response head (status line plus headers) accepted.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// Budget for one API attempt: the query's own class deadline, capped. Every
/// current control-plane class deadline (20 s and up) caps to five seconds;
/// the `min` keeps the honest plumbing so a future tighter class still wins.
pub(crate) fn api_budget(class_deadline: Duration) -> Duration {
    #[cfg(test)]
    if let Some(override_ms) = test_budget_override() {
        return Duration::from_millis(override_ms).min(class_deadline);
    }
    class_deadline.min(API_BUDGET_CAP)
}

// ---------------------------------------------------------------------------
// Routing gate
// ---------------------------------------------------------------------------

const ROUTE_UNSET: u8 = 0;
const ROUTE_ON: u8 = 1;
const ROUTE_OFF: u8 = 2;

static ROUTE_OVERRIDE: AtomicU8 = AtomicU8::new(ROUTE_UNSET);

/// Whether facade queries try the Engine API before the CLI.
///
/// Production default is on, with `VELNOR_DOCKER_ENGINE_API=0` as the
/// operator escape hatch. The unit-test default is off so scripted-runner
/// tests stay hermetic on hosts with a live daemon socket: without this, a
/// test asserting exact CLI argument vectors would see zero CLI calls on a
/// Linux dev host and fail there while passing on macOS. Tests that cover
/// the API path enable it explicitly through `EngineTestGuard`.
pub(crate) fn engine_api_enabled() -> bool {
    match ROUTE_OVERRIDE.load(Ordering::Relaxed) {
        ROUTE_ON => true,
        ROUTE_OFF => false,
        _ => std::env::var("VELNOR_DOCKER_ENGINE_API")
            .map(|value| value != "0")
            .unwrap_or(!cfg!(test)),
    }
}

#[cfg(test)]
static TEST_SOCKET: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg(test)]
static TEST_BUDGET_MS: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn test_socket_override() -> Option<PathBuf> {
    TEST_SOCKET
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

#[cfg(test)]
fn test_budget_override() -> Option<u64> {
    *TEST_BUDGET_MS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

#[cfg(test)]
fn set_test_overrides(socket: Option<PathBuf>, budget_ms: Option<u64>) {
    *TEST_SOCKET
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = socket;
    *TEST_BUDGET_MS
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = budget_ms;
}

/// Test guard that routes the facade at a mock socket. Holds the metrics
/// serial lock for its lifetime so parallel tests neither thrash the
/// process-global overrides nor pollute each other's counter deltas.
#[cfg(test)]
pub(crate) struct EngineTestGuard {
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl EngineTestGuard {
    /// Force the API path on at `socket`, with an optional budget override
    /// in milliseconds for timeout tests.
    pub(crate) fn serve(socket: PathBuf, budget_ms: Option<u64>) -> Self {
        let serial = super::metrics::lock_serial_for_test();
        ROUTE_OVERRIDE.store(ROUTE_ON, Ordering::Relaxed);
        set_test_overrides(Some(socket), budget_ms);
        Self { _serial: serial }
    }
}

#[cfg(test)]
impl Drop for EngineTestGuard {
    fn drop(&mut self) {
        set_test_overrides(None, None);
        ROUTE_OVERRIDE.store(ROUTE_UNSET, Ordering::Relaxed);
    }
}

/// Test guard that simulates an unbuildable engine runtime: every facade
/// query falls back to the CLI. Hold alongside an [`EngineTestGuard`] so
/// the metrics serial lock keeps parallel tests off the process-global flag.
#[cfg(test)]
pub(crate) struct FailRuntimeBuildGuard;

#[cfg(test)]
impl FailRuntimeBuildGuard {
    pub(crate) fn inject() -> Self {
        FAIL_RUNTIME_BUILD.store(1, Ordering::Relaxed);
        Self
    }
}

#[cfg(test)]
impl Drop for FailRuntimeBuildGuard {
    fn drop(&mut self) {
        FAIL_RUNTIME_BUILD.store(0, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Sync/async bridge
// ---------------------------------------------------------------------------

static ENGINE_RUNTIME: std::sync::OnceLock<Option<tokio::runtime::Runtime>> =
    std::sync::OnceLock::new();

#[cfg(test)]
static FAIL_RUNTIME_BUILD: AtomicU8 = AtomicU8::new(0);

/// Runtime for API calls issued where no runtime is current: the dedicated
/// job execution thread is a plain OS thread (`runner.rs`
/// `run_on_job_execution_thread`). One slot process runs one job, so at most
/// the job thread and one maintenance thread block here concurrently; two
/// workers keep either from queueing behind the other outside its budget.
///
/// Fallible on purpose: a runtime that cannot build (thread or fd
/// exhaustion on a sick host) must route the query to the CLI fallback,
/// never panic the job thread. The first build's outcome is cached, so a
/// sick host stays on the CLI without rebuilding per query.
fn engine_runtime() -> Option<&'static tokio::runtime::Runtime> {
    #[cfg(test)]
    if FAIL_RUNTIME_BUILD.load(Ordering::Relaxed) != 0 {
        return None;
    }
    ENGINE_RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("velnor-engine-api")
                .enable_all()
                .build()
                .ok()
        })
        .as_ref()
}

/// Drive one Engine future from the synchronous facade.
///
/// * Inside the main multi-thread runtime (async control-plane contexts),
///   this blocks the worker without stalling the scheduler.
/// * Inside a current-thread runtime (`#[tokio::test]` today; any embedded
///   single-thread context tomorrow), `block_in_place` would panic and the
///   current thread is already inside a runtime, so the future is driven on
///   a helper thread where no runtime is current.
/// * Anywhere else, the dedicated runtime above drives it.
///
/// `None` is a dead driver — unbuildable runtime or a failed helper thread —
/// and the facade answers it with the CLI fallback, exactly like any other
/// API failure.
pub(crate) fn block_on_engine<F>(future: F) -> Option<F::Output>
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            return Some(tokio::task::block_in_place(|| handle.block_on(future)));
        }
        return drive_on_helper_thread(future);
    }
    Some(engine_runtime()?.block_on(future))
}

/// Drive `future` on the dedicated runtime from a scope-borrowed helper
/// thread. A panicking helper joins as an error, never propagates: the
/// facade falls back to the CLI.
fn drive_on_helper_thread<F>(future: F) -> Option<F::Output>
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    let runtime = engine_runtime()?;
    std::thread::scope(|scope| scope.spawn(|| runtime.block_on(future)).join().ok())
}

/// Poll interval for the cancellation race: a cancelled job abandons the
/// socket wait within this bound instead of riding the API budget.
const CANCEL_POLL: Duration = Duration::from_millis(10);

/// Drive `future` unless the active job cancels first: `None` is the
/// cancelled leg winning, and the facade answers it with the CLI fallback —
/// the same spawn-and-ladder-kill the CLI path has always run under cancel,
/// so cancellation keeps its historical shape. Without an active token
/// (maintenance/host path) this is a direct await with zero overhead.
pub(crate) async fn cancel_race<F>(future: F) -> Option<F::Output>
where
    F: std::future::Future,
{
    let token = crate::execution::cancel::active();
    let Some(token) = token else {
        return Some(future.await);
    };
    if token.is_cancelled() {
        return None;
    }
    tokio::select! {
        biased;
        output = future => Some(output),
        () = wait_cancelled(token) => None,
    }
}

/// Resolve when `token` fires. Polling, because the token is a level flag,
/// not a waker — and a 10 ms sleep is nothing next to a socket wait.
async fn wait_cancelled(token: crate::execution::cancel::JobCancellation) {
    loop {
        tokio::time::sleep(CANCEL_POLL).await;
        if token.is_cancelled() {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Errors: status codes and I/O strings only, never body bytes
// ---------------------------------------------------------------------------

/// What went wrong with one API attempt, for the fallback telemetry. Bodies
/// never appear here: container inspect carries `Config.Env`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineFaultKind {
    Connect,
    Write,
    Read,
    /// The daemon answered with a non-2xx status; the detail carries the
    /// code only, never the error document.
    Status,
    Framing,
    TooLarge,
    Json,
    Schema,
    /// The driver itself was dead: the dedicated runtime could not build,
    /// or the helper thread for a current-thread context failed.
    Runtime,
    /// The active job cancelled during the socket wait; the CLI fallback is
    /// the same spawn-and-ladder-kill the CLI path always ran under cancel.
    Cancelled,
}

impl std::fmt::Display for EngineFaultKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Connect => "connect",
            Self::Write => "write",
            Self::Read => "read",
            Self::Status => "status",
            Self::Framing => "framing",
            Self::TooLarge => "too-large",
            Self::Json => "json",
            Self::Schema => "schema",
            Self::Runtime => "runtime",
            Self::Cancelled => "cancelled",
        };
        formatter.write_str(label)
    }
}

/// One failed Engine API attempt. Never surfaces past the facade: every
/// variant routes to the CLI fallback, so callers keep seeing exactly the
/// CLI's errors (`NotFound`, `DockerTimeout`, `DockerCommandError`) with
/// the taxonomy untouched.
#[derive(Debug)]
pub(crate) enum EngineError {
    Timeout {
        op: DockerOp,
        budget: Duration,
    },
    Fault {
        op: DockerOp,
        kind: EngineFaultKind,
        detail: String,
    },
}

impl EngineError {
    pub(crate) fn fault(op: DockerOp, fault: (EngineFaultKind, String)) -> Self {
        Self::Fault {
            op,
            kind: fault.0,
            detail: fault.1,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn kind(&self) -> Option<EngineFaultKind> {
        match self {
            Self::Timeout { .. } => None,
            Self::Fault { kind, .. } => Some(*kind),
        }
    }

    /// Closed-vocabulary fallback reason for the `velnor.docker` telemetry.
    #[must_use]
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Timeout { .. } => "timeout",
            Self::Fault { kind, .. } => match kind {
                EngineFaultKind::Connect => "connect",
                EngineFaultKind::Write => "write",
                EngineFaultKind::Read => "read",
                EngineFaultKind::Status => "status",
                EngineFaultKind::Framing => "framing",
                EngineFaultKind::TooLarge => "too-large",
                EngineFaultKind::Json => "json",
                EngineFaultKind::Schema => "schema",
                EngineFaultKind::Runtime => "runtime",
                EngineFaultKind::Cancelled => "cancelled",
            },
        }
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { op, budget } => write!(
                formatter,
                "engine api {} timed out after {}ms",
                op.label(),
                budget.as_millis()
            ),
            Self::Fault { op, kind, detail } => {
                write!(formatter, "engine api {} {kind}: {detail}", op.label())
            }
        }
    }
}

impl std::error::Error for EngineError {}

// ---------------------------------------------------------------------------
// URL encoding: image references carry '/' and ':' inside one segment
// ---------------------------------------------------------------------------

/// Percent-encode one URL path segment. Only RFC 3986 unreserved bytes pass
/// through; `velnor/job-ubuntu:26.04` becomes
/// `velnor%2Fjob-ubuntu%3A26.04`, without which the daemon would route the
/// inspect at a nonexistent path and the facade would pay a fallback per
/// image query.
pub(crate) fn encode_segment(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[usize::from(byte >> 4)] as char);
            encoded.push(HEX[usize::from(byte & 0x0F)] as char);
        }
    }
    encoded
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

/// Async Engine API client. One fresh connection per call; see the module
/// docs for the transport rules.
pub(crate) struct EngineClient {
    socket: PathBuf,
}

impl EngineClient {
    #[must_use]
    pub(crate) fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    /// `GET` one path and return the status code with the exact body bytes.
    /// The whole attempt — connect, write, read — runs under `budget`.
    async fn get(
        &self,
        path: &str,
        op: DockerOp,
        budget: Duration,
    ) -> EngineResult<(u16, Vec<u8>)> {
        let socket = self.socket.clone();
        let path = path.to_string();
        let attempt = async move {
            let mut stream = tokio::net::UnixStream::connect(&socket)
                .await
                .map_err(|error| (EngineFaultKind::Connect, error.to_string()))?;
            let request = format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
            );
            tokio::io::AsyncWriteExt::write_all(&mut stream, request.as_bytes())
                .await
                .map_err(|error| (EngineFaultKind::Write, error.to_string()))?;
            read_response(&mut stream).await
        };
        match tokio::time::timeout(budget, attempt).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(fault)) => Err(EngineError::fault(op, fault)),
            Err(_) => Err(EngineError::Timeout { op, budget }),
        }
    }

    /// `GET` one path and return its JSON body. Non-2xx is an API error: the
    /// facade's CLI fallback re-derives whatever the daemon meant (including
    /// positive-missing), so this layer never interprets statuses.
    async fn get_json(
        &self,
        path: &str,
        op: DockerOp,
        budget: Duration,
    ) -> EngineResult<serde_json::Value> {
        let (status, body) = self.get(path, op, budget).await?;
        if !(200..300).contains(&status) {
            return Err(EngineError::fault(
                op,
                (EngineFaultKind::Status, format!("http {status}")),
            ));
        }
        serde_json::from_slice(&body)
            .map_err(|error| EngineError::fault(op, (EngineFaultKind::Json, error.to_string())))
    }

    /// `GET /containers/{name}/json`. Serves five facade queries from one
    /// document: readiness, running, id, exit info, published ports.
    pub(crate) async fn inspect_container(
        &self,
        name: &str,
        budget: Duration,
    ) -> EngineResult<EngineContainer> {
        let op = DockerOp::Query;
        let path = format!("/containers/{}/json", encode_segment(name));
        let value = self.get_json(&path, op, budget).await?;
        parse_container(&value).map_err(|fault| EngineError::fault(op, fault))
    }

    /// `GET /images/{reference}/json`. Serves the facade's image-id query.
    pub(crate) async fn inspect_image(
        &self,
        reference: &str,
        budget: Duration,
    ) -> EngineResult<EngineImage> {
        let op = DockerOp::Query;
        let path = format!("/images/{}/json", encode_segment(reference));
        let value = self.get_json(&path, op, budget).await?;
        parse_image(&value).map_err(|fault| EngineError::fault(op, fault))
    }

    /// `GET /info`. Serves the facade's daemon-cgroup query.
    pub(crate) async fn daemon_info(&self, budget: Duration) -> EngineResult<EngineInfo> {
        let op = DockerOp::DaemonQuery;
        let value = self.get_json("/info", op, budget).await?;
        parse_info(&value).map_err(|fault| EngineError::fault(op, fault))
    }

    /// `GET /version`. Client-layer coverage for daemon-version reads; no
    /// facade query needs it in this slice, so no facade method exists yet.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "next slice routes version through the facade")
    )]
    pub(crate) async fn version(&self, budget: Duration) -> EngineResult<EngineVersion> {
        let op = DockerOp::DaemonQuery;
        let value = self.get_json("/version", op, budget).await?;
        parse_version(&value).map_err(|fault| EngineError::fault(op, fault))
    }

    /// `GET /networks/{name}`. Client-layer coverage; no facade consumer yet.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "next slice routes network inspect through the facade"
        )
    )]
    pub(crate) async fn inspect_network(
        &self,
        name: &str,
        budget: Duration,
    ) -> EngineResult<EngineNetwork> {
        let op = DockerOp::Query;
        let path = format!("/networks/{}", encode_segment(name));
        let value = self.get_json(&path, op, budget).await?;
        parse_network(&value).map_err(|fault| EngineError::fault(op, fault))
    }

    /// `GET /containers/json?all=1&filters=...`. Serves the facade's
    /// owned-container listing; the daemon applies the label filter, the same
    /// translation the CLI's `--filter label=` performs.
    pub(crate) async fn list_containers(
        &self,
        filter: &ListFilter,
        budget: Duration,
    ) -> EngineResult<Vec<EngineContainerSummary>> {
        let op = DockerOp::Query;
        let path = format!("/containers/json?all=1&filters={}", filter.encode());
        let value = self.get_json(&path, op, budget).await?;
        parse_container_list(&value).map_err(|fault| EngineError::fault(op, fault))
    }

    /// `GET /networks?filters=...`. Serves the facade's owned-network
    /// listing.
    pub(crate) async fn list_networks(
        &self,
        filter: &ListFilter,
        budget: Duration,
    ) -> EngineResult<Vec<EngineNetwork>> {
        let op = DockerOp::Query;
        let path = format!("/networks?filters={}", filter.encode());
        let value = self.get_json(&path, op, budget).await?;
        parse_network_list(&value).map_err(|fault| EngineError::fault(op, fault))
    }

    /// `GET /volumes?filters=...`. Serves the facade's owned-volume listing.
    pub(crate) async fn list_volumes(
        &self,
        filter: &ListFilter,
        budget: Duration,
    ) -> EngineResult<Vec<EngineVolume>> {
        let op = DockerOp::Query;
        let path = format!("/volumes?filters={}", filter.encode());
        let value = self.get_json(&path, op, budget).await?;
        parse_volume_list(&value).map_err(|fault| EngineError::fault(op, fault))
    }
}

/// One daemon list filter. Label-equality only: every listing this slice
/// routes filters on `velnor.job-id=<job>` server-side. Key-exists and name
/// filters are a later slice's variants, not a stringly escape hatch here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ListFilter {
    LabelEquals { key: String, value: String },
}

impl ListFilter {
    pub(crate) fn label_equals(key: &str, value: &str) -> Self {
        Self::LabelEquals {
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    /// The `filters=` query value: the daemon's `{"label": ["k=v"]}` JSON
    /// document, percent-encoded. Built with `serde_json` so a label value
    /// carrying quotes or backslashes can never break the document shape.
    fn encode(&self) -> String {
        let Self::LabelEquals { key, value } = self;
        let document = serde_json::json!({ "label": [format!("{key}={value}")] });
        encode_segment(&document.to_string())
    }
}

type EngineResult<T> = Result<T, EngineError>;
type FaultResult<T> = Result<T, (EngineFaultKind, String)>;

// ---------------------------------------------------------------------------
// HTTP/1.1 framing over the socket
// ---------------------------------------------------------------------------

fn header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

struct ResponseHead {
    status: u16,
    content_length: Option<usize>,
    chunked: bool,
}

fn parse_head(head: &[u8]) -> FaultResult<ResponseHead> {
    let text = std::str::from_utf8(head).map_err(|_| {
        (
            EngineFaultKind::Framing,
            "response head is not UTF-8".into(),
        )
    })?;
    let mut lines = text.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| (EngineFaultKind::Framing, "empty response head".into()))?;
    let mut words = status_line.split_whitespace();
    let version = words
        .next()
        .ok_or_else(|| (EngineFaultKind::Framing, "missing HTTP version".into()))?;
    if !version.starts_with("HTTP/") {
        return Err((EngineFaultKind::Framing, format!("bad version {version:?}")));
    }
    let code = words.next().and_then(|word| word.parse::<u16>().ok());
    let Some(status) = code else {
        return Err((
            EngineFaultKind::Framing,
            format!("bad status line {status_line:?}"),
        ));
    };
    let mut content_length = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            let length = value.trim().parse::<usize>().map_err(|_| {
                (
                    EngineFaultKind::Framing,
                    format!("bad content-length {value:?}"),
                )
            })?;
            content_length = Some(length);
        } else if name.trim().eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
    }
    Ok(ResponseHead {
        status,
        content_length,
        chunked,
    })
}

/// Read one full response: head plus exactly its body. Bodies are capped at
/// [`MAX_BODY_BYTES`]; chunked framing (what the live daemon sends) is
/// slurped under the same cap and decoded from memory by [`decode_chunked`].
async fn read_response(stream: &mut tokio::net::UnixStream) -> FaultResult<(u16, Vec<u8>)> {
    use tokio::io::AsyncReadExt as _;
    let mut buffer = Vec::with_capacity(8 * 1024);
    let mut chunk = [0_u8; 8192];
    let (head, body_start) = loop {
        if buffer.len() > MAX_HEAD_BYTES {
            return Err((
                EngineFaultKind::TooLarge,
                format!("response head exceeds {MAX_HEAD_BYTES} bytes"),
            ));
        }
        if let Some(end) = header_end(&buffer) {
            break (parse_head(&buffer[..end])?, end);
        }
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| (EngineFaultKind::Read, error.to_string()))?;
        if read == 0 {
            return Err((EngineFaultKind::Read, "eof before response head".into()));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    // Chunked wins over a declared length when both are present (RFC 9112:
    // a sender must not generate both, a recipient must ignore the length).
    if head.chunked {
        let framed = read_to_end(stream, &mut buffer, &mut chunk, body_start, "chunked").await?;
        return Ok((head.status, decode_chunked(&framed)?));
    }
    if let Some(length) = head.content_length {
        if length > MAX_BODY_BYTES {
            return Err((
                EngineFaultKind::TooLarge,
                format!("declared body {length} exceeds {MAX_BODY_BYTES} bytes"),
            ));
        }
        let end = body_start + length;
        while buffer.len() < end {
            let read = stream
                .read(&mut chunk)
                .await
                .map_err(|error| (EngineFaultKind::Read, error.to_string()))?;
            if read == 0 {
                return Err((EngineFaultKind::Read, "eof inside response body".into()));
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        return Ok((head.status, buffer[body_start..end].to_vec()));
    }
    // Close-delimited: read until the daemon closes, still capped.
    let body = read_to_end(
        stream,
        &mut buffer,
        &mut chunk,
        body_start,
        "close-delimited",
    )
    .await?;
    Ok((head.status, body))
}

/// Drain the stream from `body_start` to EOF under [`MAX_BODY_BYTES`].
/// `Connection: close` guarantees the EOF; the cap guarantees the bound.
async fn read_to_end(
    stream: &mut tokio::net::UnixStream,
    buffer: &mut Vec<u8>,
    chunk: &mut [u8; 8192],
    body_start: usize,
    what: &str,
) -> FaultResult<Vec<u8>> {
    use tokio::io::AsyncReadExt as _;
    while buffer.len() - body_start <= MAX_BODY_BYTES {
        let read = stream
            .read(chunk)
            .await
            .map_err(|error| (EngineFaultKind::Read, error.to_string()))?;
        if read == 0 {
            return Ok(buffer[body_start..].to_vec());
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Err((
        EngineFaultKind::TooLarge,
        format!("{what} body exceeds {MAX_BODY_BYTES} bytes"),
    ))
}

/// Decode one chunked-framed body: `size[;extensions] CRLF data CRLF`,
/// ending in a zero chunk plus trailers. Pure over the slurped bytes so
/// every framing edge is a unit case, not a socket dance.
fn decode_chunked(framed: &[u8]) -> FaultResult<Vec<u8>> {
    let mut body = Vec::new();
    let mut cursor = 0;
    loop {
        let line_end = memchr_crlf(framed, cursor).ok_or_else(|| {
            (
                EngineFaultKind::Framing,
                "chunked body ends inside a chunk line".to_string(),
            )
        })?;
        let line = std::str::from_utf8(&framed[cursor..line_end]).map_err(|_| {
            (
                EngineFaultKind::Framing,
                "chunk size line is not UTF-8".to_string(),
            )
        })?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| {
            (
                EngineFaultKind::Framing,
                format!("bad chunk size {size_text:?}"),
            )
        })?;
        cursor = line_end + 2;
        if size == 0 {
            // Trailers end at the first empty line; content is metadata the
            // daemon does not send on these endpoints, so skip, do not parse.
            loop {
                let trailer_end = memchr_crlf(framed, cursor).ok_or_else(|| {
                    (
                        EngineFaultKind::Framing,
                        "chunked body ends inside trailers".to_string(),
                    )
                })?;
                if trailer_end == cursor {
                    return Ok(body);
                }
                cursor = trailer_end + 2;
            }
        }
        let data_end = cursor.checked_add(size).ok_or_else(|| {
            (
                EngineFaultKind::Framing,
                "chunk size overflows the body".to_string(),
            )
        })?;
        if framed.len() < data_end + 2 || &framed[data_end..data_end + 2] != b"\r\n" {
            return Err((
                EngineFaultKind::Framing,
                "chunk data is short or unterminated".to_string(),
            ));
        }
        if body.len() + size > MAX_BODY_BYTES {
            return Err((
                EngineFaultKind::TooLarge,
                format!("decoded body exceeds {MAX_BODY_BYTES} bytes"),
            ));
        }
        body.extend_from_slice(&framed[cursor..data_end]);
        cursor = data_end + 2;
    }
}

/// Offset of the `\r` in the first CRLF at or after `from`, if any.
fn memchr_crlf(buffer: &[u8], from: usize) -> Option<usize> {
    buffer[from..]
        .windows(2)
        .position(|window| window == b"\r\n")
        .map(|index| from + index)
}

// ---------------------------------------------------------------------------
// Typed documents: strict fields, so skew falls back instead of misreading
// ---------------------------------------------------------------------------

/// The container inspect fields the facade's five container queries read.
/// Everything required: a missing field is a schema error (CLI fallback),
/// never a defaulted guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineContainer {
    pub id: String,
    pub running: bool,
    pub status: String,
    pub health: Option<String>,
    pub finished_at: String,
    pub ports: Vec<super::client::PortMapping>,
}

impl EngineContainer {
    /// The word the readiness projection would have rendered: health status
    /// when a healthcheck exists, else the lifecycle status — mirroring the
    /// `{{if .State.Health}}...{{else}}...{{end}}` template exactly.
    #[must_use]
    pub fn readiness_word(&self) -> &str {
        self.health.as_deref().unwrap_or(&self.status)
    }
}

/// Resolved image identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineImage {
    pub id: String,
}

/// The daemon-generation fact the cgroup boundary proof caches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineInfo {
    pub cgroup_driver: String,
    pub cgroup_version: String,
}

/// Daemon version identity.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "next slice routes version through the facade")
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineVersion {
    pub version: String,
    pub api_version: String,
}

/// Network identity: one list row or one inspect document carry the same
/// `Name`/`Id` pair, so one struct serves both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineNetwork {
    pub name: String,
    pub id: String,
}

/// One container list row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineContainerSummary {
    pub id: String,
    /// Raw daemon names (leading `/` kept: that is the documented shape).
    pub names: Vec<String>,
    pub labels: std::collections::BTreeMap<String, String>,
    pub state: String,
}

/// One volume list row: the name `volume ls -q` prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineVolume {
    pub name: String,
}

fn schema_missing(pointer: &str) -> (EngineFaultKind, String) {
    (
        EngineFaultKind::Schema,
        format!("inspect document has no {pointer}"),
    )
}

fn require_str<'v>(value: &'v serde_json::Value, pointer: &str) -> FaultResult<&'v str> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| schema_missing(pointer))
}

fn require_bool(value: &serde_json::Value, pointer: &str) -> FaultResult<bool> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| schema_missing(pointer))
}

fn parse_container(value: &serde_json::Value) -> FaultResult<EngineContainer> {
    Ok(EngineContainer {
        id: require_str(value, "/Id")?.to_string(),
        running: require_bool(value, "/State/Running")?,
        status: require_str(value, "/State/Status")?.to_string(),
        health: value
            .pointer("/State/Health/Status")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        finished_at: require_str(value, "/State/FinishedAt")?.to_string(),
        ports: parse_inspect_ports(value)?,
    })
}

/// Published bindings from `NetworkSettings.Ports` into the exact shape
/// `docker port` prints: one mapping per binding, IPv6 hosts bracketed
/// (`[::]:41062`, proven by the facade's live fixture). Exposed-but-
/// unpublished ports (`null` bindings) are skipped exactly as the CLI omits
/// them, and an absent `Ports` object reads as no bindings. Sorted in the
/// shared order ([`super::client::sort_port_mappings`]): the daemon prints
/// `docker port` in its own numeric order while JSON maps iterate sorted,
/// so unsorted projections would disagree and `service_context`'s
/// first-wins pick per container port could differ by transport.
fn parse_inspect_ports(value: &serde_json::Value) -> FaultResult<Vec<super::client::PortMapping>> {
    let Some(ports) = value.pointer("/NetworkSettings/Ports") else {
        return Ok(Vec::new());
    };
    if ports.is_null() {
        return Ok(Vec::new());
    }
    let object = ports
        .as_object()
        .ok_or_else(|| schema_missing("/NetworkSettings/Ports"))?;
    let mut mappings = Vec::new();
    for (container_port, bindings) in object {
        if bindings.is_null() {
            continue;
        }
        let bindings = bindings
            .as_array()
            .ok_or_else(|| schema_missing("/NetworkSettings/Ports bindings"))?;
        for binding in bindings {
            let host_ip = require_str(binding, "/HostIp")?;
            let host_port = require_str(binding, "/HostPort")?;
            let host_address = if host_ip.contains(':') {
                format!("[{host_ip}]:{host_port}")
            } else {
                format!("{host_ip}:{host_port}")
            };
            mappings.push(super::client::PortMapping {
                container_port: container_port.clone(),
                host_address,
            });
        }
    }
    super::client::sort_port_mappings(&mut mappings);
    Ok(mappings)
}

fn parse_image(value: &serde_json::Value) -> FaultResult<EngineImage> {
    Ok(EngineImage {
        id: require_str(value, "/Id")?.to_string(),
    })
}

fn parse_info(value: &serde_json::Value) -> FaultResult<EngineInfo> {
    let version = value.pointer("/CgroupVersion");
    let cgroup_version = version
        .and_then(serde_json::Value::as_u64)
        .map(|version| version.to_string())
        .or_else(|| {
            version
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| schema_missing("/CgroupVersion"))?;
    Ok(EngineInfo {
        cgroup_driver: require_str(value, "/CgroupDriver")?.to_string(),
        cgroup_version,
    })
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "next slice routes version through the facade")
)]
fn parse_version(value: &serde_json::Value) -> FaultResult<EngineVersion> {
    Ok(EngineVersion {
        version: require_str(value, "/Version")?.to_string(),
        api_version: require_str(value, "/ApiVersion")?.to_string(),
    })
}

fn parse_network(value: &serde_json::Value) -> FaultResult<EngineNetwork> {
    Ok(EngineNetwork {
        name: require_str(value, "/Name")?.to_string(),
        id: require_str(value, "/Id")?.to_string(),
    })
}

fn parse_network_list(value: &serde_json::Value) -> FaultResult<Vec<EngineNetwork>> {
    let rows = value
        .as_array()
        .ok_or_else(|| schema_missing("network list array"))?;
    rows.iter().map(parse_network).collect()
}

/// Volume rows from `{"Volumes": [...], "Warnings": ...}`. A null `Volumes`
/// reads as empty: null and `[]` both unambiguously mean "no volumes"
/// (the CLI prints nothing for either), so there is nothing to misread —
/// but a missing key or a non-array is skew, and falls back.
fn parse_volume_list(value: &serde_json::Value) -> FaultResult<Vec<EngineVolume>> {
    let Some(volumes) = value.get("Volumes") else {
        return Err(schema_missing("/Volumes"));
    };
    if volumes.is_null() {
        return Ok(Vec::new());
    }
    let rows = volumes
        .as_array()
        .ok_or_else(|| schema_missing("/Volumes array"))?;
    rows.iter()
        .map(|row| {
            Ok(EngineVolume {
                name: require_str(row, "/Name")?.to_string(),
            })
        })
        .collect()
}

fn parse_container_list(value: &serde_json::Value) -> FaultResult<Vec<EngineContainerSummary>> {
    let rows = value
        .as_array()
        .ok_or_else(|| schema_missing("container list array"))?;
    let mut summaries = Vec::with_capacity(rows.len());
    for row in rows {
        let id = require_str(row, "/Id")?.to_string();
        let state = require_str(row, "/State")?.to_string();
        let names = row
            .pointer("/Names")
            .and_then(serde_json::Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let labels = row
            .pointer("/Labels")
            .and_then(serde_json::Value::as_object)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.clone(), value.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        summaries.push(EngineContainerSummary {
            id,
            names,
            labels,
            state,
        });
    }
    Ok(summaries)
}

#[cfg(test)]
pub(crate) use mock::MockEngine;

#[cfg(test)]
pub(crate) mod mock {
    //! Scripted Engine socket server for tests: std blocking I/O on a helper
    //! thread (the `unix_api.rs` test shape), routing canned responses by
    //! request path.

    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::Duration;

    type MockHandler = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

    pub(crate) struct MockEngine {
        pub socket: PathBuf,
        dir: PathBuf,
        shutdown: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    /// Process-unique mock id. Wall-clock nanos alone collide across
    /// parallel tests on coarse clocks (macOS), taking two mocks to the
    /// same socket path and failing the second bind. The counter alone is
    /// the name: short enough for `SUN_LEN` on macOS (104 bytes) under a
    /// deep `TMPDIR`, which timestamps are not.
    static MOCK_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    impl MockEngine {
        /// Serve `handler` (request head in, raw response bytes out) until
        /// dropped or `max_connections` connections have been served.
        pub(crate) fn serve(
            handler: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static,
            max_connections: usize,
        ) -> Self {
            let seq = MOCK_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("ve-{}-{seq}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let socket = dir.join("d.sock");
            // A crashed run can leave its socket file behind; only a live
            // server can hold the path, and no live server shares this
            // process-unique directory.
            std::fs::remove_file(&socket).ok();
            let listener = UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let handler: MockHandler = Arc::new(handler);
            let shutdown = Arc::new(AtomicBool::new(false));
            let shutdown_thread = Arc::clone(&shutdown);
            let handle = std::thread::spawn(move || {
                let mut served = 0;
                while served < max_connections && !shutdown_thread.load(Ordering::Relaxed) {
                    let accepted = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        Err(_) => break,
                    };
                    // The listener is nonblocking so shutdown stays prompt,
                    // but an accepted stream inherits that on some platforms
                    // (macOS): force blocking before the blocking read loop,
                    // or reads fail with WouldBlock and the server exits.
                    if accepted.set_nonblocking(false).is_err() {
                        continue;
                    }
                    let mut stream = accepted;
                    served += 1;
                    if Self::handle_one(&mut stream, &handler).is_err() {
                        break;
                    }
                }
            });
            Self {
                socket,
                dir,
                shutdown,
                handle: Some(handle),
            }
        }

        fn handle_one(stream: &mut UnixStream, handler: &MockHandler) -> std::io::Result<()> {
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            let mut buffer = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let read = stream.read(&mut chunk)?;
                if read == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                if buffer.len() > 64 * 1024 {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buffer).into_owned();
            let response = handler(&head);
            stream.write_all(&response)?;
            stream.shutdown(std::net::Shutdown::Both).ok();
            Ok(())
        }
    }

    impl Drop for MockEngine {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            if let Some(handle) = self.handle.take() {
                handle.join().ok();
            }
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    /// Raw `200` JSON response with an explicit length, the shape these
    /// endpoints serve.
    pub(crate) fn json_response(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// Raw close-delimited `200` response (no length): the client must read
    /// until the daemon closes.
    pub(crate) fn close_delimited(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}"
        )
        .into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::mock::{close_delimited, json_response};
    use super::*;
    use std::sync::Arc;

    const BUDGET: Duration = Duration::from_secs(5);

    fn inspect_body() -> &'static str {
        r#"{"Id":"a530e70d9e1e35941b6fc12db9b51a7b19c6d02","State":{"Running":true,"Status":"running","FinishedAt":"0001-01-01T00:00:00Z","Health":{"Status":"healthy"}},"NetworkSettings":{"Ports":{"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"41062"},{"HostIp":"::","HostPort":"41062"}],"9090/tcp":null}}}"#
    }

    #[test]
    fn encode_segment_escapes_references_but_keeps_names_readable() {
        assert_eq!(encode_segment("velnor-job-9"), "velnor-job-9");
        assert_eq!(
            encode_segment("velnor/job-ubuntu:26.04"),
            "velnor%2Fjob-ubuntu%3A26.04"
        );
        assert_eq!(encode_segment("a b+c"), "a%20b%2Bc");
    }

    #[test]
    fn error_display_carries_codes_never_bodies() {
        let timeout = EngineError::Timeout {
            op: DockerOp::Query,
            budget: Duration::from_millis(50),
        };
        assert_eq!(
            format!("{timeout}"),
            "engine api query timed out after 50ms"
        );
        let fault = EngineError::fault(
            DockerOp::DaemonQuery,
            (EngineFaultKind::Status, "http 404".into()),
        );
        assert_eq!(
            format!("{fault}"),
            "engine api daemon-query status: http 404"
        );
        assert_eq!(fault.kind(), Some(EngineFaultKind::Status));
        assert_eq!(timeout.kind(), None);
    }

    #[test]
    fn head_parser_reads_status_length_and_chunked() {
        let head = parse_head(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\n").unwrap();
        assert_eq!(head.status, 200);
        assert_eq!(head.content_length, Some(12));
        assert!(!head.chunked);
        let head =
            parse_head(b"HTTP/1.0 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
        assert_eq!(head.status, 404);
        assert!(head.chunked);
        assert!(parse_head(b"HTTP/1.1 OK\r\n\r\n").is_err());
        assert!(parse_head(b"GARBAGE\r\n\r\n").is_err());
        assert!(parse_head(b"HTTP/1.1 200 OK\r\nContent-Length: huge\r\n\r\n").is_err());
    }

    #[test]
    fn inspect_parse_reads_all_five_facade_fields() {
        let value: serde_json::Value = serde_json::from_str(inspect_body()).unwrap();
        let container = parse_container(&value).unwrap();
        assert_eq!(container.id, "a530e70d9e1e35941b6fc12db9b51a7b19c6d02");
        assert!(container.running);
        assert_eq!(container.readiness_word(), "healthy");
        assert_eq!(container.finished_at, "0001-01-01T00:00:00Z");
        assert_eq!(
            container.ports,
            vec![
                super::super::client::PortMapping {
                    container_port: "8080/tcp".into(),
                    host_address: "0.0.0.0:41062".into(),
                },
                super::super::client::PortMapping {
                    container_port: "8080/tcp".into(),
                    host_address: "[::]:41062".into(),
                },
            ]
        );
    }

    #[test]
    fn inspect_parse_falls_back_to_lifecycle_without_healthcheck() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"Id":"id","State":{"Running":false,"Status":"exited","FinishedAt":"2026-09-12T20:11:02.437775991Z"}}"#)
                .unwrap();
        let container = parse_container(&value).unwrap();
        assert!(!container.running);
        assert_eq!(container.readiness_word(), "exited");
        assert!(container.ports.is_empty());
    }

    #[test]
    fn inspect_parse_accepts_live_engine_shape() {
        // Id/State/NetworkSettings verbatim from Engine 29.4.0
        // `GET /containers/velnor-eng-probe/json` (Config/HostConfig elided:
        // the parser never reads them, and Env carries image secrets).
        // Live notes: no `Health` key at all without a healthcheck, and
        // `Ports` is `{}` on a created container.
        let live = r#"{"Id":"bd9ace6535cb18f6f20d0532c1276a51e50ab1db44885140c600a0c736fe2977","State":{"Status":"created","Running":false,"Paused":false,"Restarting":false,"OOMKilled":false,"Dead":false,"Pid":0,"ExitCode":0,"Error":"","StartedAt":"0001-01-01T00:00:00Z","FinishedAt":"0001-01-01T00:00:00Z"},"NetworkSettings":{"SandboxID":"","SandboxKey":"","Ports":{},"Networks":{"bridge":{"IPAMConfig":null,"Links":null,"Aliases":null,"DriverOpts":null,"GwPriority":0,"NetworkID":"","EndpointID":"","Gateway":"","IPAddress":"","MacAddress":"","IPPrefixLen":0,"IPv6Gateway":"","GlobalIPv6Address":"","GlobalIPv6PrefixLen":0,"DNSNames":null}}}}"#;
        let value: serde_json::Value = serde_json::from_str(live).unwrap();
        let container = parse_container(&value).unwrap();
        assert_eq!(
            container.id,
            "bd9ace6535cb18f6f20d0532c1276a51e50ab1db44885140c600a0c736fe2977"
        );
        assert!(!container.running);
        assert_eq!(container.readiness_word(), "created");
        assert_eq!(container.finished_at, "0001-01-01T00:00:00Z");
        assert!(container.ports.is_empty());
    }

    #[test]
    fn inspect_parse_rejects_missing_fields_as_schema_errors() {
        for body in [
            r#"{"State":{"Running":true,"Status":"running","FinishedAt":"x"}}"#,
            r#"{"Id":"id","State":{"Status":"running","FinishedAt":"x"}}"#,
            r#"{"Id":"id","State":{"Running":true,"FinishedAt":"x"}}"#,
            r#"{"Id":"id","State":{"Running":true,"Status":"running"}}"#,
        ] {
            let value: serde_json::Value = serde_json::from_str(body).unwrap();
            let (kind, detail) = parse_container(&value).unwrap_err();
            assert_eq!(kind, EngineFaultKind::Schema, "{body}");
            assert!(
                detail.contains("/State") || detail.contains("/Id"),
                "{detail}"
            );
        }
    }

    #[test]
    fn info_version_network_and_list_parse() {
        let info: serde_json::Value =
            serde_json::from_str(r#"{"CgroupDriver":"systemd","CgroupVersion":2}"#).unwrap();
        assert_eq!(
            parse_info(&info).unwrap(),
            EngineInfo {
                cgroup_driver: "systemd".into(),
                cgroup_version: "2".into(),
            }
        );
        // Live 29.4.0 serves the version as a string; the CLI template
        // renders both forms identically, so the parser accepts both.
        let live: serde_json::Value =
            serde_json::from_str(r#"{"CgroupDriver":"cgroupfs","CgroupVersion":"2"}"#).unwrap();
        assert_eq!(
            parse_info(&live).unwrap(),
            EngineInfo {
                cgroup_driver: "cgroupfs".into(),
                cgroup_version: "2".into(),
            }
        );
        let version: serde_json::Value =
            serde_json::from_str(r#"{"Version":"29.4.0","ApiVersion":"1.52"}"#).unwrap();
        assert_eq!(
            parse_version(&version).unwrap(),
            EngineVersion {
                version: "29.4.0".into(),
                api_version: "1.52".into(),
            }
        );
        let network: serde_json::Value =
            serde_json::from_str(r#"{"Name":"velnor-net","Id":"abc123"}"#).unwrap();
        assert_eq!(
            parse_network(&network).unwrap(),
            EngineNetwork {
                name: "velnor-net".into(),
                id: "abc123".into(),
            }
        );
        let list: serde_json::Value = serde_json::from_str(
            r#"[{"Id":"aaa","Names":["/velnor-job-1"],"Labels":{"velnor.job-id":"velnor-job-1"},"State":"running"},{"Id":"bbb","State":"exited"}]"#,
        )
        .unwrap();
        let summaries = parse_container_list(&list).unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].names, vec!["/velnor-job-1".to_string()]);
        assert_eq!(
            summaries[0].labels.get("velnor.job-id").map(String::as_str),
            Some("velnor-job-1")
        );
        assert!(summaries[1].names.is_empty());
    }

    #[tokio::test]
    async fn each_endpoint_gets_unversioned_native_paths() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_server = Arc::clone(&seen);
        let mock = MockEngine::serve(
            move |head| {
                let line = head.lines().next().unwrap_or("").to_string();
                seen_server.lock().unwrap().push(line);
                if head.contains("GET /version ") {
                    json_response(r#"{"Version":"29.4.0","ApiVersion":"1.52"}"#)
                } else if head.contains("GET /info ") {
                    json_response(r#"{"CgroupDriver":"systemd","CgroupVersion":2}"#)
                } else if head.contains("GET /containers/json?") {
                    json_response(r#"[{"Id":"aaa","State":"running"}]"#)
                } else if head.contains("/containers/") {
                    json_response(inspect_body())
                } else if head.contains("/images/") {
                    json_response(r#"{"Id":"sha256:feed"}"#)
                } else if head.contains("GET /networks?") {
                    json_response(r#"[{"Name":"n","Id":"i"}]"#)
                } else if head.contains("GET /volumes?") {
                    json_response(r#"{"Volumes":[{"Name":"v"}],"Warnings":null}"#)
                } else {
                    json_response(r#"{"Name":"n","Id":"i"}"#)
                }
            },
            8,
        );
        let client = EngineClient::new(mock.socket.clone());
        let filter = ListFilter::label_equals("velnor.job-id", "velnor-job-1");
        assert_eq!(client.version(BUDGET).await.unwrap().api_version, "1.52");
        assert_eq!(
            client.daemon_info(BUDGET).await.unwrap().cgroup_driver,
            "systemd"
        );
        assert_eq!(
            client.list_containers(&filter, BUDGET).await.unwrap().len(),
            1
        );
        assert_eq!(
            client.list_networks(&filter, BUDGET).await.unwrap().len(),
            1
        );
        assert_eq!(client.list_volumes(&filter, BUDGET).await.unwrap().len(), 1);
        assert!(
            client
                .inspect_container("svc", BUDGET)
                .await
                .unwrap()
                .running
        );
        assert_eq!(
            client.inspect_image("img:tag", BUDGET).await.unwrap().id,
            "sha256:feed"
        );
        assert_eq!(client.inspect_network("n", BUDGET).await.unwrap().id, "i");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 8);
        for line in seen.iter() {
            assert!(line.starts_with("GET /"), "{line}");
            assert!(!line.contains("/v1."), "no pinned old version: {line}");
        }
        assert!(seen.iter().any(|line| line == "GET /version HTTP/1.1"));
        assert!(seen.iter().any(|line| line == "GET /info HTTP/1.1"));
        let filters = "%7B%22label%22%3A%5B%22velnor.job-id%3Dvelnor-job-1%22%5D%7D";
        assert!(
            seen.iter()
                .any(|line| line
                    == &format!("GET /containers/json?all=1&filters={filters} HTTP/1.1")),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .any(|line| line == &format!("GET /networks?filters={filters} HTTP/1.1")),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .any(|line| line == &format!("GET /volumes?filters={filters} HTTP/1.1")),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .any(|line| line == "GET /images/img%3Atag/json HTTP/1.1"),
            "{seen:?}"
        );
    }

    #[test]
    fn list_filter_encodes_label_equality_as_daemon_json() {
        assert_eq!(
            ListFilter::label_equals("velnor.job-id", "velnor-job-1").encode(),
            "%7B%22label%22%3A%5B%22velnor.job-id%3Dvelnor-job-1%22%5D%7D"
        );
        // A hostile value stays inside the JSON string: quotes are JSON
        // escapes first, percent-encoding second, so the document shape holds.
        let encoded = ListFilter::label_equals("k", r#"a"b\c"#).encode();
        assert!(
            encoded.contains("%5C%22"),
            "quote must arrive escaped, got {encoded}"
        );
    }

    #[test]
    fn network_and_volume_lists_parse_live_shapes() {
        // `GET /networks?filters=...`, Engine 29.4.0, verbatim.
        let networks: serde_json::Value = serde_json::from_str(
            r#"[{"Name":"velnor-eng2-net","Id":"51890326820b1aaec84f85251e1ae0695801bffc1f6504481191ce7c6f647bdc","Created":"2026-09-14T01:40:37.95747574+07:00","Scope":"local","Driver":"bridge","EnableIPv4":true,"EnableIPv6":false,"IPAM":{"Driver":"default","Options":{},"Config":[{"Subnet":"192.168.97.0/24","Gateway":"192.168.97.1"}]},"Internal":false,"Attachable":false,"Ingress":false,"ConfigFrom":{"Network":""},"ConfigOnly":false,"Options":{},"Labels":{"velnor.job-id":"velnor-eng2-probe"}}]"#,
        )
        .unwrap();
        assert_eq!(
            parse_network_list(&networks).unwrap(),
            vec![EngineNetwork {
                name: "velnor-eng2-net".into(),
                id: "51890326820b1aaec84f85251e1ae0695801bffc1f6504481191ce7c6f647bdc".into(),
            }]
        );
        // `GET /volumes?filters=...`, Engine 29.4.0, verbatim.
        let volumes: serde_json::Value = serde_json::from_str(
            r#"{"Volumes":[{"CreatedAt":"2026-09-14T01:40:38+07:00","Driver":"local","Labels":{"velnor.job-id":"velnor-eng2-probe"},"Mountpoint":"/var/lib/docker/volumes/velnor-eng2-vol/_data","Name":"velnor-eng2-vol","Options":null,"Scope":"local"}],"Warnings":null}"#,
        )
        .unwrap();
        assert_eq!(
            parse_volume_list(&volumes).unwrap(),
            vec![EngineVolume {
                name: "velnor-eng2-vol".into(),
            }]
        );
        // Empty matches read as empty on every daemon generation: live
        // 29.4.0 sends `[]`; a null `Volumes` is the same fact, not skew.
        for body in [
            r#"{"Volumes":[],"Warnings":null}"#,
            r#"{"Volumes":null,"Warnings":null}"#,
        ] {
            let value: serde_json::Value = serde_json::from_str(body).unwrap();
            assert!(parse_volume_list(&value).unwrap().is_empty(), "{body}");
        }
        assert!(parse_network_list(&serde_json::json!([]))
            .unwrap()
            .is_empty());
        // A missing `Volumes` key or a trashed row is skew, and falls back.
        for body in [r#"{"Warnings":null}"#, r#"{"Volumes":[{}]}"#] {
            let value: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(
                parse_volume_list(&value).unwrap_err().0,
                EngineFaultKind::Schema,
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn close_delimited_bodies_read_until_eof() {
        let mock = MockEngine::serve(
            |_| close_delimited(r#"{"Version":"x","ApiVersion":"y"}"#),
            1,
        );
        let client = EngineClient::new(mock.socket.clone());
        assert_eq!(client.version(BUDGET).await.unwrap().version, "x");
    }

    #[test]
    fn chunked_bodies_decode_across_chunks_extensions_and_trailers() {
        // Multi-chunk with an extension and a trailer, the live shape.
        let framed = b"7;ext=1\r\n{\"a\":1,\r\n7\r\n\"b\":22}\r\n0\r\nX-Trailer: yes\r\n\r\n";
        assert_eq!(
            decode_chunked(framed).unwrap(),
            br#"{"a":1,"b":22}"#.to_vec()
        );
        // Empty body is one zero chunk.
        assert_eq!(decode_chunked(b"0\r\n\r\n").unwrap(), Vec::<u8>::new());
        // Truncations and bad sizes never decode.
        for bad in [
            b"3\r\nab".as_slice(),
            b"zz\r\nabc\r\n0\r\n\r\n".as_slice(),
            b"3\r\nabc\r\n".as_slice(),
            b"3\r\nabcXX0\r\n\r\n".as_slice(),
            b"1\r\na\r\n0\r\n".as_slice(),
        ] {
            assert_eq!(
                decode_chunked(bad).unwrap_err().0,
                EngineFaultKind::Framing,
                "{bad:?}"
            );
        }
    }

    #[tokio::test]
    async fn unbounded_bodies_hit_the_cap_instead_of_the_heap() {
        let big = "0".repeat(9 * 1024 * 1024);
        let mock = MockEngine::serve(move |_| close_delimited(&big), 1);
        let error = EngineClient::new(mock.socket.clone())
            .version(BUDGET)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), Some(EngineFaultKind::TooLarge));
    }

    #[tokio::test]
    async fn chunked_responses_decode_over_the_socket() {
        let mock = MockEngine::serve(
            |_| {
                let body = r#"{"CgroupDriver":"systemd","CgroupVersion":2}"#;
                let (first, second) = body.split_at(body.len() / 2);
                format!(
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{first_len:x}\r\n{first}\r\n{second_len:x}\r\n{second}\r\n0\r\n\r\n",
                    first_len = first.len(),
                    second_len = second.len(),
                )
                .into_bytes()
            },
            1,
        );
        let info = EngineClient::new(mock.socket.clone())
            .daemon_info(BUDGET)
            .await
            .unwrap();
        assert_eq!(info.cgroup_driver, "systemd");
        assert_eq!(info.cgroup_version, "2");
    }

    #[tokio::test]
    async fn daemon_statuses_garbage_and_skew_are_typed_api_errors() {
        // 404: code only, never the error document.
        let mock = MockEngine::serve(
            |_| {
                let body = r#"{"message":"No such container"}"#;
                format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .into_bytes()
            },
            1,
        );
        let error = EngineClient::new(mock.socket.clone())
            .inspect_container("gone", BUDGET)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), Some(EngineFaultKind::Status));
        assert!(!format!("{error}").contains("No such"), "{error}");

        // Garbage bytes are a JSON error, not a misread.
        let mock = MockEngine::serve(|_| json_response("not json{{{"), 1);
        let error = EngineClient::new(mock.socket.clone())
            .daemon_info(BUDGET)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), Some(EngineFaultKind::Json));

        // Skewed schema is a schema error (facade falls back).
        let mock = MockEngine::serve(|_| json_response(r#"{"surprise":true}"#), 1);
        let error = EngineClient::new(mock.socket.clone())
            .inspect_container("svc", BUDGET)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), Some(EngineFaultKind::Schema));

        // Malformed chunked framing is a framing error, not a misread.
        let mock = MockEngine::serve(
            |_| {
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nabc\r\n0\r\n\r\n"
                    .to_vec()
            },
            1,
        );
        let error = EngineClient::new(mock.socket.clone())
            .daemon_info(BUDGET)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), Some(EngineFaultKind::Framing));

        // Oversize declarations never allocate.
        let mock = MockEngine::serve(
            |_| b"HTTP/1.1 200 OK\r\nContent-Length: 99999999\r\n\r\n".to_vec(),
            1,
        );
        let error = EngineClient::new(mock.socket.clone())
            .daemon_info(BUDGET)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), Some(EngineFaultKind::TooLarge));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn current_thread_context_drives_through_a_helper_thread() {
        // `block_in_place` panics on this flavor; the facade must not.
        let mock = MockEngine::serve(|_| json_response(inspect_body()), 1);
        let client = EngineClient::new(mock.socket.clone());
        let container = block_on_engine(client.inspect_container("svc", BUDGET))
            .expect("helper thread drives the future")
            .expect("mock serves inspect");
        assert!(container.running);
        assert_eq!(container.readiness_word(), "healthy");
    }

    #[test]
    fn cli_and_api_projections_agree_on_live_multiport_order() {
        // `docker create -p 9090 -p 10000`, Engine 29.4.0, both documents
        // verbatim (the container id is elided: the parser reads it but the
        // order skew lives in Ports). The daemon prints `docker port` in
        // numeric port order — the 9090 group first — while the inspect
        // document's Ports object arrives lexicographic — 10000 first. The
        // transports agree only because both projections sort.
        let cli_text = "9090/tcp -> 0.0.0.0:41895\n\
             9090/tcp -> [::]:41895\n\
             10000/tcp -> 0.0.0.0:41896\n\
             10000/tcp -> [::]:41896\n";
        let document: serde_json::Value = serde_json::from_str(
            r#"{"Id":"live-multiport-probe","State":{"Running":true,"Status":"running","FinishedAt":"0001-01-01T00:00:00Z"},"NetworkSettings":{"Ports":{"10000/tcp":[{"HostIp":"0.0.0.0","HostPort":"41896"},{"HostIp":"::","HostPort":"41896"}],"9090/tcp":[{"HostIp":"0.0.0.0","HostPort":"41895"},{"HostIp":"::","HostPort":"41895"}]}}}"#,
        )
        .unwrap();
        let cli = super::super::client::parse_port_mappings(cli_text);
        let api = parse_container(&document).unwrap().ports;
        assert_eq!(cli.len(), 4);
        assert_eq!(api.len(), 4);
        assert_eq!(cli, api, "transports must project identical mappings");
    }

    #[tokio::test]
    async fn missing_socket_is_a_connect_fault_and_slow_daemon_times_out() {
        let missing = std::path::Path::new("/no/such/velnor-engine.sock").to_path_buf();
        let error = EngineClient::new(missing)
            .daemon_info(BUDGET)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), Some(EngineFaultKind::Connect));

        let mock = MockEngine::serve(
            |_| {
                std::thread::sleep(Duration::from_millis(300));
                json_response(r#"{"CgroupDriver":"systemd","CgroupVersion":2}"#)
            },
            1,
        );
        let error = EngineClient::new(mock.socket.clone())
            .daemon_info(Duration::from_millis(30))
            .await
            .unwrap_err();
        assert!(matches!(error, EngineError::Timeout { .. }), "{error}");
    }

    #[test]
    fn routing_defaults_off_in_tests_and_env_gates_production() {
        let _serial = crate::docker::metrics::lock_serial_for_test();
        assert!(!engine_api_enabled(), "test default is off");
        unsafe { std::env::set_var("VELNOR_DOCKER_ENGINE_API", "0") };
        ROUTE_OVERRIDE.store(ROUTE_ON, Ordering::Relaxed);
        assert!(engine_api_enabled(), "explicit override beats the env gate");
        ROUTE_OVERRIDE.store(ROUTE_OFF, Ordering::Relaxed);
        assert!(!engine_api_enabled());
        ROUTE_OVERRIDE.store(ROUTE_UNSET, Ordering::Relaxed);
        assert!(!engine_api_enabled(), "env 0 disables");
        unsafe { std::env::set_var("VELNOR_DOCKER_ENGINE_API", "1") };
        assert!(engine_api_enabled(), "env 1 enables even in tests");
        unsafe { std::env::remove_var("VELNOR_DOCKER_ENGINE_API") };
        assert!(!engine_api_enabled(), "test default is off");
    }
}
