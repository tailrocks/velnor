//! `OpenTofu` detector: one verification unit for `OpenTofu` source trees.

use super::file_walk::has_extension;
use super::{unit, RepositoryShape, ScanContext};
use crate::{CacheSpec, UnitKind};

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    if context
        .files
        .iter()
        .any(|file| has_extension(file, "tf") || has_extension(file, "tofu"))
    {
        shape.units.push(unit(
            UnitKind::OpenTofu,
            ".",
            vec![
                "**/*.tf".to_owned(),
                "**/*.tofu".to_owned(),
                "**/.terraform.lock.hcl".to_owned(),
            ],
            vec![
                "tofu fmt -check -recursive -no-color".to_owned(),
                "TF_IN_AUTOMATION=1 TF_INPUT=0 tofu init -backend=false -input=false -no-color"
                    .to_owned(),
                "TF_IN_AUTOMATION=1 TF_INPUT=0 tofu validate -no-color".to_owned(),
            ],
            Some(CacheSpec {
                key_files: vec!["**/.terraform.lock.hcl".to_owned()],
                paths: vec!["~/.terraform.d/plugin-cache".to_owned()],
            }),
        ));
    }
}
