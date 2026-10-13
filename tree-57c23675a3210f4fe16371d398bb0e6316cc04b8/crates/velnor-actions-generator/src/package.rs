//! Closed package-consumer policy and generated workflow contracts.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

const POLICY_SCHEMA: &str = "velnor-actions.package-policy.v1";
const TAP_CONSUMERS: [&str; 4] = [
    "jackin-project/homebrew-tap",
    "tailrocks/homebrew-holla",
    "tailrocks/homebrew-parallax",
    "tailrocks/homebrew-tablerock",
];
const APT_CONSUMERS: [&str; 2] = ["tailrocks/holla-apt", "tailrocks/velnor-apt"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagePolicy {
    schema: String,
    consumer: Vec<Consumer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Consumer {
    slug: String,
    kind: String,
    source: String,
    source_ref: String,
    channels: Vec<String>,
    assets: Vec<String>,
}

impl PackagePolicy {
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join("fleet/packages.toml");
        let bytes = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let policy: Self =
            toml::from_str(&bytes).map_err(|e| format!("parsing {}: {e}", path.display()))?;
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != POLICY_SCHEMA {
            return Err(format!("unknown package policy schema {:?}", self.schema));
        }
        let mut seen = BTreeSet::new();
        for row in &self.consumer {
            if !seen.insert(row.slug.as_str()) {
                return Err(format!("duplicate package consumer {:?}", row.slug));
            }
            let expected_kind = if TAP_CONSUMERS.contains(&row.slug.as_str()) {
                "tap"
            } else if APT_CONSUMERS.contains(&row.slug.as_str()) {
                "apt"
            } else {
                return Err(format!("unapproved package consumer {:?}", row.slug));
            };
            if row.kind != expected_kind {
                return Err(format!("{} must have kind {expected_kind}", row.slug));
            }
            if !matches!(
                row.source.as_str(),
                "tailrocks/tablerock"
                    | "tailrocks/parallax"
                    | "tailrocks/holla"
                    | "tailrocks/velnor"
                    | "jackin-project/jackin"
            ) {
                return Err(format!("unapproved package source {:?}", row.source));
            }
            if row.source_ref != "refs/tags/v*" {
                return Err(format!("{} has mutable or unknown source_ref", row.slug));
            }
            if row.channels.is_empty() || row.assets.is_empty() {
                return Err(format!(
                    "{} has an empty channel or asset allowlist",
                    row.slug
                ));
            }
            if row
                .channels
                .iter()
                .any(|c| !matches!(c.as_str(), "stable" | "preview" | "dev"))
            {
                return Err(format!("{} has an unknown channel", row.slug));
            }
            if row
                .assets
                .iter()
                .any(|a| a.is_empty() || a.contains('/') || a.contains(".."))
            {
                return Err(format!("{} has an unsafe asset pattern", row.slug));
            }
        }
        let expected: BTreeSet<_> = TAP_CONSUMERS.into_iter().chain(APT_CONSUMERS).collect();
        if seen != expected {
            return Err(
                "package consumer set is not exactly four taps and two APT repositories".into(),
            );
        }
        Ok(())
    }

    pub fn render_updater(&self) -> String {
        let mut policy_cases = String::new();
        let mut source_cases = String::new();
        for row in &self.consumer {
            let patterns = row.assets.join("\\n");
            policy_cases.push_str(&format!(
                "            {}) SOURCE_REPOSITORY={}; ASSET_PATTERNS=$'{}' ;;\n",
                row.slug, row.source, patterns
            ));
            source_cases.push_str(&format!(
                "            {}) SOURCE_REPOSITORY={} ;;\n",
                row.slug, row.source
            ));
        }
        policy_cases.push_str("            *) echo \"unknown consumer\" >&2; exit 1 ;;\n");
        source_cases.push_str("            *) echo \"unknown consumer\" >&2; exit 1 ;;\n");
        UPDATER_WORKFLOW
            .replace(
                "@CONSUMER_POLICY_CASES@",
                &format!(
                    "          case \"$CONSUMER_REPOSITORY\" in\n{policy_cases}          esac"
                ),
            )
            .replace(
                "@CONSUMER_SOURCE_CASES@",
                &format!(
                    "          case \"$CONSUMER_REPOSITORY\" in\n{source_cases}          esac"
                ),
            )
    }
}

pub const SIGNER_WORKFLOW: &str = include_str!("package_signer.yml");
pub const UPDATER_WORKFLOW: &str = include_str!("package_updater.yml");
pub const TAP_TEMPLATE: &str = include_str!("package_tap.yml");
pub const APT_TEMPLATE: &str = include_str!("package_apt.yml");
