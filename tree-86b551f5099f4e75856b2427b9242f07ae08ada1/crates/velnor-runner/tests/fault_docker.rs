//! Docker-engine faults: a daemon that went away, a daemon that came back as a
//! new process, and a call that never returns.
//!
//! None of these need a real daemon, and that is deliberate. A fault test that
//! needs `dockerd` running is a fault test that gets `#[ignore]`d, and an
//! ignored test proves nothing. The three seams used here are the ones that
//! decide the behavior under a broken engine: the per-operation deadline
//! policy, the fact cache's invalidation rule, and the per-job accounting an
//! operator reads afterwards.
//!
//! The accounting counters are process-global, so this suite is written to be
//! run with `--test-threads=1` like the rest of the fault suites; each test
//! that reads them opens its own job scope, which resets them.

mod fault_support;

use std::time::Duration;

use velnor_runner::docker::{
    begin_job, classify, deadline_for, observe, DockerOp, DockerTimeout, Fact, FactLifetime,
};

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

/// A step deadline far larger than any control-plane bound, so a control-plane
/// call that wrongly inherited it would be obvious.
const STEP_DEADLINE: Duration = Duration::from_secs(360 * 60);

#[test]
fn every_docker_operation_class_is_bounded_well_below_the_step_deadline() {
    // The fault this pins is "a Docker call that hangs": a wedged daemon must
    // cost one bounded operation, not the job's entire 6-hour step budget.
    for op in DockerOp::ALL {
        if op == DockerOp::Payload {
            continue;
        }
        let sample = match op {
            DockerOp::DaemonQuery => argv(&["info"]),
            DockerOp::Query => argv(&["inspect", "velnor-job-1"]),
            DockerOp::Create => argv(&["create", "--name", "velnor-job-1", "image"]),
            DockerOp::Start => argv(&["start", "velnor-job-1"]),
            DockerOp::Stop => argv(&["stop", "velnor-job-1"]),
            DockerOp::Kill => argv(&["kill", "velnor-job-1"]),
            DockerOp::Remove => argv(&["rm", "-f", "velnor-job-1"]),
            DockerOp::Prune => argv(&["system", "prune", "-f"]),
            DockerOp::Transfer => argv(&["pull", "velnor/job-ubuntu:26.04"]),
            DockerOp::Copy => argv(&["cp", "velnor-job-1:/out", "/tmp/out"]),
            DockerOp::Unclassified => argv(&["definitely-not-a-subcommand"]),
            DockerOp::Payload => unreachable!("payload is skipped above"),
        };
        let (classified, deadline) = deadline_for(&sample, STEP_DEADLINE);
        assert_eq!(
            classified,
            op,
            "argv {sample:?} did not classify as {}",
            op.label()
        );
        assert!(
            deadline > Duration::ZERO,
            "{} has no deadline at all",
            op.label()
        );
        assert!(
            deadline < STEP_DEADLINE,
            "{} inherited the step deadline ({deadline:?}); a hung daemon would hold the slot for hours",
            op.label()
        );
    }
}

#[test]
fn an_unknown_docker_subcommand_is_still_bounded_rather_than_unbounded_by_default() {
    // The dangerous default is "unknown means unlimited". A subcommand this
    // policy has never seen must still be killable, and must be visible as a
    // gap rather than silently absorbed.
    let (op, deadline) = deadline_for(&argv(&["quantum-entangle", "velnor-job-1"]), STEP_DEADLINE);
    assert_eq!(op, DockerOp::Unclassified);
    assert!(deadline > Duration::ZERO && deadline < STEP_DEADLINE);
    fault_support::assert_actionable_diagnostic(op.diagnosis(), &["docker"]);
}

#[test]
fn a_graceful_stop_deadline_clears_the_grace_period_the_caller_asked_for() {
    // A `docker stop -t 300` that is killed at 120s leaves a container the job
    // believes it stopped: an orphan, created by the timeout policy itself.
    let (op, deadline) = deadline_for(&argv(&["stop", "-t", "300", "velnor-job-1"]), STEP_DEADLINE);
    assert_eq!(op, DockerOp::Stop);
    assert!(
        deadline > Duration::from_secs(300),
        "the stop deadline must clear its own grace period, got {deadline:?}"
    );
    assert!(deadline < STEP_DEADLINE);
}

#[test]
fn the_jobs_payload_call_is_the_only_class_that_inherits_the_step_deadline() {
    let (op, deadline) = deadline_for(
        &argv(&["exec", "velnor-job-1", "sh", "-c", "make all"]),
        STEP_DEADLINE,
    );
    assert_eq!(op, DockerOp::Payload);
    assert_eq!(deadline, STEP_DEADLINE);

    // A command word that looks like a global option must not be re-parsed as
    // one: `docker run image -H something` is payload, not a daemon override.
    assert_eq!(
        classify(&argv(&["run", "image", "-H", "tcp://elsewhere"])),
        DockerOp::Payload
    );
}

#[test]
fn a_timed_out_docker_call_reports_a_diagnosis_an_operator_can_act_on() {
    let timeout = DockerTimeout::new(DockerOp::DaemonQuery, Duration::from_secs(30));
    let rendered = timeout.to_string();
    fault_support::assert_actionable_diagnostic(&rendered, &["docker", "30s", "deadline"]);
    // The class has to be in the message: "docker timed out" sends an operator
    // to the wrong subsystem, "the daemon did not answer" does not.
    assert!(
        rendered.contains(DockerOp::DaemonQuery.label()),
        "the timeout must name the operation class: {rendered}"
    );
}

#[test]
fn a_hung_then_failing_daemon_is_visible_in_the_jobs_docker_accounting() {
    let scope = begin_job("job-docker-fault");
    assert_eq!(scope.invocations(), 0);

    observe(DockerOp::DaemonQuery, Duration::from_secs(30), 124, true);
    observe(DockerOp::Create, Duration::from_millis(12), 1, false);
    observe(DockerOp::Remove, Duration::from_millis(8), 0, false);

    assert_eq!(scope.invocations(), 3);
    let counts = scope.per_class_counts();
    for op in [DockerOp::DaemonQuery, DockerOp::Create, DockerOp::Remove] {
        assert!(
            counts.contains(op.label()),
            "the {} class is missing from {counts}",
            op.label()
        );
    }
    // Latency is reported per class, which is what makes "the daemon got slow"
    // distinguishable from "the job got slow".
    assert!(scope.per_class_latency_ms().contains("daemon-query"));
}

#[test]
fn a_daemon_whose_restarts_cannot_be_observed_never_serves_a_cached_fact() {
    // Docker daemon disconnect, in the only form that matters for correctness:
    // the runner cannot see the daemon's process identity, so it cannot know
    // whether the daemon it is talking to is the one it learned the fact from.
    // Caching without an invalidation signal is how a fact becomes quietly
    // wrong, so the rule is that it is not cached at all.
    static CGROUP_DRIVER: Fact<String> = Fact::new("cgroup-driver", FactLifetime::Daemon);

    let mut computed = 0_usize;
    for expected in 1..=4 {
        let value = CGROUP_DRIVER
            .get_or_try_init::<std::convert::Infallible>(None, || {
                computed += 1;
                Ok("systemd 2".to_string())
            })
            .expect("the computation cannot fail");
        assert_eq!(value, "systemd 2");
        assert_eq!(
            computed, expected,
            "an unobservable daemon generation must force a recomputation every time"
        );
    }
}

#[test]
fn invalidating_a_fact_forces_the_next_reader_to_relearn_it() {
    // What a daemon restart must do to every fact learned from the old daemon.
    // The generation key itself is not constructible from an integration test
    // (see the report accompanying this suite), so the invalidation edge is
    // driven directly.
    static DAEMON_VERSION: Fact<u32> = Fact::new("daemon-version", FactLifetime::Daemon);

    let mut generation = 1_u32;
    let first = DAEMON_VERSION
        .get_or_try_init::<std::convert::Infallible>(None, || Ok(generation))
        .expect("learn the first generation");
    assert_eq!(first, 1);

    DAEMON_VERSION.invalidate();
    generation = 2;
    let second = DAEMON_VERSION
        .get_or_try_init::<std::convert::Infallible>(None, || Ok(generation))
        .expect("relearn after invalidation");
    assert_eq!(
        second, 2,
        "a fact learned from a daemon that has since restarted must not survive"
    );
}

#[test]
fn a_fact_whose_computation_fails_is_not_cached_as_a_failure() {
    // A daemon that is down when the fact is first needed must not poison the
    // cache: the next caller, after the daemon comes back, has to be able to
    // learn the true value.
    static PROBE: Fact<String> = Fact::new("probe", FactLifetime::Daemon);

    let failed = PROBE.get_or_try_init::<String>(None, || Err("daemon is down".to_string()));
    assert_eq!(failed, Err("daemon is down".to_string()));

    let recovered = PROBE
        .get_or_try_init::<String>(None, || Ok("systemd 2".to_string()))
        .expect("the daemon came back");
    assert_eq!(recovered, "systemd 2");
}
