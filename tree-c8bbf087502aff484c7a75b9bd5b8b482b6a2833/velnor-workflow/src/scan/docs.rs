//! Documentation detector: one verification unit for Markdown sources.

use super::file_walk::has_extension;
use super::{unit, RepositoryShape, ScanContext};
use crate::{CacheSpec, UnitKind};

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    if context.files.iter().any(|file| has_extension(file, "md")) {
        shape.units.push(unit(
            UnitKind::Docs,
            ".",
            vec![
                "**/*.md".to_owned(),
                "mkdocs.yml".to_owned(),
                "docs/**".to_owned(),
            ],
            vec![
                "npx --yes markdownlint-cli2@0.20.0 \"**/*.md\" \"#node_modules\" \"#**/AGENTS.md\" \"#**/CLAUDE.md\" \"#target\" \"#**/target/**\" \"#dist\" \"#coverage\" \"#**/.cache/**\"".to_owned(),
            ],
            Some(CacheSpec {
                key_files: vec!["package-lock.json".to_owned(), "bun.lock".to_owned()],
                paths: vec!["~/.npm".to_owned()],
            }),
        ));
    }
}
