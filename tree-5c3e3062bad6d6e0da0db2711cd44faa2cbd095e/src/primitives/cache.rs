//! `cache-contract`: the cache transport a unit job restores and saves.

use super::{Args, CacheBackend, Primitive, RenderCtx, Rendered, CACHE_CONTRACT};
use crate::{CacheSpec, GeneratorError, RunnerMode, Unit, WorkflowIr};

/// Container mount for the shared sccache store. Must stay aligned with
/// `velnor-runner::sccache_compat::CONTAINER_DIR`.
const VELNOR_SCCACHE_CONTAINER_DIR: &str = "/var/cache/sccache";

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

fn path_or_child(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// True when a declared cache path resolves to a Velnor host-persistent store.
///
/// Matches `velnor-runner::executor::velnor_persistent_cache_path` so the
/// generator and executor agree about which actions/cache steps are no-ops on
/// warm Velnor hosts.
pub(crate) fn velnor_host_persistent_cache_path(path: &str) -> bool {
    let path = path.trim();
    if path_or_child(path, ".cargo/registry")
        || path_or_child(path, ".cargo/git")
        || path_or_child(path, ".cache/mise")
        || path_or_child(path, ".local/share/mise/installs")
    {
        return true;
    }
    let home_relative = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("/github/home/"));
    if let Some(rest) = home_relative {
        return path_or_child(rest, ".cargo/registry")
            || path_or_child(rest, ".cargo/git")
            || path_or_child(rest, ".rustup");
    }
    path_or_child(path, "/opt/mise")
        || path_or_child(path, "/root/.rustup")
        || path_or_child(path, VELNOR_SCCACHE_CONTAINER_DIR)
}

/// True when every declared cache path is host-persistent on Velnor.
pub(crate) fn cache_is_velnor_host_persistent(cache: &CacheSpec) -> bool {
    !cache.paths.is_empty()
        && cache
            .paths
            .iter()
            .all(|path| velnor_host_persistent_cache_path(path))
}

impl CacheBackend {
    /// Whether a lane job should emit actions/cache restore and save steps.
    ///
    /// The Velnor lane skips restore when every declared path already lives on
    /// the runner's host-persistent mounts: the executor would no-op the copy,
    /// but the step still costs hashFiles evaluation and a cache lookup.
    pub(crate) fn lane_enables_actions_cache(
        self,
        lane: RunnerMode,
        ir: &WorkflowIr,
        unit: &Unit,
    ) -> bool {
        if !self.enables_actions_cache(ir, unit) {
            return false;
        }
        if lane == RunnerMode::Velnor
            && unit
                .cache
                .as_ref()
                .is_some_and(cache_is_velnor_host_persistent)
        {
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CachePurpose;

    #[test]
    fn velnor_host_persistent_cache_path_matches_runner_contract() {
        for path in [
            "~/.cargo/registry",
            "~/.cargo/git",
            "/github/home/.cargo/registry/cache",
            ".cargo/registry",
            ".cargo/git/db",
            "/opt/mise/installs/node/24",
            "/root/.rustup/toolchains",
            "/var/cache/sccache",
        ] {
            assert!(
                velnor_host_persistent_cache_path(path),
                "{path} should be host-persistent"
            );
        }
        for path in ["target", "target/debug", "~/.terraform.d/plugin-cache"] {
            assert!(
                !velnor_host_persistent_cache_path(path),
                "{path} should not be host-persistent"
            );
        }
    }

    #[test]
    fn cache_is_velnor_host_persistent_requires_every_path() {
        let cargo = CacheSpec {
            key_files: vec!["Cargo.lock".to_owned()],
            paths: vec![
                "~/.cargo/registry".to_owned(),
                "~/.cargo/git".to_owned(),
            ],
            purpose: CachePurpose::CargoSources,
            mbx_output_cache_justification: None,
            mutable_mount_seed: false,
        };
        assert!(cache_is_velnor_host_persistent(&cargo));
        let mixed = CacheSpec {
            paths: vec!["~/.cargo/registry".to_owned(), "target".to_owned()],
            ..cargo.clone()
        };
        assert!(!cache_is_velnor_host_persistent(&mixed));
    }
}
