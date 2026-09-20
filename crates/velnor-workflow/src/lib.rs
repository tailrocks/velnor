//! Schema-2 workflow generator and CI execution client.
//!
//! Repository discovery, config validation, rendering, and policy checks all
//! use the single typed provider pipeline in [`s2`].

#[cfg(not(unix))]
compile_error!(
    "velnor-workflow requires Unix descriptor-relative filesystem APIs; its workflow targets are Linux and macOS"
);

pub(crate) mod s2;

pub use s2::GeneratorError;
pub use s2::{SOURCE_CLOSURE, SOURCE_REVISION};

/// Run the generator or one of its runtime commands from the process
/// argument vector.
///
/// # Errors
/// Returns argument, repository, policy, or write failures.
pub fn run_from_env() -> Result<(), GeneratorError> {
    s2::dispatch::run_from_env()
}

/// Uniqueness must never depend on clock resolution: parallel tests that
/// start in the same instant must not collide on one temporary root.
static UNIQUE_SUFFIX_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn unique_suffix_for(nanos: u128, sequence: u64) -> u128 {
    (nanos << 64) | (u128::from(std::process::id()) << 32) | u128::from(sequence)
}

pub(crate) fn unique_suffix() -> u128 {
    use std::sync::atomic::Ordering;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    unique_suffix_for(
        nanos,
        UNIQUE_SUFFIX_SEQUENCE.fetch_add(1, Ordering::Relaxed),
    )
}

#[cfg(test)]
pub(crate) fn unique_suffix_at(nanos: u128) -> u128 {
    use std::sync::atomic::Ordering;
    unique_suffix_for(
        nanos,
        UNIQUE_SUFFIX_SEQUENCE.fetch_add(1, Ordering::Relaxed),
    )
}

#[cfg(test)]
pub(crate) fn unique_suffix_sequence() -> &'static std::sync::atomic::AtomicU64 {
    &UNIQUE_SUFFIX_SEQUENCE
}
