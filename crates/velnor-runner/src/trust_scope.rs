//! The pool trust boundary: one flag declaration, one resolution, one value.
//!
//! Velnor used to learn its trust boundary from two unrelated places. The
//! capability gates (`--trust-scope` on the daemon command line) were passed a
//! value threaded down from clap, while the persistent store paths called a
//! helper that read `VELNOR_TRUST_SCOPE` out of the process environment on its
//! own. The shipped systemd environment file set that variable to `trusted`,
//! so `velnor-runner daemon --trust-scope public` produced a split brain: the
//! host Docker socket was correctly withheld, and the cargo, mise, sccache and
//! actions stores were still resolved into the *trusted* namespace. A fork
//! pull request then wrote into `$CARGO_HOME/bin` and the mise install
//! directories, which the next job mounts read-write onto its `PATH`. The
//! documented hardening step handed an attacker the poisoning it was supposed
//! to prevent.
//!
//! On top of that, the same flag was declared twice — once in
//! `velnor-runner`'s service entrypoint with `default_value = "trusted"` and an
//! `env` binding, once in `velnorctl` with `default_value = "public"` and no
//! `env` binding — so the two shipped binaries disagreed about a security gate.
//!
//! This module removes both enabling conditions structurally:
//!
//! * [`TrustScopeArg`] is the only declaration of `--trust-scope`. Both
//!   binaries `#[command(flatten)]` it, so there is nothing left to diverge.
//! * [`resolve`] is the only way to obtain a [`TrustScope`], and it is the only
//!   writer of the process-wide cell. Nothing anywhere reads
//!   `VELNOR_TRUST_SCOPE` any more; clap owns that variable, resolves it
//!   against the command line with the command line winning, and hands the one
//!   answer to everybody.
//! * An unresolved process reports [`FAIL_CLOSED`]. Trust never appears out of
//!   nowhere.

use clap::Args;

/// Boundary reported by a process that has not resolved one. Trust fails
/// closed: no Docker socket, no privileged container options, no privileged
/// service containers, no host port publishing, no user secrets, and an
/// untrusted store namespace.
pub const FAIL_CLOSED: &str = "untrusted";

/// The one boundary value that unlocks host-level capability. Every gate
/// compares against exactly this, case-insensitively.
pub const TRUSTED: &str = "trusted";

const HELP: &str = "Trust boundary for this daemon/pool. \"trusted\" keeps full capabilities; \
     any other value disables shared Docker socket access, privileged container options, \
     privileged service containers, host port publishing, and user secrets. \
     Defaults to \"untrusted\": trust must be granted deliberately.";

/// The single clap declaration of `--trust-scope`.
///
/// Both shipped binaries flatten this struct rather than restating the flag, so
/// the default value and the `VELNOR_TRUST_SCOPE` binding cannot drift apart
/// again. `crates/velnorctl/tests/trust_scope_single_source.rs` proves the two
/// rendered commands still agree.
#[derive(Debug, Clone, Args)]
pub struct TrustScopeArg {
    #[arg(
        long = "trust-scope",
        env = "VELNOR_TRUST_SCOPE",
        default_value = FAIL_CLOSED,
        help = HELP,
        long_help = HELP
    )]
    pub trust_scope: String,
}

impl TrustScopeArg {
    /// Resolve this pool's trust boundary and publish it as the process-wide
    /// value. See [`resolve`].
    #[must_use]
    pub fn resolve(&self) -> TrustScope {
        resolve(&self.trust_scope)
    }
}

/// A trust boundary that came from the one resolution point.
///
/// The inner string is private and there is no public constructor other than
/// [`resolve`], so a `TrustScope` in hand is proof that the process-wide cell
/// holds the same answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustScope(String);

impl TrustScope {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    fn normalize(raw: &str) -> Self {
        let trimmed = raw.trim();
        Self(
            if trimmed.is_empty() {
                FAIL_CLOSED
            } else {
                trimmed
            }
            .to_owned(),
        )
    }
}

/// The one cell that holds the resolved boundary.
///
/// In a shipped binary it is a process-wide write-once cell. Under `cfg(test)`
/// it is per test thread, because `cargo test` runs every test on its own
/// thread in one shared process: a per-thread cell gives each test the
/// semantics of a fresh process instead of letting one test's resolution leak
/// into another's store paths.
#[cfg(not(test))]
mod cell {
    use super::TrustScope;
    use std::sync::RwLock;

    static RESOLVED: RwLock<Option<TrustScope>> = RwLock::new(None);

    pub(super) fn get() -> Option<TrustScope> {
        RESOLVED
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(super) fn set_once(scope: TrustScope) -> TrustScope {
        let mut resolved = RESOLVED.write().unwrap_or_else(|error| error.into_inner());
        match resolved.as_ref() {
            Some(existing) => existing.clone(),
            None => {
                *resolved = Some(scope.clone());
                scope
            }
        }
    }
}

#[cfg(test)]
mod cell {
    use super::TrustScope;
    use std::cell::RefCell;

    thread_local! {
        static RESOLVED: RefCell<Option<TrustScope>> = const { RefCell::new(None) };
    }

    pub(super) fn get() -> Option<TrustScope> {
        RESOLVED.with_borrow(Clone::clone)
    }

    pub(super) fn set_once(scope: TrustScope) -> TrustScope {
        RESOLVED.with_borrow_mut(|resolved| match resolved.as_ref() {
            Some(existing) => existing.clone(),
            None => {
                *resolved = Some(scope.clone());
                scope
            }
        })
    }

    pub(super) fn clear() {
        RESOLVED.with_borrow_mut(|resolved| *resolved = None);
    }
}

/// Resolve the pool trust boundary from the value clap produced, and publish it
/// as the process-wide answer.
///
/// The first resolution in a process wins for the life of that process; a later
/// call with a different value is refused rather than honoured, so no code path
/// can move the trust boundary after startup. Empty and whitespace-only input
/// resolves to [`FAIL_CLOSED`].
#[must_use]
pub fn resolve(raw: &str) -> TrustScope {
    cell::set_once(TrustScope::normalize(raw))
}

/// The boundary this process resolved, or [`FAIL_CLOSED`] if it resolved none.
///
/// This is what every trust-scoped store path consults. It can only ever return
/// the value [`resolve`] published, which is the same value threaded into the
/// capability gates.
#[must_use]
pub fn current() -> String {
    installed().unwrap_or_else(|| FAIL_CLOSED.to_owned())
}

/// The boundary this process is actually running with, falling back to a
/// configured value carried in a job/exec record when the process itself never
/// resolved one (node proof probes run in such processes).
#[must_use]
pub fn observed(configured: &str) -> String {
    installed().unwrap_or_else(|| TrustScope::normalize(configured).into_string())
}

fn installed() -> Option<String> {
    cell::get().map(TrustScope::into_string)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Mutex, MutexGuard};

    /// Serializes the tests that mutate the process environment while proving
    /// clap's resolution order. The resolved boundary itself is per thread, so
    /// this guard is only about `std::env`.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Minimal binary whose only flag is the shared declaration, so tests can
    /// exercise the real clap resolution order (command line over `env` over
    /// default) without standing up a whole daemon command tree.
    #[derive(Debug, clap::Parser)]
    #[command(name = "trust-scope-probe")]
    struct Probe {
        #[command(flatten)]
        trust: super::TrustScopeArg,
    }

    pub(crate) fn probe_command() -> clap::Command {
        <Probe as clap::CommandFactory>::command()
    }

    pub(crate) fn parse(argv: &[&str]) -> super::TrustScopeArg {
        use clap::Parser;
        Probe::try_parse_from(std::iter::once("trust-scope-probe").chain(argv.iter().copied()))
            .expect("the shared --trust-scope flag parses")
            .trust
    }

    /// Holds this test thread's trust scope for the body of one test and clears
    /// it on drop.
    pub(crate) struct InstalledScope {
        _serial: MutexGuard<'static, ()>,
    }

    impl Drop for InstalledScope {
        fn drop(&mut self) {
            super::cell::clear();
        }
    }

    /// Take this test thread's trust scope, starting unresolved.
    pub(crate) fn serialized() -> InstalledScope {
        let serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        super::cell::clear();
        InstalledScope { _serial: serial }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_blank_input_fails_closed() {
        assert_eq!(TrustScope::normalize("").as_str(), FAIL_CLOSED);
        assert_eq!(TrustScope::normalize("   ").as_str(), FAIL_CLOSED);
        assert_eq!(TrustScope::normalize(" public ").as_str(), "public");
    }

    #[test]
    fn unresolved_process_reports_fail_closed() {
        let _guard = test_support::serialized();
        assert_eq!(current(), FAIL_CLOSED);
        assert_eq!(observed("trusted"), "trusted");
    }

    #[test]
    fn the_first_resolution_owns_the_process() {
        let _guard = test_support::serialized();
        assert_eq!(resolve("public").as_str(), "public");
        // A second resolution cannot move the boundary, so no later code path
        // can widen trust after startup.
        assert_eq!(resolve("trusted").as_str(), "public");
        assert_eq!(current(), "public");
        assert_eq!(observed("trusted"), "public");
    }

    #[test]
    fn the_flag_defaults_to_fail_closed() {
        let default = test_support::probe_command()
            .get_arguments()
            .find(|arg| arg.get_id() == "trust_scope")
            .expect("the flag exists")
            .get_default_values()
            .to_vec();
        assert_eq!(default, vec![FAIL_CLOSED]);
    }
}
