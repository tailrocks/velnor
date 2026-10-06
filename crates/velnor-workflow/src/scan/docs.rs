//! Documentation detector: one verification unit for Markdown sources.

use std::path::Path;

use super::file_walk::has_extension;
use super::{unit, RepositoryShape, ScanContext};
use crate::{CachePurpose, CacheSpec, UnitKind};

/// The markdownlint-cli2 configuration files the test command discovers on
/// its own. A repository opts into Markdown linting by checking one in.
const MARKDOWNLINT_CONFIGS: &[&str] = &[
    ".markdownlint-cli2.yaml",
    ".markdownlint-cli2.yml",
    ".markdownlint-cli2.jsonc",
    ".markdownlint-cli2.json",
];

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    let has_markdown = context.files.iter().any(|file| has_extension(file, "md"));
    let has_lint_contract = context.files.iter().any(|file| {
        Path::new(file)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| MARKDOWNLINT_CONFIGS.contains(&name))
    });
    // The unit runs the linter with default discovery: without a checked-in
    // contract the default rule set fails repositories that never adopted
    // Markdown linting, so the unit exists only where the contract does.
    if has_markdown && has_lint_contract {
        shape.units.push(unit(
            UnitKind::Docs,
            ".",
            vec![
                "**/*.md".to_owned(),
                "mkdocs.yml".to_owned(),
                "docs/**".to_owned(),
            ],
            vec![
                "npx --yes markdownlint-cli2@0.20.0 \"**/*.md\" \"#node_modules\" \"#**/AGENTS.md\" \"#**/CLAUDE.md\" \"#target\" \"#**/target/**\" \"#dist\" \"#coverage\" \"#**/.cache/**\" \"#migrations/**\"".to_owned(),
            ],
            Some(CacheSpec {
                key_files: vec!["package-lock.json".to_owned(), "bun.lock".to_owned()],
                paths: vec!["~/.npm".to_owned()],
                purpose: CachePurpose::Generic,
                mbx_output_cache_justification: None,
                mutable_mount_seed: false,
            }),
        ));
    }
}
