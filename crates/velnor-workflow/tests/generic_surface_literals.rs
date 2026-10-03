//! The law under test: the crate names families, units, and the generator's
//! own distribution paths — never a consumer repository. Every literal here
//! was deleted from the crate during the estate ports; if one reaches the
//! generic engine again, the scan + config split is broken.
//!
//! The one admitted code boundary is `src/estate.rs`: it still carries the
//! legacy runner selector the earliest generated surfaces embedded, which
//! static-template adoption replaces with the adopting repository's own
//! selector.

#![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]

use std::path::{Path, PathBuf};

/// Repository slugs and estate identifiers this crate must never name again.
/// Each entry is one deleted profile, template family, unit id, or runner
/// placement; the list only ever grows. The list covers every repository on
/// the legacy table, so a new consumer means a new entry here in the same
/// change.
const DENY_LIST: &[&str] = &[
    // Consumer repositories, owner blocks, and their path fragments.
    "tailrocks/velnor-apt",
    "velnor-apt",
    "velnor-actions-fixture",
    "tailrocks/holla",
    "holla-apt",
    "holla",
    "jackin",
    "tailrocks/termrock",
    "tailrocks/parallax",
    "parallax",
    "ruxel",
    "chainargos",
    "tailrocks/schemalane",
    "schemalane",
    "tailrocks/tablerock",
    "tablerock",
    "tailrocks/pg-bigdecimal",
    "pg-bigdecimal",
    "tailrocks/tracing-request-level",
    "tracing-request-level",
    "tailrocks/cloudflare-tofu",
    "cloudflare-tofu",
    "github-terraform",
    "tailrocks/tailrocks-skills",
    "tailrocks-skills",
    "homebrew-tablerock",
    "homebrew-holla",
    "homebrew-parallax",
    "homebrew-ruxel",
    "parallax-telemetry-playground",
    "agent-brown",
    "agent-smith",
    "agent-sentinel",
    "jackin-the-architect",
    "java-monorepo",
    // Estate runner placement.
    "velnor-trusted",
    "velnor-target-mvp",
    "velnor-host-docker",
    // The self-consumer's own job image and Dockerfile. They live in the
    // unscanned `.github-gen/` tree and reach generated output only as
    // consumer-declared data; the engine must never spell them.
    "velnor-job-ubuntu",
    "job-ubuntu.Dockerfile",
    // Estate unit ids and template families.
    "rust-velnor",
    "bun-velnor",
    "rust-production-topology",
    "package-release.v1",
];

/// The estate boundary: the one module allowed to name the retired estate
/// selector, plus the files whose bytes document the rule itself rather than
/// engine behavior. This test is one of them: the deny list has to spell out
/// what it forbids.
const ADMITTED_FILES: &[&str] = &[
    "src/estate.rs",
    "src/s2/estate.rs",
    "tests/generic_surface_literals.rs",
    "AGENTS.md",
];

/// The `termrock` toolkit enters the crate as a dev-dependency of the TUI; the
/// dependency URL is generator identity, the same class as the generator's own
/// distribution paths. Everything else in the manifest is scanned like code.
const CARGO_TOML: &str = "Cargo.toml";
const ADMITTED_CARGO_LINE_MARKERS: &[&str] = &["termrock = { git"];

/// The generator's own distribution paths (`tailrocks/velnor/.github/...`) and
/// repository arguments are generator identity, not consumer knowledge. The
/// bare slug may appear exactly `BARE_GENERATOR_SLUG_OCCURRENCES` times: the
/// pinned install URL and regeneration marker in each engine, plus two
/// `gh release download --repo` arguments in each bootstrap renderer.
const BARE_GENERATOR_SLUG_OCCURRENCES: usize = 8;
const GENERATOR_REPOSITORY_SLUG: &str = "tailrocks/velnor";

/// The bootstrap transport test models the generator repository as its
/// consumer, so its repository constant and two config fixtures name the
/// generator repository. Keep those sites separate from literals in generator
/// code.
const GENERATOR_SLUG_FIXTURE_FILES: &[&str] = &["tests/bootstrap_transport.rs"];
const BOOTSTRAP_FIXTURE_GENERATOR_SLUG_OCCURRENCES: usize = 3;

/// Everything the deny list applies to: the crate's Rust sources, its
/// templates, its tests and fixtures, its build scripts, benches, examples,
/// and the manifests and readme a contributor reads first.
fn scanned_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![
        root.join("src"),
        root.join("templates"),
        root.join("tests"),
        root.join("benches"),
        root.join("examples"),
    ];
    for name in ["build.rs", CARGO_TOML, "README.md"] {
        let path = root.join(name);
        if path.is_file() {
            files.push(path);
        }
    }
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("read source directory {}: {error}", current.display()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

/// Whether a path is exempt from the deny list. The estate module is the only
/// code allowed to name the retired estate runner selector.
fn admitted(root: &Path, path: &Path) -> bool {
    let _ = root;
    ADMITTED_FILES.iter().any(|file| path.ends_with(file))
}

/// Strip everything that is not part of a slug, path, or identifier, and fold
/// to lowercase. A literal split across string fragments (`concat!`, adjacent
/// pieces, line breaks) recombines here, so splitting a name no longer hides
/// it.
fn normalized_text(source: &str) -> String {
    source
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
        })
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Phase 1 collapsed verify jobs use a generic trusted-lane id that happens to
/// contain the retired estate runner label as a substring.
fn normalized_for_deny_scan(source: &str) -> String {
    normalized_text(source).replace("verify-velnor-trusted", "verify-trusted-lane")
}

fn is_admitted_literal_site(literal: &str, line: &str) -> bool {
    literal == "velnor-trusted" && line.contains("verify-velnor-trusted")
}

/// Remove the admitted dependency declarations from `Cargo.toml` before the
/// scan; what is left is scanned whole like every other file.
fn cargo_toml_scan_text(root: &Path) -> Option<String> {
    let source = std::fs::read_to_string(root.join(CARGO_TOML)).ok()?;
    let kept = source
        .lines()
        .filter(|line| {
            !ADMITTED_CARGO_LINE_MARKERS
                .iter()
                .any(|marker| line.contains(marker))
        })
        .collect::<Vec<&str>>()
        .join("\n");
    Some(kept)
}

/// Columns of bare slug occurrences, excluding each occurrence that continues
/// into a slash-delimited repository path. Inspect matches individually so a
/// path elsewhere on the same line cannot hide a bare literal or file suffix.
fn bare_generator_slug_columns(line: &str) -> Vec<usize> {
    let mut columns = Vec::new();
    let mut offset = 0;
    while let Some(relative_start) = line[offset..].find(GENERATOR_REPOSITORY_SLUG) {
        let start = offset + relative_start;
        let end = start + GENERATOR_REPOSITORY_SLUG.len();
        let suffix = &line[end..];
        if !suffix.starts_with('/') {
            columns.push(line[..start].chars().count() + 1);
        }
        offset = end;
    }
    columns
}

fn is_generator_slug_fixture(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    GENERATOR_SLUG_FIXTURE_FILES
        .iter()
        .any(|file| relative == Path::new(file))
}

#[test]
fn generator_slug_scan_counts_matches_not_lines() {
    let line = "tailrocks/velnor/.github/actions/a tailrocks/velnor --repo tailrocks/velnor";
    assert_eq!(bare_generator_slug_columns(line).len(), 2);
}

#[test]
fn generator_slug_scan_requires_a_path_separator() {
    assert_eq!(
        bare_generator_slug_columns("tailrocks/velnor.github").len(),
        1
    );
    assert!(bare_generator_slug_columns("tailrocks/velnor/.github/workflows/ci.yml").is_empty());
}

#[test]
fn generic_modules_never_name_a_repository() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    let mut bare_slug_sites = Vec::new();
    let mut fixture_slug_sites = Vec::new();
    for path in scanned_files(root) {
        if admitted(root, &path) {
            continue;
        }
        let source = if path.ends_with(CARGO_TOML) {
            cargo_toml_scan_text(root).unwrap_or_default()
        } else {
            std::fs::read_to_string(&path).unwrap_or_default()
        };
        // Whole-file scan: catches split and concatenated literals.
        let normalized = normalized_for_deny_scan(&source);
        for literal in DENY_LIST {
            if normalized.contains(&normalized_text(literal)) {
                offenders.push(format!(
                    "{} names `{literal}` (normalized content match)",
                    path.display()
                ));
            }
        }
        // Line scan: names the exact offending site when the literal is whole.
        for (number, line) in source.lines().enumerate() {
            for literal in DENY_LIST {
                if line.contains(literal) && !is_admitted_literal_site(literal, line) {
                    offenders.push(format!(
                        "{}:{} names `{literal}`",
                        path.display(),
                        number + 1
                    ));
                }
            }
            for column in bare_generator_slug_columns(line) {
                let site = format!("{}:{}:{column}", path.display(), number + 1);
                if is_generator_slug_fixture(root, &path) {
                    fixture_slug_sites.push(site);
                } else {
                    bare_slug_sites.push(site);
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "repository literals outside the estate modules:\n{}",
        offenders.join("\n")
    );
    assert_eq!(
        bare_slug_sites.len(),
        BARE_GENERATOR_SLUG_OCCURRENCES,
        "unexpected bare generator slug occurrences in engine source: {bare_slug_sites:?}"
    );
    assert_eq!(
        fixture_slug_sites.len(),
        BOOTSTRAP_FIXTURE_GENERATOR_SLUG_OCCURRENCES,
        "bootstrap fixtures must keep only their declared generator repository literals: {fixture_slug_sites:?}"
    );
}

/// The admitted set is not an excuse: the estate boundary has to be there, and
/// the files it covers cannot silently disappear or move.
#[test]
fn the_admitted_estate_modules_exist() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in ADMITTED_FILES {
        assert!(
            root.join(file).is_file(),
            "admitted module {file} is gone; revisit the deny list"
        );
    }
}
