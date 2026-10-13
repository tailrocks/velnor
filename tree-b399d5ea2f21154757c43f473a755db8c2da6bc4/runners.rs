//! Trust-gated Velnor job availability.
//!
//! Trust-gated Velnor jobs need a runner that claims an extra label. Whether
//! such a runner exists is declared in the generation config
//! (`[workflow] velnor_trusted_runner_available`), never probed: the rendered
//! tree must be a pure function of the checked-in inputs so that `--check` and
//! the policy validator's regeneration at the declared pin are reproducible.
//! When the declaration says no runner is online, the generator renders the
//! gated jobs as explicit skips instead of emitting work that would queue
//! indefinitely.

use crate::{GeneratorError, ProjectConfig};

/// The config key that decides trust-gated job emission.
pub(crate) const TRUSTED_RUNNER_AVAILABLE_KEY: &str = "[workflow] velnor_trusted_runner_available";

/// Whether trust-gated Velnor jobs may run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TrustedRunnerAvailability {
    pub(crate) required: bool,
    pub(crate) online: bool,
    pub(crate) skip_reason: Option<String>,
}

impl TrustedRunnerAvailability {
    fn not_required() -> Self {
        Self {
            required: false,
            online: true,
            skip_reason: None,
        }
    }

    fn available() -> Self {
        Self {
            required: true,
            online: true,
            skip_reason: None,
        }
    }

    fn unavailable(reason: String) -> Self {
        Self {
            required: true,
            online: false,
            skip_reason: Some(reason),
        }
    }
}

/// True when the config has a unit that routes to the trusted label.
fn requires_trusted_runner(config: &ProjectConfig) -> Option<&str> {
    let label = config.velnor_trusted_label.as_deref()?;
    config
        .units
        .iter()
        .any(|unit| unit.requires_trusted)
        .then_some(label)
}

/// Refuse to render a tree whose trust-gated shape is undecided.
///
/// The config loader enforces the same rule for declared configs; this guard
/// covers every other way a `ProjectConfig` reaches the renderer.
pub(crate) fn validate_trusted_runner_availability(
    config: &ProjectConfig,
) -> Result<(), GeneratorError> {
    if requires_trusted_runner(config).is_some() && config.velnor_trusted_runner_available.is_none()
    {
        return Err(GeneratorError::usage(format!(
            "a unit requires a trusted runner but {TRUSTED_RUNNER_AVAILABLE_KEY} is not declared; \
             set it to true when an online runner claims velnor_trusted_label, false to render \
             the trust-gated jobs as skips"
        )));
    }
    Ok(())
}

/// Resolve whether trust-gated Velnor jobs are emitted, from the config alone.
///
/// An undecided config is rendered as unavailable so that a renderer reached
/// without [`validate_trusted_runner_availability`] fails closed (skips, never
/// queues); generation itself refuses such a config before rendering.
pub(crate) fn resolve_trusted_runner_availability(
    config: &ProjectConfig,
) -> TrustedRunnerAvailability {
    let Some(label) = requires_trusted_runner(config) else {
        return TrustedRunnerAvailability::not_required();
    };
    match config.velnor_trusted_runner_available {
        Some(true) => TrustedRunnerAvailability::available(),
        Some(false) => TrustedRunnerAvailability::unavailable(format!(
            "no online runner claims {label} ({TRUSTED_RUNNER_AVAILABLE_KEY} = false)"
        )),
        None => TrustedRunnerAvailability::unavailable(format!(
            "{TRUSTED_RUNNER_AVAILABLE_KEY} is not declared"
        )),
    }
}
