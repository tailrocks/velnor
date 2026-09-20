//! Exact GitHub-hosted platform labels used by generated workflow routing.
//!
//! Runner labels are provider details, so typed platform contracts must resolve
//! them in one place. Callers never infer architecture from selector order.

/// The latest verified hosted Linux platform labels from the official runner
/// image matrix. The arm64 image is a distinct label, not an extra selector
/// label on the x64 image.
pub(crate) const LATEST_HOSTED_LINUX_X64_RUNNER: &str = "ubuntu-26.04";
pub(crate) const LATEST_HOSTED_LINUX_ARM64_RUNNER: &str = "ubuntu-26.04-arm";

/// The latest verified hosted Apple label. Native capability validation owns
/// the SDK and architecture proof; this module only centralizes the spelling.
pub(crate) const LATEST_HOSTED_MACOS_ARM64_RUNNER: &str = "xcode-27";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostedLinuxPlatform {
    X64,
    Arm64,
}

/// Resolve a typed hosted Linux platform to its exact official label.
#[must_use]
pub(crate) const fn linux_runner(platform: HostedLinuxPlatform) -> &'static str {
    match platform {
        HostedLinuxPlatform::X64 => LATEST_HOSTED_LINUX_X64_RUNNER,
        HostedLinuxPlatform::Arm64 => LATEST_HOSTED_LINUX_ARM64_RUNNER,
    }
}

/// Reject labels whose aliases or older image names would hide the selected
/// image major or architecture. Unknown custom labels remain available to the
/// generic generator; typed hosted platform routing never uses them.
pub(crate) fn reject_stale_or_alias(label: &str) -> Result<(), String> {
    if matches!(
        label,
        "ubuntu-latest" | "ubuntu-22.04" | "ubuntu-22.04-arm" | "ubuntu-24.04" | "ubuntu-24.04-arm"
    ) {
        return Err(format!(
            "hosted Linux label `{label}` is stale or an alias; use the exact current labels `{LATEST_HOSTED_LINUX_X64_RUNNER}` or `{LATEST_HOSTED_LINUX_ARM64_RUNNER}`"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{linux_runner, reject_stale_or_alias, HostedLinuxPlatform};

    #[test]
    fn current_labels_are_selected_by_typed_platform() {
        assert_eq!(linux_runner(HostedLinuxPlatform::X64), "ubuntu-26.04");
        assert_eq!(linux_runner(HostedLinuxPlatform::Arm64), "ubuntu-26.04-arm");
    }

    #[test]
    fn stale_and_alias_labels_fail_closed() {
        for label in [
            "ubuntu-latest",
            "ubuntu-22.04",
            "ubuntu-22.04-arm",
            "ubuntu-24.04",
            "ubuntu-24.04-arm",
        ] {
            assert!(reject_stale_or_alias(label).is_err(), "{label}");
        }
        assert!(reject_stale_or_alias("ubuntu-26.04").is_ok());
        assert!(reject_stale_or_alias("ubuntu-26.04-arm").is_ok());
        assert!(reject_stale_or_alias("custom-hosted-label").is_ok());
    }
}
