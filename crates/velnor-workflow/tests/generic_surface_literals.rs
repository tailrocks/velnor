//! The law under test: the crate names families, units, and the generator's
//! own distribution paths — never a consumer repository. Every literal here
//! was deleted from the crate during the estate ports; if one reaches the
//! generic engine again, the scan + config split is broken.
//!
//! The one admitted boundary is `src/estate.rs`: it still carries the catalog
//! entries of consumers that have not adopted a repo-owned generation config
//! yet, and each of those entries is a port away from this file.

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
    // Estate unit ids and template families.
    "rust-velnor",
    "bun-velnor",
    "rust-production-topology",
    "package-release.v1",
];

/// The estate boundary: the only module allowed to name a repository, plus the
/// files whose bytes document the rule itself rather than engine behavior.
/// This test is one of them: the deny list has to spell out what it forbids.
const ADMITTED_FILES: &[&str] = &[
    "src/estate.rs",
    "tests/generic_surface_literals.rs",
    "AGENTS.md",
    "CLAUDE.md",
];

/// The `termrock` toolkit enters the crate as a dev-dependency of the TUI; the
/// dependency URL is generator identity, the same class as the generator's own
/// distribution paths. Everything else in the manifest is scanned like code.
const CARGO_TOML: &str = "Cargo.toml";
const ADMITTED_CARGO_LINE_MARKERS: &[&str] = &["termrock = { git"];

/// The generator's own distribution paths (`tailrocks/velnor/.github/...`) and
/// the regeneration marker are generator identity, not consumer knowledge. The
/// bare slug may appear exactly `BARE_GENERATOR_SLUG_OCCURRENCES` times: the
/// pinned install URL, the inline policy transport's documentation of the
/// reusable-call hazard it replaced, and the regeneration marker constant.
const BARE_GENERATOR_SLUG_OCCURRENCES: usize = 3;

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
/// code allowed to name a repository.
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

/// Occurrences of the generator slug that are part of a `.github/...` path are
/// the generator pointing at its own infrastructure.
fn is_generator_path(line: &str) -> bool {
    let mut rest = line;
    while let Some(start) = rest.find("tailrocks/velnor") {
        rest = &rest[start + "tailrocks/velnor".len()..];
        if rest.starts_with('/') || rest.starts_with(".github") {
            return true;
        }
    }
    false
}

#[test]
fn generic_modules_never_name_a_repository() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    let mut bare_slug_sites = Vec::new();
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
        let normalized = normalized_text(&source);
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
                if line.contains(literal) {
                    offenders.push(format!(
                        "{}:{} names `{literal}`",
                        path.display(),
                        number + 1
                    ));
                }
            }
            if line.contains("tailrocks/velnor") && !is_generator_path(line) {
                bare_slug_sites.push(format!("{}:{}", path.display(), number + 1));
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
        "the bare generator slug must stay pinned to the regeneration marker: {bare_slug_sites:?}"
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
