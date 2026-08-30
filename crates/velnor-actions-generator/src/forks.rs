//! Validated owner-local Velnor Actions fork data.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

const FORKS_SCHEMA: &str = "velnor-actions.forks.v1";

/// One owner-local mirror and its consumer-template release placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fork {
    owner: String,
    placeholder: String,
}

impl Fork {
    /// The GitHub organization owning this mirror.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The release SHA placeholder rendered for this mirror.
    #[must_use]
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }

    /// The safe job identifier used by generated workflow fan-out jobs.
    #[must_use]
    pub fn job_id(&self) -> String {
        self.owner.replace('-', "_").to_ascii_lowercase()
    }
}

/// The complete, ordered owner-local mirror set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkTable {
    repository: String,
    canonical_owner: String,
    forks: Vec<Fork>,
}

impl ForkTable {
    /// Load and validate `fleet/forks.toml` under `root`.
    pub fn load(root: &Path) -> Result<Self, String> {
        let path = root.join("fleet").join("forks.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Parse a fork table from TOML text.
    pub fn parse(text: &str, source: &str) -> Result<Self, String> {
        let file: ForksFile =
            toml::from_str(text).map_err(|error| format!("parsing {source}: {error}"))?;
        if file.schema != FORKS_SCHEMA {
            return Err(format!("unknown fork table schema {:?}", file.schema));
        }
        if file.repository.is_empty()
            || !valid_segment(&file.repository)
            || file.canonical_owner.is_empty()
            || !valid_segment(&file.canonical_owner)
        {
            return Err("fork table has an invalid repository or canonical_owner".to_string());
        }
        if file.fork.is_empty() {
            return Err("fork table must declare at least one fork".to_string());
        }

        let mut owners = BTreeSet::new();
        let mut placeholders = BTreeSet::new();
        let mut job_ids = BTreeSet::new();
        let mut forks = Vec::with_capacity(file.fork.len());
        for entry in file.fork {
            if !valid_segment(&entry.owner) {
                return Err(format!("invalid fork owner {:?}", entry.owner));
            }
            if !owners.insert(entry.owner.clone()) {
                return Err(format!("duplicate fork owner {:?}", entry.owner));
            }
            if !valid_placeholder(&entry.placeholder) {
                return Err(format!("invalid fork placeholder {:?}", entry.placeholder));
            }
            if !placeholders.insert(entry.placeholder.clone()) {
                return Err(format!(
                    "duplicate fork placeholder {:?}",
                    entry.placeholder
                ));
            }
            let fork = Fork {
                owner: entry.owner,
                placeholder: entry.placeholder,
            };
            if !job_ids.insert(fork.job_id()) {
                return Err("fork owners collide after job-id normalization".to_string());
            }
            forks.push(fork);
        }
        if !owners.contains(&file.canonical_owner) {
            return Err(format!(
                "canonical owner {:?} is not declared in the fork table",
                file.canonical_owner
            ));
        }
        Ok(Self {
            repository: file.repository,
            canonical_owner: file.canonical_owner,
            forks,
        })
    }

    /// Return the checked-in canonical table used by compatibility render helpers.
    #[must_use]
    pub fn canonical() -> Self {
        Self::parse(
            include_str!("../../../fleet/forks.toml"),
            "embedded fleet/forks.toml",
        )
        .expect("embedded fork table is valid")
    }

    /// The mirrored repository name.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// The canonical mirror owner.
    #[must_use]
    pub fn canonical_owner(&self) -> &str {
        &self.canonical_owner
    }

    /// All forks in their declared rendering order.
    #[must_use]
    pub fn forks(&self) -> &[Fork] {
        &self.forks
    }

    /// The number of owner-local mirrors.
    #[must_use]
    pub fn len(&self) -> usize {
        self.forks.len()
    }

    /// Whether no owner-local mirrors are declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.forks.is_empty()
    }

    /// Find the declared fork for an owner.
    #[must_use]
    pub fn by_owner(&self, owner: &str) -> Option<&Fork> {
        self.forks.iter().find(|fork| fork.owner == owner)
    }

    /// Return owners in stable lexical order for shell truth tables.
    #[must_use]
    pub fn sorted_owners(&self) -> Vec<&str> {
        let mut owners = self.forks.iter().map(Fork::owner).collect::<Vec<_>>();
        owners.sort_unstable();
        owners
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForksFile {
    schema: String,
    repository: String,
    canonical_owner: String,
    #[serde(default)]
    fork: Vec<ForkEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForkEntry {
    owner: String,
    placeholder: String,
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_placeholder(value: &str) -> bool {
    value.len() >= 4
        && value.starts_with('@')
        && value.ends_with('@')
        && value[1..value.len() - 1]
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_table_has_one_placeholder_per_owner() {
        let table = ForkTable::canonical();
        assert_eq!(table.repository(), "velnor-actions");
        assert_eq!(table.canonical_owner(), "tailrocks");
        assert_eq!(table.len(), 3);
        assert_eq!(
            table
                .forks()
                .iter()
                .map(Fork::placeholder)
                .collect::<BTreeSet<_>>()
                .len(),
            table.len()
        );
    }

    #[test]
    fn normalized_job_id_collisions_are_rejected() {
        let text = r#"
schema = "velnor-actions.forks.v1"
repository = "velnor-actions"
canonical_owner = "a-b"

[[fork]]
owner = "a-b"
placeholder = "@A_B@"

[[fork]]
owner = "a_b"
placeholder = "@A_C@"
"#;
        let error = ForkTable::parse(text, "test").unwrap_err();
        assert!(error.contains("job-id normalization"));
    }
}
