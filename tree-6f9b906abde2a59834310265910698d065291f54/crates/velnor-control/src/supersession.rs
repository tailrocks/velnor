//! Controller-facing export of the journal supersession state machine.
//!
//! The durable journal remains the authority; this module gives the control
//! service a stable boundary without duplicating consumer or retention logic.

pub use velnor_action_journal::supersession::*;

/// Stable controller alias for the journal-backed coordinator.
pub type Controller<C> = SupersessionCoordinator<C>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_exports_bounded_default() {
        assert_eq!(
            SupersessionConfig::default().retention_ms,
            DEFAULT_RETENTION_MS
        );
    }
}
