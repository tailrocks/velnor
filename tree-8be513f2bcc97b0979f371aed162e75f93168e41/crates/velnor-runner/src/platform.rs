use anyhow::{bail, Result};

pub fn validate_linux_host(host_os: &str) -> Result<()> {
    if host_os != "linux" {
        bail!("unsupported host OS '{host_os}'; Velnor runner execution is Linux-only");
    }
    Ok(())
}

pub fn validate_arm_label_matches_host(labels: &[String], host_arch: &str) -> Result<()> {
    let has_arm_label = labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case("ubuntu-24.04-arm"));
    if has_arm_label && !is_arm64_arch(host_arch) {
        bail!(
            "unsupported ARM runner label 'ubuntu-24.04-arm' on host architecture '{host_arch}'; only claim it on an ARM Linux host"
        );
    }
    Ok(())
}

fn is_arm64_arch(arch: &str) -> bool {
    matches!(arch.to_ascii_lowercase().as_str(), "aarch64" | "arm64")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_host_is_required() {
        assert!(validate_linux_host("linux").is_ok());

        let error = validate_linux_host("macos").unwrap_err().to_string();
        assert!(error.contains("Velnor runner execution is Linux-only"));
    }

    #[test]
    fn arm_label_requires_arm_host() {
        let labels = vec!["ubuntu-24.04-arm".to_string()];
        assert!(validate_arm_label_matches_host(&labels, "aarch64").is_ok());
        assert!(validate_arm_label_matches_host(&labels, "arm64").is_ok());

        let error = validate_arm_label_matches_host(&labels, "x86_64")
            .unwrap_err()
            .to_string();
        assert!(error.contains("only claim it on an ARM Linux host"));
    }
}
