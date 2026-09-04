//! Typed deadline policy for host `docker` invocations.
//!
//! Every invocation is classified into a [`DockerOp`], and every class carries
//! a deadline justified by what that class actually does. Only
//! [`DockerOp::Payload`] — the classes that run the job's own work inside the
//! process, `run`/`exec`/`build` — takes the caller's step deadline. No
//! control-plane call can inherit it, which is the whole point: a control-plane
//! call that has not answered within its class deadline is a wedged daemon, not
//! slow work, and waiting longer never turns it into a success.

use std::fmt;
use std::time::Duration;

/// Operation class of a host `docker` invocation.
///
/// The class is the unit the deadline policy and the per-job latency metrics
/// are expressed in. It is derived from the argument vector so that every call
/// site is covered without each one having to name its class by hand; the
/// classifier is the single place that has to change when the CLI is replaced
/// by an Engine API client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DockerOp {
    /// Daemon-wide state: `docker info`, `docker version`, `docker system df`.
    /// The Engine's heaviest read endpoints, but still memory-served.
    DaemonQuery,
    /// Read-only object queries served from Engine memory: `inspect`, `ps`,
    /// `port`, `images`, `network ls`.
    Query,
    /// Object creation that transfers no image data: `create`, `network
    /// create`, `volume create`, `tag`.
    Create,
    /// Detached container start.
    Start,
    /// Graceful stop and restart. Carries the Engine's own kill grace period.
    Stop,
    /// Immediate signal delivery: `docker kill`.
    Kill,
    /// Object deletion: `rm`, `rmi`, `volume rm`, `network rm`.
    Remove,
    /// Reclaim over an unbounded object set: any `prune`.
    Prune,
    /// Registry or archive transfer: `pull`, `push`, `save`, `load`, `login`.
    Transfer,
    /// Filesystem copy across the container boundary: `docker cp`.
    Copy,
    /// The job's own work runs inside this invocation: `run`, `exec`, `build`,
    /// `attach`, `wait`, `logs --follow`. The only class whose deadline is the
    /// caller's step deadline.
    Payload,
    /// A subcommand this policy does not know. Bounded anyway, and reported so
    /// the gap gets closed rather than silently inheriting a huge deadline.
    Unclassified,
}

/// A daemon that answers `docker info` in under a second on a healthy host;
/// 30s is two orders of magnitude of headroom and still 720x below the step
/// default it replaces.
const DAEMON_QUERY_DEADLINE: Duration = Duration::from_secs(30);
/// Object queries are answered from the Engine's in-memory state.
const QUERY_DEADLINE: Duration = Duration::from_secs(20);
/// Creation touches the storage driver and the network stack but moves no
/// image data.
const CREATE_DEADLINE: Duration = Duration::from_secs(60);
/// Container start sets up cgroups, mounts and networking; a cold overlay
/// mount of a large image is the slow case.
const START_DEADLINE: Duration = Duration::from_secs(120);
/// `docker stop` waits its own grace period before SIGKILL; an explicit
/// `--time` is added to this floor.
const STOP_DEADLINE: Duration = Duration::from_secs(120);
/// Beyond the grace period an explicit `--time` needs, plus Engine bookkeeping.
const STOP_GRACE_HEADROOM: Duration = Duration::from_secs(60);
/// Signal delivery is a single syscall behind one Engine round trip.
const KILL_DEADLINE: Duration = Duration::from_secs(20);
/// Deletion is bounded by unmount and layer unlink. Matches the bound job
/// teardown already relied on before this policy existed.
const REMOVE_DEADLINE: Duration = Duration::from_secs(20);
/// Prune walks every object of a kind and can legitimately unlink a very large
/// build cache.
const PRUNE_DEADLINE: Duration = Duration::from_secs(600);
/// Registry transfer is network- and registry-bound; a large multi-arch image
/// over a slow link is the honest worst case.
const TRANSFER_DEADLINE: Duration = Duration::from_secs(1800);
/// `docker cp` streams a tar of a workspace that can be gigabytes.
const COPY_DEADLINE: Duration = Duration::from_secs(900);
/// An unknown subcommand is bounded generously but finitely, and reported.
const UNCLASSIFIED_DEADLINE: Duration = Duration::from_secs(300);

impl DockerOp {
    /// Stable label for tracing fields and operator-facing messages.
    ///
    /// Deliberately a closed vocabulary: it never echoes any part of the
    /// argument vector, so it cannot carry an image reference, a registry URL
    /// or a secret into an unredacted log sink.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DaemonQuery => "daemon-query",
            Self::Query => "query",
            Self::Create => "create",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Kill => "kill",
            Self::Remove => "remove",
            Self::Prune => "prune",
            Self::Transfer => "transfer",
            Self::Copy => "copy",
            Self::Payload => "payload",
            Self::Unclassified => "unclassified",
        }
    }

    /// Every class this policy knows, in declaration order. Backs the per-class
    /// latency histogram in [`super::metrics`].
    pub const ALL: [Self; 12] = [
        Self::DaemonQuery,
        Self::Query,
        Self::Create,
        Self::Start,
        Self::Stop,
        Self::Kill,
        Self::Remove,
        Self::Prune,
        Self::Transfer,
        Self::Copy,
        Self::Payload,
        Self::Unclassified,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::DaemonQuery => 0,
            Self::Query => 1,
            Self::Create => 2,
            Self::Start => 3,
            Self::Stop => 4,
            Self::Kill => 5,
            Self::Remove => 6,
            Self::Prune => 7,
            Self::Transfer => 8,
            Self::Copy => 9,
            Self::Payload => 10,
            Self::Unclassified => 11,
        }
    }

    /// True when the invocation is Velnor's own control traffic rather than the
    /// job's work. Control-plane expiry is a daemon fault and is reported as
    /// one; payload expiry is the workflow's own `timeout-minutes`.
    #[must_use]
    pub const fn is_control_plane(self) -> bool {
        !matches!(self, Self::Payload)
    }

    /// What an operator should look at when this class times out.
    #[must_use]
    pub const fn diagnosis(self) -> &'static str {
        match self {
            Self::Payload => {
                "the container did not finish inside the step deadline; check the step's own timeout-minutes"
            }
            Self::Transfer => {
                "the registry or the local link is not making progress; check registry reachability and dockerd's pull logs"
            }
            Self::Prune | Self::Remove => {
                "an Engine delete is stuck, usually on a container lock or a busy mount; check `docker ps --all` and dockerd's container lock state"
            }
            _ => {
                "dockerd did not answer a control-plane request; check `systemctl status docker` and `journalctl -u docker --since -5m` for a wedged daemon"
            }
        }
    }
}

impl fmt::Display for DockerOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// A host `docker` invocation that did not answer inside its class deadline.
///
/// Carries only the operation class and the deadline. It never carries the
/// argument vector: these messages reach log sinks that perform no redaction,
/// and an argument vector can hold an image reference, a registry URL or a
/// credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DockerTimeout {
    pub op: DockerOp,
    pub deadline: Duration,
}

impl DockerTimeout {
    #[must_use]
    pub const fn new(op: DockerOp, deadline: Duration) -> Self {
        Self { op, deadline }
    }
}

impl fmt::Display for DockerTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "docker {} operation exceeded its {}s deadline: {}",
            self.op.label(),
            self.deadline.as_secs(),
            self.op.diagnosis()
        )
    }
}

impl std::error::Error for DockerTimeout {}

/// Split a `docker` argument vector into its subcommand and the arguments that
/// follow it, skipping the client's global options.
///
/// Global options end at the first non-option argument, exactly as the Docker
/// CLI parses them. A command passed to `docker run`, such as `sh -c ...`, is
/// opaque payload and must never be mistaken for a global option.
#[must_use]
pub fn subcommand(args: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        if argument == "--" {
            index += 1;
            break;
        }
        if !argument.starts_with('-') {
            break;
        }
        if global_option_takes_separate_value(argument) {
            index += 2;
        } else {
            index += 1;
        }
    }
    let subcommand = args.get(index)?;
    Some((subcommand.as_str(), &args[index + 1..]))
}

fn global_option_takes_separate_value(argument: &str) -> bool {
    matches!(
        argument,
        "--config"
            | "-l"
            | "--log-level"
            | "--tlscacert"
            | "--tlscert"
            | "--tlskey"
            | "-H"
            | "--host"
            | "-c"
            | "--context"
    )
}

/// Classify a host `docker` argument vector into its operation class.
#[must_use]
pub fn classify(args: &[String]) -> DockerOp {
    let Some((command, rest)) = subcommand(args) else {
        return DockerOp::Unclassified;
    };
    let nested = rest
        .iter()
        .find(|argument| !argument.starts_with('-'))
        .map(String::as_str);
    classify_pair(command, nested, rest)
}

fn classify_pair(command: &str, nested: Option<&str>, rest: &[String]) -> DockerOp {
    // Management commands delegate to the same verbs as their top-level
    // shorthands, so classify the verb once and reuse it.
    let management = matches!(
        command,
        "container"
            | "image"
            | "network"
            | "volume"
            | "system"
            | "builder"
            | "buildx"
            | "context"
            | "node"
            | "secret"
            | "config"
            | "manifest"
            | "plugin"
            | "trust"
            | "swarm"
            | "stack"
            | "compose"
    );
    if management {
        return classify_management(command, nested, rest);
    }
    classify_verb(command, rest)
}

fn classify_management(command: &str, nested: Option<&str>, rest: &[String]) -> DockerOp {
    // `docker compose` runs the job's own containers, and `docker buildx
    // build`/`bake` runs the job's own build.
    if command == "compose" {
        return DockerOp::Payload;
    }
    let Some(verb) = nested else {
        // Bare management command prints help; treat as a query.
        return DockerOp::Query;
    };
    match verb {
        "build" | "bake" | "run" | "exec" => DockerOp::Payload,
        "df" | "info" | "events" => DockerOp::DaemonQuery,
        _ => classify_verb(verb, rest),
    }
}

fn classify_verb(verb: &str, rest: &[String]) -> DockerOp {
    if verb.ends_with("prune") {
        return DockerOp::Prune;
    }
    match verb {
        "info" | "version" | "df" | "events" => DockerOp::DaemonQuery,
        "ps" | "ls" | "list" | "inspect" | "port" | "images" | "top" | "stats" | "diff"
        | "history" | "search" | "use" | "show" => DockerOp::Query,
        "logs" => {
            if rest
                .iter()
                .any(|argument| argument == "-f" || argument == "--follow")
            {
                DockerOp::Payload
            } else {
                DockerOp::Query
            }
        }
        "create" | "connect" | "disconnect" | "tag" | "commit" | "rename" | "update"
        | "annotate" => DockerOp::Create,
        "start" => {
            // An attached start runs the container's own process here.
            if rest.iter().any(|argument| {
                argument == "-a"
                    || argument == "--attach"
                    || argument == "-i"
                    || argument == "--interactive"
            }) {
                DockerOp::Payload
            } else {
                DockerOp::Start
            }
        }
        "stop" | "restart" | "pause" | "unpause" => DockerOp::Stop,
        "kill" => DockerOp::Kill,
        "rm" | "rmi" | "remove" | "uninstall" | "disable" | "leave" | "down" => DockerOp::Remove,
        "pull" | "push" | "save" | "load" | "import" | "export" | "login" | "logout"
        | "install" | "fetch" => DockerOp::Transfer,
        "cp" => DockerOp::Copy,
        "run" | "exec" | "attach" | "wait" | "build" | "bake" => DockerOp::Payload,
        _ => DockerOp::Unclassified,
    }
}

/// Deadline for a `docker` invocation, with the class it was derived from.
///
/// `payload_deadline` is used by, and only by, [`DockerOp::Payload`]. Every
/// control-plane class ignores it, which is what stops the 360-minute step
/// default from reaching a control-plane call.
#[must_use]
pub fn deadline_for(args: &[String], payload_deadline: Duration) -> (DockerOp, Duration) {
    let op = classify(args);
    let deadline = match op {
        DockerOp::DaemonQuery => DAEMON_QUERY_DEADLINE,
        DockerOp::Query => QUERY_DEADLINE,
        DockerOp::Create => CREATE_DEADLINE,
        DockerOp::Start => START_DEADLINE,
        DockerOp::Stop => stop_deadline(args),
        DockerOp::Kill => KILL_DEADLINE,
        DockerOp::Remove => REMOVE_DEADLINE,
        DockerOp::Prune => PRUNE_DEADLINE,
        DockerOp::Transfer => TRANSFER_DEADLINE,
        DockerOp::Copy => COPY_DEADLINE,
        DockerOp::Payload => payload_deadline,
        DockerOp::Unclassified => UNCLASSIFIED_DEADLINE,
    };
    (op, deadline)
}

/// `docker stop -t N` promises to wait `N` seconds before SIGKILL, so the
/// deadline has to clear the grace period the caller asked for.
fn stop_deadline(args: &[String]) -> Duration {
    let Some((_, rest)) = subcommand(args) else {
        return STOP_DEADLINE;
    };
    let mut grace = None;
    let mut index = 0;
    while let Some(argument) = rest.get(index) {
        if argument == "-t" || argument == "--time" || argument == "--timeout" {
            grace = rest
                .get(index + 1)
                .and_then(|value| value.parse::<u64>().ok());
            index += 2;
            continue;
        }
        if let Some(value) = argument
            .strip_prefix("--time=")
            .or_else(|| argument.strip_prefix("--timeout="))
            .or_else(|| argument.strip_prefix("-t="))
        {
            grace = value.parse::<u64>().ok();
        }
        index += 1;
    }
    match grace {
        Some(seconds) => STOP_DEADLINE.max(Duration::from_secs(seconds) + STOP_GRACE_HEADROOM),
        None => STOP_DEADLINE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    const SIX_HOURS: Duration = Duration::from_secs(6 * 3600);

    #[test]
    fn no_control_plane_class_inherits_the_step_deadline() {
        for command in [
            vec!["info", "--format", "{{.CgroupDriver}}"],
            vec!["ps", "--all"],
            vec!["inspect", "--format={{.Id}}", "name"],
            vec!["rm", "--force", "id"],
            vec!["volume", "rm", "--force", "v"],
            vec!["network", "rm", "n"],
            vec!["kill", "name"],
            vec!["stop", "name"],
            vec!["create", "--name", "n", "image"],
            vec!["start", "name"],
            vec!["pull", "image"],
            vec!["login", "--username", "u", "--password-stdin", "registry"],
            vec!["cp", "src", "container:/dst"],
            vec!["system", "prune", "--force"],
            vec!["frobnicate"],
        ] {
            let (op, deadline) = deadline_for(&args(&command), SIX_HOURS);
            assert!(
                op.is_control_plane(),
                "{command:?} must classify as control plane, got {op}"
            );
            assert!(
                deadline < SIX_HOURS,
                "{command:?} ({op}) inherited the step deadline"
            );
        }
    }

    #[test]
    fn payload_classes_take_the_step_deadline() {
        for command in [
            vec!["run", "--rm", "image", "sh", "-c", "true"],
            vec!["exec", "container", "sh", "-c", "true"],
            vec!["buildx", "build", "."],
            vec!["build", "."],
            vec!["attach", "container"],
            vec!["wait", "container"],
            vec!["logs", "--follow", "container"],
            vec!["start", "--attach", "container"],
            vec!["compose", "up"],
        ] {
            let (op, deadline) = deadline_for(&args(&command), SIX_HOURS);
            assert_eq!(op, DockerOp::Payload, "{command:?}");
            assert_eq!(deadline, SIX_HOURS, "{command:?}");
        }
    }

    #[test]
    fn removal_keeps_the_twenty_second_teardown_bound() {
        for command in [
            vec!["rm", "--force", "id"],
            vec!["volume", "rm", "--force", "v"],
            vec!["network", "rm", "n"],
            vec!["image", "rm", "i"],
            vec!["rmi", "i"],
        ] {
            let (op, deadline) = deadline_for(&args(&command), SIX_HOURS);
            assert_eq!(op, DockerOp::Remove, "{command:?}");
            assert_eq!(deadline, Duration::from_secs(20), "{command:?}");
        }
    }

    #[test]
    fn global_options_do_not_hide_the_subcommand() {
        let vector = args(&["--config", "/tmp/client", "-l", "info", "ps", "--all"]);
        assert_eq!(classify(&vector), DockerOp::Query);
        let payload = args(&["--config", "/tmp/client", "run", "--rm", "img", "-l", "ps"]);
        assert_eq!(classify(&payload), DockerOp::Payload);
    }

    #[test]
    fn payload_arguments_are_not_read_as_global_options() {
        // `-c` after `run` is the shell's flag, not the client's context flag.
        let vector = args(&["run", "--rm", "image", "sh", "-c", "docker ps"]);
        assert_eq!(classify(&vector), DockerOp::Payload);
    }

    #[test]
    fn stop_deadline_clears_an_explicit_grace_period() {
        assert_eq!(
            deadline_for(&args(&["stop", "name"]), SIX_HOURS).1,
            Duration::from_secs(120)
        );
        assert_eq!(
            deadline_for(&args(&["stop", "-t", "300", "name"]), SIX_HOURS).1,
            Duration::from_secs(360)
        );
        assert_eq!(
            deadline_for(&args(&["stop", "--time=30", "name"]), SIX_HOURS).1,
            Duration::from_secs(120)
        );
    }

    #[test]
    fn prune_is_bounded_but_generous() {
        for command in [
            vec!["system", "prune", "--force"],
            vec!["builder", "prune", "--force"],
            vec!["volume", "prune"],
        ] {
            let (op, deadline) = deadline_for(&args(&command), SIX_HOURS);
            assert_eq!(op, DockerOp::Prune, "{command:?}");
            assert_eq!(deadline, Duration::from_secs(600), "{command:?}");
        }
    }

    #[test]
    fn timeout_message_names_the_class_and_the_operator_action() {
        let message = DockerTimeout::new(DockerOp::Remove, Duration::from_secs(20)).to_string();
        assert!(message.contains("remove"), "{message}");
        assert!(message.contains("20s"), "{message}");
        assert!(message.contains("Engine delete is stuck"), "{message}");
    }

    #[test]
    fn class_indexes_are_unique_and_dense() {
        for (position, op) in DockerOp::ALL.iter().enumerate() {
            assert_eq!(op.index(), position, "{op}");
        }
    }
}
