//! Generation-time GitHub runner availability probes.
//!
//! Trust-gated Velnor jobs need a runner that claims an extra label. When none
//! are online, the generator skips those jobs instead of emitting work that
//! would queue indefinitely.

use std::env;
use std::process::{Command, Stdio};

use crate::ProjectConfig;

const TRUSTED_RUNNER_AVAILABLE_ENV: &str = "VELNOR_WORKFLOW_TRUSTED_RUNNER_AVAILABLE";

/// Whether trust-gated Velnor jobs may run at generation time.
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

/// Resolve whether an online runner claims the configured trusted label.
///
/// Precedence: environment override, explicit config pin, live `gh api` probe.
/// When the probe fails and nothing is pinned, fail closed to skip (not queue).
pub(crate) fn resolve_trusted_runner_availability(
    config: &ProjectConfig,
) -> TrustedRunnerAvailability {
    let Some(label) = config.velnor_trusted_label.as_deref() else {
        return TrustedRunnerAvailability::not_required();
    };
    if !config.units.iter().any(|unit| unit.requires_trusted) {
        return TrustedRunnerAvailability::not_required();
    }

    if let Ok(value) = env::var(TRUSTED_RUNNER_AVAILABLE_ENV) {
        return match parse_boolish(&value) {
            Some(true) => TrustedRunnerAvailability::available(),
            Some(false) => TrustedRunnerAvailability::unavailable(format!(
                "no online runner claims {label} ({TRUSTED_RUNNER_AVAILABLE_ENV}=false)"
            )),
            None => TrustedRunnerAvailability::unavailable(format!(
                "{TRUSTED_RUNNER_AVAILABLE_ENV} must be true or false, found `{value}`"
            )),
        };
    }

    if let Some(available) = config.velnor_trusted_runner_available {
        return if available {
            TrustedRunnerAvailability::available()
        } else {
            TrustedRunnerAvailability::unavailable(format!(
                "no online runner claims {label} ([workflow] velnor_trusted_runner_available = false)"
            ))
        };
    }

    match probe_online_runner_label(&config.repository, label) {
        Ok(true) => TrustedRunnerAvailability::available(),
        Ok(false) => TrustedRunnerAvailability::unavailable(format!(
            "no online runner claims {label} (generation-time gh api probe)"
        )),
        Err(error) => TrustedRunnerAvailability::unavailable(format!(
            "trusted runner probe failed ({error}); skipping trust-gated Velnor jobs instead of queueing"
        )),
    }
}

fn parse_boolish(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn probe_online_runner_label(repository: &str, label: &str) -> Result<bool, String> {
    let jq = format!(
        r#".runners[] | select(.status == "online") | .labels[] | select(.name == "{label}") | .name"#
    );
    let output = Command::new("gh")
        .args([
            "api",
            &format!("repos/{repository}/actions/runners"),
            "--paginate",
            "--jq",
            &jq,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("spawn gh: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let detail = if stderr.is_empty() {
            format!("exit status {}", output.status)
        } else {
            stderr
        };
        return Err(format!(
            "gh api repos/{repository}/actions/runners: {detail}"
        ));
    }
    Ok(!output.stdout.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_boolish_accepts_common_literals() {
        assert_eq!(parse_boolish("true"), Some(true));
        assert_eq!(parse_boolish("FALSE"), Some(false));
        assert_eq!(parse_boolish("1"), Some(true));
        assert_eq!(parse_boolish("off"), Some(false));
        assert_eq!(parse_boolish("maybe"), None);
    }
}
