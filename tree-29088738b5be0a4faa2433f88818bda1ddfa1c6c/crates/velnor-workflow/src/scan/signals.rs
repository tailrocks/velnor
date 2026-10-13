//! Capability signals read from policy and tooling files. Publish and asset
//! signals land here once a repository declares them instead of an estate
//! profile.

use super::{RepositoryShape, ScanContext};

pub(crate) fn detect(context: &ScanContext<'_>, shape: &mut RepositoryShape) {
    if context.file_set.contains("deny.toml") || context.file_set.contains(".cargo/deny.toml") {
        shape.detected.push("cargo-deny-policy".to_owned());
    }
    if context.file_set.contains(".config/nextest.toml")
        || context.file_set.contains("nextest.toml")
    {
        shape.detected.push("cargo-nextest-policy".to_owned());
    }
    if context.file_set.contains("renovate.json")
        || context.root.join(".github/renovate.json").is_file()
    {
        shape.detected.push("renovate-configuration".to_owned());
        shape.limitations.push(
            "Renovate credentials, runner placement, and write permissions cannot be inferred from repository files.".to_owned(),
        );
    }
}
