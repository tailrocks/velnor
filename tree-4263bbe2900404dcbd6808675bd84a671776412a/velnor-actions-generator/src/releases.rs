//! Release-keyed signer data used by package consumers.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use crate::forks::ForkTable;

const RELEASES_SCHEMA: &str = "velnor-actions.releases.v1";

/// One signer digest slot for one package consumer in one release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerSlot {
    consumer: String,
    signer_fork: String,
    current_digest: String,
    old_digest: Option<String>,
    old_activated_at: Option<String>,
    old_expires_at: Option<String>,
}

impl SignerSlot {
    /// The package consumer slug.
    #[must_use]
    pub fn consumer(&self) -> &str {
        &self.consumer
    }

    /// The owner-local fork whose workflow identity signs stable artifacts.
    #[must_use]
    pub fn signer_fork(&self) -> &str {
        &self.signer_fork
    }

    /// The current accepted signer digest.
    #[must_use]
    pub fn current_digest(&self) -> &str {
        &self.current_digest
    }

    /// The bounded previous signer digest, if rotation is active.
    #[must_use]
    pub fn old_digest(&self) -> Option<&str> {
        self.old_digest.as_deref()
    }

    /// The previous signer activation time, if rotation is active.
    #[must_use]
    pub fn old_activated_at(&self) -> Option<&str> {
        self.old_activated_at.as_deref()
    }

    /// The previous signer expiry time, if rotation is active.
    #[must_use]
    pub fn old_expires_at(&self) -> Option<&str> {
        self.old_expires_at.as_deref()
    }
}

/// One release label and all rendered signer slots attached to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    calver: String,
    signer: Vec<SignerSlot>,
}

impl Release {
    /// The release CalVer label.
    #[must_use]
    pub fn calver(&self) -> &str {
        &self.calver
    }

    /// All signer slots in declaration order.
    #[must_use]
    pub fn signers(&self) -> &[SignerSlot] {
        &self.signer
    }

    /// Find a consumer's signer slot.
    #[must_use]
    pub fn signer_for(&self, consumer: &str) -> Option<&SignerSlot> {
        self.signer.iter().find(|slot| slot.consumer == consumer)
    }
}

/// The release table that owns current/old signer digest rotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTable {
    current: String,
    releases: Vec<Release>,
}

impl ReleaseTable {
    /// Load and validate `fleet/releases.toml` under `root`.
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join("fleet").join("releases.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Parse a release table from TOML text.
    pub fn parse(text: &str, source: &str) -> Result<Self, String> {
        let file: ReleasesFile =
            toml::from_str(text).map_err(|error| format!("parsing {source}: {error}"))?;
        if file.schema != RELEASES_SCHEMA {
            return Err(format!("unknown release table schema {:?}", file.schema));
        }
        if file.current.is_empty() || file.release.is_empty() {
            return Err("release table must declare current and release rows".to_string());
        }
        let mut versions = BTreeSet::new();
        let mut releases = Vec::with_capacity(file.release.len());
        for entry in file.release {
            if !valid_calver(&entry.calver) {
                return Err(format!("invalid release CalVer {:?}", entry.calver));
            }
            if !versions.insert(entry.calver.clone()) {
                return Err(format!("duplicate release {:?}", entry.calver));
            }
            let mut consumers = BTreeSet::new();
            let mut signers = Vec::with_capacity(entry.signer.len());
            if entry.signer.is_empty() {
                return Err(format!("release {} has no signer slots", entry.calver));
            }
            for slot in entry.signer {
                if !valid_slug(&slot.consumer) {
                    return Err(format!("invalid signer consumer {:?}", slot.consumer));
                }
                if !consumers.insert(slot.consumer.clone()) {
                    return Err(format!(
                        "release {} has duplicate signer consumer {:?}",
                        entry.calver, slot.consumer
                    ));
                }
                if !valid_segment(&slot.signer_fork) || !is_sha40(&slot.current_digest) {
                    return Err(format!(
                        "release {} has invalid signer fork or current digest for {}",
                        entry.calver, slot.consumer
                    ));
                }
                let old_fields = [
                    slot.old_digest.as_deref(),
                    slot.old_activated_at.as_deref(),
                    slot.old_expires_at.as_deref(),
                ];
                if old_fields.iter().any(Option::is_some) {
                    let (Some(old_digest), Some(activated), Some(expires)) =
                        (old_fields[0], old_fields[1], old_fields[2])
                    else {
                        return Err(format!(
                            "release {} has incomplete signer rotation for {}",
                            entry.calver, slot.consumer
                        ));
                    };
                    if !is_sha40(old_digest)
                        || old_digest == slot.current_digest
                        || !looks_rfc3339_utc(activated)
                        || !looks_rfc3339_utc(expires)
                    {
                        return Err(format!(
                            "release {} has invalid signer rotation for {}",
                            entry.calver, slot.consumer
                        ));
                    }
                }
                signers.push(SignerSlot {
                    consumer: slot.consumer,
                    signer_fork: slot.signer_fork,
                    current_digest: slot.current_digest,
                    old_digest: slot.old_digest,
                    old_activated_at: slot.old_activated_at,
                    old_expires_at: slot.old_expires_at,
                });
            }
            releases.push(Release {
                calver: entry.calver,
                signer: signers,
            });
        }
        if !versions.contains(&file.current) {
            return Err(format!(
                "current release {:?} is not declared in the release table",
                file.current
            ));
        }
        Ok(Self {
            current: file.current,
            releases,
        })
    }

    /// Return the release selected by the table's current pointer.
    #[must_use]
    pub fn current(&self) -> &Release {
        self.releases
            .iter()
            .find(|release| release.calver == self.current)
            .expect("validated current release exists")
    }

    /// Find an exact release label.
    #[must_use]
    pub fn by_calver(&self, calver: &str) -> Option<&Release> {
        self.releases
            .iter()
            .find(|release| release.calver == calver)
    }

    /// Validate consumer coverage and signer-fork membership against the fork table.
    pub fn validate_consumers(
        &self,
        consumers: impl IntoIterator<Item = String>,
        forks: &ForkTable,
    ) -> Result<(), String> {
        let expected: BTreeSet<_> = consumers.into_iter().collect();
        for release in &self.releases {
            let actual: BTreeSet<_> = release
                .signer
                .iter()
                .map(|slot| slot.consumer.clone())
                .collect();
            if actual != expected {
                return Err(format!(
                    "release {} signer consumers do not match package consumers",
                    release.calver
                ));
            }
            for slot in &release.signer {
                if forks.by_owner(&slot.signer_fork).is_none() {
                    return Err(format!(
                        "release {} signer fork {:?} is not in fleet/forks.toml",
                        release.calver, slot.signer_fork
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasesFile {
    schema: String,
    current: String,
    #[serde(default)]
    release: Vec<ReleaseEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseEntry {
    calver: String,
    #[serde(default)]
    signer: Vec<SignerEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignerEntry {
    consumer: String,
    signer_fork: String,
    current_digest: String,
    #[serde(default)]
    old_digest: Option<String>,
    #[serde(default)]
    old_activated_at: Option<String>,
    #[serde(default)]
    old_expires_at: Option<String>,
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_slug(value: &str) -> bool {
    let Some((owner, repository)) = value.split_once('/') else {
        return false;
    };
    valid_segment(owner) && valid_segment(repository) && !repository.contains('/')
}

fn valid_calver(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_sha40(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn looks_rfc3339_utc(value: &str) -> bool {
    value.len() == 20
        && value.ends_with('Z')
        && value.as_bytes()[4] == b'-'
        && value.as_bytes()[7] == b'-'
        && value.as_bytes()[10] == b'T'
        && value.as_bytes()[13] == b':'
        && value.as_bytes()[16] == b':'
        && value[..19]
            .bytes()
            .filter(|byte| byte.is_ascii_digit())
            .count()
            == 14
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn release_table_exposes_current_signer_slot() {
        let text = format!(
            r#"
schema = "velnor-actions.releases.v1"
current = "2026.8.33"

[[release]]
calver = "2026.8.33"

[[release.signer]]
consumer = "tailrocks/holla-apt"
signer_fork = "tailrocks"
current_digest = "{SHA}"
"#
        );
        let table = ReleaseTable::parse(&text, "test").unwrap();
        let slot = table.current().signer_for("tailrocks/holla-apt").unwrap();
        assert_eq!(slot.signer_fork(), "tailrocks");
        assert_eq!(slot.current_digest(), SHA);
        assert!(slot.old_digest().is_none());
    }

    #[test]
    fn incomplete_rotation_is_rejected() {
        let text = format!(
            r#"
schema = "velnor-actions.releases.v1"
current = "2026.8.33"

[[release]]
calver = "2026.8.33"

[[release.signer]]
consumer = "tailrocks/holla-apt"
signer_fork = "tailrocks"
current_digest = "{SHA}"
old_digest = "fedcba9876543210fedcba9876543210fedcba98"
"#
        );
        let error = ReleaseTable::parse(&text, "test").unwrap_err();
        assert!(error.contains("incomplete signer rotation"));
    }
}
