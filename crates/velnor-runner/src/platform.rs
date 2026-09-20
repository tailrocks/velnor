use anyhow::{bail, Result};

pub(crate) const TARGET_MVP_X64_LABEL: &str = "velnor-target-mvp-x64";
pub(crate) const TARGET_MVP_ARM64_LABEL: &str = "velnor-target-mvp-arm64";

const GITHUB_HOSTED_IMAGE_LABEL_PREFIXES: [&str; 3] = ["ubuntu-", "macos-", "windows-"];

/// GitHub-hosted image labels identify execution capacity owned by GitHub.
/// Velnor-managed registration paths must never claim these namespaces.
pub(crate) fn validate_no_hosted_image_labels(labels: &[String]) -> Result<()> {
    if let Some(label) = labels
        .iter()
        .find(|label| is_github_hosted_image_label(label))
    {
        bail!(
            "unsupported GitHub-hosted image label '{label}' on a Velnor-managed runner; use a Velnor-owned label"
        );
    }
    Ok(())
}

fn is_github_hosted_image_label(label: &str) -> bool {
    let normalized = label.trim().to_ascii_lowercase();
    GITHUB_HOSTED_IMAGE_LABEL_PREFIXES
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
}

pub fn validate_arm_label_matches_host(labels: &[String], host_arch: &str) -> Result<()> {
    let has_arm_label = labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case(TARGET_MVP_ARM64_LABEL));
    if has_arm_label && !is_arm64_arch(host_arch) {
        bail!(
            "unsupported ARM runner label '{TARGET_MVP_ARM64_LABEL}' on host architecture '{host_arch}'; only claim it when Docker can provide ARM64 Linux job containers"
        );
    }
    Ok(())
}

fn is_arm64_arch(arch: &str) -> bool {
    matches!(arch.to_ascii_lowercase().as_str(), "aarch64" | "arm64")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    #[test]
    fn arm_label_requires_arm_host() {
        let labels = vec![TARGET_MVP_ARM64_LABEL.to_string()];
        assert!(validate_arm_label_matches_host(&labels, "aarch64").is_ok());
        assert!(validate_arm_label_matches_host(&labels, "arm64").is_ok());

        let error = validate_arm_label_matches_host(&labels, "x86_64")
            .unwrap_err()
            .to_string();
        assert!(error.contains("only claim it when Docker can provide ARM64 Linux job containers"));
    }

    #[test]
    fn github_hosted_image_prefixes_are_rejected_case_insensitively() {
        for label in [
            "ubuntu-24.04",
            " UBUNTU-LATEST ",
            "MacOs-26",
            "WINDOWS-2025",
        ] {
            let error = validate_no_hosted_image_labels(&[label.to_owned()])
                .unwrap_err()
                .to_string();
            assert!(error.contains(label), "{error}");
        }
        validate_no_hosted_image_labels(&[
            "velnor-target-mvp".to_owned(),
            "velnor-target-mvp-x64".to_owned(),
        ])
        .unwrap();
    }
}
