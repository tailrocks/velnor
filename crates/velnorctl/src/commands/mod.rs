//! Leaf command modules.
//!
//! Plan 064 registers zero commands: every leaf (including `version`, owned by
//! command task C001) is added here in its own later task and registered into
//! the shared [`crate::cli::CliRegistry`].

use std::sync::OnceLock;

use crate::cli::CliRegistry;

/// The process-wide registry. Zero commands are registered until their own
/// command tasks land; see `crates/velnorctl/src/cli.rs` for the seam.
pub fn registry() -> &'static CliRegistry {
    static REGISTRY: OnceLock<CliRegistry> = OnceLock::new();
    REGISTRY.get_or_init(CliRegistry::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_shared_across_calls() {
        assert!(std::ptr::eq(registry(), registry()));
    }

    #[test]
    fn zero_commands_are_registered() {
        assert!(registry().is_empty());
        assert_eq!(registry().len(), 0);
        for name in [
            "version",
            "cache",
            "capabilities",
            "configure",
            "remove",
            "status",
            "run",
        ] {
            assert!(
                !registry().contains(name),
                "{name} must stay unregistered until its own command task"
            );
        }
    }
}
