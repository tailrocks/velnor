//! Homebrew detector: one verification unit for Formula/Casks/Brewfile.

use super::{unit, RepositoryShape, ScanContext};
use crate::UnitKind;

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    if context.files.iter().any(|file| {
        file.starts_with("Formula/") || file.starts_with("Casks/") || file == "Brewfile"
    }) {
        shape.units.push(unit(
            UnitKind::Homebrew,
            ".",
            vec![
                "Formula/**".to_owned(),
                "Casks/**".to_owned(),
                "Brewfile".to_owned(),
            ],
            vec!["brew audit --strict --online".to_owned()],
            None,
        ));
    }
}
