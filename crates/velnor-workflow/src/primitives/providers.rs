//! `provider-matrix`: the per-provider fan-out over the provider universe.
//!
//! The provider matrix is the contract every unit pipeline renders against:
//! which provider jobs a unit emits, which selector each provider routes to,
//! and which providers may save a cache entry. Each selected eligible unit
//! fans out to every provider in the universe that passes eligibility, with
//! the same source, command, profile, features, fixtures, and test
//! expectations on each.

use super::{Args, Primitive, ProviderJob, RenderCtx, Rendered, PROVIDER_MATRIX};
use crate::provider::ProviderId;
use crate::{GeneratorError, ProjectConfig};

/// Render nothing: the provider matrix is a contract, not a workflow family.
pub(crate) struct ProviderMatrix;

impl Primitive for ProviderMatrix {
    fn id(&self) -> &'static str {
        PROVIDER_MATRIX
    }

    fn schema(&self) -> &'static [&'static str] {
        &["jobs"]
    }

    fn render(&self, _ctx: &RenderCtx<'_>, _args: &Args<'_>) -> Result<Rendered, GeneratorError> {
        Ok(Rendered::default())
    }
}

/// The resolved provider matrix.
pub(crate) struct ResolvedProviders {
    ir: super::WorkflowIr,
    jobs: Vec<ProviderJob>,
}

impl ResolvedProviders {
    pub(crate) fn ir(&self) -> &super::WorkflowIr {
        &self.ir
    }

    pub(crate) fn jobs(&self) -> &[ProviderJob] {
        &self.jobs
    }
}

/// Resolve the provider matrix from the declared rows over the provider set.
///
/// # Errors
/// Returns a usage error for a declared provider job the universe does not
/// support.
pub(crate) fn resolve(
    config: &ProjectConfig,
    rows: &[super::ResolvedRow],
) -> Result<ResolvedProviders, GeneratorError> {
    let ir = super::WorkflowIr::from_config(config);
    let default = super::WorkflowIr::default_provider_jobs(&config.providers, true);
    let mut jobs = default.clone();
    for row in rows.iter().filter(|row| row.primitive == PROVIDER_MATRIX) {
        if let Some(names) = Args(&row.args).strings("jobs")? {
            jobs = names
                .iter()
                .map(|name| {
                    let provider = ProviderId::parse(name).map_err(|_| {
                        GeneratorError::usage(format!(
                            "`{PROVIDER_MATRIX}` job `{name}` is not a provider id; supported jobs: {}",
                            default
                                .iter()
                                .map(|job| job.provider.as_str().to_owned())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                    })?;
                    default
                        .iter()
                        .find(|job| job.provider == provider)
                        .copied()
                        .ok_or_else(|| {
                            GeneratorError::usage(format!(
                                "`{PROVIDER_MATRIX}` job `{name}` is not supported by the provider universe; supported jobs: {}",
                                default
                                    .iter()
                                    .map(|job| job.provider.as_str().to_owned())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                        })
                })
                .collect::<Result<Vec<_>, GeneratorError>>()?;
        }
    }
    if jobs.is_empty() {
        return Err(GeneratorError::usage(
            "the provider matrix resolved to no provider job; at least one provider is required",
        ));
    }
    // Canonical provider order: hosted, self-hosted, native.
    jobs.sort_by_key(|job| job.provider);
    Ok(ResolvedProviders { ir, jobs })
}
