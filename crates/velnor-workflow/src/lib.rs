//! Single schema-2 workflow generator entry point.
//!
//! Provider IDs, generation configuration, and CLI parsing all live in
//! [`s2`]. There is no schema-1 parser or fallback dispatch.

mod renovate_renderer;
mod s2;

pub use s2::{run_from_env, GeneratorError, GENERATOR_REVISION, SOURCE_CLOSURE, SOURCE_REVISION};
