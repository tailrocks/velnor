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
//! [`client::Docker`] is the typed owner of the calls themselves: every query
//! returns a typed value, and control-plane methods take no timeout because
//! both transports apply [`deadline_for`]. The classification input is the
//! `docker` argument vector, which keeps the policy usable unchanged now
//! that an API client exists: the classes and their deadlines survive, only
//! the transport changes.
//!
//! `engine` is the Engine API fast path over the daemon Unix socket:
//! read-only queries plus the idempotent container-lifecycle mutations.
//! Migrated facade calls try it first under a capped budget and fall back
//! to their historical CLI call on any API failure, so the CLI stays the
//! arbiter — and the error taxonomy stays the CLI's — whenever the API
//! does not affirmatively succeed.

pub(crate) mod client;
pub mod deadline;
pub(crate) mod engine;
pub mod facts;
pub mod metrics;

pub(crate) use client::Docker;
pub use deadline::{classify, deadline_for, DockerOp, DockerTimeout};
pub use facts::{Fact, FactKey, FactLifetime};
pub use metrics::{
    begin_job, observe, observe_api, observe_api_fallback, snapshot, ClassTotal, JobDockerScope,
    Snapshot,
};
