//! Fleet data model and validation.
//!
//! This module owns the parsed, validated fleet: the 24-member repository
//! manifest and the five class contracts. Parsing is fail-closed — an invalid
//! slug, a non-40-hex SHA, a duplicate or unclassified member, a wrong per-class
//! count, an empty gate command, or an implicit gate applicability all reject
//! before any output is accepted.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

use crate::RepositoryClass;

/// The three recognized fleet organizations, in canonical rendering order.
pub const OWNERS: [&str; 3] = ["jackin-project", "tailrocks", "ChainArgos"];

/// The code class's standard repository-owned project command. Repository-specific
/// work composes beneath this single checked-in task; the shared workflow never
/// branches on repository slug.
pub const CODE_STANDARD_COMMAND: &str = "mise run ci";

/// Required member count per class, in canonical [`crate::ALL_CLASSES`] order:
/// 16 code, 4 tap, 2 apt, 1 infra, 1 fixture (total 24).
pub const REQUIRED_COUNTS: [(RepositoryClass, usize); 5] = [
    (RepositoryClass::Code, 16),
    (RepositoryClass::Tap, 4),
    (RepositoryClass::Apt, 2),
    (RepositoryClass::Infra, 1),
    (RepositoryClass::Fixture, 1),
];

/// The five ordered universal gate names every class declares.
pub const GATE_NAMES: [&str; 5] = ["install", "build", "test", "lint", "format"];

/// One lane selector. `Velnor` is the safe default; `Both` reports each lane
/// independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// The self-hosted Velnor lane (default).
    Velnor,
    /// The GitHub-hosted lane.
    GitHub,
    /// Both lanes, reported independently.
    Both,
}

impl Lane {
    /// Parse a lane selector from its lowercase token.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Lane> {
        match token {
            "velnor" => Some(Lane::Velnor),
            "github" => Some(Lane::GitHub),
            "both" => Some(Lane::Both),
            _ => None,
        }
    }

    /// The lowercase selector token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Lane::Velnor => "velnor",
            Lane::GitHub => "github",
            Lane::Both => "both",
        }
    }
}

/// Resolve a lane selector to the concrete execution lanes it runs. `Both`
/// expands to the Velnor lane then the GitHub lane; each runs independently and a
/// failure on one is never substituted by the other.
#[must_use]
pub fn resolve_lanes(selector: Lane) -> Vec<Lane> {
    match selector {
        Lane::Velnor => vec![Lane::Velnor],
        Lane::GitHub => vec![Lane::GitHub],
        Lane::Both => vec![Lane::Velnor, Lane::GitHub],
    }
}

/// Whether a gate is lane-portable or platform-only. Explicit for every gate;
/// there is no implicit default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applicability {
    /// Runs on both the Velnor lane and the GitHub-hosted lane.
    Both,
    /// Platform-only: runs only on the GitHub-hosted lane. Not applicable on the
    /// Velnor lane and never misreported there as an equivalent Velnor check.
    Github,
}

impl Applicability {
    fn from_token(token: &str) -> Result<Applicability, String> {
        match token {
            "both" => Ok(Applicability::Both),
            "github" => Ok(Applicability::Github),
            other => Err(format!(
                "invalid applicability {other:?} (expected \"both\" or \"github\")"
            )),
        }
    }

    /// Whether this gate is applicable on the given concrete lane.
    #[must_use]
    pub fn applies_on(self, lane: Lane) -> bool {
        match (self, lane) {
            (Applicability::Both, _) => true,
            (Applicability::Github, Lane::GitHub) => true,
            (Applicability::Github, _) => false,
        }
    }
}

/// One named CI gate with its exact command and explicit applicability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// Gate name (install, build, test, lint, or format).
    pub name: String,
    /// Exact non-empty command executed by the `run-gate` building block.
    pub command: String,
    /// Explicit lane applicability.
    pub applicability: Applicability,
}

/// The shared contract for one repository class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassContract {
    /// The class this contract governs.
    pub class: RepositoryClass,
    /// The five ordered gates (install, build, test, lint, format).
    pub gates: Vec<Gate>,
    /// Whether the class carries any genuinely platform-only gate.
    pub platform_only: bool,
}

impl ClassContract {
    /// The gates that are applicable on the given concrete lane.
    #[must_use]
    pub fn applicable_gates(&self, lane: Lane) -> Vec<&Gate> {
        self.gates
            .iter()
            .filter(|g| g.applicability.applies_on(lane))
            .collect()
    }
}

/// One fleet member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// The `owner/repo` slug.
    pub slug: String,
    /// The owning organization (the slug's owner segment).
    pub organization: String,
    /// The normalized class.
    pub class: RepositoryClass,
    /// The immutable 40-hex default-branch baseline commit.
    pub baseline_sha: String,
}

impl Repository {
    /// The repository name (the slug's repo segment).
    #[must_use]
    pub fn name(&self) -> &str {
        self.slug
            .split_once('/')
            .map_or(self.slug.as_str(), |(_, name)| name)
    }
}

/// The fully validated fleet: 24 repositories and five class contracts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetManifest {
    repositories: Vec<Repository>,
    classes: Vec<ClassContract>,
}

impl FleetManifest {
    /// Load and validate the fleet from `fleet/repositories.toml` and
    /// `fleet/classes.toml` under `root`.
    ///
    /// # Errors
    ///
    /// Returns `Err` for any I/O failure, TOML syntax error, or contract
    /// violation (invalid slug/SHA, duplicate or unclassified member, wrong
    /// count, empty gate command, or implicit/invalid applicability).
    pub fn load(root: &Path) -> Result<FleetManifest, String> {
        let repositories = load_repositories(&root.join("fleet").join("repositories.toml"))?;
        let classes = load_classes(&root.join("fleet").join("classes.toml"))?;
        Ok(FleetManifest {
            repositories,
            classes,
        })
    }

    /// All 24 repositories, in declaration order.
    #[must_use]
    pub fn repositories(&self) -> &[Repository] {
        &self.repositories
    }

    /// The five class contracts, in canonical [`crate::ALL_CLASSES`] order.
    #[must_use]
    pub fn classes(&self) -> &[ClassContract] {
        &self.classes
    }

    /// The contract for one class.
    #[must_use]
    pub fn class(&self, class: RepositoryClass) -> &ClassContract {
        self.classes
            .iter()
            .find(|c| c.class == class)
            .expect("every class present after validation")
    }

    /// The members of one class, in declaration order.
    #[must_use]
    pub fn members_of(&self, class: RepositoryClass) -> Vec<&Repository> {
        self.repositories
            .iter()
            .filter(|r| r.class == class)
            .collect()
    }
}

/// Whether `value` is exactly 40 lowercase hexadecimal characters.
#[must_use]
pub fn is_sha40(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn valid_slug_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoriesFile {
    #[serde(default)]
    repository: Vec<RepoEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoEntry {
    slug: String,
    class: String,
    baseline_sha: String,
}

fn load_repositories(path: &Path) -> Result<Vec<Repository>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let file: RepositoriesFile =
        toml::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))?;

    let mut repositories = Vec::with_capacity(file.repository.len());
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for entry in file.repository {
        let (owner, name) = entry
            .slug
            .split_once('/')
            .ok_or_else(|| format!("invalid slug {:?}: expected owner/repo", entry.slug))?;
        if !valid_slug_segment(owner) || !valid_slug_segment(name) || name.contains('/') {
            return Err(format!("invalid slug {:?}", entry.slug));
        }
        if !is_sha40(&entry.baseline_sha) {
            return Err(format!(
                "member {:?} baseline_sha {:?} is not 40 lowercase hex",
                entry.slug, entry.baseline_sha
            ));
        }
        let class = parse_class(&entry.class).ok_or_else(|| {
            format!(
                "member {:?} has unknown class {:?}",
                entry.slug, entry.class
            )
        })?;
        if !seen.insert(entry.slug.clone()) {
            return Err(format!("duplicate member {:?}", entry.slug));
        }
        repositories.push(Repository {
            slug: entry.slug.clone(),
            organization: owner.to_string(),
            class,
            baseline_sha: entry.baseline_sha,
        });
    }

    for &(class, want) in &REQUIRED_COUNTS {
        let have = repositories.iter().filter(|r| r.class == class).count();
        if have != want {
            return Err(format!(
                "class {} has {have} members, expected {want}",
                class.code()
            ));
        }
    }
    let total: usize = REQUIRED_COUNTS.iter().map(|&(_, n)| n).sum();
    if repositories.len() != total {
        return Err(format!(
            "fleet has {} members, expected {total}",
            repositories.len()
        ));
    }

    // Reject any organization outside the three recognized owners.
    for repo in &repositories {
        if !OWNERS.contains(&repo.organization.as_str()) {
            return Err(format!(
                "member {:?} has unknown organization {:?}",
                repo.slug, repo.organization
            ));
        }
    }

    Ok(repositories)
}

fn parse_class(token: &str) -> Option<RepositoryClass> {
    crate::ALL_CLASSES
        .iter()
        .copied()
        .find(|c| c.code() == token)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassesFile {
    code: ClassEntry,
    tap: ClassEntry,
    apt: ClassEntry,
    infra: ClassEntry,
    fixture: ClassEntry,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassEntry {
    platform_only: bool,
    install: GateEntry,
    build: GateEntry,
    test: GateEntry,
    lint: GateEntry,
    format: GateEntry,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GateEntry {
    command: String,
    applicability: String,
}

fn load_classes(path: &Path) -> Result<Vec<ClassContract>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let file: ClassesFile =
        toml::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))?;

    let entries = [
        (RepositoryClass::Code, file.code),
        (RepositoryClass::Tap, file.tap),
        (RepositoryClass::Apt, file.apt),
        (RepositoryClass::Infra, file.infra),
        (RepositoryClass::Fixture, file.fixture),
    ];

    let mut classes = Vec::with_capacity(entries.len());
    for (class, entry) in entries {
        let ordered = [
            ("install", entry.install),
            ("build", entry.build),
            ("test", entry.test),
            ("lint", entry.lint),
            ("format", entry.format),
        ];
        let mut gates = Vec::with_capacity(ordered.len());
        let mut platform_seen = false;
        for (name, spec) in ordered {
            if spec.command.trim().is_empty() {
                return Err(format!(
                    "class {} gate {name:?} has an empty command",
                    class.code()
                ));
            }
            let applicability = Applicability::from_token(&spec.applicability)
                .map_err(|e| format!("class {} gate {name:?}: {e}", class.code()))?;
            if applicability == Applicability::Github {
                platform_seen = true;
            }
            gates.push(Gate {
                name: name.to_string(),
                command: spec.command,
                applicability,
            });
        }
        if entry.platform_only != platform_seen {
            return Err(format!(
                "class {} declares platform_only={} but {} a platform-only gate",
                class.code(),
                entry.platform_only,
                if platform_seen { "has" } else { "has no" }
            ));
        }
        classes.push(ClassContract {
            class,
            gates,
            platform_only: entry.platform_only,
        });
    }

    Ok(classes)
}
