//! Closed package-consumer policy and generated workflow contracts.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::Path;

use crate::forks::ForkTable;
use crate::releases::ReleaseTable;

const POLICY_SCHEMA: &str = "velnor-actions.package-policy.v1";
const TAP_CONSUMERS: [&str; 4] = [
    "jackin-project/homebrew-tap",
    "tailrocks/homebrew-holla",
    "tailrocks/homebrew-parallax",
    "tailrocks/homebrew-tablerock",
];
const APT_CONSUMERS: [&str; 2] = ["tailrocks/holla-apt", "tailrocks/velnor-apt"];

#[derive(Debug)]
pub struct PackagePolicy {
    schema: String,
    consumer: Vec<Consumer>,
    forks: ForkTable,
    releases: ReleaseTable,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageFile {
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
    #[serde(default)]
    preview_assets: Vec<String>,
}

impl PackagePolicy {
    pub fn load(root: &Path, forks: &ForkTable) -> Result<Self, String> {
        let path = root.join("fleet/packages.toml");
        let bytes = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let file: PackageFile =
            toml::from_str(&bytes).map_err(|e| format!("parsing {}: {e}", path.display()))?;
        let policy = Self {
            schema: file.schema,
            consumer: file.consumer,
            forks: forks.clone(),
            releases: ReleaseTable::load(root)?,
        };
        policy.validate()?;
        policy
            .releases
            .validate_consumers(policy.consumer.iter().map(|row| row.slug.clone()), forks)?;
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
            let valid_package_format = |asset: &str| match row.kind.as_str() {
                "tap" => asset.ends_with(".tar.gz") || asset.ends_with(".zip"),
                "apt" => asset.ends_with(".deb"),
                _ => false,
            };
            if row.assets.iter().any(|asset| !valid_package_format(asset)) {
                return Err(format!(
                    "{} has an asset outside its package formats",
                    row.slug
                ));
            }
            if row.slug == "jackin-project/homebrew-tap" {
                if row.preview_assets.len() != 6
                    || !row.channels.iter().any(|channel| channel == "preview")
                {
                    return Err("jackin preview policy must bind exactly six rolling assets".into());
                }
            } else if !row.preview_assets.is_empty() {
                return Err(format!("{} has unsupported preview assets", row.slug));
            }
            if row.preview_assets.iter().any(|asset| {
                asset.is_empty()
                    || asset.contains('/')
                    || asset.contains("..")
                    || !valid_package_format(asset)
            }) {
                return Err(format!("{} has an unsafe preview asset", row.slug));
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
            let preview_patterns = row.preview_assets.join("\\n");
            let channels = row.channels.join("\\n");
            policy_cases.push_str(&format!(
                "            {}) SOURCE_REPOSITORY={}; ASSET_PATTERNS=$'{}'; PREVIEW_ASSET_PATTERNS=$'{}'; ALLOWED_CHANNELS=$'{}' ;;\n",
                row.slug, row.source, patterns, preview_patterns, channels
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

    pub fn render_consumer(
        &self,
        repository: &str,
        release_shas: [&str; 3],
        calver: &str,
    ) -> Result<String, String> {
        if release_shas.iter().any(|sha| !is_sha40(sha)) {
            return Err(
                "every owner release SHA must be 40 lowercase hexadecimal characters".into(),
            );
        }
        if !valid_calver(calver) {
            return Err("CalVer must be YYYY.M.D with numeric components".into());
        }
        let row = self
            .consumer
            .iter()
            .find(|row| row.slug == repository)
            .ok_or_else(|| format!("{repository:?} is not a package consumer"))?;
        let release = self
            .releases
            .by_calver(calver)
            .ok_or_else(|| format!("release table has no row for {calver}"))?;
        let signer = release
            .signer_for(repository)
            .ok_or_else(|| format!("release {calver} has no signer row for {repository}"))?;
        if release_shas.len() != self.forks.len() {
            return Err("release SHA count does not match the fork table".into());
        }
        let template = match row.kind.as_str() {
            "tap" => TAP_TEMPLATE,
            "apt" => APT_TEMPLATE,
            _ => return Err("package policy contains an unknown kind".into()),
        };
        let mut rendered = template.to_string();
        for (fork, sha) in self.forks.forks().iter().zip(release_shas) {
            rendered = rendered.replace(fork.placeholder(), sha);
        }
        let rendered = rendered
            .replace("@CALVER@", calver)
            .replace("@SIGNER_FORK@", signer.signer_fork())
            .replace("@CURRENT_SIGNER_DIGEST@", signer.current_digest())
            .replace(
                "@OLD_SIGNER_DIGEST@",
                signer.old_digest().unwrap_or_default(),
            )
            .replace(
                "@OLD_SIGNER_ACTIVATED_AT@",
                signer.old_activated_at().unwrap_or_default(),
            )
            .replace(
                "@OLD_SIGNER_EXPIRES_AT@",
                signer.old_expires_at().unwrap_or_default(),
            )
            .replace(
                "@PACKAGE_CHANNELS@",
                if row.slug == "jackin-project/homebrew-tap" {
                    "[stable, preview]"
                } else {
                    "[stable]"
                },
            );
        for placeholder in self
            .forks
            .forks()
            .iter()
            .map(|fork| fork.placeholder())
            .chain([
                "@CALVER@",
                "@SIGNER_FORK@",
                "@CURRENT_SIGNER_DIGEST@",
                "@OLD_SIGNER_DIGEST@",
                "@OLD_SIGNER_ACTIVATED_AT@",
                "@OLD_SIGNER_EXPIRES_AT@",
                "@PACKAGE_CHANNELS@",
            ])
        {
            if rendered.contains(placeholder) {
                return Err(format!("package consumer rendering left {placeholder}"));
            }
        }
        Ok(rendered)
    }
}

fn is_sha40(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_calver(value: &str) -> bool {
    let parts: Vec<_> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

pub const SIGNER_WORKFLOW: &str = include_str!("package_signer.yml");
pub const UPDATER_WORKFLOW: &str = include_str!("package_updater.yml");
pub const TAP_TEMPLATE: &str = include_str!("package_tap.yml");
pub const APT_TEMPLATE: &str = include_str!("package_apt.yml");
