//! Docker detector: one verification unit per directory holding Dockerfiles.

use std::collections::BTreeMap;

use super::file_walk::is_test_support_path;
use super::{unit, RepositoryShape, ScanContext};
use crate::{identifier_suffix, parent_path, shell_quote, UnitKind};

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    let mut dockerfiles_by_root: BTreeMap<String, Vec<&String>> = BTreeMap::new();
    for dockerfile in context.files.iter().filter(|file| {
        !is_test_support_path(file)
            && file
                .rsplit('/')
                .next()
                .is_some_and(|name| name.starts_with("Dockerfile"))
    }) {
        let docker_root = parent_path(dockerfile);
        dockerfiles_by_root
            .entry(docker_root)
            .or_default()
            .push(dockerfile);
    }
    for (docker_root, dockerfiles) in dockerfiles_by_root {
        let build_context = if docker_root == "." {
            "."
        } else {
            &docker_root
        };
        shape.units.push(unit(
            UnitKind::Docker,
            &docker_root,
            if build_context == "." {
                vec!["**".to_owned()]
            } else {
                vec![format!("{build_context}/**")]
            },
            dockerfiles
                .into_iter()
                .map(|dockerfile| {
                    let tag = identifier_suffix(dockerfile);
                    format!(
                        "docker build --file {} --tag local-ci:{tag} {}",
                        shell_quote(dockerfile),
                        shell_quote(build_context)
                    )
                })
                .collect(),
            None,
        ));
    }
}
