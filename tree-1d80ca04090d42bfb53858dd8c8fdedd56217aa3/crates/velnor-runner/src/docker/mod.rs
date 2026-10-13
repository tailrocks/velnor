//! Host Docker control-plane policy.
//!
//! Three concerns that used to be implicit, spread across call sites, or
//! simply absent:
//!
//! * [`deadline`] — every host `docker` invocation belongs to a named
//!   operation class, and every class has a deadline chosen for that class.
//!   Before this module a control-plane call inherited the 360-minute default
//!   step timeout, so a wedged `dockerd` parked a runner slot for six hours.
//! * [`facts`] — facts learned from the host and from the Engine have
//!   different lifetimes. A fact is cached only against the generation that
//!   can actually invalidate it; a fact with no invalidation signal is never
//!   cached.
//! * [`metrics`] — how many host `docker` processes a job spawns, and how long
//!   each class of call takes. This is the measurement the pending Engine-API
//!   client migration will be judged against, so it has to exist first.
//!
//! Nothing here talks to the Engine itself. The classification input is the
//! `docker` argument vector, which keeps the policy usable unchanged when the
//! CLI is replaced by an API client: the classes and their deadlines survive,
//! only the classifier's input changes.

pub mod deadline;
pub mod facts;
pub mod metrics;

pub use deadline::{classify, deadline_for, DockerOp, DockerTimeout};
pub use facts::{Fact, FactKey, FactLifetime};
pub use metrics::{begin_job, observe, JobDockerScope};
