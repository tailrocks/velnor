//! Independently supervised OS processes: guardian, controller, slot, job.
//!
//! The daemon `JoinSet` is not the availability boundary. Each ready slot is
//! one child process. systemd watchdog pings happen only after a completed
//! local cycle. The transitional job executor remains host Docker and is not
//! the Build L3 isolation boundary.

pub mod canary;
pub mod cleanup;
pub mod complete;
pub mod controller;
pub mod exec;
pub mod guardian;
pub mod health;
pub mod job;
pub mod prove;
pub mod scheduler;
pub mod slot;
pub mod watchdog;

pub use canary::{run as run_canary, CanaryArgs, CanaryReport};
pub use controller::{run as run_controller, ControllerArgs};
pub use guardian::{run as run_guardian, GuardianArgs};
pub use job::{run as run_job, JobArgs};
pub use slot::{run as run_slot, SlotArgs};
