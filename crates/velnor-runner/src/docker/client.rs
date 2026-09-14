//! Typed owner of host Docker control-plane calls.
//!
//! Before this module, the runner talked to the host Engine through a dozen
//! free argument builders whose tab-separated `--format` output was re-parsed
//! by hand at every call site: eleven functions split the same shapes, and a
//! caller that needed a container's state reached past the policy layer for a
//! go-template string. That is why one missing label could fail a whole
//! snapshot in one place and silently pass in another — there was no single
//! owner of what the output means.
//!
//! [`Docker`] is that owner. Every call here returns a typed value; the
//! argument vectors and their parsers live in this file and nowhere else.
//! Control-plane methods take no timeout: the [`crate::executor`] runner seam
//! and the host transport below both apply
//! [`crate::docker::deadline_for`], so a step deadline is inexpressible at
//! these signatures by construction, not by convention. (`container_stop`'s
//! grace is the daemon's own SIGKILL wait, not a client timeout: the class
//! deadline still bounds the call above it.)
//!
//! Two transports, one policy. The job path borrows the job's
//! [`CommandRunner`](crate::executor::CommandRunner) ([`Docker::job`]), which
//! registers the process group for cancellation and attributes metrics to the
//! running job. Maintenance, startup, doctor, and the cancellation ladder have
//! no runner, so they use the host transport ([`Docker::host`]), which spawns
//! directly with the same per-class deadlines, the same invocation metrics,
//! and the in-flight `rm` claim that keeps two daemons from deadlocking on one
//! `Created` BuildKit. Both transports observe through
//! [`crate::docker::observe`]: there is still exactly one invocation counter.
//!
//! What this module deliberately does not own:
//!
//! * `run`/`exec`/`create` argument construction. That stays at its single
//!   sites (`crate::docker_argv`, `crate::container`) per the one-construction-site
//!   rule; this client owns queries and idempotent lifecycle mutations,
//!   not workload configuration.
//! * Teardown's bounded-timeout remove tolerance. `rm` under exit 124
//!   succeeds in teardown (the object is converging; doctor/boot retry
//!   until it is gone) but is a typed timeout everywhere else, so that
//!   tolerance stays at teardown's owned call site while the shared
//!   in-flight and already-removed tolerance lives in
//!   [`Docker::container_remove`].
//!
//! A missing `{{.Label}}` renders as an empty field on current Engines
//! (proven live on 29.4.0); the row parsers treat empty as absent.
//! Live-shape provenance: every parser test in this module feeds output
//! captured from a real Engine 29.4.0 invocation of the exact argument vector
//! the method runs — never hand-shaped fixtures.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use crate::executor::{CommandResult, CommandRunner, CommandStream};

// ---------------------------------------------------------------------------
// Typed results
// ---------------------------------------------------------------------------

/// Lifecycle state of a container, as reported by the Engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ContainerState {
    Created,
    Running,
    Restarting,
    Removing,
    Paused,
    Exited,
    Dead,
}

impl ContainerState {
    /// Parse an Engine state word. `None` means the daemon answered with a
    /// state this client does not know; callers treat that as their own
    /// fail-closed direction (liveness checks treat unknown as alive,
    /// deletion checks skip the row).
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "created" => Some(Self::Created),
            "running" => Some(Self::Running),
            "restarting" => Some(Self::Restarting),
            "removing" => Some(Self::Removing),
            "paused" => Some(Self::Paused),
            "exited" => Some(Self::Exited),
            "dead" => Some(Self::Dead),
            _ => None,
        }
    }

    /// True when deleting the container cannot interrupt running work.
    #[must_use]
    pub(crate) fn safe_to_reclaim(self) -> bool {
        matches!(
            self,
            Self::Created | Self::Removing | Self::Exited | Self::Dead
        )
    }
}

/// A container's readiness as the service wait loop reads it: the health
/// status when a healthcheck exists, else the lifecycle status. Mirrors the
/// `{{if .State.Health}}...{{else}}...{{end}}` projection this replaces, which
/// is why `Paused` and friends stay reachable here instead of collapsing to a
/// boolean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Readiness {
    Healthy,
    Running,
    Starting,
    Unhealthy,
    Exited,
    Dead,
    /// Empty answer: container gone between list and inspect, or an Engine
    /// that reported nothing. The wait loop treats it as ready — there is
    /// nothing left to wait for.
    Gone,
    /// A status word this client does not know. The wait loop keeps waiting
    /// on it (never ready, never stopped): an unknown state is not evidence
    /// in either direction.
    Other,
}

impl Readiness {
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim() {
            "healthy" => Self::Healthy,
            "running" => Self::Running,
            "starting" => Self::Starting,
            "unhealthy" => Self::Unhealthy,
            "exited" => Self::Exited,
            "dead" => Self::Dead,
            _ if value.trim().is_empty() => Self::Gone,
            _ => Self::Other,
        }
    }

    /// True when the wait loop may proceed.
    #[must_use]
    pub(crate) fn ready(self) -> bool {
        matches!(self, Self::Healthy | Self::Running | Self::Gone)
    }

    /// True when the container stopped before becoming ready: fail, do not wait.
    #[must_use]
    pub(crate) fn stopped(self) -> bool {
        matches!(self, Self::Exited | Self::Dead)
    }
}

/// One `docker port` mapping: container port with protocol, host address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PortMapping {
    /// `8080/tcp` as the Engine reports it; callers strip the suffix.
    pub container_port: String,
    pub host_address: String,
}

/// One owned-container list row: the short id `ps` prints plus the
/// `{{.Names}}` rendering. Both transports project this shape: the CLI leg
/// parses its tab-separated line, the API leg truncates the full id to the
/// 12-char short form and strips the leading `/` the daemon prefixes every
/// API name (both proven live on 29.4.0: `ps --format {{.ID}}` prints 12
/// chars, `{{.Names}}` renders no slash).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnedContainer {
    pub id: String,
    pub names: String,
}

/// The Engine's cgroup driver and version: the daemon-generation fact the job
/// cgroup boundary proof caches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CgroupDriver {
    pub driver: String,
    pub version: String,
}

/// A container's lifecycle word plus when it last stopped, for maintenance
/// idleness decisions. `finished` is `None` while running (the Engine
/// reports the zero time) and whenever the timestamp does not parse: `None`
/// never proves idleness, so callers treat it as recently active.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExitInfo {
    pub status: Option<ContainerState>,
    pub finished: Option<std::time::SystemTime>,
}

/// What an idempotent container start did. Both variants are success: the
/// container is running either way. The API leg distinguishes precisely
/// (204 vs 304); the CLI leg reports [`Self::Started`] on any exit 0
/// because `docker start` prints the name identically whether it started
/// the container or found it running (proven live on 29.4.0) — the
/// already-bit is exact on API, conservative on CLI, and success on both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartOutcome {
    Started,
    AlreadyStarted,
}

/// What an idempotent container stop did. Same already-bit rule as
/// [`StartOutcome`]: `docker stop` prints the name identically for a
/// fresh and a redundant stop (proven live on 29.4.0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopOutcome {
    Stopped,
    AlreadyStopped,
}

/// What an idempotent container remove did. Both legs agree exactly
/// here: the API maps 404 to [`Self::AlreadyRemoved`] and the CLI maps
/// its missing-object answer to the same variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoveOutcome {
    Removed,
    AlreadyRemoved,
}

/// A `docker` object the daemon positively reports missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotFound {
    pub object: String,
}

impl std::fmt::Display for NotFound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "docker object '{}' does not exist", self.object)
    }
}

impl std::error::Error for NotFound {}

/// The daemon reported that an object is present but not running. Callers may
/// treat this as an idempotent no-op only where their operation's contract
/// says so (BuildKit maintenance and cancellation do); it is not a generic
/// Docker failure category.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotRunning {
    pub object: String,
}

impl std::fmt::Display for NotRunning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "docker object '{}' is not running", self.object)
    }
}

impl std::error::Error for NotRunning {}

/// Buildx reports a missing client-side builder differently from the Engine's
/// `No such ...` vocabulary. Keep that distinction typed at the Docker
/// boundary so BuildKit maintenance never has to re-match formatted errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BuildkitBuilderNotFound {
    pub builder: String,
}

impl std::fmt::Display for BuildkitBuilderNotFound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "BuildKit builder '{}' does not exist",
            self.builder
        )
    }
}

impl std::error::Error for BuildkitBuilderNotFound {}

/// True when `error` is a daemon positive-missing answer surfaced through this
/// client — the only signal any caller may treat as proof of absence.
pub(crate) fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<NotFound>().is_some())
}

/// True when a Docker command positively reported its target as stopped.
pub(crate) fn is_not_running(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<NotRunning>().is_some())
}

/// True when Buildx positively reported the requested builder as absent.
pub(crate) fn is_buildkit_builder_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<BuildkitBuilderNotFound>().is_some())
}

/// The daemon's missing-object vocabulary, both generations: modern Engines
/// answer `No such container|object|image|volume`, older ones `no such ...`.
/// Deliberately narrow: the cancellation ladder treats any other failure as
/// "still alive", so a loose match here would stop the ladder early. Shared
/// by every maintenance tolerant path so the vocabulary stays single-sourced.
pub(crate) fn daemon_reports_missing(stderr: &str) -> bool {
    stderr.contains("No such") || stderr.contains("no such")
}

/// Retry category of a failed `docker` invocation, decided once at the
/// boundary where the exit code and stderr are observed. Retry policy matches
/// on this category; it never re-parses error text (GOAL 31).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DockerErrorCategory {
    /// Daemon restart or transport break: the same request may succeed later.
    Transient,
    /// Conflicting leftover state a cleanup pass can remove: one stale
    /// cleanup plus a single immediate retry.
    Conflict,
    /// Same inputs fail the same way: fail fast, no retry.
    Terminal,
}

/// Transport, daemon-unavailable, and registry-transfer vocabulary. A
/// daemon/containerd restart briefly returns the transport needles for every
/// `docker` invocation; a registry rate limit, 5xx, or stalled transfer
/// returns the transfer needles for pulls and pushes. The historical single
/// immediate retry always landed inside the same restart window, which is why
/// transient failures retry with backoff instead.
const DOCKER_TRANSIENT_NEEDLES: &[&str] = &[
    "failed to create ttrpc connection",
    "error reading from server: eof",
    "unexpected eof",
    "connection reset by peer",
    "cannot connect to the docker daemon",
    "is the docker daemon running",
    "transport is closing",
    // Registry transfer: a `docker run` that pulls fails with the registry's
    // rate limit or 5xx, or the transfer stalls and the HTTP client times out
    // mid-flight. Same shape as a daemon restart — the next attempt
    // re-resolves and resumes — so backoff, never fail fast.
    "toomanyrequests",
    "too many requests",
    "http status: 429",
    "bad gateway",
    "gateway timeout",
    "internal server error",
    "i/o timeout",
    "context deadline exceeded",
];

/// Resource-contention vocabulary: another writer (a previous attempt, a
/// concurrent daemon) holds the object this invocation wanted to own, or
/// the object is live in a way the invocation cannot take over (`docker
/// rm` on a running container: `cannot remove container ...: container
/// is running`, the daemon's 409 sentence on both transports, proven
/// live on 29.4.0).
const DOCKER_CONFLICT_NEEDLES: &[&str] = &[
    "already exists",
    "already in use",
    "already in progress",
    "cannot remove",
];

/// Boundary classifier: raw daemon/CLI stderr becomes a retry category.
/// Transient is checked first; anything unrecognized is terminal (fail
/// closed, including class-deadline timeouts: a call that never answered in
/// its class deadline is a wedged daemon, and waiting longer never turns it
/// into a success).
pub(crate) fn classify_docker_stderr(stderr: &str) -> DockerErrorCategory {
    let lower = stderr.to_ascii_lowercase();
    if DOCKER_TRANSIENT_NEEDLES
        .iter()
        .any(|needle| lower.contains(needle))
        || contains_http_5xx_status(&lower)
        || contains_registry_service_unavailable(&lower)
    {
        DockerErrorCategory::Transient
    } else if DOCKER_CONFLICT_NEEDLES
        .iter()
        .any(|needle| lower.contains(needle))
    {
        DockerErrorCategory::Conflict
    } else {
        DockerErrorCategory::Terminal
    }
}

/// Match an actual three-digit HTTP 5xx status, not the former broad
/// `http status: 5` substring. The status may be followed by punctuation or
/// prose, but never another digit.
fn contains_http_5xx_status(lower: &str) -> bool {
    let marker = "http status:";
    let mut rest = lower;
    while let Some(index) = rest.find(marker) {
        let after = &rest[index + marker.len()..];
        let digits = after.trim_start().as_bytes();
        if digits.len() >= 3
            && digits[..3].iter().all(u8::is_ascii_digit)
            && digits[0] == b'5'
            && digits.get(3).is_none_or(|byte| !byte.is_ascii_digit())
        {
            return true;
        }
        rest = &after[1.min(after.len())..];
    }
    false
}

/// `service unavailable` is useful when it is part of a registry/HTTP
/// response, but too broad as a free-standing substring: repository names,
/// labels, and operator prose can contain it without describing a retryable
/// transfer failure.
fn contains_registry_service_unavailable(lower: &str) -> bool {
    let phrase = "service unavailable";
    lower.contains(phrase)
        && (lower.contains("http")
            || lower.contains("registry")
            || lower.contains("response from daemon"))
}

/// A failed `docker` invocation with its retry category attached at the
/// boundary. `Display` reproduces the exact historical boundary message, so
/// logs and downstream text are unchanged; only the category is new.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct DockerCommandError {
    message: String,
    category: DockerErrorCategory,
}

impl DockerCommandError {
    /// Attach the boundary category to the historical failure message.
    /// `stderr` is the failing command's own stderr — the classification
    /// input — not the composite message.
    pub(crate) fn classified(message: String, stderr: &str) -> Self {
        Self {
            message,
            category: classify_docker_stderr(stderr),
        }
    }

    #[must_use]
    pub(crate) fn category(&self) -> DockerErrorCategory {
        self.category
    }
}

/// Retry-policy accessor: the category of the docker failure in this error
/// chain, or [`DockerErrorCategory::Terminal`] when no typed docker failure
/// is present. Fail closed: an error the boundary never classified
/// (filesystem, config, unknown) must not retry as if the daemon hiccupped.
/// A daemon positive-missing answer ([`NotFound`]) is Conflict-shaped: the
/// start path — the only consumer — only queries objects its own attempt
/// created, so a missing answer means another writer removed the object
/// mid-flight, and one stale cleanup plus a single retry recovers the race.
pub(crate) fn docker_error_category(error: &anyhow::Error) -> DockerErrorCategory {
    let mut category = DockerErrorCategory::Terminal;
    for cause in error.chain() {
        let candidate = cause.downcast_ref::<DockerCommandError>().map_or_else(
            || {
                cause
                    .downcast_ref::<NotFound>()
                    .map_or(DockerErrorCategory::Terminal, |_| {
                        DockerErrorCategory::Conflict
                    })
            },
            DockerCommandError::category,
        );
        if candidate.precedence() > category.precedence() {
            category = candidate;
        }
    }
    category
}

impl DockerErrorCategory {
    const fn precedence(self) -> u8 {
        match self {
            Self::Terminal => 0,
            Self::Conflict => 1,
            Self::Transient => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Fixed query shapes
// ---------------------------------------------------------------------------

/// Health status when a healthcheck exists, else the lifecycle status.
/// Kept as a projection rather than `--format=json` on purpose: the service
/// wait loop polls this up to ~10 times per service, and the projection
/// answers in bytes where the JSON document costs kilobytes. Owned here so no
/// call site spells a template.
const CONTAINER_READINESS_FORMAT: &str =
    "{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}";
const CONTAINER_RUNNING_FORMAT: &str = "{{.State.Running}}";
const CONTAINER_ID_FORMAT: &str = "{{.Id}}";
const CONTAINER_EXIT_FORMAT: &str = "{{.State.Status}} {{.State.FinishedAt}}";
const IMAGE_ID_FORMAT: &str = "{{.Id}}";
const CGROUP_FORMAT: &str = "{{.CgroupDriver}} {{.CgroupVersion}}";

// Every name operand sits behind `--`: names that reach the Engine are
// runner-generated, but a leading dash must never be able to turn an operand
// into a flag.
pub(crate) fn readiness_args(name: &str) -> Vec<String> {
    vec![
        "inspect".to_string(),
        format!("--format={CONTAINER_READINESS_FORMAT}"),
        "--".to_string(),
        name.to_string(),
    ]
}

pub(crate) fn running_args(name: &str) -> Vec<String> {
    vec![
        "inspect".to_string(),
        format!("--format={CONTAINER_RUNNING_FORMAT}"),
        "--".to_string(),
        name.to_string(),
    ]
}

pub(crate) fn container_id_args(name: &str) -> Vec<String> {
    vec![
        "inspect".to_string(),
        format!("--format={CONTAINER_ID_FORMAT}"),
        "--".to_string(),
        name.to_string(),
    ]
}

pub(crate) fn image_id_args(reference: &str) -> Vec<String> {
    vec![
        "image".to_string(),
        "inspect".to_string(),
        "-f".to_string(),
        IMAGE_ID_FORMAT.to_string(),
        "--".to_string(),
        reference.to_string(),
    ]
}

pub(crate) fn exit_info_args(name: &str) -> Vec<String> {
    vec![
        "inspect".to_string(),
        format!("--format={CONTAINER_EXIT_FORMAT}"),
        "--".to_string(),
        name.to_string(),
    ]
}

pub(crate) fn buildx_disk_usage_args(builder: &str) -> Vec<String> {
    vec![
        "buildx".to_string(),
        "du".to_string(),
        "--builder".to_string(),
        builder.to_string(),
    ]
}

pub(crate) fn daemon_cgroup_args() -> Vec<String> {
    vec![
        "info".to_string(),
        "--format".to_string(),
        CGROUP_FORMAT.to_string(),
    ]
}

pub(crate) fn mapped_ports_args(name: &str) -> Vec<String> {
    vec!["port".to_string(), "--".to_string(), name.to_string()]
}

/// Detached `docker start` of one container: the historical CLI leg of
/// [`Docker::container_start`]. Classifies `DockerOp::Start`.
pub(crate) fn container_start_args(name: &str) -> Vec<String> {
    vec!["start".to_string(), "--".to_string(), name.to_string()]
}

/// `docker stop` of one container, with the SIGKILL grace when given:
/// the historical CLI leg of [`Docker::container_stop`]. The `-t`
/// form is the non-deprecated spelling (`--time` warns on 29.4.0);
/// `None` omits the flag so the daemon default applies, exactly as the
/// API leg omits `t`. Classifies `DockerOp::Stop`, whose deadline
/// clears the explicit grace plus headroom.
pub(crate) fn container_stop_args(name: &str, timeout: Option<u64>) -> Vec<String> {
    let mut args = vec!["stop".to_string()];
    if let Some(secs) = timeout {
        args.push("-t".to_string());
        args.push(secs.to_string());
    }
    args.push("--".to_string());
    args.push(name.to_string());
    args
}

/// `docker rm` of one container: the historical CLI leg of
/// [`Docker::container_remove`]. `force` is `--force`, `volumes` is
/// `--volumes` (anonymous volumes; named volumes survive on both
/// transports, proven live on 29.4.0). Classifies `DockerOp::Remove`.
pub(crate) fn container_remove_args(name: &str, force: bool, volumes: bool) -> Vec<String> {
    let mut args = vec!["rm".to_string()];
    if force {
        args.push("--force".to_string());
    }
    if volumes {
        args.push("--volumes".to_string());
    }
    args.push("--".to_string());
    args.push(name.to_string());
    args
}

// ---------------------------------------------------------------------------
// Parsers: one owner per output shape
// ---------------------------------------------------------------------------

/// Parse a `--quiet` id listing: one id per line, sorted and deduplicated.
pub(crate) fn parse_id_list(stdout: &str) -> Vec<String> {
    let mut ids = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// The 12-char short id the CLI list legs print (`ps --format {{.ID}}` and
/// `network ls -q` both print short, proven live on 29.4.0), so the API
/// projections truncate full daemon ids to the same form. Pass-through when
/// already short: never fabricate chars.
fn short_id(full: &str) -> String {
    full.chars().take(12).collect()
}

/// Sorted, deduplicated ids: the [`parse_id_list`] postcondition for API
/// legs, which arrive unsorted from JSON arrays.
fn sorted_ids(mut ids: Vec<String>) -> Vec<String> {
    ids.sort();
    ids.dedup();
    ids
}

/// Parse `ps --format '{{.ID}}\t{{.Names}}'` rows into owned containers.
/// The row split is the historical reading, unchanged: id before the first
/// tab (the whole line when no tab is present), names after, empty ids
/// skipped. Both the text decision below and the facade's CLI leg parse
/// through here, so there is one owner of what the line means.
pub(crate) fn parse_owned_container_rows(formatted: &str) -> Vec<OwnedContainer> {
    let mut rows = Vec::new();
    for line in formatted.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (id, names) = line.split_once('\t').unwrap_or((line, line));
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        rows.push(OwnedContainer {
            id: id.to_string(),
            names: names.trim().to_string(),
        });
    }
    rows
}

/// The port order both transports agree on: lexicographic by container
/// port, then host address.
pub(crate) fn sort_port_mappings(mappings: &mut [PortMapping]) {
    mappings.sort_by(|a, b| {
        (&a.container_port, &a.host_address).cmp(&(&b.container_port, &b.host_address))
    });
}

/// Parse `docker port` output (`container-port/proto -> host:port`) into
/// mappings. Malformed lines are skipped: a service with one unparsable
/// mapping still reports the rest. Sorted in the shared order: the daemon
/// prints its own numeric order (Engine 29.4.0 prints 9090 before 10000,
/// proven live) while the API projection iterates JSON maps sorted, so an
/// unsorted parse would disagree with the API leg and `service_context`'s
/// first-wins pick per container port could differ by transport.
pub(crate) fn parse_port_mappings(text: &str) -> Vec<PortMapping> {
    let mut mappings = Vec::new();
    for line in text.lines() {
        let Some((container_port, address)) = line.split_once(" -> ") else {
            continue;
        };
        let Some((_, _host_port)) = address.rsplit_once(':') else {
            continue;
        };
        mappings.push(PortMapping {
            container_port: container_port.to_string(),
            host_address: address.to_string(),
        });
    }
    sort_port_mappings(&mut mappings);
    mappings
}

/// Parse the cgroup projection (`<driver> <version>`) into its typed fact.
pub(crate) fn parse_cgroup_projection(output: &str) -> Result<CgroupDriver> {
    let mut words = output.split_whitespace();
    let (Some(driver), Some(version), None) = (words.next(), words.next(), words.next()) else {
        anyhow::bail!("docker info cgroup probe answered {output:?}");
    };
    Ok(CgroupDriver {
        driver: driver.to_string(),
        version: version.to_string(),
    })
}

/// Parse the exit projection (`<status> <RFC3339 finished-at>`).
pub(crate) fn parse_exit_info(output: &str) -> Result<ExitInfo> {
    let (status, finished) = output
        .trim()
        .split_once(char::is_whitespace)
        .context("docker exit probe answered a single word")?;
    let finished = finished.trim();
    Ok(ExitInfo {
        status: ContainerState::parse(status),
        finished: parse_finished_at(finished),
    })
}

/// Parse an Engine `FinishedAt`. The zero time (still running) and anything
/// unparseable yield `None`, which never proves idleness.
fn parse_finished_at(value: &str) -> Option<std::time::SystemTime> {
    if value.starts_with("0001-01-01") {
        return None;
    }
    let parsed =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()?;
    let nanos = parsed.unix_timestamp_nanos();
    if nanos < 0 {
        return None;
    }
    let nanos = u64::try_from(nanos).ok()?;
    std::time::UNIX_EPOCH.checked_add(Duration::from_nanos(nanos))
}

/// Parse `buildx du` into the builder's total cache bytes, from the `Total:`
/// footer. Sizes are 1000-based (`4.096kB` is 4096 bytes, proven live) and
/// display-rounded to four significant figures, so callers treat the answer
/// as approximate — exact enough to order builders largest-first and to
/// account reclaimed bytes within a percent.
pub(crate) fn parse_buildx_disk_usage(output: &str) -> Result<u64> {
    for line in output.lines() {
        let Some(total) = line.trim().strip_prefix("Total:") else {
            continue;
        };
        return parse_human_size(total.trim())
            .with_context(|| format!("parse buildx du total {total:?}"));
    }
    anyhow::bail!("buildx du reported no Total line: {output:?}")
}

fn parse_human_size(value: &str) -> Option<u64> {
    let (number, multiplier) = [
        ("B", 1_u128),
        ("kB", 1_000),
        ("MB", 1_000_000),
        ("GB", 1_000_000_000),
        ("TB", 1_000_000_000_000),
        ("PB", 1_000_000_000_000_000),
    ]
    .iter()
    .find_map(|(unit, multiplier)| {
        value.strip_suffix(unit).and_then(|number| {
            // `kB` ends in `B`: only accept the bare-`B` split when nothing
            // longer matched, i.e. the number itself carries no unit letter.
            if *unit == "B" && number.ends_with(|ch: char| ch.is_ascii_alphabetic()) {
                None
            } else {
                Some((number, *multiplier))
            }
        })
    })?;
    let number = number.trim().parse::<f64>().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    let bytes = (number * multiplier as f64).round();
    if bytes > u64::MAX as f64 {
        return None;
    }
    Some(bytes as u64)
}

/// Names of the buildx builders Velnor owns, from `docker buildx ls` output.
///
/// The builder name always carries a scope suffix; ownership is the
/// `velnor-builder` prefix, so enumerate and match the prefix instead of
/// guessing one name.
pub(crate) fn owned_builder_names(buildx_ls_stdout: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in buildx_ls_stdout.lines() {
        let Some(first) = line.split_whitespace().next() else {
            continue;
        };
        // `docker buildx ls` marks the selected builder with a trailing `*` and
        // indents each builder's nodes; nodes are not builders.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let name = first.trim_end_matches('*');
        if name.starts_with(crate::cache::OWNED_BUILDER_PREFIX)
            && !names.iter().any(|seen| seen == name)
        {
            names.push(name.to_string());
        }
    }
    names
}

// ---------------------------------------------------------------------------
// Reclaim decisions over the listings
// ---------------------------------------------------------------------------

/// True when a `velnor.daemon-id` label value belongs to `daemon_id`: either
/// the shared work root itself or one of its direct `slot-N` children (job
/// containers are labelled with the slot work directory, while daemon
/// startup knows only the shared root).
pub(crate) fn daemon_owns_label(owner: &str, daemon_id: &str) -> bool {
    if owner == daemon_id {
        return true;
    }
    std::path::Path::new(owner).parent() == Some(std::path::Path::new(daemon_id))
        && std::path::Path::new(owner)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.strip_prefix("slot-").is_some_and(|slot| {
                    !slot.is_empty() && slot.chars().all(|c| c.is_ascii_digit())
                })
            })
}

pub(crate) struct StaleJobOwnedSnapshot {
    pub stopped_container_ids: Vec<String>,
    pub job_container_absent_or_stopped: bool,
}

/// Parse a reclaim snapshot only when it is structurally valid. The explicit
/// job-container proof is checked on both the initial and immediate snapshots
/// before any owned network or volume can be removed.
pub(crate) fn stale_job_owned_snapshot(
    job_id: &str,
    formatted: &str,
) -> Option<StaleJobOwnedSnapshot> {
    let mut ids = Vec::new();
    let mut job_container_present = false;
    let mut job_container_stopped = true;
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 {
            return None;
        }
        let id = fields[0].trim();
        let name = fields[1].trim();
        let label = fields[2].trim();
        let state = ContainerState::parse(fields[3])?;
        if id.is_empty() || name.is_empty() || label != job_id {
            return None;
        }
        if name == job_id {
            job_container_present = true;
            job_container_stopped &= state.safe_to_reclaim();
        }
        if !name.contains(crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX)
            && state.safe_to_reclaim()
        {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    Some(StaleJobOwnedSnapshot {
        stopped_container_ids: ids,
        job_container_absent_or_stopped: !job_container_present || job_container_stopped,
    })
}

/// Project API container rows into the CLI `ps` shape: short ids,
/// slashless comma-joined names. Named so the parity test covers the
/// projection directly, including the shapes the socket test never serves
/// (multi-name rows, empty names, already-short ids).
fn project_owned_rows(summaries: &[super::engine::EngineContainerSummary]) -> Vec<OwnedContainer> {
    summaries
        .iter()
        .map(|summary| OwnedContainer {
            id: short_id(&summary.id),
            names: summary
                .names
                .iter()
                .map(|name| name.strip_prefix('/').unwrap_or(name))
                .collect::<Vec<_>>()
                .join(","),
        })
        .collect()
}

/// Labeled job containers minus docker-container BuildKit daemons.
///
/// BuildKit carries `velnor.job-id`, so the generic owned-container reclaim
/// used to `docker rm --force` it with the 6h step timeout while job-end
/// and doctor also rm'd the same id. Concurrent Engine deletes of a Created
/// `buildx_buildkit_velnor-builder-*` deadlock; the leftover stays. BuildKit
/// has its own prefix reclaim with a 20s bound. Decides over typed rows —
/// CLI text parses through [`parse_owned_container_rows`] first — so the
/// two transports cannot disagree on what gets removed.
pub(crate) fn owned_container_ids_excluding_buildkit_rows(rows: &[OwnedContainer]) -> Vec<String> {
    let mut ids = rows
        .iter()
        .filter(|row| {
            !row.names
                .contains(crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX)
        })
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// Current-job LEGACY builders including Created/removing. Job-end must
/// delete them even while the job container is still running (cleanup happens
/// before rm). Persistent builders are excluded from both disjuncts: they
/// carry the creating job's label by design, and matching it here would
/// destroy a daemon other jobs share. They outlive teardown; the claim
/// release stops them, and the reclaim paths own them.
pub(crate) fn job_buildkit_ids_for_job(formatted: &str, job_id: &str, scope: &str) -> Vec<String> {
    let needle = format!(
        "{}{scope}",
        crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX
    );
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let mut parts = line.split('\t');
        let id = parts.next().unwrap_or("").trim();
        let names = parts.next().unwrap_or("").trim();
        let labeled_job = parts.next().unwrap_or("").trim();
        if id.is_empty() {
            continue;
        }
        if crate::buildkit::is_persistent_builder_object(names) {
            continue;
        }
        if labeled_job == job_id || names.contains(&needle) {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

pub(crate) fn orphan_job_buildkit_ids(
    formatted: &str,
    live_jobs: &BTreeSet<String>,
    daemon_id: Option<&str>,
) -> Vec<String> {
    let mut ids = Vec::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            continue;
        }
        let id = fields[0].trim();
        let names = fields[1].trim();
        let job_id = fields[2].trim();
        let owner = fields[3].trim();
        let Some(state) = ContainerState::parse(fields[4]) else {
            continue;
        };
        if id.is_empty()
            || !names.contains(crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX)
            || crate::buildkit::is_persistent_builder_object(names)
            || !state.safe_to_reclaim()
        {
            continue;
        }
        // Startup reclaim is daemon-scoped. An absent ownership label is
        // not proof that this daemon owns the builder; fail closed so a
        // co-located daemon cannot reclaim an unlabeled live resource.
        if let Some(daemon_id) = daemon_id
            && (owner.is_empty() || !daemon_owns_label(owner, daemon_id))
        {
            continue;
        }
        let job_live = if job_id.is_empty() {
            live_jobs
                .iter()
                .any(|live| names.contains(live.trim_start_matches("velnor-job-")))
        } else {
            live_jobs.contains(job_id)
        };
        if !job_live {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    ids
}

pub(crate) fn live_job_ids(formatted: &str) -> BTreeSet<String> {
    let mut live = BTreeSet::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        let Some(job_id) = fields.get(1).map(|field| field.trim()) else {
            continue;
        };
        if job_id.is_empty() {
            continue;
        }
        if fields.len() != 3 {
            live.insert(job_id.to_string());
            continue;
        }
        let name = fields[0].trim();
        let Some(state) = ContainerState::parse(fields[2]) else {
            live.insert(job_id.to_string());
            continue;
        };
        if name == job_id && !state.safe_to_reclaim() {
            live.insert(job_id.to_string());
        }
    }
    live
}

pub(crate) fn live_daemon_job_ids(formatted: &str, daemon_id: &str) -> BTreeSet<String> {
    let mut live = BTreeSet::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        let Some(job_id) = fields.get(1).map(|field| field.trim()) else {
            continue;
        };
        if job_id.is_empty() {
            continue;
        }
        if fields.len() != 4 {
            live.insert(job_id.to_string());
            continue;
        }
        let name = fields[0].trim();
        let owner = fields[2].trim();
        if owner.is_empty() {
            live.insert(job_id.to_string());
            continue;
        }
        if !daemon_owns_label(owner, daemon_id) {
            continue;
        }
        let Some(state) = ContainerState::parse(fields[3]) else {
            live.insert(job_id.to_string());
            continue;
        };
        if name == job_id && !state.safe_to_reclaim() {
            live.insert(job_id.to_string());
        }
    }
    live
}

/// Job ids whose job container is not running. Guest objects for those jobs are orphans.
pub(crate) fn orphan_job_ids(formatted: &str) -> Vec<String> {
    let mut seen_jobs = BTreeSet::new();
    let mut protected_jobs = BTreeSet::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            continue;
        }
        let name = fields[0].trim();
        let job_id = fields[1].trim();
        if job_id.is_empty() {
            continue;
        }
        let Some(state) = ContainerState::parse(fields[2]) else {
            protected_jobs.insert(job_id.to_string());
            continue;
        };
        seen_jobs.insert(job_id.to_string());
        if name == job_id && !state.safe_to_reclaim() {
            protected_jobs.insert(job_id.to_string());
        }
    }
    seen_jobs
        .into_iter()
        .filter(|job_id| !protected_jobs.contains(job_id))
        .collect()
}

/// Orphan job ids restricted to containers owned by `daemon_id` (see
/// [`daemon_owns_label`]). Input is the
/// `name \t job-id \t daemon-id \t state` row format.
pub(crate) fn daemon_orphan_job_ids(formatted: &str, daemon_id: &str) -> Vec<String> {
    let mut seen_jobs = BTreeSet::new();
    let mut protected_jobs = BTreeSet::new();
    for line in formatted.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 4 {
            continue;
        }
        let name = fields[0].trim();
        let job_id = fields[1].trim();
        let owner = fields[2].trim();
        if job_id.is_empty() || !daemon_owns_label(owner, daemon_id) {
            continue;
        }
        let Some(state) = ContainerState::parse(fields[3]) else {
            protected_jobs.insert(job_id.to_string());
            continue;
        };
        seen_jobs.insert(job_id.to_string());
        if name == job_id && !state.safe_to_reclaim() {
            protected_jobs.insert(job_id.to_string());
        }
    }
    seen_jobs
        .into_iter()
        .filter(|job_id| !protected_jobs.contains(job_id))
        .collect()
}

/// IDs of testcontainers that were created before the lease proxy (no job label).
///
/// This remains a best-effort inspection helper for compatibility. Automatic
/// cleanup must use [`validate_legacy_testcontainer_listing`], which refuses
/// both malformed rows and every unlabeled container before any delete call.
pub(crate) fn unlabeled_testcontainer_ids(formatted: &str) -> Vec<String> {
    let mut ids = formatted
        .lines()
        .filter_map(|line| {
            let (id, job_id) = line.split_once('\t')?;
            let id = id.trim();
            if id.is_empty() || !job_id.trim().is_empty() {
                return None;
            }
            Some(id.to_string())
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// IDs of `velnor/job-ubuntu` siblings with no job label (docker-generated names).
pub(crate) fn unlabeled_job_image_ids(formatted: &str) -> Vec<String> {
    unlabeled_testcontainer_ids(formatted)
}

/// A legacy Testcontainers listing is not an ownership proof. Keep its
/// failure modes typed so callers cannot accidentally turn an inspection
/// result into a destructive cleanup decision.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum LegacyTestcontainerReclaimError {
    #[error("refusing legacy Testcontainers reclaim: malformed docker ps row {line}: {row:?}")]
    MalformedRow { line: usize, row: String },
    #[error(
        "refusing legacy Testcontainers reclaim: unlabeled containers have no Velnor ownership proof: {ids:?}"
    )]
    Unlabeled { ids: Vec<String> },
}

/// Validate the legacy Testcontainers listing without producing deleteable
/// IDs. Rows carrying a Velnor job label are intentionally ignored here;
/// their cleanup belongs to the job-owned reclaim paths. An unlabeled row is
/// a hard refusal because the `org.testcontainers.managed-by` label proves
/// only Testcontainers origin, not Velnor ownership.
pub(crate) fn validate_legacy_testcontainer_listing(
    formatted: &str,
) -> std::result::Result<(), LegacyTestcontainerReclaimError> {
    let mut unlabeled = Vec::new();
    for (line_number, row) in formatted.lines().enumerate() {
        let fields = row.split('\t').collect::<Vec<_>>();
        if fields.len() != 2 || fields[0].trim().is_empty() {
            return Err(LegacyTestcontainerReclaimError::MalformedRow {
                line: line_number + 1,
                row: row.to_string(),
            });
        }
        if fields[1].trim().is_empty() {
            unlabeled.push(fields[0].trim().to_string());
        }
    }

    if unlabeled.is_empty() {
        return Ok(());
    }
    unlabeled.sort();
    unlabeled.dedup();
    Err(LegacyTestcontainerReclaimError::Unlabeled { ids: unlabeled })
}

pub(crate) fn daemon_owned_buildkit_volume_names(
    formatted: &str,
    daemon_id: &str,
    protected_jobs: &BTreeSet<String>,
) -> Vec<String> {
    let mut names = formatted
        .lines()
        .filter_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() != 3 {
                return None;
            }
            let name = fields[0].trim();
            let job_id = fields[1].trim();
            let owner = fields[2].trim();
            if name.is_empty()
                || job_id.is_empty()
                || !name.contains(crate::docker_lease::BUILDKIT_CONTAINER_NAME_PREFIX)
                || crate::buildkit::is_persistent_builder_object(name)
                || !daemon_owns_label(owner, daemon_id)
                || protected_jobs.contains(job_id)
                || protected_jobs
                    .iter()
                    .any(|live| name.contains(live.trim_start_matches("velnor-job-")))
            {
                return None;
            }
            Some(name.to_string())
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

// ---------------------------------------------------------------------------
// In-flight `rm` claims
// ---------------------------------------------------------------------------

static IN_FLIGHT_CONTAINER_RM: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn container_ids_from_rm_args(args: &[String]) -> Vec<String> {
    if args.first().map(String::as_str) != Some("rm") {
        return Vec::new();
    }
    args.iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .cloned()
        .collect()
}

#[derive(Clone, Copy)]
pub(crate) struct NonEmptyDockerArgs<'a> {
    first: &'a String,
    rest: &'a [String],
}

impl<'a> NonEmptyDockerArgs<'a> {
    pub(crate) fn new(args: &'a [String]) -> Option<Self> {
        let (first, rest) = args.split_first()?;
        Some(Self { first, rest })
    }
}

pub(crate) fn container_rm_args_with_claimed_ids(
    args: NonEmptyDockerArgs<'_>,
    ids: &[String],
) -> Vec<String> {
    let mut claimed = vec![args.first.clone()];
    claimed.extend(args.rest.iter().filter(|arg| arg.starts_with('-')).cloned());
    claimed.extend(ids.iter().cloned());
    claimed
}

/// Claim container ids for a `docker rm` so job-end and doctor never start a
/// second Engine delete of the same Created BuildKit (that deadlock is the
/// leftover class). Empty `ids` means every id is already in flight: skip.
pub struct DockerContainerRmClaim {
    pub ids: Vec<String>,
}

impl Drop for DockerContainerRmClaim {
    fn drop(&mut self) {
        if self.ids.is_empty() {
            return;
        }
        let mut held = IN_FLIGHT_CONTAINER_RM
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        for id in &self.ids {
            held.remove(id);
        }
    }
}

pub(crate) fn claim_docker_container_rm(args: &[String]) -> Option<DockerContainerRmClaim> {
    let ids = container_ids_from_rm_args(args);
    if ids.is_empty() {
        return None;
    }
    let mut held = IN_FLIGHT_CONTAINER_RM
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let claimed = ids
        .into_iter()
        .filter(|id| held.insert(id.clone()))
        .collect::<Vec<_>>();
    Some(DockerContainerRmClaim { ids: claimed })
}

// ---------------------------------------------------------------------------
// Host transport (maintenance, startup, doctor, cancellation)
// ---------------------------------------------------------------------------

const HOST_DOCKER_ENDPOINT: &str = "unix:///var/run/docker.sock";

/// Host maintenance runs no job step, so a payload-classified command reaching
/// the host transport has no step deadline to inherit. Thirty minutes is the
/// registry-transfer bound: long enough for any maintenance pull, finite.
pub(crate) const MAINTENANCE_PAYLOAD_DEADLINE: Duration = Duration::from_secs(1800);

fn host_docker_command(args: &[String]) -> Result<std::process::Command> {
    let mut command = std::process::Command::new("docker");
    crate::executor::configure_host_docker_command(&mut command, "docker", args)?;
    command
        .env("DOCKER_HOST", HOST_DOCKER_ENDPOINT)
        .env_remove("DOCKER_CONTEXT");
    Ok(command)
}

/// Reap a Docker CLI child while keeping its timeout kill tied to the live
/// [`std::process::Child`] handle.
///
/// A watchdog that retains only `Child::id()` has a PID-reuse race: the child
/// can exit and be reaped just before the watchdog's timeout branch runs, and
/// the numeric PID can then belong to an unrelated host process. Polling and
/// killing through the owned handle closes that race. The pipes are drained on
/// reader threads so a verbose Docker CLI cannot deadlock while this thread
/// waits for its exit.
fn wait_for_child_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<(std::process::Output, bool)> {
    use std::io::Read as _;
    let mut stdout = child.stdout.take().context("capture timed child stdout")?;
    let mut stderr = child.stderr.take().context("capture timed child stderr")?;
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });

    let started = std::time::Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                let remaining = timeout.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    timed_out = true;
                    // `Child::kill` addresses the still-owned child handle,
                    // never a bare PID that may already have been recycled.
                    let _ = child.kill();
                    break child.wait().context("reap timed-out Docker child");
                }
                std::thread::sleep(Duration::from_millis(10).min(remaining));
            }
            Err(error) => {
                // Do not leave a child running if the status probe itself
                // fails. Reap it before returning the probe error.
                let _ = child.kill();
                let _ = child.wait();
                break Err(anyhow::Error::new(error).context("poll Docker child status"));
            }
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("timed child stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("timed child stderr reader panicked"))??;
    Ok((
        std::process::Output {
            status: status?,
            stdout,
            stderr,
        },
        timed_out,
    ))
}

/// Run one host `docker` command under the deadline its operation class earns.
///
/// Every path is bounded. Before the deadline policy existed only the `rm`
/// family was, and every other maintenance call — `ps`, `inspect`, reclaim
/// listings — waited on a wedged daemon forever.
pub(crate) fn host_call(args: &[String]) -> Result<String> {
    let (_, deadline) = crate::docker::deadline_for(args, MAINTENANCE_PAYLOAD_DEADLINE);
    host_call_bounded(args, deadline)
}

/// Run one host `docker` command under an explicit deadline.
///
/// Expiry is a failure. The process is SIGKILLed and the caller gets a typed
/// [`crate::docker::DockerTimeout`] naming the operation class and what to look
/// at, never an empty success.
pub(crate) fn host_call_bounded(args: &[String], timeout: Duration) -> Result<String> {
    let op = crate::docker::classify(args);
    let rm_claim = claim_docker_container_rm(args);
    if let Some(claim) = rm_claim.as_ref()
        && claim.ids.is_empty()
    {
        return Ok(String::new());
    }
    let claimed_args = rm_claim
        .as_ref()
        .map(|claim| {
            NonEmptyDockerArgs::new(args)
                .map(|args| container_rm_args_with_claimed_ids(args, &claim.ids))
                .ok_or_else(|| anyhow::anyhow!("docker rm claim requires non-empty arguments"))
        })
        .transpose()?;
    let args = claimed_args.as_deref().unwrap_or(args);
    let mut command = host_docker_command(args)?;
    let child = command
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("run docker {}", args.join(" ")))?;
    let started = std::time::Instant::now();
    let (output, expired) = wait_for_child_with_timeout(child, timeout)
        .with_context(|| format!("wait docker {}", args.join(" ")))?;
    crate::docker::observe(
        op,
        started.elapsed(),
        output.status.code().unwrap_or(-1),
        expired,
    );
    if expired {
        // We killed it. Reporting that as success turned a resource leak into
        // a silent one: teardown believed the object was gone.
        return Err(anyhow::Error::new(crate::docker::DockerTimeout::new(
            op, timeout,
        )));
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.to_ascii_lowercase();
        if daemon_reports_missing(&stderr) {
            return Err(anyhow::Error::new(NotFound {
                object: args.join(" "),
            }));
        }
        if let Some(builder) =
            buildkit_builder_from_args(args).filter(|_| detail.contains("no builder"))
        {
            return Err(anyhow::Error::new(BuildkitBuilderNotFound { builder }));
        }
        if is_not_running_command(args, &detail) {
            return Err(anyhow::Error::new(NotRunning {
                object: args.join(" "),
            }));
        }
        // Another writer already owns this object (Conflict): when that
        // writer is a concurrent `rm` of the same object, the desired end
        // state is in flight — report success. Narrow to the in-progress
        // needle on purpose: other conflicts ("already exists") are real
        // errors on the host path, not tolerance signals. A stderr that also
        // matches Transient (daemon down mid-removal) surfaces instead of
        // masking as success — transient checks first in the classifier.
        let removal_in_flight = stderr.contains("already in progress")
            && classify_docker_stderr(&stderr) == DockerErrorCategory::Conflict;
        if removal_in_flight {
            return Ok(String::new());
        }
        return Err(DockerCommandError::classified(
            format!("docker {} failed: {}", args.join(" "), stderr),
            &stderr,
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn buildkit_builder_from_args(args: &[String]) -> Option<String> {
    if !args.first().is_some_and(|arg| arg == "buildx")
        || !args.get(1).is_some_and(|arg| arg == "du" || arg == "prune")
    {
        return None;
    }
    args.windows(2)
        .find(|pair| pair[0] == "--builder")
        .map(|pair| pair[1].clone())
}

fn is_not_running_command(args: &[String], lower_stderr: &str) -> bool {
    lower_stderr.contains("is not running")
        && (args.first().is_some_and(|arg| arg == "kill")
            || (args.first().is_some_and(|arg| arg == "buildx")
                && args.get(1).is_some_and(|arg| arg == "du")))
}

// ---------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------

enum Transport<'r> {
    Job(&'r mut dyn CommandRunner),
    Host,
}

/// Outcome of the Engine-API attempt for one script-step exec. See
/// [`Docker::try_exec_script`] for the contract each arm carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScriptExecRoute {
    /// The API ran the step end to end: exit code plus demuxed streams,
    /// zero subprocesses.
    Served(CommandResult),
    /// The step deadline expired on the API leg: the CLI watchdog's own
    /// 124 result, the container left running as on the CLI leg. Never a
    /// fallback.
    Expired(CommandResult),
    /// Any API failure, the API disabled, or a non-host runner: run the
    /// historical CLI call unchanged.
    UseCli,
}

/// The typed owner of host Docker control-plane calls.
///
/// Construct with [`Docker::job`] on the job path (cancellation-aware,
/// metrics-attributed) or [`Docker::host`] on the maintenance/cancel path.
/// Every method returns a typed value; a daemon that positively reports an
/// object missing surfaces as [`NotFound`], never as an empty success —
/// except [`Docker::container_remove`], where missing is the desired end
/// state and succeeds as already-removed.
/// Migrated calls try the Engine API first ([`Docker::engine_or_cli`]) and run
/// their historical one-`docker`-process CLI call only when the API does
/// not affirmatively succeed — unmigrated calls (the buildx pair, which
/// has no Engine equivalent) always run the CLI. One payload call is
/// migrated too: [`Docker::try_exec_script`] serves script steps via
/// `exec_create`/`exec_start` under the step deadline.
pub(crate) struct Docker<'r> {
    transport: Transport<'r>,
}

impl<'r> Docker<'r> {
    pub(crate) fn job(runner: &'r mut dyn CommandRunner) -> Self {
        Self {
            transport: Transport::Job(runner),
        }
    }

    pub(crate) fn host() -> Self {
        Self {
            transport: Transport::Host,
        }
    }

    /// Run one query and return its stdout. Non-zero exits become errors:
    /// a daemon missing-object answer becomes [`NotFound`], a runner timeout
    /// (exit 124) becomes the operation's [`crate::docker::DockerTimeout`],
    /// anything else carries the stderr plus its [`DockerErrorCategory`].
    fn call(&mut self, args: &[String], object: &str) -> Result<String> {
        self.call_as(args, object, "query")
    }

    /// Run one mutation and return its stdout, with the verb in the failure
    /// messages instead of "query". Same error contract as [`Self::call`]:
    /// [`NotFound`] for daemon missing-object answers, typed
    /// [`crate::docker::DockerTimeout`] for exit 124, classified
    /// [`DockerCommandError`] otherwise.
    fn mutate(&mut self, args: &[String], object: &str, verb: &str) -> Result<String> {
        self.call_as(args, object, verb)
    }

    fn call_as(&mut self, args: &[String], object: &str, action: &str) -> Result<String> {
        match &mut self.transport {
            Transport::Host => host_call(args),
            Transport::Job(runner) => {
                let result: CommandResult = runner
                    .run("docker", args)
                    .with_context(|| format!("{action} docker {object}"))?;
                if result.code == 0 {
                    return Ok(result.stdout);
                }
                if result.code == 124 {
                    let (op, deadline) =
                        crate::docker::deadline_for(args, crate::executor::DEFAULT_STEP_TIMEOUT);
                    return Err(anyhow::Error::new(crate::docker::DockerTimeout::new(
                        op, deadline,
                    )));
                }
                if daemon_reports_missing(&result.stderr) {
                    return Err(anyhow::Error::new(NotFound {
                        object: object.to_string(),
                    }));
                }
                Err(anyhow::Error::new(DockerCommandError::classified(
                    format!(
                        "docker {action} {object} exited {}: {}",
                        result.code,
                        result.stderr.trim()
                    ),
                    &result.stderr,
                )))
            }
        }
    }

    /// Engine-API fast path for one migrated call. `cli_args` is the call's
    /// historical CLI vector: it classifies the operation and its class
    /// deadline exactly as the CLI call would, so the API attempt and its
    /// metrics carry the same class the policy always assigned this call.
    ///
    /// Returns the API value on success. On ANY API failure — transport,
    /// timeout, status, framing, JSON, schema, dead runtime, or a job
    /// cancellation winning the socket-wait race — records the fallback with
    /// telemetry, logs the reason at `warn`, and returns `None` so the
    /// caller runs its historical CLI query unchanged. Engine errors never
    /// surface: the CLI stays the arbiter whenever the API does not
    /// affirmatively succeed, which is what keeps results, error taxonomy
    /// (`NotFound`, `DockerTimeout`, `DockerCommandError`), and deadlines
    /// identical. A fallback costs exactly one subprocess: the same as
    /// before the migration.
    fn engine_or_cli<T, F>(
        &self,
        cli_args: &[String],
        run: impl FnOnce(super::engine::EngineClient, Duration) -> F,
    ) -> Option<T>
    where
        F: std::future::Future<Output = Result<T, super::engine::EngineError>> + Send,
        T: Send,
    {
        if !super::engine::engine_api_enabled() {
            return None;
        }
        // An Engine answer is a host fact, so only a runner that spawns host
        // processes may consult it — the same rule fact caching applies
        // (`execution/docker.rs` consults `is_host_process_runner` too).
        // Test doubles stay on their scripts, which keeps scripted tests
        // hermetic on daemon hosts and immune to the process-global test
        // override while a routing test holds it.
        if let Transport::Job(runner) = &self.transport
            && !runner.is_host_process_runner()
        {
            return None;
        }
        let (op, class_deadline) =
            crate::docker::deadline_for(cli_args, crate::executor::DEFAULT_STEP_TIMEOUT);
        debug_assert!(
            op.is_control_plane(),
            "engine fast path serves control-plane calls only"
        );
        let budget = super::engine::api_budget(class_deadline);
        let engine = super::engine::EngineClient::new(super::engine::socket_path());
        let started = std::time::Instant::now();
        let attempt =
            super::engine::block_on_engine(super::engine::cancel_race(run(engine, budget)));
        let result = match attempt {
            None => Err(super::engine::EngineError::fault(
                op,
                (
                    super::engine::EngineFaultKind::Runtime,
                    "engine runtime unavailable".to_string(),
                ),
            )),
            Some(None) => Err(super::engine::EngineError::fault(
                op,
                (
                    super::engine::EngineFaultKind::Cancelled,
                    "job cancelled during engine api wait".to_string(),
                ),
            )),
            Some(Some(result)) => result,
        };
        match result {
            Ok(value) => {
                crate::docker::observe_api(op, started.elapsed());
                Some(value)
            }
            Err(error) => {
                crate::docker::observe_api_fallback(op);
                tracing::warn!(
                    target: "velnor.docker",
                    docker_op = op.label(),
                    docker_transport = "cli-fallback",
                    docker_api_fault = error.label(),
                    reason = %error,
                    "engine api failed; falling back to docker cli"
                );
                None
            }
        }
    }

    /// Engine-API fast path for one script step's `docker exec` — the ONLY
    /// payload call on the API, and only for the script-step shape: no
    /// stdin, no TTY, attached. Every other exec shape (stdin forwards,
    /// host/maintenance calls) stays on the CLI at its own call site;
    /// there is no TTY or detach exec in the runner at all, so those
    /// semantics are preserved by never being offered this route.
    ///
    /// `cli_args` is the step's historical CLI vector: it classifies the
    /// operation exactly as the CLI call would (always
    /// [`DockerOp::Payload`](crate::docker::DockerOp::Payload)) and
    /// `timeout` is the step's own deadline, which bounds the whole API
    /// attempt — no 5 s cap: the payload class takes the caller's
    /// deadline on both transports. Live lines reach `on_line` per stream
    /// as they demux, with the CLI runner's own line semantics.
    ///
    /// * [`ScriptExecRoute::Served`] — the API ran the step end to end:
    ///   exit code plus demuxed streams, zero subprocesses. A nonzero
    ///   step exit is a served outcome, not a docker failure: no host
    ///   `docker` invocation existed, so the docker failure counter (a
    ///   per-invocation count) does not move — the step outcome is
    ///   recorded in the step telemetry, as on the CLI leg.
    /// * [`ScriptExecRoute::Expired`] — the step deadline expired on the
    ///   API leg: the CLI watchdog's own 124 result (same message, same
    ///   partial streams). The timed-out attempt is dropped, which closes
    ///   the exec stream — the API leg's client side — while the container
    ///   survives, exactly as on the CLI leg, where the watchdog's parser
    ///   hits the `--` separator and kills only the docker client. Served,
    ///   never a fallback — expiry must not rerun the step. Like `Served`
    ///   this records `observe_api` only: `timeouts`/`failures` count host
    ///   `docker` invocations, and no invocation existed — the 124 lives in
    ///   the step result, as on the CLI leg.
    /// * [`ScriptExecRoute::UseCli`] — ANY API failure (transport,
    ///   status, framing, JSON, schema, dead runtime, or a job
    ///   cancellation winning the race), the API disabled, or a
    ///   non-host runner: run the historical CLI call unchanged. Under
    ///   cancel this is the same spawn-and-ladder-kill the CLI path has
    ///   always run, so cancellation keeps its historical shape by
    ///   construction. A fallback costs exactly one subprocess — the
    ///   same as before the migration — but note the exec caveat: when
    ///   the exec'd process already started, the fallback reruns the
    ///   step. Every rerun is telemetry-visible with its fault label.
    pub(crate) fn try_exec_script(
        &self,
        container: &str,
        cli_args: &[String],
        config: &super::engine::ExecConfig,
        timeout: Duration,
        on_line: &mut (dyn FnMut(CommandStream, &str) + Send),
    ) -> ScriptExecRoute {
        if !super::engine::engine_api_enabled() {
            return ScriptExecRoute::UseCli;
        }
        // An Engine answer is a host fact, so only a runner that spawns host
        // processes may consult it — the same rule `engine_or_cli` applies.
        // Test doubles stay on their scripts.
        if let Transport::Job(runner) = &self.transport
            && !runner.is_host_process_runner()
        {
            return ScriptExecRoute::UseCli;
        }
        let (op, step_deadline) = crate::docker::deadline_for(cli_args, timeout);
        debug_assert_eq!(op, crate::docker::DockerOp::Payload);
        let engine = super::engine::EngineClient::new(super::engine::socket_path());
        let started = std::time::Instant::now();
        let mut output = super::engine::ExecOutput::default();
        // The timeout builds inside the async block: `tokio::time::timeout`
        // needs a runtime context at construction, and the block first
        // polls on the engine runtime that `block_on_engine` provides.
        let attempt = super::engine::block_on_engine(async {
            tokio::time::timeout(
                step_deadline,
                super::engine::cancel_race(engine.exec_run(
                    container,
                    config,
                    step_deadline,
                    &mut output,
                    on_line,
                )),
            )
            .await
        });
        match attempt {
            Some(Ok(Some(Ok(code)))) => {
                crate::docker::observe_api(op, started.elapsed());
                ScriptExecRoute::Served(CommandResult {
                    code,
                    stdout: output.stdout,
                    stderr: output.stderr,
                })
            }
            Some(Err(_)) => {
                // No container kill: dropping the timed-out attempt above
                // already closed the exec stream — the API leg's client
                // side — which is exactly what the CLI watchdog does for
                // real script-step argv (its parser hits the `--`
                // separator, resolves no target, and kills only the
                // docker client). The container and its orphaned step
                // process survive on both legs, so post/failure/
                // continue-on-error steps see the same live container.
                // Nothing here touches the daemon, so 124 always returns
                // promptly, never behind a wedged-daemon round trip.
                let result = crate::executor::timeout_command_result(
                    Some(crate::docker::DockerOp::Payload),
                    step_deadline,
                    output.stdout,
                    output.stderr,
                );
                crate::docker::observe_api(op, started.elapsed());
                ScriptExecRoute::Expired(result)
            }
            Some(Ok(Some(Err(error)))) => {
                Self::exec_fallback(op, &error);
                ScriptExecRoute::UseCli
            }
            Some(Ok(None)) => {
                Self::exec_fallback(
                    op,
                    &super::engine::EngineError::fault(
                        op,
                        (
                            super::engine::EngineFaultKind::Cancelled,
                            "job cancelled during engine api wait".to_string(),
                        ),
                    ),
                );
                ScriptExecRoute::UseCli
            }
            None => {
                Self::exec_fallback(
                    op,
                    &super::engine::EngineError::fault(
                        op,
                        (
                            super::engine::EngineFaultKind::Runtime,
                            "engine runtime unavailable".to_string(),
                        ),
                    ),
                );
                ScriptExecRoute::UseCli
            }
        }
    }

    /// Record one exec attempt that falls back to the CLI: the fallback
    /// counter plus the fault at `warn`, the same shape `engine_or_cli`
    /// emits. The CLI call that follows records its own `observe`.
    fn exec_fallback(op: crate::docker::DockerOp, error: &super::engine::EngineError) {
        crate::docker::observe_api_fallback(op);
        tracing::warn!(
            target: "velnor.docker",
            docker_op = op.label(),
            docker_transport = "cli-fallback",
            docker_api_fault = error.label(),
            reason = %error,
            "engine api failed; falling back to docker cli"
        );
    }

    /// Readiness of one container: health status when a healthcheck exists,
    /// else the lifecycle status.
    pub(crate) fn container_readiness(&mut self, name: &str) -> Result<Readiness> {
        if let Some(readiness) =
            self.engine_or_cli(&readiness_args(name), |engine, budget| async move {
                Ok(Readiness::parse(
                    engine
                        .inspect_container(name, budget)
                        .await?
                        .readiness_word(),
                ))
            })
        {
            return Ok(readiness);
        }
        let args = readiness_args(name);
        Ok(Readiness::parse(&self.call(&args, name)?))
    }

    /// Whether the Engine reports the container running. A positive-missing
    /// answer reads as not running; any other failure propagates so the
    /// caller takes its own fail-closed direction.
    pub(crate) fn container_running(&mut self, name: &str) -> Result<bool> {
        if let Some(running) = self
            .engine_or_cli(&running_args(name), |engine, budget| async move {
                Ok(engine.inspect_container(name, budget).await?.running)
            })
        {
            return Ok(running);
        }
        let args = running_args(name);
        match self.call(&args, name) {
            Ok(output) => Ok(output.trim() == "true"),
            Err(error) if is_not_found(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Full id of one container.
    pub(crate) fn container_id(&mut self, name: &str) -> Result<String> {
        if let Some(id) = self
            .engine_or_cli(&container_id_args(name), |engine, budget| async move {
                Ok(engine.inspect_container(name, budget).await?.id)
            })
        {
            return Ok(id);
        }
        let args = container_id_args(name);
        Ok(self.call(&args, name)?.trim().to_string())
    }

    /// Resolved id of one image reference.
    pub(crate) fn image_id(&mut self, reference: &str) -> Result<String> {
        if let Some(id) = self
            .engine_or_cli(&image_id_args(reference), |engine, budget| async move {
                Ok(engine.inspect_image(reference, budget).await?.id)
            })
        {
            return Ok(id);
        }
        let args = image_id_args(reference);
        Ok(self.call(&args, reference)?.trim().to_string())
    }

    /// Published ports of one container.
    pub(crate) fn mapped_ports(&mut self, name: &str) -> Result<Vec<PortMapping>> {
        if let Some(ports) = self
            .engine_or_cli(&mapped_ports_args(name), |engine, budget| async move {
                Ok(engine.inspect_container(name, budget).await?.ports)
            })
        {
            return Ok(ports);
        }
        let args = mapped_ports_args(name);
        Ok(parse_port_mappings(&self.call(&args, name)?))
    }

    /// The Engine's cgroup driver and version.
    pub(crate) fn daemon_cgroup(&mut self) -> Result<CgroupDriver> {
        if let Some(cgroup) =
            self.engine_or_cli(&daemon_cgroup_args(), |engine, budget| async move {
                let info = engine.daemon_info(budget).await?;
                Ok(CgroupDriver {
                    driver: info.cgroup_driver,
                    version: info.cgroup_version,
                })
            })
        {
            return Ok(cgroup);
        }
        let args = daemon_cgroup_args();
        parse_cgroup_projection(&self.call(&args, "daemon cgroup")?)
    }

    /// Names of the buildx builders Velnor owns.
    pub(crate) fn buildx_builders(&mut self) -> Result<Vec<String>> {
        let args = vec!["buildx".to_string(), "ls".to_string()];
        Ok(owned_builder_names(&self.call(&args, "buildx builders")?))
    }

    /// Lifecycle word and last stop time of one container.
    pub(crate) fn inspect_exit(&mut self, name: &str) -> Result<ExitInfo> {
        if let Some(exit) = self.engine_or_cli(&exit_info_args(name), |engine, budget| async move {
            let container = engine.inspect_container(name, budget).await?;
            Ok(ExitInfo {
                status: ContainerState::parse(&container.status),
                finished: parse_finished_at(&container.finished_at),
            })
        }) {
            return Ok(exit);
        }
        let args = exit_info_args(name);
        parse_exit_info(&self.call(&args, name)?)
    }

    /// Total cache bytes one builder holds, from `buildx du`.
    pub(crate) fn buildx_disk_usage(&mut self, builder: &str) -> Result<u64> {
        let args = buildx_disk_usage_args(builder);
        parse_buildx_disk_usage(&self.call(&args, builder)?)
    }

    /// Containers carrying `velnor.job-id=<job_id>`: short ids plus names.
    /// Terminal cleanup's list phase consumes this. The API leg truncates
    /// full ids to the short form the CLI leg prints and strips the leading
    /// `/` from every API name, so both legs return identical rows and the
    /// shared BuildKit-exclusion decision cannot differ by transport.
    pub(crate) fn list_owned_containers(&mut self, job_id: &str) -> Result<Vec<OwnedContainer>> {
        if let Some(rows) = self.engine_or_cli(
            &crate::docker_lease::list_owned_containers_args(job_id),
            |engine, budget| async move {
                let filter = super::engine::ListFilter::label_equals(
                    crate::docker_lease::JOB_ID_LABEL,
                    job_id,
                );
                let summaries = engine.list_containers(&filter, budget).await?;
                Ok(project_owned_rows(&summaries))
            },
        ) {
            return Ok(rows);
        }
        let args = crate::docker_lease::list_owned_containers_args(job_id);
        Ok(parse_owned_container_rows(&self.call(&args, job_id)?))
    }

    /// Short ids of the networks carrying `velnor.job-id=<job_id>`, sorted.
    /// `network ls -q` prints short ids (proven live), so the API leg
    /// truncates to the same 12 chars; both legs sort and dedup.
    pub(crate) fn list_owned_networks(&mut self, job_id: &str) -> Result<Vec<String>> {
        if let Some(ids) = self.engine_or_cli(
            &crate::docker_lease::list_owned_networks_args(job_id),
            |engine, budget| async move {
                let filter = super::engine::ListFilter::label_equals(
                    crate::docker_lease::JOB_ID_LABEL,
                    job_id,
                );
                let networks = engine.list_networks(&filter, budget).await?;
                Ok(sorted_ids(
                    networks
                        .iter()
                        .map(|network| short_id(&network.id))
                        .collect(),
                ))
            },
        ) {
            return Ok(ids);
        }
        let args = crate::docker_lease::list_owned_networks_args(job_id);
        Ok(parse_id_list(&self.call(&args, job_id)?))
    }

    /// Names of the volumes carrying `velnor.job-id=<job_id>`, sorted.
    /// `volume ls -q` prints names, the API's `Name` field verbatim.
    pub(crate) fn list_owned_volumes(&mut self, job_id: &str) -> Result<Vec<String>> {
        if let Some(names) = self.engine_or_cli(
            &crate::docker_lease::list_owned_volumes_args(job_id),
            |engine, budget| async move {
                let filter = super::engine::ListFilter::label_equals(
                    crate::docker_lease::JOB_ID_LABEL,
                    job_id,
                );
                let volumes = engine.list_volumes(&filter, budget).await?;
                Ok(sorted_ids(
                    volumes.iter().map(|volume| volume.name.clone()).collect(),
                ))
            },
        ) {
            return Ok(names);
        }
        let args = crate::docker_lease::list_owned_volumes_args(job_id);
        Ok(parse_id_list(&self.call(&args, job_id)?))
    }

    /// Idempotently start one container. Starting an already-running
    /// container succeeds on both legs; a missing container is [`NotFound`]
    /// (Conflict-shaped: another writer removed it mid-flight), never an
    /// empty success.
    pub(crate) fn container_start(&mut self, name: &str) -> Result<StartOutcome> {
        if let Some(outcome) = self
            .engine_or_cli(&container_start_args(name), |engine, budget| async move {
                engine.start_container(name, budget).await
            })
        {
            return Ok(outcome);
        }
        let args = container_start_args(name);
        self.mutate(&args, name, "start")?;
        Ok(StartOutcome::Started)
    }

    /// Idempotently stop one container, waiting up to `timeout` seconds
    /// before SIGKILL (`None` is the daemon default on both legs).
    /// Stopping an already-stopped container succeeds on both legs; a
    /// missing container is [`NotFound`], never an empty success.
    pub(crate) fn container_stop(
        &mut self,
        name: &str,
        timeout: Option<u64>,
    ) -> Result<StopOutcome> {
        if let Some(outcome) = self.engine_or_cli(
            &container_stop_args(name, timeout),
            |engine, budget| async move { engine.stop_container(name, timeout, budget).await },
        ) {
            return Ok(outcome);
        }
        let args = container_stop_args(name, timeout);
        self.mutate(&args, name, "stop")?;
        Ok(StopOutcome::Stopped)
    }

    /// Idempotently remove one container. Removing a missing container
    /// succeeds as [`RemoveOutcome::AlreadyRemoved`] on both legs, and a
    /// removal already in flight succeeds as [`RemoveOutcome::Removed`]
    /// (the desired end state is converging — the same tolerance the
    /// host and teardown paths apply). A running container without
    /// `force` is a genuine conflict: typed [`DockerErrorCategory::Conflict`]
    /// on both legs, never swallowed as success.
    pub(crate) fn container_remove(
        &mut self,
        name: &str,
        force: bool,
        volumes: bool,
    ) -> Result<RemoveOutcome> {
        if let Some(outcome) =
            self.engine_or_cli(
                &container_remove_args(name, force, volumes),
                |engine, budget| async move {
                    engine.remove_container(name, force, volumes, budget).await
                },
            )
        {
            return Ok(outcome);
        }
        let args = container_remove_args(name, force, volumes);
        match self.mutate(&args, name, "remove") {
            Ok(_) => Ok(RemoveOutcome::Removed),
            Err(error) if is_not_found(&error) => Ok(RemoveOutcome::AlreadyRemoved),
            Err(error) if removal_in_flight_error(&error) => Ok(RemoveOutcome::Removed),
            Err(error) => Err(error),
        }
    }
}

/// True when `error` is the daemon's removal-in-progress answer: Conflict
/// category narrowed to the in-progress needle, the same tolerance the
/// host transport ([`host_call`]) and teardown apply. A stderr that also
/// matches Transient surfaces instead of masking as success.
fn removal_in_flight_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<DockerCommandError>()
            .is_some_and(|command| {
                command.category() == DockerErrorCategory::Conflict
                    && command.to_string().contains("already in progress")
            })
    })
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
    use crate::docker::engine::mock::{
        error_response, exec_frame, json_response, start_response, status_response,
    };
    use crate::docker::engine::{EngineTestGuard, ExecConfig, FailRuntimeBuildGuard, MockEngine};
    use crate::docker::{begin_job, snapshot};
    use crate::execution::cancel::{set_active, CancelReason, JobCancellation};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    /// Every fixture below is output captured from a real Engine 29.4.0
    /// invocation of the exact argument vector the parser consumes.

    #[test]
    fn empty_argv_cannot_form_a_claimed_rm_argument_view() {
        assert!(NonEmptyDockerArgs::new(&[]).is_none());
        let argv = vec!["rm".to_string(), "-f".to_string(), "stale".to_string()];
        assert_eq!(
            container_rm_args_with_claimed_ids(
                NonEmptyDockerArgs::new(&argv).unwrap(),
                &["id-a".to_string()],
            ),
            vec!["rm".to_string(), "-f".to_string(), "id-a".to_string()]
        );
    }

    #[test]
    fn readiness_maps_every_status_word_to_the_wait_loop_decision() {
        // ready
        for word in ["healthy", "running", ""] {
            let readiness = Readiness::parse(word);
            assert!(readiness.ready(), "{word:?} must be ready");
            assert!(!readiness.stopped(), "{word:?} must not read as stopped");
        }
        // stopped before ready: fail, do not wait
        for word in ["exited", "dead"] {
            let readiness = Readiness::parse(word);
            assert!(!readiness.ready(), "{word:?} must not be ready");
            assert!(readiness.stopped(), "{word:?} must read as stopped");
        }
        // keep waiting: transitional, unhealthy, or unknown
        for word in [
            "starting",
            "unhealthy",
            "paused",
            "restarting",
            "confused-state",
        ] {
            let readiness = Readiness::parse(word);
            assert!(!readiness.ready(), "{word:?} must not be ready");
            assert!(!readiness.stopped(), "{word:?} must not read as stopped");
        }
        assert_eq!(Readiness::parse("  running\n"), Readiness::Running);
    }

    #[test]
    fn container_state_rejects_unknown_words_and_grades_reclaim() {
        for (word, reclaimable) in [
            ("created", true),
            ("running", false),
            ("restarting", false),
            ("removing", true),
            ("paused", false),
            ("exited", true),
            ("dead", true),
        ] {
            let state = ContainerState::parse(word).unwrap_or_else(|| panic!("{word} parses"));
            assert_eq!(state.safe_to_reclaim(), reclaimable, "{word}");
        }
        assert_eq!(ContainerState::parse("vaporized"), None);
        assert_eq!(ContainerState::parse(""), None);
    }

    #[test]
    fn port_mappings_keep_both_families_and_skip_garbage() {
        // `docker port velnor-shape-probe`, live: one mapping, two lines.
        let mappings = parse_port_mappings("8080/tcp -> 0.0.0.0:41062\n8080/tcp -> [::]:41062\n");
        assert_eq!(
            mappings,
            vec![
                PortMapping {
                    container_port: "8080/tcp".to_string(),
                    host_address: "0.0.0.0:41062".to_string(),
                },
                PortMapping {
                    container_port: "8080/tcp".to_string(),
                    host_address: "[::]:41062".to_string(),
                },
            ]
        );
        assert!(parse_port_mappings("not a mapping\nno-colon -> \n").is_empty());
    }

    #[test]
    fn cgroup_projection_accepts_exactly_two_words() {
        let probed = parse_cgroup_projection("systemd 2\n").expect("two words parse");
        assert_eq!(
            probed,
            CgroupDriver {
                driver: "systemd".to_string(),
                version: "2".to_string(),
            }
        );
        assert!(parse_cgroup_projection("systemd\n").is_err());
        assert!(parse_cgroup_projection("").is_err());
        assert!(parse_cgroup_projection("a b c\n").is_err());
    }

    #[test]
    fn missing_vocabulary_covers_both_generations_and_stays_narrow() {
        // Modern Engine: `Error: No such object: <name>` on stdout `[]`.
        assert!(daemon_reports_missing("Error: No such object: velnor-x"));
        assert!(daemon_reports_missing("Error: No such container: velnor-x"));
        // Older generation.
        assert!(daemon_reports_missing("error: no such object: velnor-x"));
        assert!(daemon_reports_missing("no such container"));
        // Narrow on purpose: the cancel ladder reads anything else as alive.
        assert!(!daemon_reports_missing(""));
        assert!(!daemon_reports_missing(
            "Error response from daemon: network velnor-net-1 not found"
        ));
        assert!(!daemon_reports_missing(
            "Cannot connect to the Docker daemon"
        ));
    }

    #[test]
    fn docker_stderr_classifier_separates_transient_conflict_and_terminal() {
        use DockerErrorCategory::{Conflict, Terminal, Transient};
        // Daemon restart / transport break: retry with backoff.
        for stderr in [
            "failed to create TTRPC connection: unsupported protocol",
            "error reading from server: EOF",
            "Cannot connect to the Docker daemon. Is the docker daemon running?",
            "rpc error: transport is closing",
            "connection reset by peer",
            "unexpected EOF reading trailer",
        ] {
            assert_eq!(classify_docker_stderr(stderr), Transient, "{stderr:?}");
        }
        // Registry transfer during pull/push: rate limit, 429/5xx, stalled
        // transfer. The next attempt re-resolves and resumes: backoff, never
        // fail fast.
        for stderr in [
            "Error response from daemon: toomanyrequests: You have reached your pull rate limit",
            "received unexpected HTTP status: 429 Too Many Requests",
            "received unexpected HTTP status: 503 Service Unavailable",
            "received unexpected HTTP status: 500 Internal Server Error",
            "received unexpected HTTP status: 502 Bad Gateway",
            "received unexpected HTTP status: 504 Gateway Timeout",
            "unexpected status from HEAD request to https://registry-1.docker.io/v2/library/ubuntu/manifests/24.04: 503 Service Unavailable",
            "dial tcp: lookup registry-1.docker.io: i/o timeout",
            "Get \"https://registry-1.docker.io/v2/\": context deadline exceeded",
        ] {
            assert_eq!(classify_docker_stderr(stderr), Transient, "{stderr:?}");
        }
        // Another writer holds the object: one stale cleanup plus one retry.
        for stderr in [
            r#"Error response from daemon: network with name "net" already exists"#,
            "Conflict. The container name \"/job\" is already in use by container abc123",
            "Error response from daemon: removal of container abc is already in progress",
            // Live `docker rm` on a running container, Engine 29.4.0
            // verbatim: the daemon's 409 sentence, shared with the API leg.
            "Error response from daemon: cannot remove container \"velnor-job-1\": container is running: stop the container before removing or force remove",
        ] {
            assert_eq!(classify_docker_stderr(stderr), Conflict, "{stderr:?}");
        }
        // Same inputs fail the same way: fail fast. Unknown output fails
        // closed to terminal, never to a hopeful retry.
        for stderr in [
            "pull access denied for private/image",
            "invalid reference format",
            "failed to create task for container: OCI runtime create failed: executable not found",
            "",
            "timed out",
        ] {
            assert_eq!(classify_docker_stderr(stderr), Terminal, "{stderr:?}");
        }
    }

    #[test]
    fn docker_command_error_preserves_message_and_exposes_category() {
        let message = "docker network create failed with code 1: Error response from \
            daemon: network with name \"net\" already exists"
            .to_string();
        let error = anyhow::Error::new(DockerCommandError::classified(
            message.clone(),
            "Error response from daemon: network with name \"net\" already exists",
        ));
        assert_eq!(error.to_string(), message);
        assert_eq!(docker_error_category(&error), DockerErrorCategory::Conflict);
        // The category survives context wrapping: the policy reads the chain.
        let wrapped = error.context("start job network");
        assert_eq!(
            docker_error_category(&wrapped),
            DockerErrorCategory::Conflict
        );
        // No typed docker failure in the chain: terminal, fail fast.
        assert_eq!(
            docker_error_category(&anyhow::anyhow!("boom")),
            DockerErrorCategory::Terminal
        );
        // A daemon positive-missing answer is Conflict-shaped: on the start
        // path it means another writer removed an object this attempt
        // created, so one stale cleanup plus a single retry recovers the race.
        assert_eq!(
            docker_error_category(
                &anyhow::Error::new(NotFound {
                    object: "svc".to_string()
                })
                .context("query")
            ),
            DockerErrorCategory::Conflict
        );
    }

    #[test]
    fn every_facade_query_is_a_bounded_control_plane_call() {
        const SIX_HOURS: Duration = Duration::from_secs(6 * 3600);
        let mut queries: Vec<Vec<String>> = vec![
            readiness_args("velnor-service-postgres"),
            running_args("velnor-job-1"),
            container_id_args("velnor-service-postgres"),
            daemon_cgroup_args(),
            mapped_ports_args("velnor-service-postgres"),
            image_id_args("velnor/job-ubuntu:26.04"),
            exit_info_args("buildx_buildkit_velnor-builder-shared-trusted-owner_repo0"),
            buildx_disk_usage_args("velnor-builder-shared-trusted-owner_repo"),
            vec!["buildx".to_string(), "ls".to_string()],
        ];
        // The listing builders the reclaim decisions consume through this
        // module's parsers must hold the same guarantee.
        queries.push(crate::docker_lease::list_owned_containers_args("job"));
        queries.push(crate::docker_lease::list_owned_containers_state_args("job"));
        queries.push(crate::docker_lease::list_owned_networks_args("job"));
        queries.push(crate::docker_lease::list_owned_volumes_args("job"));
        queries.push(crate::docker_lease::list_owned_job_format_args());
        queries.push(crate::docker_lease::list_daemon_owned_job_format_args());
        queries.push(crate::docker_lease::list_testcontainers_format_args());
        queries.push(crate::docker_lease::list_job_image_format_args());
        queries.push(crate::docker_lease::list_job_buildkit_format_args());
        queries.push(crate::docker_lease::list_job_buildkit_volume_args());
        queries.push(crate::docker_lease::list_daemon_owned_job_buildkit_volume_format_args());
        queries.push(crate::docker_lease::list_preflight_format_args());
        for args in &queries {
            let (op, deadline) = crate::docker::deadline_for(args, SIX_HOURS);
            assert!(
                op.is_control_plane(),
                "{args:?} must classify as control plane, got {op}"
            );
            assert!(
                deadline < SIX_HOURS,
                "{args:?} ({op}) inherited the step deadline"
            );
        }
    }

    /// Scripted [`CommandRunner`] that records every invocation.
    struct ScriptRunner {
        results: std::collections::VecDeque<CommandResult>,
        calls: AtomicUsize,
        seen_args: std::sync::Mutex<Vec<Vec<String>>>,
        host: bool,
    }

    impl ScriptRunner {
        fn scripted(results: Vec<CommandResult>) -> Self {
            Self {
                results: results.into(),
                calls: AtomicUsize::new(0),
                seen_args: std::sync::Mutex::new(Vec::new()),
                host: false,
            }
        }

        /// Scripted runner that passes as a host process runner, so the
        /// facade routes it through the Engine fast path like production.
        /// Only routing tests use this, all under the shared serial lock.
        fn scripted_host(results: Vec<CommandResult>) -> Self {
            Self {
                results: results.into(),
                calls: AtomicUsize::new(0),
                seen_args: std::sync::Mutex::new(Vec::new()),
                host: true,
            }
        }
    }

    impl CommandRunner for ScriptRunner {
        fn is_host_process_runner(&self) -> bool {
            self.host
        }

        fn run(&mut self, program: &str, args: &[String]) -> Result<CommandResult> {
            assert_eq!(program, "docker");
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen_args
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(args.to_vec());
            self.results.pop_front().context("script exhausted")
        }
    }

    fn ok(stdout: &str) -> CommandResult {
        CommandResult {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn failed(code: i32, stderr: &str) -> CommandResult {
        CommandResult {
            code,
            stdout: String::new(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn each_query_costs_exactly_one_process() {
        let mut runner = ScriptRunner::scripted(vec![
            ok("healthy\n"),
            ok("a530e70d9e1e35941b6fc12db9b51a7b19c6d02\n"),
            ok("8080/tcp -> 0.0.0.0:41062\n"),
            ok("systemd 2\n"),
        ]);
        {
            let mut docker = Docker::job(&mut runner);
            assert_eq!(
                docker.container_readiness("svc").expect("readiness"),
                Readiness::Healthy
            );
            assert!(docker
                .container_id("svc")
                .expect("id")
                .starts_with("a530e70d9e1e"));
            assert_eq!(docker.mapped_ports("svc").expect("ports").len(), 1);
            assert_eq!(docker.daemon_cgroup().expect("cgroup").driver, "systemd");
        }
        assert_eq!(runner.calls.load(Ordering::SeqCst), 4);
        assert_eq!(
            *runner
                .seen_args
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
            vec![
                readiness_args("svc"),
                container_id_args("svc"),
                mapped_ports_args("svc"),
                daemon_cgroup_args(),
            ]
        );
    }

    #[test]
    fn job_transport_maps_missing_timeout_and_failure() {
        // Positive-missing reads as not running (the cancel direction).
        let mut runner =
            ScriptRunner::scripted(vec![failed(1, "Error: No such object: velnor-job-9\n")]);
        assert!(!Docker::job(&mut runner)
            .container_running("velnor-job-9")
            .expect("missing reads as not running"));

        // Positive-missing on other queries is a typed error, never empty.
        let mut runner = ScriptRunner::scripted(vec![failed(1, "Error: No such object: svc\n")]);
        let error = Docker::job(&mut runner)
            .container_id("svc")
            .expect_err("missing must error");
        assert!(is_not_found(&error), "expected NotFound, got {error:#}");
        assert!(!is_not_found(&anyhow::anyhow!("boom")));

        // Exit 124 normalizes to the query's class deadline, not the step's.
        let mut runner = ScriptRunner::scripted(vec![failed(124, "timed out")]);
        let error = Docker::job(&mut runner)
            .container_id("svc")
            .expect_err("timeout must error");
        let timeout = error
            .downcast_ref::<crate::docker::DockerTimeout>()
            .expect("typed DockerTimeout");
        assert_eq!(timeout.op, crate::docker::DockerOp::Query);
        assert_eq!(timeout.deadline, Duration::from_secs(20));

        // Any other failure carries the stderr.
        let mut runner =
            ScriptRunner::scripted(vec![failed(1, "Cannot connect to the Docker daemon\n")]);
        let error = Docker::job(&mut runner)
            .container_id("svc")
            .expect_err("daemon failure must error");
        assert!(!is_not_found(&error));
        assert!(
            format!("{error:#}").contains("Cannot connect"),
            "stderr must survive: {error:#}"
        );
    }

    #[test]
    fn exit_info_reports_stop_time_and_never_proves_running_idle() {
        let exited = parse_exit_info("exited 2026-09-12T20:11:02.437775991Z\n").expect("parse");
        assert_eq!(exited.status, Some(ContainerState::Exited));
        let finished = exited.finished.expect("stopped has a stop time");
        assert!(
            finished < std::time::SystemTime::now(),
            "a past stop reads as past"
        );

        // Running reports the zero time: no idleness proof.
        let running = parse_exit_info("running 0001-01-01T00:00:00Z\n").expect("parse");
        assert_eq!(running.status, Some(ContainerState::Running));
        assert_eq!(running.finished, None);

        // Garbage timestamps never prove idleness either.
        let broken = parse_exit_info("exited not-a-time\n").expect("parse");
        assert_eq!(broken.status, Some(ContainerState::Exited));
        assert_eq!(broken.finished, None);

        assert!(parse_exit_info("exited\n").is_err());
    }

    #[test]
    fn buildx_disk_usage_reads_the_total_footer_in_decimal_units() {
        // `docker buildx du`, live: per-record rows plus the footer.
        let output = "\
ID                           RECLAIMABLE   SIZE      LAST ACCESSED
qoyzm9h3t5d8kc4avc1jzrft8*   true          0B        Less than a second ago
ut03rtsmqbdemi4moqok6mtc2    true          6.054MB   Less than a second ago
Reclaimable:\t6.054MB
Total:\t\t6.054MB
";
        assert_eq!(parse_buildx_disk_usage(output).expect("total"), 6_054_000);
        assert_eq!(parse_buildx_disk_usage("Total:\t\t0B\n").expect("zero"), 0);
        // 4096 bytes display as 4.096kB: units are 1000-based, proven live.
        assert_eq!(parse_human_size("4.096kB"), Some(4096));
        assert_eq!(parse_human_size("27.03MB"), Some(27_030_000));
        assert_eq!(parse_human_size("1.5GB"), Some(1_500_000_000));
        assert_eq!(parse_human_size("2TB"), Some(2_000_000_000_000));
        assert_eq!(parse_human_size("bogus"), None);
        assert_eq!(parse_human_size("-1MB"), None);
        assert!(parse_buildx_disk_usage("no footer here\n").is_err());
    }

    #[test]
    fn teardown_matcher_skips_persistent_builders_by_label_and_needle() {
        // A persistent builder carries its creating job's label BY DESIGN;
        // matching it here would destroy a daemon other jobs share.
        let listed = "aaa111\tbuildx_buildkit_velnor-builder-shared-trusted-o_r0\tvelnor-job-9\n\
             bbb222\tbuildx_buildkit_velnor-builder-slot-30\tvelnor-job-9\n\
             ccc333\tbuildx_buildkit_velnor-builder-shared-trusted-o_r0\tvelnor-job-1\n";
        // Label match: only the legacy builder.
        assert_eq!(
            job_buildkit_ids_for_job(listed, "velnor-job-9", "other-scope"),
            vec!["bbb222".to_string()]
        );
        // Slot-needle match: the persistent daemon can never carry a slot
        // suffix, and is skipped even so.
        assert_eq!(
            job_buildkit_ids_for_job(listed, "velnor-job-nobody", "slot-3"),
            vec!["bbb222".to_string()]
        );
    }

    #[test]
    fn orphan_matcher_skips_persistent_builders_and_volumes() {
        let live = BTreeSet::new();
        let daemons = "aaa111\tbuildx_buildkit_velnor-builder-shared-trusted-o_r0\tvelnor-job-9\t/daemon\texited\n\
             bbb222\tbuildx_buildkit_velnor-builder-slot-30\tvelnor-job-9\t/daemon\texited\n";
        assert_eq!(
            orphan_job_buildkit_ids(daemons, &live, Some("/daemon")),
            vec!["bbb222".to_string()]
        );
        let volumes =
            "buildx_buildkit_velnor-builder-shared-trusted-o_r0_state\tvelnor-job-9\t/daemon\n\
             buildx_buildkit_velnor-builder-slot-30_state\tvelnor-job-9\t/daemon\n";
        assert_eq!(
            daemon_owned_buildkit_volume_names(volumes, "/daemon", &live),
            vec!["buildx_buildkit_velnor-builder-slot-30_state".to_string()]
        );
    }

    #[test]
    fn timed_child_kill_uses_owned_handle_and_reaps_child() {
        let child = std::process::Command::new("sh")
            .args(["-c", "exec sleep 5"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn timed child");
        let (output, timed_out) =
            wait_for_child_with_timeout(child, Duration::from_millis(20)).expect("wait child");

        assert!(timed_out);
        assert!(!output.status.success());
    }

    // ------------------------------------------------------------------
    // Engine-API routing: identical values, subprocesses only on fallback
    // ------------------------------------------------------------------

    const ROUTED_INSPECT: &str = r#"{"Id":"a530e70d9e1e35941b6fc12db9b51a7b19c6d02","State":{"Running":true,"Status":"running","FinishedAt":"0001-01-01T00:00:00Z","Health":{"Status":"healthy"}},"NetworkSettings":{"Ports":{"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"41062"},{"HostIp":"::","HostPort":"41062"}]}}}"#;
    const ROUTED_IMAGE: &str = r#"{"Id":"sha256:feedface"}"#;
    const ROUTED_INFO: &str = r#"{"CgroupDriver":"systemd","CgroupVersion":2}"#;

    fn routed_mock(connections: usize) -> MockEngine {
        MockEngine::serve(
            |head| {
                if head.contains("GET /info ") {
                    json_response(ROUTED_INFO)
                } else if head.contains("GET /images/") {
                    json_response(ROUTED_IMAGE)
                } else {
                    json_response(ROUTED_INSPECT)
                }
            },
            connections,
        )
    }

    /// The seven migrated queries in one fixed order, returning their typed
    /// values for cross-transport comparison.
    type RoutedValues = (
        Readiness,
        bool,
        String,
        String,
        Vec<PortMapping>,
        CgroupDriver,
        ExitInfo,
    );

    fn routed_sequence(docker: &mut Docker<'_>) -> Result<RoutedValues> {
        Ok((
            docker.container_readiness("svc")?,
            docker.container_running("svc")?,
            docker.container_id("svc")?,
            docker.image_id("img:tag")?,
            docker.mapped_ports("svc")?,
            docker.daemon_cgroup()?,
            docker.inspect_exit("svc")?,
        ))
    }

    fn cli_script_for_routed_sequence() -> Vec<CommandResult> {
        vec![
            ok("healthy\n"),
            ok("true\n"),
            ok("a530e70d9e1e35941b6fc12db9b51a7b19c6d02\n"),
            ok("sha256:feedface\n"),
            ok("8080/tcp -> 0.0.0.0:41062\n8080/tcp -> [::]:41062\n"),
            ok("systemd 2\n"),
            ok("running 0001-01-01T00:00:00Z\n"),
        ]
    }

    #[test]
    fn representative_sequence_is_identical_with_zero_subprocess_on_api() {
        let mock = routed_mock(7);

        // Before: engine off (the test default), every query is one CLI call.
        let cli_values = {
            let _serial = crate::docker::metrics::lock_serial_for_test();
            let _scope = begin_job("seq-cli");
            let mut runner = ScriptRunner::scripted_host(cli_script_for_routed_sequence());
            let values = {
                let mut docker = Docker::job(&mut runner);
                routed_sequence(&mut docker).expect("cli sequence serves")
            };
            assert_eq!(runner.calls.load(Ordering::SeqCst), 7);
            let counts = snapshot();
            assert_eq!(counts.api_calls, 0);
            assert_eq!(counts.api_fallbacks, 0);
            // The scripted runner bypasses the real spawn seam where
            // `invocations` is observed, so it stays zero here; the seven
            // runner calls above are the seven subprocesses, one per query,
            // as `each_query_costs_exactly_one_process` pins down.
            assert_eq!(counts.invocations, 0);
            values
        };

        // After: engine on, the same values with no runner call at all.
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("seq-api");
        let mut runner = ScriptRunner::scripted_host(Vec::new());
        let api_values = {
            let mut docker = Docker::job(&mut runner);
            routed_sequence(&mut docker).expect("api sequence serves")
        };
        assert_eq!(
            runner.calls.load(Ordering::SeqCst),
            0,
            "no CLI call may run while the API serves"
        );
        let counts = snapshot();
        assert_eq!(counts.api_calls, 7);
        assert_eq!(counts.api_fallbacks, 0);
        assert_eq!(counts.invocations, 0);
        assert_eq!(api_values, cli_values, "transports must agree exactly");
    }

    #[test]
    fn api_status_failure_falls_back_to_one_cli_call() {
        let mock = MockEngine::serve(
            |_| b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_vec(),
            1,
        );
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("seq-fallback-status");
        let mut runner =
            ScriptRunner::scripted_host(vec![ok("a530e70d9e1e35941b6fc12db9b51a7b19c6d02\n")]);
        let id = {
            let mut docker = Docker::job(&mut runner);
            docker.container_id("svc").expect("fallback serves")
        };
        assert_eq!(id, "a530e70d9e1e35941b6fc12db9b51a7b19c6d02");
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        let counts = snapshot();
        assert_eq!(counts.api_calls, 0);
        assert_eq!(counts.api_fallbacks, 1);
    }

    #[test]
    fn api_timeout_falls_back_to_cli() {
        let mock = MockEngine::serve(
            |_| {
                std::thread::sleep(Duration::from_millis(300));
                json_response(ROUTED_INSPECT)
            },
            1,
        );
        let _guard = EngineTestGuard::serve(mock.socket.clone(), Some(30));
        let _scope = begin_job("seq-fallback-timeout");
        let mut runner = ScriptRunner::scripted_host(vec![ok("healthy\n")]);
        let readiness = {
            let mut docker = Docker::job(&mut runner);
            docker.container_readiness("svc").expect("fallback serves")
        };
        assert_eq!(readiness, Readiness::Healthy);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(snapshot().api_fallbacks, 1);
    }

    #[test]
    fn missing_container_falls_back_and_still_reads_as_not_running() {
        let mock = MockEngine::serve(
            |_| b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
            1,
        );
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("seq-fallback-missing");
        let mut runner =
            ScriptRunner::scripted_host(vec![failed(1, "Error: No such object: svc\n")]);
        let running = {
            let mut docker = Docker::job(&mut runner);
            docker
                .container_running("svc")
                .expect("missing reads as not running")
        };
        assert!(!running);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(snapshot().api_fallbacks, 1);
    }

    #[test]
    fn runtime_build_failure_falls_back_to_one_cli_call() {
        // The mock is healthy and would serve: the dead runtime alone must
        // route to the CLI, never panic the calling thread.
        let mock = routed_mock(1);
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _fail = FailRuntimeBuildGuard::inject();
        let _scope = begin_job("seq-fallback-runtime");
        let mut runner = ScriptRunner::scripted_host(vec![ok("healthy\n")]);
        let readiness = {
            let mut docker = Docker::job(&mut runner);
            docker.container_readiness("svc").expect("fallback serves")
        };
        assert_eq!(readiness, Readiness::Healthy);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        let counts = snapshot();
        assert_eq!(counts.api_calls, 0);
        assert_eq!(counts.api_fallbacks, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn current_thread_context_serves_api_without_panicking() {
        // `block_in_place` panics on this flavor; the facade drives the
        // helper thread instead and serves from the API with no CLI call.
        let mock = routed_mock(1);
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("seq-current-thread");
        let mut runner = ScriptRunner::scripted_host(Vec::new());
        let id = {
            let mut docker = Docker::job(&mut runner);
            docker
                .container_id("svc")
                .expect("api serves on current-thread")
        };
        assert_eq!(id, "a530e70d9e1e35941b6fc12db9b51a7b19c6d02");
        assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
        assert_eq!(snapshot().api_calls, 1);
    }

    /// A mock that answers one inspect after `delay`: the API leg would
    /// ride the wait, so any fast CLI answer proves the race abandoned it.
    fn slow_mock(delay: Duration) -> MockEngine {
        MockEngine::serve(
            move |_| {
                std::thread::sleep(delay);
                json_response(ROUTED_INSPECT)
            },
            1,
        )
    }

    #[test]
    fn cancelled_before_start_skips_the_socket_wait() {
        let mock = slow_mock(Duration::from_millis(500));
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let token = JobCancellation::recording(None);
        token.request(CancelReason::ServerRequested);
        let _active = set_active(token);
        let _scope = begin_job("seq-cancel-before");
        let mut runner = ScriptRunner::scripted_host(vec![ok("healthy\n")]);
        let started = Instant::now();
        let readiness = {
            let mut docker = Docker::job(&mut runner);
            docker
                .container_readiness("svc")
                .expect("cancelled api falls back")
        };
        assert_eq!(readiness, Readiness::Healthy);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(snapshot().api_fallbacks, 1);
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "a cancelled wait must not ride the 500ms socket delay, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn cancelled_mid_flight_abandons_the_socket_wait() {
        let mock = slow_mock(Duration::from_millis(500));
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let token = JobCancellation::recording(None);
        let _active = set_active(token.clone());
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            token.request(CancelReason::ServerRequested);
        });
        let _scope = begin_job("seq-cancel-midflight");
        let mut runner = ScriptRunner::scripted_host(vec![ok("healthy\n")]);
        let started = Instant::now();
        let readiness = {
            let mut docker = Docker::job(&mut runner);
            docker
                .container_readiness("svc")
                .expect("cancelled api falls back")
        };
        canceller.join().expect("canceller joins");
        assert_eq!(readiness, Readiness::Healthy);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(snapshot().api_fallbacks, 1);
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "a mid-flight cancel must abandon the 500ms socket delay, took {:?}",
            started.elapsed()
        );
    }

    /// Live Engine latency bench: API vs CLI on the daemon-cgroup query,
    /// which needs no container and runs on any daemon host. Ignored: it
    /// needs a live `/var/run/docker.sock` and spawns real `docker`
    /// children. Run it with:
    /// `cargo test -p velnor-runner --lib live_engine_latency_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_engine_latency_bench() {
        use std::os::unix::net::UnixStream;
        const ITERATIONS: usize = 25;
        let socket = super::super::engine::socket_path();
        if UnixStream::connect(&socket).is_err() {
            println!(
                "live_engine_latency_bench: SKIP, no live daemon at {}",
                socket.display()
            );
            return;
        }
        fn summarize(name: &str, mut samples: Vec<Duration>) {
            samples.sort();
            let percentile = |p: usize| samples[(samples.len() * p / 100).min(samples.len() - 1)];
            println!(
                "live_engine_latency_bench: {name} n={} p50={:?} p95={:?} min={:?} max={:?}",
                samples.len(),
                percentile(50),
                percentile(95),
                samples[0],
                samples[samples.len() - 1],
            );
        }
        let api: Vec<(Duration, CgroupDriver)> = {
            let _guard = EngineTestGuard::serve(socket, None);
            (0..ITERATIONS)
                .map(|_| {
                    let started = Instant::now();
                    let value = Docker::host()
                        .daemon_cgroup()
                        .expect("live api serves daemon cgroup");
                    (started.elapsed(), value)
                })
                .collect()
        };
        let cli: Vec<(Duration, CgroupDriver)> = (0..ITERATIONS)
            .map(|_| {
                let started = Instant::now();
                let value = Docker::host()
                    .daemon_cgroup()
                    .expect("live cli serves daemon cgroup");
                (started.elapsed(), value)
            })
            .collect();
        for (iteration, ((_, api_value), (_, cli_value))) in api.iter().zip(cli.iter()).enumerate()
        {
            assert_eq!(
                api_value, cli_value,
                "live transports disagree on iteration {iteration}"
            );
        }
        summarize(
            "api ",
            api.into_iter().map(|(elapsed, _)| elapsed).collect(),
        );
        summarize(
            "cli ",
            cli.into_iter().map(|(elapsed, _)| elapsed).collect(),
        );
    }

    // ------------------------------------------------------------------
    // Owned-list routing: identical rows, subprocesses only on fallback
    // ------------------------------------------------------------------

    /// Live-shaped list documents: Id/Names/Labels/State values verbatim
    /// from Engine 29.4.0 `GET` captures (unread fields elided, as in the
    /// inspect fixtures), plus the short-id-prefix CLI text the daemon
    /// printed for the same objects.
    const ROUTED_PS: &str = r#"[{"Id":"9f7d9044009aa16e83c437b67d71e7325a2377d6d00b1ec6beb3adad0812e7b4","Names":["/velnor-eng2-probe"],"Labels":{"velnor.job-id":"velnor-eng2-probe"},"State":"created"},{"Id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","Names":["/buildx_buildkit_velnor-builder-dead0"],"Labels":{"velnor.job-id":"velnor-eng2-probe"},"State":"exited"}]"#;
    const ROUTED_NETWORKS: &str = r#"[{"Name":"velnor-eng2-net","Id":"51890326820b1aaec84f85251e1ae0695801bffc1f6504481191ce7c6f647bdc","Scope":"local","Driver":"bridge","Labels":{"velnor.job-id":"velnor-eng2-probe"}}]"#;
    const ROUTED_VOLUMES: &str = r#"{"Volumes":[{"Driver":"local","Labels":{"velnor.job-id":"velnor-eng2-probe"},"Name":"velnor-eng2-vol","Scope":"local"}],"Warnings":null}"#;

    fn routed_list_mock(connections: usize) -> MockEngine {
        MockEngine::serve(
            |head| {
                if head.contains("GET /containers/json?") {
                    json_response(ROUTED_PS)
                } else if head.contains("GET /networks?") {
                    json_response(ROUTED_NETWORKS)
                } else {
                    json_response(ROUTED_VOLUMES)
                }
            },
            connections,
        )
    }

    type ListValues = (Vec<OwnedContainer>, Vec<String>, Vec<String>);

    fn list_sequence(docker: &mut Docker<'_>) -> Result<ListValues> {
        Ok((
            docker.list_owned_containers("velnor-eng2-probe")?,
            docker.list_owned_networks("velnor-eng2-probe")?,
            docker.list_owned_volumes("velnor-eng2-probe")?,
        ))
    }

    fn cli_script_for_list_sequence() -> Vec<CommandResult> {
        vec![
            ok("9f7d9044009a\tvelnor-eng2-probe\nbbbbbbbbbbbb\tbuildx_buildkit_velnor-builder-dead0\n"),
            ok("51890326820b\n"),
            ok("velnor-eng2-vol\n"),
        ]
    }

    #[test]
    fn owned_list_sequence_is_identical_with_zero_subprocess_on_api() {
        let mock = routed_list_mock(3);

        // Before: engine off, three listings are three CLI calls.
        let cli_values = {
            let _serial = crate::docker::metrics::lock_serial_for_test();
            let _scope = begin_job("list-cli");
            let mut runner = ScriptRunner::scripted_host(cli_script_for_list_sequence());
            let values = {
                let mut docker = Docker::job(&mut runner);
                list_sequence(&mut docker).expect("cli sequence serves")
            };
            assert_eq!(runner.calls.load(Ordering::SeqCst), 3);
            values
        };

        // After: engine on, the same values with no runner call at all.
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("list-api");
        let mut runner = ScriptRunner::scripted_host(Vec::new());
        let api_values = {
            let mut docker = Docker::job(&mut runner);
            list_sequence(&mut docker).expect("api sequence serves")
        };
        assert_eq!(
            runner.calls.load(Ordering::SeqCst),
            0,
            "no CLI call may run while the API serves"
        );
        let counts = snapshot();
        assert_eq!(counts.api_calls, 3);
        assert_eq!(counts.api_fallbacks, 0);
        assert_eq!(api_values, cli_values, "transports must agree exactly");
        // The shared decision sees the same rows on both legs: the guest is
        // reclaimed, the BuildKit daemon is excluded.
        assert_eq!(
            owned_container_ids_excluding_buildkit_rows(&api_values.0),
            vec!["9f7d9044009a".to_string()]
        );
    }

    #[test]
    fn owned_list_status_failure_falls_back_to_one_cli_call() {
        let mock = MockEngine::serve(
            |_| b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_vec(),
            1,
        );
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("list-fallback-status");
        let mut runner = ScriptRunner::scripted_host(vec![ok("51890326820b\n")]);
        let networks = {
            let mut docker = Docker::job(&mut runner);
            docker
                .list_owned_networks("velnor-eng2-probe")
                .expect("fallback serves")
        };
        assert_eq!(networks, vec!["51890326820b".to_string()]);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        let counts = snapshot();
        assert_eq!(counts.api_calls, 0);
        assert_eq!(counts.api_fallbacks, 1);
    }

    #[test]
    fn owned_row_projection_matches_cli_rendering() {
        use super::super::engine::EngineContainerSummary;
        use std::collections::BTreeMap;
        let summaries = vec![
            EngineContainerSummary {
                id: "9f7d9044009aa16e83c437b67d71e7325a2377d6d00b1ec6beb3adad0812e7b4".into(),
                names: vec!["/velnor-eng2-probe".into()],
                labels: BTreeMap::new(),
                state: "created".into(),
            },
            // Multi-name rows join with a comma, like the CLI's renderer;
            // the join keeps the BuildKit-contains check total.
            EngineContainerSummary {
                id: "short".into(),
                names: vec!["/guest".into(), "buildx_buildkit_velnor-builder-x0".into()],
                labels: BTreeMap::new(),
                state: "running".into(),
            },
            EngineContainerSummary {
                id: "cccccccccccc".into(),
                names: Vec::new(),
                labels: BTreeMap::new(),
                state: "exited".into(),
            },
        ];
        assert_eq!(
            project_owned_rows(&summaries),
            vec![
                OwnedContainer {
                    id: "9f7d9044009a".into(),
                    names: "velnor-eng2-probe".into(),
                },
                OwnedContainer {
                    id: "short".into(),
                    names: "guest,buildx_buildkit_velnor-builder-x0".into(),
                },
                OwnedContainer {
                    id: "cccccccccccc".into(),
                    names: String::new(),
                },
            ]
        );
        // Full ids truncate to the 12 chars the CLI prints; short ids pass
        // through untouched.
        assert_eq!(
            short_id("9f7d9044009aa16e83c437b67d71e7325a2377d6d00b1ec6beb3adad0812e7b4"),
            "9f7d9044009a"
        );
        assert_eq!(short_id("short"), "short");
        // Text and rows decide identically: the BuildKit row drops out of
        // both, the nameless row stays in both.
        let text = "9f7d9044009a\tvelnor-eng2-probe\nshort\tguest,buildx_buildkit_velnor-builder-x0\ncccccccccccc\t\n";
        assert_eq!(
            owned_container_ids_excluding_buildkit_rows(&parse_owned_container_rows(text)),
            owned_container_ids_excluding_buildkit_rows(&project_owned_rows(&summaries)),
        );
    }

    #[test]
    fn buildx_queries_stay_on_cli_without_api_attempts() {
        let mock = routed_mock(0);
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("seq-buildx-cli");
        let mut runner = ScriptRunner::scripted_host(vec![
            ok("velnor-builder-shared-trusted-o_r0\n"),
            ok("Total:\t\t6.054MB\n"),
        ]);
        let (builders, usage) = {
            let mut docker = Docker::job(&mut runner);
            (
                docker.buildx_builders().expect("builders list"),
                docker
                    .buildx_disk_usage("velnor-builder-shared-trusted-o_r0")
                    .expect("disk usage"),
            )
        };
        assert_eq!(
            builders,
            vec!["velnor-builder-shared-trusted-o_r0".to_string()]
        );
        assert_eq!(usage, 6_054_000);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
        let counts = snapshot();
        assert_eq!(counts.api_calls, 0);
        assert_eq!(counts.api_fallbacks, 0);
    }

    // ------------------------------------------------------------------
    // Lifecycle routing: idempotent mutations, subprocesses only on
    // fallback. CLI text below is Engine 29.4.0 verbatim; API bodies
    // are its error documents verbatim.
    // ------------------------------------------------------------------

    /// `docker start` on a missing container answers two lines, not one.
    const CLI_START_MISSING: &str =
        "Error response from daemon: No such container: svc\nfailed to start containers: svc\n";
    const CLI_RM_MISSING: &str = "Error response from daemon: No such container: svc\n";
    const CLI_RM_RUNNING: &str = "Error response from daemon: cannot remove container \"svc\": container is running: stop the container before removing or force remove\n";
    const CLI_RM_IN_FLIGHT: &str =
        "Error response from daemon: removal of container svc is already in progress\n";
    const API_NO_SUCH: &str = r#"{"message":"No such container: svc"}"#;
    const API_RUNNING: &str = r#"{"message":"cannot remove container \"svc\": container is running: stop the container before removing or force remove"}"#;
    const API_IN_FLIGHT: &str = r#"{"message":"removal of container svc is already in progress"}"#;

    type LifecycleValues = (StartOutcome, StopOutcome, RemoveOutcome);

    fn lifecycle_sequence(docker: &mut Docker<'_>) -> Result<LifecycleValues> {
        Ok((
            docker.container_start("svc")?,
            docker.container_stop("svc", Some(5))?,
            docker.container_remove("svc", true, false)?,
        ))
    }

    fn lifecycle_mock(connections: usize) -> MockEngine {
        MockEngine::serve(|_| status_response("204 No Content"), connections)
    }

    #[test]
    fn lifecycle_cycle_is_identical_with_zero_subprocess_on_api() {
        // Before: engine off, one start/stop/remove cycle is three CLI calls.
        let cli_values = {
            let _serial = crate::docker::metrics::lock_serial_for_test();
            let _scope = begin_job("lifecycle-cli");
            let mut runner =
                ScriptRunner::scripted_host(vec![ok("svc\n"), ok("svc\n"), ok("svc\n")]);
            let values = {
                let mut docker = Docker::job(&mut runner);
                lifecycle_sequence(&mut docker).expect("cli cycle serves")
            };
            assert_eq!(runner.calls.load(Ordering::SeqCst), 3);
            assert_eq!(
                *runner
                    .seen_args
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
                vec![
                    container_start_args("svc"),
                    container_stop_args("svc", Some(5)),
                    container_remove_args("svc", true, false),
                ]
            );
            values
        };

        // After: engine on, the same outcomes with no runner call at all.
        let mock = lifecycle_mock(3);
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("lifecycle-api");
        let mut runner = ScriptRunner::scripted_host(Vec::new());
        let api_values = {
            let mut docker = Docker::job(&mut runner);
            lifecycle_sequence(&mut docker).expect("api cycle serves")
        };
        assert_eq!(
            runner.calls.load(Ordering::SeqCst),
            0,
            "no CLI call may run while the API serves"
        );
        let counts = snapshot();
        assert_eq!(counts.api_calls, 3);
        assert_eq!(counts.api_fallbacks, 0);
        assert_eq!(api_values, cli_values, "transports must agree exactly");
        assert_eq!(
            api_values,
            (
                StartOutcome::Started,
                StopOutcome::Stopped,
                RemoveOutcome::Removed
            )
        );
    }

    #[test]
    fn already_states_succeed_on_both_legs() {
        // CLI leg: exit 0 reads as acted (the CLI prints the name
        // identically for fresh and redundant starts/stops), missing on
        // remove reads as already-removed.
        let cli_values = {
            let _serial = crate::docker::metrics::lock_serial_for_test();
            let _scope = begin_job("lifecycle-cli-already");
            let mut runner = ScriptRunner::scripted_host(vec![
                ok("svc\n"),
                ok("svc\n"),
                failed(1, CLI_RM_MISSING),
            ]);
            let values = {
                let mut docker = Docker::job(&mut runner);
                lifecycle_sequence(&mut docker).expect("cli already-states serve")
            };
            assert_eq!(runner.calls.load(Ordering::SeqCst), 3);
            values
        };
        assert_eq!(
            cli_values,
            (
                StartOutcome::Started,
                StopOutcome::Stopped,
                RemoveOutcome::AlreadyRemoved
            )
        );

        // API leg: 304/304/404 distinguish precisely, with no subprocess.
        let mock = MockEngine::serve(
            |head| {
                if head.contains("DELETE /containers/") {
                    error_response("404 Not Found", API_NO_SUCH)
                } else {
                    status_response("304 Not Modified")
                }
            },
            3,
        );
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("lifecycle-api-already");
        let mut runner = ScriptRunner::scripted_host(Vec::new());
        let api_values = {
            let mut docker = Docker::job(&mut runner);
            lifecycle_sequence(&mut docker).expect("api already-states serve")
        };
        assert_eq!(runner.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            api_values,
            (
                StartOutcome::AlreadyStarted,
                StopOutcome::AlreadyStopped,
                RemoveOutcome::AlreadyRemoved
            )
        );
        let counts = snapshot();
        assert_eq!(counts.api_calls, 3);
        assert_eq!(counts.api_fallbacks, 0);
    }

    #[test]
    fn genuine_conflicts_surface_typed_conflict() {
        // Removing a running container without force: the API 409 falls
        // back, and the CLI re-derives Conflict — never success.
        {
            let mock = MockEngine::serve(|_| error_response("409 Conflict", API_RUNNING), 1);
            let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
            let _scope = begin_job("lifecycle-conflict-rm");
            let mut runner = ScriptRunner::scripted_host(vec![failed(1, CLI_RM_RUNNING)]);
            let error = {
                let mut docker = Docker::job(&mut runner);
                docker
                    .container_remove("svc", false, false)
                    .expect_err("running rm without force must fail")
            };
            assert!(!is_not_found(&error));
            assert_eq!(
                docker_error_category(&error),
                DockerErrorCategory::Conflict,
                "{error:#}"
            );
            assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
            let counts = snapshot();
            assert_eq!(counts.api_calls, 0);
            assert_eq!(counts.api_fallbacks, 1);
        }

        // Starting a missing container: the API 404 falls back, and the
        // CLI re-derives NotFound, which is Conflict-shaped (another
        // writer removed the object mid-flight).
        {
            let mock = MockEngine::serve(|_| error_response("404 Not Found", API_NO_SUCH), 1);
            let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
            let _scope = begin_job("lifecycle-conflict-start");
            let mut runner = ScriptRunner::scripted_host(vec![failed(1, CLI_START_MISSING)]);
            let error = {
                let mut docker = Docker::job(&mut runner);
                docker
                    .container_start("svc")
                    .expect_err("missing start must fail")
            };
            assert!(is_not_found(&error), "{error:#}");
            assert_eq!(
                docker_error_category(&error),
                DockerErrorCategory::Conflict,
                "{error:#}"
            );
            assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
            assert_eq!(snapshot().api_fallbacks, 1);
        }
    }

    #[test]
    fn mutation_api_failure_falls_back_to_one_cli_call() {
        // A daemon 500 on stop: one CLI call serves the stop.
        {
            let mock = MockEngine::serve(
                |_| b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n".to_vec(),
                1,
            );
            let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
            let _scope = begin_job("lifecycle-fallback-status");
            let mut runner = ScriptRunner::scripted_host(vec![ok("svc\n")]);
            let outcome = {
                let mut docker = Docker::job(&mut runner);
                docker
                    .container_stop("svc", Some(5))
                    .expect("fallback serves")
            };
            assert_eq!(outcome, StopOutcome::Stopped);
            assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
            assert_eq!(snapshot().api_fallbacks, 1);
        }

        // A slow daemon on start: the capped API budget falls back instead
        // of riding the class deadline on the socket.
        {
            let mock = MockEngine::serve(
                |_| {
                    std::thread::sleep(Duration::from_millis(300));
                    status_response("204 No Content")
                },
                1,
            );
            let _guard = EngineTestGuard::serve(mock.socket.clone(), Some(30));
            let _scope = begin_job("lifecycle-fallback-timeout");
            let mut runner = ScriptRunner::scripted_host(vec![ok("svc\n")]);
            let outcome = {
                let mut docker = Docker::job(&mut runner);
                docker.container_start("svc").expect("fallback serves")
            };
            assert_eq!(outcome, StartOutcome::Started);
            assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
            assert_eq!(snapshot().api_fallbacks, 1);
        }
    }

    #[test]
    fn removal_in_flight_succeeds_but_transient_surfaces() {
        // Pure CLI leg: an in-flight removal is the desired end state
        // converging, so remove succeeds.
        {
            let _serial = crate::docker::metrics::lock_serial_for_test();
            let _scope = begin_job("lifecycle-cli-inflight");
            let mut runner = ScriptRunner::scripted_host(vec![failed(1, CLI_RM_IN_FLIGHT)]);
            let outcome = {
                let mut docker = Docker::job(&mut runner);
                docker
                    .container_remove("svc", true, false)
                    .expect("in-flight remove succeeds")
            };
            assert_eq!(outcome, RemoveOutcome::Removed);
            assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        }

        // API leg: a 409 carrying the in-progress sentence falls back, and
        // the CLI leg tolerates it the same way.
        {
            let mock = MockEngine::serve(|_| error_response("409 Conflict", API_IN_FLIGHT), 1);
            let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
            let _scope = begin_job("lifecycle-api-inflight");
            let mut runner = ScriptRunner::scripted_host(vec![failed(1, CLI_RM_IN_FLIGHT)]);
            let outcome = {
                let mut docker = Docker::job(&mut runner);
                docker
                    .container_remove("svc", true, false)
                    .expect("in-flight remove succeeds")
            };
            assert_eq!(outcome, RemoveOutcome::Removed);
            assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
            assert_eq!(snapshot().api_fallbacks, 1);
        }

        // A stderr that also matches Transient is a sick daemon, not a
        // converging removal: it surfaces instead of masking as success.
        {
            let _serial = crate::docker::metrics::lock_serial_for_test();
            let _scope = begin_job("lifecycle-cli-transient");
            let mut runner = ScriptRunner::scripted_host(vec![failed(
                1,
                "Error response from daemon: removal of container svc is already in progress: connection reset by peer\n",
            )]);
            let error = {
                let mut docker = Docker::job(&mut runner);
                docker
                    .container_remove("svc", true, false)
                    .expect_err("transient must surface")
            };
            assert_eq!(
                docker_error_category(&error),
                DockerErrorCategory::Transient,
                "{error:#}"
            );
        }
    }

    #[test]
    fn concurrent_lifecycle_races_all_succeed_idempotently() {
        // Two racers, one object: the first acts, the second observes the
        // already-state. Both succeed; any fallback would exhaust the
        // empty scripts and fail the test.
        {
            let hits = std::sync::Arc::new(AtomicUsize::new(0));
            let hits_server = std::sync::Arc::clone(&hits);
            let mock = MockEngine::serve(
                move |_| {
                    if hits_server.fetch_add(1, Ordering::SeqCst) == 0 {
                        status_response("204 No Content")
                    } else {
                        status_response("304 Not Modified")
                    }
                },
                2,
            );
            let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
            let _scope = begin_job("lifecycle-race-start");
            let (first, second) = std::thread::scope(|scope| {
                // Non-capturing closures: nothing borrowed into the scope.
                let first = scope.spawn(|| {
                    let mut runner = ScriptRunner::scripted_host(Vec::new());
                    Docker::job(&mut runner).container_start("svc")
                });
                let second = scope.spawn(|| {
                    let mut runner = ScriptRunner::scripted_host(Vec::new());
                    Docker::job(&mut runner).container_start("svc")
                });
                (
                    first.join().expect("racer joins"),
                    second.join().expect("racer joins"),
                )
            });
            let outcomes = [
                first.expect("racing start succeeds"),
                second.expect("racing start succeeds"),
            ];
            assert!(outcomes.contains(&StartOutcome::Started));
            assert!(outcomes.contains(&StartOutcome::AlreadyStarted));
            let counts = snapshot();
            assert_eq!(counts.api_calls, 2);
            assert_eq!(counts.api_fallbacks, 0);
        }

        {
            let hits = std::sync::Arc::new(AtomicUsize::new(0));
            let hits_server = std::sync::Arc::clone(&hits);
            let mock = MockEngine::serve(
                move |_| {
                    if hits_server.fetch_add(1, Ordering::SeqCst) == 0 {
                        status_response("204 No Content")
                    } else {
                        status_response("304 Not Modified")
                    }
                },
                2,
            );
            let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
            let _scope = begin_job("lifecycle-race-stop");
            let (first, second) = std::thread::scope(|scope| {
                let first = scope.spawn(|| {
                    let mut runner = ScriptRunner::scripted_host(Vec::new());
                    Docker::job(&mut runner).container_stop("svc", Some(5))
                });
                let second = scope.spawn(|| {
                    let mut runner = ScriptRunner::scripted_host(Vec::new());
                    Docker::job(&mut runner).container_stop("svc", Some(5))
                });
                (
                    first.join().expect("racer joins"),
                    second.join().expect("racer joins"),
                )
            });
            let outcomes = [
                first.expect("racing stop succeeds"),
                second.expect("racing stop succeeds"),
            ];
            assert!(outcomes.contains(&StopOutcome::Stopped));
            assert!(outcomes.contains(&StopOutcome::AlreadyStopped));
            let counts = snapshot();
            assert_eq!(counts.api_calls, 2);
            assert_eq!(counts.api_fallbacks, 0);
        }

        {
            let hits = std::sync::Arc::new(AtomicUsize::new(0));
            let hits_server = std::sync::Arc::clone(&hits);
            let mock = MockEngine::serve(
                move |_| {
                    if hits_server.fetch_add(1, Ordering::SeqCst) == 0 {
                        status_response("204 No Content")
                    } else {
                        error_response("404 Not Found", API_NO_SUCH)
                    }
                },
                2,
            );
            let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
            let _scope = begin_job("lifecycle-race-remove");
            let (first, second) = std::thread::scope(|scope| {
                let first = scope.spawn(|| {
                    let mut runner = ScriptRunner::scripted_host(Vec::new());
                    Docker::job(&mut runner).container_remove("svc", true, false)
                });
                let second = scope.spawn(|| {
                    let mut runner = ScriptRunner::scripted_host(Vec::new());
                    Docker::job(&mut runner).container_remove("svc", true, false)
                });
                (
                    first.join().expect("racer joins"),
                    second.join().expect("racer joins"),
                )
            });
            let outcomes = [
                first.expect("racing remove succeeds"),
                second.expect("racing remove succeeds"),
            ];
            assert!(outcomes.contains(&RemoveOutcome::Removed));
            assert!(outcomes.contains(&RemoveOutcome::AlreadyRemoved));
            let counts = snapshot();
            assert_eq!(counts.api_calls, 2);
            assert_eq!(counts.api_fallbacks, 0);
        }
    }

    #[test]
    fn every_facade_mutation_is_a_bounded_control_plane_call() {
        const SIX_HOURS: Duration = Duration::from_secs(6 * 3600);
        let calls = vec![
            container_start_args("svc"),
            container_stop_args("svc", None),
            container_stop_args("svc", Some(5)),
            container_stop_args("svc", Some(300)),
            container_remove_args("svc", false, false),
            container_remove_args("svc", true, false),
            container_remove_args("svc", false, true),
            container_remove_args("svc", true, true),
        ];
        for args in &calls {
            let (op, deadline) = crate::docker::deadline_for(args, SIX_HOURS);
            assert!(
                op.is_control_plane(),
                "{args:?} must classify as control plane, got {op}"
            );
            assert!(
                deadline < SIX_HOURS,
                "{args:?} ({op}) inherited the step deadline"
            );
        }
        assert_eq!(
            crate::docker::deadline_for(&container_start_args("svc"), SIX_HOURS),
            (crate::docker::DockerOp::Start, Duration::from_secs(120))
        );
        // An explicit 300s grace clears the grace plus headroom, exactly as
        // the class policy promises.
        assert_eq!(
            crate::docker::deadline_for(&container_stop_args("svc", Some(300)), SIX_HOURS),
            (crate::docker::DockerOp::Stop, Duration::from_secs(360))
        );
        assert_eq!(
            crate::docker::deadline_for(&container_remove_args("svc", true, true), SIX_HOURS),
            (crate::docker::DockerOp::Remove, Duration::from_secs(20))
        );
    }

    // ------------------------------------------------------------------
    // Script-step exec routing: served, expired, or one CLI call
    // ------------------------------------------------------------------

    /// The historical CLI vector for one script step, argv-shaped as the
    /// executor builds it: flags, `--env-file`, `--` separator, container
    /// operand, command.
    fn script_exec_cli_args() -> Vec<String> {
        vec![
            "exec",
            "--workdir",
            "/__w",
            "--env-file",
            "/tmp/velnor-env-x",
            "--",
            "velnor-job-1",
            "sh",
            "-e",
            "/__t/step.sh",
        ]
        .into_iter()
        .map(str::to_string)
        .collect()
    }

    fn script_exec_config() -> ExecConfig {
        ExecConfig {
            cmd: vec!["sh".into(), "-e".into(), "/__t/step.sh".into()],
            env: vec![("A".into(), "1".into())],
            workdir: "/__w".into(),
        }
    }

    /// The three-connection exec conversation with live Engine 29.4.0
    /// fixtures: 201 create, multiplexed start stream (stderr first, as
    /// the daemon scheduled it), exit-3 inspect.
    fn served_exec_mock() -> MockEngine {
        MockEngine::serve(
            |request| {
                if request.contains("POST /containers/") {
                    error_response(
                        "201 Created",
                        r#"{"Id":"bfd9079ad9ae2473ffbc0d6eb008f4bd8c9d96ee046b040713ac823a7838e82e"}"#,
                    )
                } else if request.contains("POST /exec/") {
                    let mut stream = exec_frame(2, b"err\n");
                    stream.extend_from_slice(&exec_frame(1, b"out\n"));
                    start_response(&stream)
                } else {
                    json_response(
                        r#"{"ID":"bfd9079ad9ae2473ffbc0d6eb008f4bd8c9d96ee046b040713ac823a7838e82e","Running":false,"ExitCode":3}"#,
                    )
                }
            },
            3,
        )
    }

    #[test]
    fn script_exec_served_with_zero_subprocess_before_after_identity() {
        // Before: engine off, the CLI leg serves the scripted step result.
        let cli_args = script_exec_cli_args();
        let cli_result = CommandResult {
            code: 3,
            stdout: "out\n".into(),
            stderr: "err\n".into(),
        };
        let mut runner = ScriptRunner::scripted(vec![cli_result.clone()]);
        let before = {
            let docker = Docker::job(&mut runner);
            let route = docker.try_exec_script(
                "velnor-job-1",
                &cli_args,
                &script_exec_config(),
                Duration::from_secs(60),
                &mut |_, _| {},
            );
            assert_eq!(route, ScriptExecRoute::UseCli);
            // The caller runs its historical CLI call on UseCli.
            runner
                .run_streaming_timeout_with_env(
                    "docker",
                    &cli_args,
                    &[],
                    Duration::from_secs(60),
                    &mut |_, _| {},
                )
                .expect("cli leg serves")
        };
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        assert_eq!(before, cli_result);

        // After: engine on, the same step with no runner call at all.
        let mock = served_exec_mock();
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("exec-served");
        let mut runner = ScriptRunner::scripted_host(Vec::new());
        let mut lines = Vec::new();
        let after = {
            let docker = Docker::job(&mut runner);
            match docker.try_exec_script(
                "velnor-job-1",
                &cli_args,
                &script_exec_config(),
                Duration::from_secs(60),
                &mut |stream, line: &str| lines.push((stream, line.to_string())),
            ) {
                ScriptExecRoute::Served(result) => result,
                route => panic!("api must serve, got {route:?}"),
            }
        };
        assert_eq!(
            runner.calls.load(Ordering::SeqCst),
            0,
            "no CLI call may run while the API serves"
        );
        assert_eq!(after, before, "transports must agree exactly");
        assert_eq!(
            lines,
            vec![
                (CommandStream::Stderr, "err".to_string()),
                (CommandStream::Stdout, "out".to_string()),
            ]
        );
        let counts = snapshot();
        assert_eq!(counts.api_calls, 1);
        assert_eq!(counts.api_fallbacks, 0);
        assert_eq!(counts.invocations, 0);
    }

    #[test]
    fn script_exec_status_error_falls_back_to_one_cli_call() {
        let mock = MockEngine::serve(
            |_| error_response("500 Internal Server Error", r#"{"message":"boom"}"#),
            1,
        );
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("exec-fallback-status");
        let cli_args = script_exec_cli_args();
        let cli_result = CommandResult {
            code: 3,
            stdout: "out\n".into(),
            stderr: "err\n".into(),
        };
        let mut runner = ScriptRunner::scripted_host(vec![cli_result.clone()]);
        let route = {
            let docker = Docker::job(&mut runner);
            docker.try_exec_script(
                "velnor-job-1",
                &cli_args,
                &script_exec_config(),
                Duration::from_secs(60),
                &mut |_, _| {},
            )
        };
        assert_eq!(route, ScriptExecRoute::UseCli);
        let served = runner
            .run_streaming_timeout_with_env(
                "docker",
                &cli_args,
                &[],
                Duration::from_secs(60),
                &mut |_, _| {},
            )
            .expect("cli fallback serves");
        assert_eq!(served, cli_result);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 1);
        let counts = snapshot();
        assert_eq!(counts.api_calls, 0);
        assert_eq!(counts.api_fallbacks, 1);
    }

    #[test]
    fn script_exec_slow_create_falls_back_instead_of_expiring() {
        // The create leg stalls past the capped API budget while the step
        // deadline is far away: the step must fall back to the CLI with
        // its full deadline, not 124.
        let mock = MockEngine::serve(
            |_| {
                std::thread::sleep(Duration::from_millis(300));
                error_response("201 Created", r#"{"Id":"abc"}"#)
            },
            1,
        );
        let _guard = EngineTestGuard::serve(mock.socket.clone(), Some(30));
        let _scope = begin_job("exec-fallback-create-timeout");
        let cli_args = script_exec_cli_args();
        let mut runner = ScriptRunner::scripted_host(vec![ok("out\n")]);
        let started = Instant::now();
        let route = {
            let docker = Docker::job(&mut runner);
            docker.try_exec_script(
                "velnor-job-1",
                &cli_args,
                &script_exec_config(),
                Duration::from_secs(10),
                &mut |_, _| {},
            )
        };
        assert_eq!(route, ScriptExecRoute::UseCli);
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "a stalled create must fail at the API budget, took {:?}",
            started.elapsed()
        );
        assert_eq!(snapshot().api_fallbacks, 1);
    }

    #[test]
    fn script_exec_cancel_mid_stream_falls_back_promptly() {
        let mock = MockEngine::serve(
            |request| {
                if request.contains("POST /containers/") {
                    error_response("201 Created", r#"{"Id":"abc"}"#)
                } else {
                    std::thread::sleep(Duration::from_millis(500));
                    start_response(&exec_frame(1, b"late\n"))
                }
            },
            2,
        );
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let token = JobCancellation::recording(None);
        let _active = set_active(token.clone());
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            token.request(CancelReason::ServerRequested);
        });
        let _scope = begin_job("exec-cancel-midstream");
        let cli_args = script_exec_cli_args();
        let mut runner = ScriptRunner::scripted_host(vec![ok("out\n")]);
        let started = Instant::now();
        let route = {
            let docker = Docker::job(&mut runner);
            docker.try_exec_script(
                "velnor-job-1",
                &cli_args,
                &script_exec_config(),
                Duration::from_secs(10),
                &mut |_, _| {},
            )
        };
        canceller.join().expect("canceller joins");
        assert_eq!(route, ScriptExecRoute::UseCli);
        assert_eq!(snapshot().api_fallbacks, 1);
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "a mid-stream cancel must abandon the 500ms socket delay, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn script_exec_deadline_expires_to_124_without_fallback() {
        let mock = MockEngine::serve(
            |request| {
                if request.contains("POST /containers/") {
                    error_response("201 Created", r#"{"Id":"abc"}"#)
                } else {
                    // The exec never produces output: the step deadline
                    // expires on the API leg.
                    std::thread::sleep(Duration::from_millis(500));
                    start_response(&exec_frame(1, b"late\n"))
                }
            },
            2,
        );
        let _guard = EngineTestGuard::serve(mock.socket.clone(), None);
        let _scope = begin_job("exec-expired");
        let cli_args = script_exec_cli_args();
        let mut runner = ScriptRunner::scripted_host(Vec::new());
        let started = Instant::now();
        let route = {
            let docker = Docker::job(&mut runner);
            docker.try_exec_script(
                "velnor-eng4-test-gone",
                &cli_args,
                &script_exec_config(),
                Duration::from_millis(30),
                &mut |_, _| {},
            )
        };
        let elapsed = started.elapsed();
        // Byte-identical to the CLI watchdog's expiry: code, empty partial
        // stdout, and the payload timeout sentence.
        assert_eq!(
            route,
            ScriptExecRoute::Expired(CommandResult {
                code: 124,
                stdout: String::new(),
                stderr:
                    "##[error]The operation was canceled because it exceeded timeout-minutes.\n"
                        .into(),
            })
        );
        assert_eq!(
            runner.calls.load(Ordering::SeqCst),
            0,
            "expiry serves, it never falls back"
        );
        // Bounded on the job thread: the daemon answers at 500 ms but the
        // step deadline is 30 ms, and no kill round trip follows — 124 must
        // return on the deadline, never behind a wedged daemon.
        assert!(
            elapsed < Duration::from_secs(10),
            "expiry must return promptly, took {elapsed:?}"
        );
        let counts = snapshot();
        assert_eq!(counts.api_calls, 1);
        assert_eq!(counts.api_fallbacks, 0);
    }

    #[test]
    fn script_exec_skips_api_for_non_host_runners() {
        // A nonexistent socket proves the negative: any API attempt would
        // surface as a connect fallback, so zero fallbacks means zero
        // attempts.
        let missing = std::path::Path::new("/no/such/velnor-engine.sock").to_path_buf();
        let cli_args = script_exec_cli_args();
        let _guard = EngineTestGuard::serve(missing, None);
        let _scope = begin_job("exec-skip");
        let mut runner = ScriptRunner::scripted(Vec::new());
        let route = {
            let docker = Docker::job(&mut runner);
            docker.try_exec_script(
                "velnor-job-1",
                &cli_args,
                &script_exec_config(),
                Duration::from_secs(60),
                &mut |_, _| {},
            )
        };
        assert_eq!(route, ScriptExecRoute::UseCli);
        let counts = snapshot();
        assert_eq!(counts.api_calls, 0);
        assert_eq!(counts.api_fallbacks, 0);
    }

    #[test]
    fn script_exec_skips_api_when_disabled() {
        // The `VELNOR_DOCKER_ENGINE_API=0` early return: a host runner, so
        // the disabled flag is the only skip reason — zero calls plus zero
        // fallbacks means the API was never attempted.
        let cli_args = script_exec_cli_args();
        let _guard = EngineTestGuard::disabled();
        let _scope = begin_job("exec-disabled");
        let mut runner = ScriptRunner::scripted_host(Vec::new());
        let route = {
            let docker = Docker::job(&mut runner);
            docker.try_exec_script(
                "velnor-job-1",
                &cli_args,
                &script_exec_config(),
                Duration::from_secs(60),
                &mut |_, _| {},
            )
        };
        assert_eq!(route, ScriptExecRoute::UseCli);
        let counts = snapshot();
        assert_eq!(counts.api_calls, 0);
        assert_eq!(counts.api_fallbacks, 0);
    }

    /// Live Engine exec bench: API vs CLI on a trivial step, asserting
    /// byte-identical stdout/stderr/exit code and printing both legs'
    /// latency. Ignored: it needs a live `/var/run/docker.sock` and pulls
    /// nothing (reuses `alpine:3.20` if present, else skips). Run it with:
    /// `cargo test -p velnor-runner --lib live_exec_latency_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_exec_latency_bench() {
        use std::os::unix::net::UnixStream;
        use std::process::Command;
        const ITERATIONS: usize = 10;
        const NAME: &str = "velnor-eng4-bench";
        let socket = super::super::engine::socket_path();
        if UnixStream::connect(&socket).is_err() {
            println!(
                "live_exec_latency_bench: SKIP, no live daemon at {}",
                socket.display()
            );
            return;
        }
        // Best-effort container lifecycle for the bench: stale names go
        // away before and after, even on assertion failure.
        struct BenchContainer;
        impl BenchContainer {
            fn remove() {
                let _ = Command::new("docker").args(["rm", "-f", NAME]).status();
            }
        }
        impl Drop for BenchContainer {
            fn drop(&mut self) {
                Self::remove();
            }
        }
        let _container = BenchContainer;
        BenchContainer::remove();
        let started = Command::new("docker")
            .args(["run", "-d", "--name", NAME, "alpine:3.20", "sleep", "300"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !started {
            println!("live_exec_latency_bench: SKIP, cannot start {NAME}");
            return;
        }
        let script = "echo out; echo err >&2; exit 3";
        let cli_args: Vec<String> = [
            "exec",
            "--workdir",
            "/tmp",
            "-e",
            "A=1",
            NAME,
            "sh",
            "-c",
            script,
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let config = ExecConfig {
            cmd: vec!["sh".into(), "-c".into(), script.into()],
            env: vec![("A".into(), "1".into())],
            workdir: "/tmp".into(),
        };
        fn summarize(name: &str, mut samples: Vec<Duration>) {
            samples.sort();
            let percentile = |p: usize| samples[(samples.len() * p / 100).min(samples.len() - 1)];
            println!(
                "live_exec_latency_bench: {name} n={} p50={:?} p95={:?} min={:?} max={:?}",
                samples.len(),
                percentile(50),
                percentile(95),
                samples[0],
                samples[samples.len() - 1],
            );
        }
        let api: Vec<(Duration, CommandResult)> = {
            let _guard = EngineTestGuard::serve(socket, None);
            (0..ITERATIONS)
                .map(|_| {
                    let started = Instant::now();
                    let mut live_lines = Vec::new();
                    let route = Docker::host().try_exec_script(
                        NAME,
                        &cli_args,
                        &config,
                        Duration::from_secs(60),
                        &mut |stream, line: &str| live_lines.push((stream, line.to_string())),
                    );
                    let ScriptExecRoute::Served(result) = route else {
                        panic!("live api must serve, got {route:?}");
                    };
                    // Live lines carry both streams; interleave is racy, so
                    // compare sorted by line text.
                    live_lines.sort_by(|a, b| a.1.cmp(&b.1));
                    assert_eq!(
                        live_lines,
                        vec![
                            (CommandStream::Stderr, "err".to_string()),
                            (CommandStream::Stdout, "out".to_string()),
                        ]
                    );
                    (started.elapsed(), result)
                })
                .collect()
        };
        let cli: Vec<(Duration, CommandResult)> = (0..ITERATIONS)
            .map(|_| {
                let started = Instant::now();
                let output = Command::new("docker")
                    .args(&cli_args)
                    .output()
                    .expect("live cli serves exec");
                let result = CommandResult {
                    code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                };
                (started.elapsed(), result)
            })
            .collect();
        for (iteration, ((_, api_result), (_, cli_result))) in
            api.iter().zip(cli.iter()).enumerate()
        {
            assert_eq!(
                api_result, cli_result,
                "live transports disagree on iteration {iteration}"
            );
        }
        summarize(
            "api ",
            api.into_iter().map(|(elapsed, _)| elapsed).collect(),
        );
        summarize(
            "cli ",
            cli.into_iter().map(|(elapsed, _)| elapsed).collect(),
        );
    }

    /// Live Engine expiry parity: a timed-out API step returns 124 while
    /// the job container keeps running — the CLI watchdog shape (client
    /// killed, container live). Ignored: it needs a live daemon (reuses
    /// `alpine:3.20` if present, else skips). Run it with:
    /// `cargo test -p velnor-runner --lib live_exec_expiry_leaves_container_running -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_exec_expiry_leaves_container_running() {
        use std::os::unix::net::UnixStream;
        use std::process::Command;
        const NAME: &str = "velnor-eng4-expiry";
        let socket = super::super::engine::socket_path();
        if UnixStream::connect(&socket).is_err() {
            println!(
                "live_exec_expiry_leaves_container_running: SKIP, no live daemon at {}",
                socket.display()
            );
            return;
        }
        struct ExpiryContainer;
        impl ExpiryContainer {
            fn remove() {
                let _ = Command::new("docker").args(["rm", "-f", NAME]).status();
            }
            fn running() -> bool {
                Command::new("docker")
                    .args(["inspect", "-f", "{{.State.Running}}", NAME])
                    .output()
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "true")
                    .unwrap_or(false)
            }
        }
        impl Drop for ExpiryContainer {
            fn drop(&mut self) {
                Self::remove();
            }
        }
        let _container = ExpiryContainer;
        ExpiryContainer::remove();
        let started = Command::new("docker")
            .args(["run", "-d", "--name", NAME, "alpine:3.20", "sleep", "300"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !started {
            println!("live_exec_expiry_leaves_container_running: SKIP, cannot start {NAME}");
            return;
        }
        // Real script-step argv shape: `--` before the container operand.
        let cli_args: Vec<String> = ["exec", "--workdir", "/tmp", "--", NAME, "sleep", "30"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let config = ExecConfig {
            cmd: vec!["sleep".into(), "30".into()],
            env: Vec::new(),
            workdir: "/tmp".into(),
        };
        let _guard = EngineTestGuard::serve(socket, None);
        let route = Docker::host().try_exec_script(
            NAME,
            &cli_args,
            &config,
            Duration::from_secs(2),
            &mut |_, _| {},
        );
        let ScriptExecRoute::Expired(result) = route else {
            panic!("live expiry must expire, got {route:?}");
        };
        assert_eq!(result.code, 124);
        assert!(
            ExpiryContainer::running(),
            "expiry must leave the job container running"
        );
    }
}
