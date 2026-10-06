//! Repository visibility evidence: the attested public/private fact that
//! selects the runner provider for every emitted job.
//!
//! Acquisition and rendering are separate. This module (plus the
//! `visibility` subcommand) acquires the fact from authenticated repository
//! metadata — `gh api repos/<owner>/<name> --jq .visibility` — and records
//! it in the checked-in evidence file `.github-gen/visibility.toml`.
//! Rendering is a pure function of that file plus the generator revision:
//! the generator never probes the network (the same rule `runners.rs`
//! states for trust-gated availability), so `--check` and the policy
//! validator's regeneration at the declared pin are reproducible.
//!
//! Refresh: `velnor-workflow visibility refresh [--repo PATH]
//! [--repository owner/name]` queries the live visibility and writes the
//! evidence file. Validation, two points, both fail-closed:
//! `velnor-workflow visibility check` compares declared against live, and
//! `--check`/policy fail on any tree that does not render from the declared
//! evidence. A visibility change with no regen fails loudly at both points;
//! it never silently retains the wrong provider. (A generated CI workflow
//! asserting declared-against-live on every run is a deliberate follow-up,
//! not this increment: private trees run on Velnor fleet runners, where an
//! authenticated `gh` cannot be assumed the way hosted images provide it.)
//!
//! The evidence lives in its own file — not in `velnor-workflow.toml` — so
//! that a validator built before this policy reads the trees it knows
//! without tripping over an unknown table. Unknown, contradictory, or
//! unsupported evidence is an explicit error, never a silent default.
//!
//! Scope: enforcement reads this evidence on the schema-2 provider pipeline
//! only. The schema-1 `both` lane path is explicitly frozen (out of inc3
//! scope, see `RunnerMode`): a follow-up must enforce-or-remove it there,
//! and until then no new `both` surface may be added.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::GeneratorError;

/// The checked-in visibility evidence, relative to the repository root.
pub(crate) const VISIBILITY_EVIDENCE_PATH: &str = ".github-gen/visibility.toml";

/// The only visibilities the runner policy supports. GitHub knows a third
/// (`internal`); it is unsupported here and rejected explicitly — mapping it
/// silently to either provider would invent a placement the policy never
/// reviewed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Visibility {
    Public,
    Private,
}

impl Visibility {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }

    /// Strict parse: exactly `public` or `private`. No case folding, no
    /// whitespace trimming, no `internal` alias.
    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            "public" => Ok(Self::Public),
            "private" => Ok(Self::Private),
            _ => Err(unsupported_visibility(value)),
        }
    }

    #[must_use]
    pub(crate) fn is_public(self) -> bool {
        matches!(self, Self::Public)
    }
}

impl std::fmt::Display for Visibility {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn unsupported_visibility(value: &str) -> GeneratorError {
    GeneratorError::usage(format!(
        "unsupported repository visibility {value:?}; expected exactly \"public\" or \"private\" (GitHub \"internal\" is not supported: refresh with `velnor-workflow visibility refresh` after the repository leaves \"internal\")"
    ))
}

/// The parsed evidence file: the visibility plus the `owner/name` slug it
/// was acquired for. The slug binds the file to its repository: generation
/// rejects evidence whose slug contradicts `[generator] repository`, so a
/// file copied across repositories fails loudly instead of selecting the
/// wrong provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VisibilityFile {
    visibility: String,
    repository: String,
}

/// Validated visibility evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VisibilityEvidence {
    pub(crate) visibility: Visibility,
    pub(crate) repository: String,
}

/// The canonical evidence bytes `refresh` writes: sorted keys, one trailing
/// newline, no comments. `load` accepts any key order and ignores comments.
pub(crate) fn canonical_bytes(evidence: &VisibilityEvidence) -> String {
    format!(
        "repository = {:?}\nvisibility = {:?}\n",
        evidence.repository,
        evidence.visibility.as_str()
    )
}

fn evidence_path(root: &Path) -> PathBuf {
    root.join(VISIBILITY_EVIDENCE_PATH)
}

/// Repository-slug shape check for the evidence file: `owner/name`, both
/// sides non-empty, no whitespace. The full GitHub-name validation lives
/// with the generation config; this only guards the binding against typos.
fn validate_evidence_slug(repository: &str, path: &Path) -> Result<(), GeneratorError> {
    let mut segments = repository.split('/');
    let valid = matches!(
        (segments.next(), segments.next(), segments.next()),
        (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty()
    ) && !repository.chars().any(char::is_whitespace);
    if valid {
        return Ok(());
    }
    Err(GeneratorError::usage(format!(
        "invalid visibility evidence {}: `repository` must be `owner/name`, got {repository:?}",
        path.display()
    )))
}

/// Load and validate the checked-in evidence. A missing file, a malformed
/// file, or an unknown visibility is an explicit error naming the refresh;
/// nothing here guesses, probes, or defaults.
pub(crate) fn load(root: &Path) -> Result<VisibilityEvidence, GeneratorError> {
    let path = evidence_path(root);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(GeneratorError::usage(format!(
                "missing repository visibility evidence {}: declare it with `velnor-workflow visibility refresh --repo {} [--repository owner/name]` (queries authenticated `gh api repos/<owner>/<name> --jq .visibility`); rendering never probes the network, so generation refuses to guess",
                path.display(),
                root.display()
            )));
        }
        Err(error) => {
            return Err(GeneratorError::io(
                "read visibility evidence",
                &path,
                &error,
            ));
        }
    };
    let file = toml::from_str::<VisibilityFile>(&content).map_err(|error| {
        GeneratorError::usage(format!(
            "invalid visibility evidence {}: {error}",
            path.display()
        ))
    })?;
    validate_evidence_slug(&file.repository, &path)?;
    if file.visibility.trim() != file.visibility || file.visibility.is_empty() {
        return Err(unsupported_visibility(&file.visibility));
    }
    let visibility = Visibility::parse(&file.visibility)?;
    Ok(VisibilityEvidence {
        visibility,
        repository: file.repository,
    })
}

/// Reject evidence whose slug contradicts the generation config's slug. A
/// missing config slug cannot contradict; generation reads the evidence slug
/// as the binding in that case.
pub(crate) fn check_slug_binding(
    evidence: &VisibilityEvidence,
    config_repository: Option<&str>,
) -> Result<(), GeneratorError> {
    let Some(config) = config_repository.filter(|slug| !slug.is_empty()) else {
        return Ok(());
    };
    if config == evidence.repository {
        return Ok(());
    }
    Err(GeneratorError::usage(format!(
        "contradictory repository visibility evidence: {} names {:?}, but [generator] repository is {config:?}; refresh the evidence for this repository instead of copying it across repositories",
        VISIBILITY_EVIDENCE_PATH, evidence.repository
    )))
}

/// Query the live visibility over authenticated `gh api`. Transport failures
/// (missing `gh`, HTTP errors, unexpected payloads) are explicit errors:
/// an HTTP failure is not evidence of any visibility.
pub(crate) fn query_live_visibility(slug: &str) -> Result<String, GeneratorError> {
    let output = Command::new("gh")
        .args(["api", &format!("repos/{slug}"), "--jq", ".visibility"])
        .output()
        .map_err(|error| {
            GeneratorError::usage(format!(
                "cannot query repository visibility for {slug:?}: failed to run `gh api`: {error}; install and authenticate gh (`gh auth login`), then retry"
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        return Err(GeneratorError::usage(format!(
            "cannot query repository visibility for {slug:?}: `gh api repos/{slug}` failed{}; fix authentication or network access, then retry — an API failure never defaults to any visibility",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        )));
    }
    let visibility = String::from_utf8_lossy(&output.stdout);
    let visibility = visibility.trim().to_owned();
    if visibility.is_empty() {
        return Err(GeneratorError::usage(format!(
            "cannot query repository visibility for {slug:?}: `gh api repos/{slug} --jq .visibility` returned no value; retry once the API answers"
        )));
    }
    Ok(visibility)
}

/// Read the `[generator] repository` slug from the generation config without
/// committing to either config schema: refresh needs the slug before (and
/// without) full config validation.
fn generation_config_slug(root: &Path) -> Option<String> {
    let content = fs::read_to_string(root.join(".github-gen/velnor-workflow.toml")).ok()?;
    let table = content.parse::<toml::Table>().ok()?;
    table
        .get("generator")?
        .as_table()?
        .get("repository")?
        .as_str()
        .filter(|slug| !slug.is_empty())
        .map(str::to_owned)
}

/// Resolve the slug `refresh` queries: the explicit flag wins, then the
/// existing evidence file, then the generation config. Nothing is guessed.
fn refresh_slug(root: &Path, repository_override: Option<&str>) -> Result<String, GeneratorError> {
    if let Some(slug) = repository_override.filter(|slug| !slug.is_empty()) {
        return Ok(slug.to_owned());
    }
    let path = evidence_path(root);
    if let Ok(content) = fs::read_to_string(&path)
        && let Ok(file) = toml::from_str::<VisibilityFile>(&content)
        && !file.repository.is_empty()
    {
        return Ok(file.repository);
    }
    generation_config_slug(root).ok_or_else(|| {
        GeneratorError::usage(format!(
            "cannot refresh repository visibility evidence for {}: no repository slug found; pass `--repository owner/name` explicitly",
            root.display()
        ))
    })
}

/// Acquire the live visibility and write the canonical evidence file,
/// creating `.github-gen` when needed. Prints what it wrote.
pub(crate) fn refresh(
    root: &Path,
    repository_override: Option<&str>,
) -> Result<VisibilityEvidence, GeneratorError> {
    let slug = refresh_slug(root, repository_override)?;
    let live = query_live_visibility(&slug)?;
    let visibility = Visibility::parse(&live)?;
    let evidence = VisibilityEvidence {
        visibility,
        repository: slug,
    };
    let path = evidence_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| GeneratorError::io("create evidence directory", parent, &error))?;
    }
    fs::write(&path, canonical_bytes(&evidence))
        .map_err(|error| GeneratorError::io("write visibility evidence", &path, &error))?;
    println!(
        "visibility evidence for {}: {} ({})",
        evidence.repository,
        evidence.visibility.as_str(),
        path.display()
    );
    Ok(evidence)
}

/// Compare the declared evidence against the live visibility. A mismatch is
/// an explicit error naming both sides plus the two-step remediation
/// (refresh, then regen); it never rewrites anything by itself. The live
/// query and the staleness decision are separate steps so the decision
/// stays unit-testable without the network.
pub(crate) fn check(root: &Path) -> Result<(), GeneratorError> {
    let evidence = load(root)?;
    let live = query_live_visibility(&evidence.repository)?;
    check_evidence_against_live(root, &evidence, &live)
}

/// The pure staleness decision behind [`check`]: compare declared evidence
/// against one already-queried live answer. A mismatch is an explicit error
/// naming both sides plus the two-step remediation (refresh, then regen);
/// it never rewrites anything by itself.
fn check_evidence_against_live(
    root: &Path,
    evidence: &VisibilityEvidence,
    live: &str,
) -> Result<(), GeneratorError> {
    if live == evidence.visibility.as_str() {
        println!(
            "visibility evidence for {} matches live visibility ({})",
            evidence.repository,
            evidence.visibility.as_str()
        );
        return Ok(());
    }
    // Parse the live value strictly so an unexpected live answer (for
    // example `internal`) fails as unsupported rather than as a mismatch
    // against a guess.
    let live_visibility = Visibility::parse(live)?;
    Err(GeneratorError::usage(format!(
        "stale repository visibility evidence {}: declared {:?} for {}, but the live visibility is {:?}; refresh (`velnor-workflow visibility refresh --repo {}`) and regenerate the tree — a visibility change never silently retains the rendered provider",
        evidence_path(root).display(),
        evidence.visibility.as_str(),
        evidence.repository,
        live_visibility.as_str(),
        root.display()
    )))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]
    #![expect(
        clippy::unwrap_used,
        reason = "a test whose setup fails should panic loudly"
    )]
    use super::*;

    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error.to_string(),
        }
    }

    fn fixture_dir(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "velnor-visibility-{}-{name}",
            crate::unique_suffix()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".github-gen")).unwrap();
        root
    }

    fn write_evidence(root: &Path, content: &str) {
        std::fs::write(evidence_path(root), content).unwrap();
    }

    #[test]
    fn visibility_parses_exactly_and_rejects_aliases() {
        assert_eq!(Visibility::parse("public").unwrap(), Visibility::Public);
        assert_eq!(Visibility::parse("private").unwrap(), Visibility::Private);
        for alias in ["", "PUBLIC", "Public", " internal", "internal", "public "] {
            let error = must_fail(Visibility::parse(alias), "visibility alias");
            assert!(
                error.contains("unsupported repository visibility"),
                "unexpected error for `{alias}`: {error}"
            );
        }
    }

    #[test]
    fn load_accepts_canonical_evidence_and_ignores_comments() {
        let root = fixture_dir("load-ok");
        write_evidence(
            &root,
            "# acquired 2026-09-21\nvisibility = \"private\"\nrepository = \"example/secret\"\n",
        );
        let evidence = load(&root).unwrap();
        assert_eq!(evidence.visibility, Visibility::Private);
        assert_eq!(evidence.repository, "example/secret");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_rejects_missing_malformed_and_unknown_evidence() {
        let root = fixture_dir("load-bad");
        let error = must_fail(load(&root), "missing evidence");
        assert!(
            error.contains("missing repository visibility evidence")
                && error.contains("visibility refresh"),
            "{error}"
        );
        for (name, content, needle) in [
            (
                "malformed",
                "visibility = \n",
                "invalid visibility evidence",
            ),
            (
                "unknown-field",
                "visibility = \"public\"\nrepository = \"example/x\"\nchecked_at = \"now\"\n",
                "invalid visibility evidence",
            ),
            (
                "unknown-value",
                "visibility = \"internal\"\nrepository = \"example/x\"\n",
                "unsupported repository visibility",
            ),
            (
                "empty-value",
                "visibility = \"\"\nrepository = \"example/x\"\n",
                "unsupported repository visibility",
            ),
            (
                "missing-field",
                "visibility = \"public\"\n",
                "invalid visibility evidence",
            ),
            (
                "bad-slug",
                "visibility = \"public\"\nrepository = \"noslash\"\n",
                "must be `owner/name`",
            ),
        ] {
            write_evidence(&root, content);
            let error = must_fail(load(&root), name);
            assert!(error.contains(needle), "{name}: {error}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn slug_binding_rejects_cross_repository_copies() {
        let evidence = VisibilityEvidence {
            visibility: Visibility::Public,
            repository: "example/a".to_owned(),
        };
        assert!(check_slug_binding(&evidence, None).is_ok());
        assert!(check_slug_binding(&evidence, Some("")).is_ok());
        assert!(check_slug_binding(&evidence, Some("example/a")).is_ok());
        let error = must_fail(
            check_slug_binding(&evidence, Some("example/b")),
            "contradictory slug",
        );
        assert!(error.contains("contradictory"), "{error}");
    }

    #[test]
    fn staleness_decision_accepts_a_match_and_rejects_drift() {
        let root = fixture_dir("stale-decision");
        let evidence = VisibilityEvidence {
            visibility: Visibility::Public,
            repository: "example/a".to_owned(),
        };
        assert!(check_evidence_against_live(&root, &evidence, "public").is_ok());
        let stale = must_fail(
            check_evidence_against_live(&root, &evidence, "private"),
            "declared public, live private",
        );
        assert!(
            stale.contains("stale repository visibility evidence") && stale.contains("refresh"),
            "{stale}"
        );
        let unsupported = must_fail(
            check_evidence_against_live(&root, &evidence, "internal"),
            "live internal",
        );
        assert!(
            unsupported.contains("unsupported repository visibility"),
            "{unsupported}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn canonical_bytes_are_stable() {
        let evidence = VisibilityEvidence {
            visibility: Visibility::Private,
            repository: "example/a".to_owned(),
        };
        assert_eq!(
            canonical_bytes(&evidence),
            "repository = \"example/a\"\nvisibility = \"private\"\n"
        );
    }

    #[test]
    fn refresh_slug_prefers_flag_then_evidence_then_config() {
        let root = fixture_dir("refresh-slug");
        let error = must_fail(refresh_slug(&root, None), "no slug anywhere");
        assert!(error.contains("--repository"), "{error}");
        std::fs::write(
            root.join(".github-gen/velnor-workflow.toml"),
            "schema = 2\n\n[generator]\nrepository = \"example/config\"\n",
        )
        .unwrap();
        assert_eq!(refresh_slug(&root, None).unwrap(), "example/config");
        write_evidence(
            &root,
            "repository = \"example/evidence\"\nvisibility = \"public\"\n",
        );
        assert_eq!(refresh_slug(&root, None).unwrap(), "example/evidence");
        assert_eq!(
            refresh_slug(&root, Some("example/flag")).unwrap(),
            "example/flag"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
