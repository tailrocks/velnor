//! Versioned configuration, context, authentication, and instance contracts.
//!
//! Values that may contain credentials are represented by [`SecretRef`]; the
//! model has no field capable of carrying a credential value.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::sanitized::{SanitizedUrl, SecretRef};
use crate::time::Timestamp;

/// Source precedence for one effective configuration value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConfigSource {
    /// Compiled safe default.
    #[default]
    Builtin,
    /// Persisted named context.
    Context,
    /// Per-instance configuration file.
    Instance,
    /// Captured systemd startup environment.
    Systemd,
    /// Captured process startup override.
    Process,
    /// Explicit command-line override.
    Command,
}

/// One resolved value with source provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Sourced<T> {
    /// Effective value.
    pub value: T,
    /// Highest-precedence source that supplied the value.
    pub source: ConfigSource,
}

/// Desired local configuration with credential references only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesiredConfig {
    /// GitHub or control-plane endpoint with userinfo removed.
    pub endpoint: Option<SanitizedUrl>,
    /// Runner/instance display name.
    pub name: Option<String>,
    /// Requested stable slot count.
    pub slots: Option<u32>,
    /// Labels replacing the previous collection when set.
    pub labels: Option<BTreeMap<String, String>>,
    /// Name of the credential managed outside this model.
    pub credential: Option<SecretRef>,
}

/// Effective configuration captured by a daemon at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectiveConfig {
    /// Effective endpoint and its source.
    pub endpoint: Sourced<Option<SanitizedUrl>>,
    /// Effective instance name and its source.
    pub name: Sourced<String>,
    /// Effective slot count and its source.
    pub slots: Sourced<u32>,
    /// Effective labels and their source.
    pub labels: Sourced<BTreeMap<String, String>>,
    /// Effective credential reference and its source.
    pub credential: Sourced<Option<SecretRef>>,
    /// Capture time; never a credential value.
    pub captured_at: Timestamp,
}

/// A named local client context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextConfig {
    /// Canonical context name.
    pub name: String,
    /// Canonical local API endpoint.
    pub endpoint: SanitizedUrl,
    /// Optional credential reference name.
    pub credential: Option<SecretRef>,
    /// Whether this context is selected by default.
    pub current: bool,
}

/// Permission result for one named capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PermissionState {
    /// Read or mutation was proven.
    Proven,
    /// The identity was checked and denied.
    Denied,
    /// No safe non-mutating proof exists.
    Unproven,
    /// The upstream check could not complete.
    Unavailable,
}

/// Sanitized authentication report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthReport {
    /// Safe identity slug, when GitHub returned one.
    pub identity: Option<String>,
    /// Named permissions and their proof state.
    pub permissions: BTreeMap<String, PermissionState>,
    /// Observation time.
    pub observed_at: Timestamp,
}

/// Desired/observed difference for one configuration field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigDrift {
    /// Field path, never a secret value.
    pub field: String,
    /// Source currently supplying the desired value.
    pub desired_source: ConfigSource,
    /// Whether the observed value differs.
    pub differs: bool,
    /// Safe explanation without serialized credential material.
    pub message: String,
}

/// Planned local instance operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstanceOperation {
    /// Stable operation identity.
    pub operation_id: String,
    /// Target instance name.
    pub instance: String,
    /// Operation kind (`init`, `install`, `apply`, or `delete`).
    pub kind: String,
    /// Current plan phase.
    pub phase: String,
    /// Time the plan was created.
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_config_contains_reference_not_secret_value() {
        let config = DesiredConfig {
            endpoint: Some(SanitizedUrl::project("https://user:token@example.test/api")),
            name: Some("primary".to_owned()),
            slots: Some(2),
            labels: None,
            credential: Some(SecretRef::named("GITHUB_TOKEN")),
        };
        let json = serde_json::to_string(&config).expect("config serializes");
        assert!(!json.contains("token"));
        assert!(json.contains("GITHUB_TOKEN"));
    }
}
