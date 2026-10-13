//! `cache-contract`: the cache transport a unit job restores and saves.

use super::{Args, CacheBackend, Primitive, RenderCtx, Rendered, CACHE_CONTRACT};
use crate::{CacheSpec, GeneratorError, RunnerMode, Unit, WorkflowIr};

/// Exact Velnor mounts whose contents persist across job containers. Keep this
/// list lexical: classification must not turn path normalization into an
/// authorization boundary.
const VELNOR_CARGO_REGISTRY_MOUNT: &str = "/github/home/.cargo/registry";
const VELNOR_CARGO_GIT_MOUNT: &str = "/github/home/.cargo/git";
const VELNOR_MISE_INSTALLS_MOUNT: &str = "/opt/mise/installs";
const VELNOR_MISE_CACHE_MOUNT: &str = "/opt/mise/cache";
const VELNOR_SCCACHE_MOUNT: &str = "/var/cache/sccache";

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
    cache_path_is_lexically_valid(path)
        && (path == root
            || path
                .strip_prefix(root)
                .is_some_and(|rest| rest.starts_with('/')))
}

fn cache_path_is_lexically_valid(path: &str) -> bool {
    !path.is_empty()
        && !path.contains("//")
        && !path.contains('\\')
        && !path
            .chars()
            .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
        && path
            .split('/')
            .skip(if path.starts_with('/') { 1 } else { 0 })
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

/// True when a declared cache path resolves to a Velnor host-persistent store.
///
/// The only accepted aliases are the documented `~/.cargo/*` and relative
/// `.cargo/*` forms. Rustup is deliberately absent: its toolchain is baked
/// into the image, not persisted by the Velnor host.
///
/// Matches `velnor-runner::executor::velnor_persistent_cache_path` so the
/// generator and executor agree about which actions/cache steps are no-ops on
/// warm Velnor hosts.
pub(crate) fn velnor_host_persistent_cache_path(path: &str) -> bool {
    let path = path.trim();
    path_or_child(path, VELNOR_CARGO_REGISTRY_MOUNT)
        || path_or_child(path, VELNOR_CARGO_GIT_MOUNT)
        || path_or_child(path, VELNOR_MISE_INSTALLS_MOUNT)
        || path_or_child(path, VELNOR_MISE_CACHE_MOUNT)
        || path_or_child(path, VELNOR_SCCACHE_MOUNT)
        || path_or_child(path, "~/.cargo/registry")
        || path_or_child(path, "~/.cargo/git")
        || path_or_child(path, ".cargo/registry")
        || path_or_child(path, ".cargo/git")
}

/// True when every declared cache path is host-persistent on Velnor.
pub(crate) fn cache_is_velnor_host_persistent(cache: &CacheSpec) -> bool {
    !cache.paths.is_empty()
        && cache
            .paths
            .iter()
            .all(|path| velnor_host_persistent_cache_path(path))
}

fn should_bypass_host_persistent_cache(
    backend: CacheBackend,
    lane: RunnerMode,
    cache: Option<&CacheSpec>,
) -> bool {
    backend == CacheBackend::Detected
        && lane == RunnerMode::Velnor
        && cache.is_some_and(cache_is_velnor_host_persistent)
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
        if should_bypass_host_persistent_cache(self, lane, unit.cache.as_ref()) {
            return false;
        }
        true
    }
}

pub(crate) fn velnor_skips_pinned_rust_toolchain(lane: RunnerMode) -> bool {
    lane == RunnerMode::Velnor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CachePurpose;

    #[test]
    fn velnor_host_persistent_cache_path_matches_runner_contract() {
        for path in [
            "/github/home/.cargo/registry",
            "/github/home/.cargo/registry/cache",
            "/github/home/.cargo/git",
            "/github/home/.cargo/git/db",
            "/opt/mise/installs",
            "/opt/mise/installs/node/24",
            "/opt/mise/cache",
            "/opt/mise/cache/downloads",
            "/var/cache/sccache",
            "/var/cache/sccache/objects",
            "~/.cargo/registry",
            "~/.cargo/git",
            ".cargo/registry",
            ".cargo/git/db",
        ] {
            assert!(
                velnor_host_persistent_cache_path(path),
                "{path} should be host-persistent"
            );
        }
        for path in [
            "target",
            "target/debug",
            "~/.terraform.d/plugin-cache",
            "/github/home/.cargo",
            "/opt/mise",
            "/var/cache",
            ".cache/mise",
            ".local/share/mise/installs",
            "~/.rustup/toolchains",
            "/root/.rustup/toolchains",
            "./.cargo/registry",
            ".cargo/./registry",
            ".cargo/../.cargo/registry",
            "/github/home/.cargo/registry/../git",
            ".cargo//registry",
            "/github/home//.cargo/registry",
            "/github/home/.cargo/registry//cache",
            "~//.cargo/registry",
            ".cargo/registry/**",
            ".cargo/registry/[cache]",
            ".cargo/registry/{cache}",
        ] {
            assert!(
                !velnor_host_persistent_cache_path(path),
                "unsafe or unsupported alias {path} should not be host-persistent"
            );
        }
    }

    #[test]
    fn host_persistent_bypass_is_only_for_detected_backend_on_velnor() {
        let cache = CacheSpec {
            key_files: vec!["Cargo.lock".to_owned()],
            paths: vec!["~/.cargo/registry".to_owned()],
            purpose: CachePurpose::CargoSources,
            mbx_output_cache_justification: None,
            mutable_mount_seed: false,
        };

        assert!(should_bypass_host_persistent_cache(
            CacheBackend::Detected,
            RunnerMode::Velnor,
            Some(&cache)
        ));
        assert!(!should_bypass_host_persistent_cache(
            CacheBackend::Actions,
            RunnerMode::Velnor,
            Some(&cache)
        ));
        assert!(!should_bypass_host_persistent_cache(
            CacheBackend::Detected,
            RunnerMode::Github,
            Some(&cache)
        ));
    }

    #[test]
    fn cache_is_velnor_host_persistent_requires_every_path() {
        let cargo = CacheSpec {
            key_files: vec!["Cargo.lock".to_owned()],
            paths: vec!["~/.cargo/registry".to_owned(), "~/.cargo/git".to_owned()],
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

    #[test]
    fn velnor_lane_skips_pinned_rust_toolchain() {
        assert!(velnor_skips_pinned_rust_toolchain(RunnerMode::Velnor));
        assert!(!velnor_skips_pinned_rust_toolchain(RunnerMode::Github));
    }
}
