//! `cache-contract`: the cache transport a unit job restores and saves.

use std::fmt::Write as _;

use sha2::{Digest as _, Sha256};

use super::{Args, CacheBackend, Primitive, RenderCtx, Rendered, CACHE_CONTRACT};
use crate::s2::platform::ProductIdentity;
use crate::s2::provider::ProviderId;
use crate::s2::{CacheSpec, GeneratorError, Unit, WorkflowIr};

/// Schema for the exact native-product cache namespace. The complete typed
/// product identity is hashed into every key; compatibility-prefix restores
/// are deliberately not part of this cache contract.
pub(crate) const NATIVE_PRODUCT_CACHE_KEY_SCHEMA: &str = "velnor-native-product-cache/1";

/// Derive the exact native-product cache key from the complete typed identity.
///
/// The cache is an optional acceleration layer. Callers must verify the
/// staged product manifest against the same identity before installing it and
/// must never use a cache hit as the product-transport readiness marker.
pub(crate) fn native_product_cache_key(
    identity: &ProductIdentity,
) -> Result<String, GeneratorError> {
    identity.validate("native product cache")?;
    if !identity.exact_reuse_allowed() {
        let missing = identity.missing_dimensions().join(", ");
        return Err(GeneratorError::usage(format!(
            "native product cache identity is incomplete; missing: {missing}"
        )));
    }
    let bytes = serde_json::to_vec(identity).map_err(|error| {
        GeneratorError::usage(format!("serialize native product identity: {error}"))
    })?;
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(format!("{NATIVE_PRODUCT_CACHE_KEY_SCHEMA}-{hex}"))
}

/// Exact Velnor mounts whose contents persist across job containers. Keep this
/// list lexical: classification must not turn path normalization into an
/// authorization boundary.
const VELNOR_CARGO_REGISTRY_MOUNT: &str = "/github/home/.cargo/registry";
const VELNOR_CARGO_GIT_MOUNT: &str = "/github/home/.cargo/git";
const VELNOR_MISE_INSTALLS_MOUNT: &str = "/opt/mise/installs";
const VELNOR_MISE_CACHE_MOUNT: &str = "/opt/mise/cache";
const VELNOR_SCCACHE_MOUNT: &str = "/var/cache/sccache";
const VELNOR_BUN_INSTALL_CACHE_MOUNT: &str = "/github/home/.bun/install/cache";
const VELNOR_NPM_CACHE_MOUNT: &str = "/github/home/.npm";
const VELNOR_TERRAFORM_PLUGIN_CACHE_MOUNT: &str = "/github/home/.terraform.d/plugin-cache";

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
            .skip(usize::from(path.starts_with('/')))
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
pub(crate) fn local_host_persistent_cache_path(path: &str) -> bool {
    let path = path.trim();
    path_or_child(path, VELNOR_CARGO_REGISTRY_MOUNT)
        || path_or_child(path, VELNOR_CARGO_GIT_MOUNT)
        || path_or_child(path, VELNOR_MISE_INSTALLS_MOUNT)
        || path_or_child(path, VELNOR_MISE_CACHE_MOUNT)
        || path_or_child(path, VELNOR_SCCACHE_MOUNT)
        || path_or_child(path, VELNOR_BUN_INSTALL_CACHE_MOUNT)
        || path_or_child(path, VELNOR_NPM_CACHE_MOUNT)
        || path_or_child(path, VELNOR_TERRAFORM_PLUGIN_CACHE_MOUNT)
        || path_or_child(path, "~/.cargo/registry")
        || path_or_child(path, "~/.cargo/git")
        || path_or_child(path, ".cargo/registry")
        || path_or_child(path, ".cargo/git")
        || path_or_child(path, "~/.bun/install/cache")
        || path_or_child(path, "~/.npm")
        || path_or_child(path, "~/.terraform.d/plugin-cache")
}

/// True when every declared cache path is host-persistent on Velnor.
pub(crate) fn cache_is_local_host_persistent(cache: &CacheSpec) -> bool {
    !cache.paths.is_empty()
        && cache
            .paths
            .iter()
            .all(|path| local_host_persistent_cache_path(path))
}

fn should_bypass_host_persistent_cache(
    backend: CacheBackend,
    provider: ProviderId,
    cache: Option<&CacheSpec>,
) -> bool {
    backend == CacheBackend::Detected
        && provider.is_local()
        && cache.is_some_and(cache_is_local_host_persistent)
}

impl CacheBackend {
    /// Whether a provider job should emit actions/cache restore and save steps.
    ///
    /// Local providers skip restore when every declared path already lives on
    /// the runner's host-persistent mounts: the executor would no-op the copy,
    /// but the step still costs hashFiles evaluation and a cache lookup.
    pub(crate) fn provider_enables_actions_cache(
        self,
        provider: ProviderId,
        ir: &WorkflowIr,
        unit: &Unit,
    ) -> bool {
        if !self.enables_actions_cache(ir, unit) {
            return false;
        }
        if should_bypass_host_persistent_cache(self, provider, unit.cache.as_ref()) {
            return false;
        }
        true
    }
}

pub(crate) fn local_skips_pinned_rust_toolchain(provider: ProviderId) -> bool {
    provider.is_local()
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        reason = "cache-key tests use explicit expectations for fixed valid and invalid identities"
    )]

    use super::*;
    use crate::s2::platform::{ProductIdentity, PRODUCT_IDENTITY_SCHEMA};
    use crate::s2::CachePurpose;

    fn native_identity() -> ProductIdentity {
        ProductIdentity {
            schema: PRODUCT_IDENTITY_SCHEMA.to_owned(),
            producer: "rust-ffi".to_owned(),
            product: "xcframework-bridgecore".to_owned(),
            adapter: "boltffi@0.30.1".to_owned(),
            source: "libs/bridge-ffi/boltffi.toml".to_owned(),
            inputs_digest: Some("a".repeat(64)),
            host_abi: "macos-arm64".to_owned(),
            target: "apple-xcframework".to_owned(),
            target_triple: "aarch64-apple-darwin".to_owned(),
            architectures: vec!["macos-arm64".to_owned()],
            sdk: "macos26.sdk-26.0".to_owned(),
            deployment_target: "26.0".to_owned(),
            toolchain: [("rust.channel".to_owned(), "1.97.1".to_owned())]
                .into_iter()
                .collect(),
            profile: "release".to_owned(),
            features: Vec::new(),
            flags: vec!["--locked".to_owned()],
            generation: [("framework".to_owned(), "BridgeCore".to_owned())]
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn native_product_cache_key_hashes_complete_identity() {
        let identity = native_identity();
        let key = native_product_cache_key(&identity).expect("complete identity");
        assert!(key.starts_with("velnor-native-product-cache/1-"), "{key}");
        assert_eq!(key.len(), "velnor-native-product-cache/1-".len() + 64);

        let mut profile = identity.clone();
        profile.profile = "debug".to_owned();
        assert_ne!(
            key,
            native_product_cache_key(&profile).expect("debug identity")
        );

        let mut architecture = identity;
        architecture.architectures = vec!["macos-x86_64".to_owned()];
        assert_ne!(
            key,
            native_product_cache_key(&architecture).expect("x86 identity")
        );
    }

    #[test]
    fn native_product_cache_key_rejects_incomplete_or_invalid_identity() {
        let mut missing = native_identity();
        missing.sdk.clear();
        let error = native_product_cache_key(&missing).expect_err("missing SDK");
        assert!(error.to_string().contains("sdk"), "{error}");

        let mut invalid_digest = native_identity();
        invalid_digest.inputs_digest = Some("not-a-sha256".to_owned());
        let error = native_product_cache_key(&invalid_digest).expect_err("invalid digest");
        assert!(error.to_string().contains("inputs_digest"), "{error}");
    }

    #[test]
    fn local_host_persistent_cache_path_matches_runner_contract() {
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
            "/github/home/.bun/install/cache",
            "/github/home/.bun/install/cache/abc123",
            "/github/home/.npm",
            "/github/home/.npm/_cacache",
            "/github/home/.terraform.d/plugin-cache",
            "/github/home/.terraform.d/plugin-cache/registry.terraform.io",
            "~/.cargo/registry",
            "~/.cargo/git",
            ".cargo/registry",
            ".cargo/git/db",
            "~/.bun/install/cache",
            "~/.npm",
            "~/.terraform.d/plugin-cache",
        ] {
            assert!(
                local_host_persistent_cache_path(path),
                "{path} should be host-persistent"
            );
        }
        for path in [
            "target",
            "target/debug",
            "~/.bun",
            "~/.terraform.d",
            "/github/home/.bun",
            "/github/home/.terraform.d",
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
                !local_host_persistent_cache_path(path),
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
            ProviderId::Velnor,
            Some(&cache)
        ));
        assert!(!should_bypass_host_persistent_cache(
            CacheBackend::Actions,
            ProviderId::Velnor,
            Some(&cache)
        ));
        assert!(!should_bypass_host_persistent_cache(
            CacheBackend::Detected,
            ProviderId::GithubHosted,
            Some(&cache)
        ));
    }

    #[test]
    fn cache_is_local_host_persistent_requires_every_path() {
        let cargo = CacheSpec {
            key_files: vec!["Cargo.lock".to_owned()],
            paths: vec!["~/.cargo/registry".to_owned(), "~/.cargo/git".to_owned()],
            purpose: CachePurpose::CargoSources,
            mbx_output_cache_justification: None,
            mutable_mount_seed: false,
        };
        assert!(cache_is_local_host_persistent(&cargo));
        let mixed = CacheSpec {
            paths: vec!["~/.cargo/registry".to_owned(), "target".to_owned()],
            ..cargo.clone()
        };
        assert!(!cache_is_local_host_persistent(&mixed));
    }

    #[test]
    fn local_provider_skips_pinned_rust_toolchain() {
        assert!(local_skips_pinned_rust_toolchain(ProviderId::Velnor));
        assert!(!local_skips_pinned_rust_toolchain(ProviderId::GithubHosted));
    }
}
