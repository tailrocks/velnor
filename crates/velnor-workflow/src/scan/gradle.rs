//! Gradle detector: one verification unit per Gradle root.

use std::collections::BTreeSet;

use super::file_walk::{is_test_support_path, join_repo_path, path_prefix, roots_for_manifests};
use super::{unit, RepositoryShape, ScanContext};
use crate::{shell_change_dir, CacheSpec, UnitKind};

fn gradle_roots(files: &BTreeSet<String>) -> Vec<String> {
    let manifests = files
        .iter()
        .filter(|file| {
            !is_test_support_path(file)
                && matches!(
                    file.rsplit('/').next(),
                    Some(
                        "settings.gradle"
                            | "settings.gradle.kts"
                            | "build.gradle"
                            | "build.gradle.kts"
                    )
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    roots_for_manifests(&manifests)
}

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    let gradle_roots = gradle_roots(context.file_set);
    for root in gradle_roots {
        let prefix = path_prefix(&root);
        let command_prefix = shell_change_dir(&root);
        let gradle = if context.file_set.contains(&join_repo_path(&root, "gradlew")) {
            "./gradlew"
        } else {
            "gradle"
        };
        shape.units.push(unit(
            UnitKind::Gradle,
            &root,
            vec![
                format!("{prefix}**/*.gradle"),
                format!("{prefix}**/*.gradle.kts"),
                format!("{prefix}gradle/**"),
                format!("{prefix}src/**"),
            ],
            vec![format!("{command_prefix}{gradle} check --no-daemon")],
            Some(CacheSpec {
                key_files: vec![
                    join_repo_path(&root, "gradle/wrapper/gradle-wrapper.properties"),
                    join_repo_path(&root, "gradle/libs.versions.toml"),
                ],
                paths: vec![
                    "~/.gradle/caches".to_owned(),
                    "~/.gradle/wrapper".to_owned(),
                ],
            }),
        ));
    }
}
