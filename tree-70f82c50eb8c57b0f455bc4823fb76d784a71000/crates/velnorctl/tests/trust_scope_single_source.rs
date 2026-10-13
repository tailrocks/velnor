//! The two shipped binaries must never disagree about the trust boundary.
//!
//! `--trust-scope` used to be declared twice: `velnor-runner` declared it with
//! `default_value = "trusted"` and an `env = "VELNOR_TRUST_SCOPE"` binding,
//! `velnorctl` declared it with `default_value = "public"` and no binding at
//! all. Two shipped binaries therefore disagreed about a security gate, and
//! `velnorctl` could not even observe the variable the packaged systemd unit
//! sets. Both now flatten the single declaration in
//! `velnor_runner::trust_scope`; this test fails if anyone restates the flag
//! and lets the default or the environment binding drift apart again.

use clap::CommandFactory;

#[derive(Debug, clap::Parser)]
struct ServiceProbe {
    #[command(flatten)]
    args: velnor_runner::service::DaemonArgs,
}

#[derive(Debug, clap::Parser)]
struct ControlProbe {
    #[command(flatten)]
    args: velnorctl::runtime::DaemonArgs,
}

#[derive(Debug, PartialEq, Eq)]
struct FlagShape {
    long: Option<String>,
    defaults: Vec<String>,
    env: Option<String>,
}

fn trust_scope_shape(command: clap::Command) -> FlagShape {
    let arg = command
        .get_arguments()
        .find(|arg| arg.get_id() == "trust_scope")
        .unwrap_or_else(|| panic!("no --trust-scope argument"))
        .clone();
    FlagShape {
        long: arg.get_long().map(ToOwned::to_owned),
        defaults: arg
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect(),
        env: arg
            .get_env()
            .map(|value| value.to_string_lossy().into_owned()),
    }
}

#[test]
fn both_binaries_declare_the_same_trust_scope_flag() {
    let service = trust_scope_shape(ServiceProbe::command());
    let control = trust_scope_shape(ControlProbe::command());
    assert_eq!(
        service, control,
        "velnor-runner and velnorctl disagree about --trust-scope; \
         both must flatten velnor_runner::trust_scope::TrustScopeArg"
    );
}

#[test]
fn the_shared_trust_scope_flag_fails_closed_and_reads_the_unit_variable() {
    let shape = trust_scope_shape(ServiceProbe::command());
    assert_eq!(shape.long.as_deref(), Some("trust-scope"));
    // Trust must be granted deliberately, never inherited from a default.
    assert_eq!(
        shape.defaults,
        vec![velnor_runner::trust_scope::FAIL_CLOSED.to_owned()]
    );
    // The packaged systemd unit configures the pool through this variable, so
    // both binaries have to observe it.
    assert_eq!(shape.env.as_deref(), Some("VELNOR_TRUST_SCOPE"));
}
