//! Velnor's benchmark harness.
//!
//! This crate exists because the project had no benchmark of its product. The
//! script it replaces never invoked Velnor, Docker, or a job, so every
//! performance number the project could produce was a statement about `cargo`.
//!
//! Four rules shape everything here:
//!
//! 1. **Nothing is simulated.** A scenario either drives real work or is
//!    reported as unrun, with the missing requirement named.
//! 2. **Environment identity is mandatory.** See [`env`].
//! 3. **Internal and external latency are never mixed.** See [`stage`].
//! 4. **A statistic is only emitted when the sample supports it.** See
//!    [`stats`].

pub mod checkout_replay;
pub mod drivers;
pub mod env;
pub mod fact;
pub mod gittrace;
pub mod record;
pub mod runnertrace;
pub mod scenario;
pub mod stage;
pub mod stats;
pub mod sys;
