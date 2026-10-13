//! `cache-contract`: the cache transport a unit job restores and saves.

use super::{Args, CacheBackend, Primitive, RenderCtx, Rendered, CACHE_CONTRACT};
use crate::GeneratorError;

/// Declare the cache contract the unit pipelines render.
///
/// The detected contract is the default: a repository whose Rust commands run
/// under the shared object cache uses that cache and no actions cache, and every
/// other unit uses the actions cache keyed by its own detected cache inputs.
pub(crate) struct CacheContract;

impl Primitive for CacheContract {
    fn id(&self) -> &'static str {
        CACHE_CONTRACT
    }

    fn schema(&self) -> &'static [&'static str] {
        &["backend"]
    }

    fn render(&self, _ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        Ok(Rendered::default())
    }
}

/// The resolved cache contract.
#[derive(Clone, Copy)]
pub(crate) struct ResolvedCache {
    backend: CacheBackend,
}

impl ResolvedCache {
    pub(crate) fn backend(self) -> CacheBackend {
        self.backend
    }
}

/// Resolve the cache contract from the declared rows.
///
/// # Errors
/// Returns a usage error for a declared backend the generator does not render.
pub(crate) fn resolve(rows: &[super::ResolvedRow]) -> Result<ResolvedCache, GeneratorError> {
    let mut backend = CacheBackend::Detected;
    for row in rows.iter().filter(|row| row.primitive == CACHE_CONTRACT) {
        if let Some(name) = Args(&row.args).string("backend")? {
            backend = match name.as_str() {
                "detected" => CacheBackend::Detected,
                "actions" => CacheBackend::Actions,
                "objects" => CacheBackend::ObjectCache,
                other => {
                    return Err(GeneratorError::usage(format!(
                        "`{CACHE_CONTRACT}` backend must be `detected`, `actions`, or `objects`, found `{other}`"
                    )))
                }
            };
        }
    }
    Ok(ResolvedCache { backend })
}
