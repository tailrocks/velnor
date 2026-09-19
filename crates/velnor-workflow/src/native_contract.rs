//! Typed Apple-host requirements and hosted capability checks.
//!
//! A runner label is routing syntax, not proof that an Apple toolchain exists.
//! The scanner records the SDK family, version constraints, toolchain pin, and
//! the difference between the host that executes a job and architectures that
//! a compiler must produce. Hosted jobs then verify the selected host before
//! invoking repository commands.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A dotted Apple version. Apple toolchain and SDK versions are compared as
/// numeric components, never as strings (`26.10` must sort after `26.6`).
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct AppleVersion {
    pub(crate) major: u16,
    pub(crate) minor: u16,
    pub(crate) patch: u16,
}

impl AppleVersion {
    pub(crate) const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse `26`, `26.5`, `26.5.1`, or the SwiftPM spelling `v26`.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let value = value.trim().strip_prefix('v').unwrap_or(value.trim());
        let mut parts = value.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().map_or(Some(0), |part| part.parse().ok())?;
        let patch = parts.next().map_or(Some(0), |part| part.parse().ok())?;
        parts
            .next()
            .is_none()
            .then_some(Self::new(major, minor, patch))
    }
}

impl fmt::Display for AppleVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.patch == 0 {
            write!(formatter, "{}.{}", self.major, self.minor)
        } else {
            write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
        }
    }
}

/// A source/config constraint. `minimum` is a deployment floor; `exact` is
/// reserved for an explicitly pinned toolchain or SDK, never inferred from a
/// runner label.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub(crate) struct AppleVersionConstraint {
    pub(crate) minimum: Option<AppleVersion>,
    pub(crate) exact: Option<AppleVersion>,
}

impl AppleVersionConstraint {
    pub(crate) fn minimum(version: AppleVersion) -> Self {
        Self {
            minimum: Some(version),
            exact: None,
        }
    }

    pub(crate) fn exact(version: AppleVersion) -> Self {
        Self {
            minimum: Some(version),
            exact: Some(version),
        }
    }

    fn strengthen(self, other: Self, what: &str) -> Result<Self, String> {
        let minimum = match (self.minimum, other.minimum) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (left, right) => left.or(right),
        };
        let exact = match (self.exact, other.exact) {
            (Some(left), Some(right)) if left != right => {
                return Err(format!(
                    "{what} pins both {left} and {right}; keep one exact requirement"
                ));
            }
            (left, right) => left.or(right),
        };
        if let (Some(minimum), Some(exact)) = (minimum, exact)
            && exact < minimum
        {
            return Err(format!(
                "{what} exact version {exact} is below minimum {minimum}"
            ));
        }
        Ok(Self { minimum, exact })
    }

    fn accepts(self, actual: AppleVersion) -> bool {
        self.minimum.is_none_or(|minimum| actual >= minimum)
            && self.exact.is_none_or(|exact| actual == exact)
    }
}

/// The SDK destination selected by the native build. These are deliberately
/// the only Apple families currently needed by the scanner: macOS, iOS device,
/// and iOS simulator. A generic `apple` value would make the preflight
/// incapable of checking the selected SDK.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AppleSdkFamily {
    Macos,
    IosDevice,
    IosSimulator,
}

impl AppleSdkFamily {
    pub(crate) const fn sdk_name(self) -> &'static str {
        match self {
            Self::Macos => "macosx",
            Self::IosDevice => "iphoneos",
            Self::IosSimulator => "iphonesimulator",
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::IosDevice => "ios-device",
            Self::IosSimulator => "ios-simulator",
        }
    }
}

/// An Apple CPU architecture. Host execution and compiler output are tracked
/// separately: an arm64 host can cross-build x86_64, but cannot execute it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AppleArch {
    Arm64,
    X86_64,
}

impl AppleArch {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Arm64 => "arm64",
            Self::X86_64 => "x86_64",
        }
    }
}

/// One SDK family and its deployment/version requirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct AppleSdkRequirement {
    pub(crate) family: AppleSdkFamily,
    pub(crate) version: AppleVersionConstraint,
}

/// The complete native host contract attached to one scanned unit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct AppleNativeContract {
    /// Host macOS floor. For an Apple SDK deployment target this is normally
    /// the same floor; iOS destinations may leave it absent.
    pub(crate) macos: AppleVersionConstraint,
    pub(crate) sdk: AppleSdkRequirement,
    pub(crate) xcode: AppleVersionConstraint,
    pub(crate) swift: AppleVersionConstraint,
    pub(crate) requires_full_xcode: bool,
    /// Architecture that must execute the job itself.
    pub(crate) execution_arch: AppleArch,
    /// Architectures the compiler/build product must contain. This is not an
    /// execution promise for any architecture other than `execution_arch`.
    pub(crate) build_arches: BTreeSet<AppleArch>,
    /// Parser evidence that could not be reconciled. Keeping this marker in
    /// the typed value makes later routing fail closed instead of silently
    /// discarding one source's stronger requirement.
    pub(crate) conflicts: Vec<String>,
}

impl AppleNativeContract {
    pub(crate) fn new(family: AppleSdkFamily, minimum: Option<AppleVersion>) -> Self {
        let version = minimum.map_or_else(
            AppleVersionConstraint::default,
            AppleVersionConstraint::minimum,
        );
        let macos = (family == AppleSdkFamily::Macos)
            .then_some(version)
            .unwrap_or_default();
        Self {
            macos,
            sdk: AppleSdkRequirement { family, version },
            xcode: AppleVersionConstraint::default(),
            swift: AppleVersionConstraint::default(),
            requires_full_xcode: true,
            execution_arch: AppleArch::Arm64,
            build_arches: BTreeSet::from([AppleArch::Arm64]),
            conflicts: Vec::new(),
        }
    }

    pub(crate) fn merge(&self, other: &Self) -> Result<Self, String> {
        if self.sdk.family != other.sdk.family {
            return Err(format!(
                "SDK families {} and {} cannot serve one native unit",
                self.sdk.family.as_str(),
                other.sdk.family.as_str()
            ));
        }
        if self.execution_arch != other.execution_arch {
            return Err(format!(
                "execution architectures {} and {} cannot serve one native unit",
                self.execution_arch.as_str(),
                other.execution_arch.as_str()
            ));
        }
        let mut build_arches = self.build_arches.clone();
        build_arches.extend(&other.build_arches);
        Ok(Self {
            macos: self.macos.strengthen(other.macos, "macOS host floor")?,
            sdk: AppleSdkRequirement {
                family: self.sdk.family,
                version: self
                    .sdk
                    .version
                    .strengthen(other.sdk.version, "SDK version")?,
            },
            xcode: self.xcode.strengthen(other.xcode, "Xcode version")?,
            swift: self
                .swift
                .strengthen(other.swift, "Swift toolchain version")?,
            requires_full_xcode: self.requires_full_xcode || other.requires_full_xcode,
            execution_arch: self.execution_arch,
            build_arches,
            conflicts: self
                .conflicts
                .iter()
                .chain(other.conflicts.iter())
                .cloned()
                .collect(),
        })
    }

    /// Merge an explicit repository contract. Explicit values may raise a
    /// floor or add build architectures, but cannot weaken detected evidence.
    pub(crate) fn strengthen_with(&self, explicit: &Self) -> Result<Self, String> {
        if self.sdk.family != explicit.sdk.family {
            return Err(format!(
                "explicit SDK family {} conflicts with detected {}",
                explicit.sdk.family.as_str(),
                self.sdk.family.as_str()
            ));
        }
        if self.execution_arch != explicit.execution_arch {
            return Err(format!(
                "explicit execution architecture {} conflicts with detected {}",
                explicit.execution_arch.as_str(),
                self.execution_arch.as_str()
            ));
        }
        if !self.build_arches.is_subset(&explicit.build_arches) {
            return Err(format!(
                "explicit build architectures {:?} omit detected {:?}",
                explicit.build_arches, self.build_arches
            ));
        }
        let merged = self.merge(explicit)?;
        Ok(Self {
            build_arches: explicit.build_arches.clone(),
            ..merged
        })
    }

    pub(crate) fn describe(&self) -> String {
        let build_arches = self
            .build_arches
            .iter()
            .map(|arch| arch.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let swift = if self.swift.minimum.is_some() || self.swift.exact.is_some() {
            format!(" + Swift {}", format_constraint(self.swift))
        } else {
            String::new()
        };
        format!(
            "{} SDK ({}) + {} execution + build [{}]{}{}",
            self.sdk.family.as_str(),
            self.sdk.version.minimum.map_or_else(
                || "any version".to_owned(),
                |version| format!(">={version}")
            ),
            self.execution_arch.as_str(),
            build_arches,
            if self.conflicts.is_empty() {
                String::new()
            } else {
                format!(" + conflicts: {}", self.conflicts.join("; "))
            },
            swift
        )
    }
}

/// Explicit per-unit native contract for facts static scanning cannot prove.
/// It is intentionally narrow: SDK family/destination, version constraints,
/// toolchain, and execution/build architectures only.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppleNativeSection {
    pub(crate) sdk_family: Option<String>,
    pub(crate) sdk_minimum: Option<String>,
    pub(crate) sdk_exact: Option<String>,
    pub(crate) macos_minimum: Option<String>,
    pub(crate) xcode_minimum: Option<String>,
    pub(crate) xcode_exact: Option<String>,
    pub(crate) swift_minimum: Option<String>,
    pub(crate) swift_exact: Option<String>,
    pub(crate) execution_arch: Option<String>,
    pub(crate) build_arches: Option<Vec<String>>,
}

impl AppleNativeSection {
    pub(crate) fn contract(&self, context: &str) -> Result<AppleNativeContract, String> {
        let family = match self.sdk_family.as_deref().unwrap_or("macos") {
            "macos" => AppleSdkFamily::Macos,
            "ios-device" => AppleSdkFamily::IosDevice,
            "ios-simulator" => AppleSdkFamily::IosSimulator,
            value => {
                return Err(format!(
                    "{context} declares unknown sdk_family {value}; expected macos, ios-device, or ios-simulator"
                ));
            }
        };
        let sdk_minimum =
            parse_config_version(self.sdk_minimum.as_deref(), context, "sdk_minimum")?;
        let sdk_exact = parse_config_version(self.sdk_exact.as_deref(), context, "sdk_exact")?;
        let mut sdk_version = sdk_minimum.map_or_else(
            AppleVersionConstraint::default,
            AppleVersionConstraint::minimum,
        );
        if let Some(exact) = sdk_exact {
            sdk_version =
                sdk_version.strengthen(AppleVersionConstraint::exact(exact), "SDK version")?;
        }
        let explicit_macos = parse_constraint(
            self.macos_minimum.as_deref(),
            None,
            context,
            "macOS host floor",
        )?;
        let macos = if family == AppleSdkFamily::Macos {
            explicit_macos.strengthen(sdk_version, "macOS host floor")?
        } else {
            explicit_macos
        };
        let xcode = parse_constraint(
            self.xcode_minimum.as_deref(),
            self.xcode_exact.as_deref(),
            context,
            "Xcode version",
        )?;
        let swift = parse_constraint(
            self.swift_minimum.as_deref(),
            self.swift_exact.as_deref(),
            context,
            "Swift toolchain version",
        )?;
        let execution_arch = match self.execution_arch.as_deref().unwrap_or("arm64") {
            "arm64" => AppleArch::Arm64,
            "x86_64" => AppleArch::X86_64,
            value => {
                return Err(format!(
                    "{context} declares unknown execution_arch {value}; expected arm64 or x86_64"
                ));
            }
        };
        let build_arches = match &self.build_arches {
            Some(values) if values.is_empty() => {
                return Err(format!("{context}.build_arches must not be empty"));
            }
            Some(values) => values
                .iter()
                .map(|value| match value.as_str() {
                    "arm64" => Ok(AppleArch::Arm64),
                    "x86_64" => Ok(AppleArch::X86_64),
                    _ => Err(format!(
                        "{context}.build_arches contains unknown architecture {value}; expected arm64 or x86_64"
                    )),
                })
                .collect::<Result<BTreeSet<_>, _>>()?,
            None => BTreeSet::from([execution_arch]),
        };
        let contract = AppleNativeContract {
            macos,
            sdk: AppleSdkRequirement {
                family,
                version: sdk_version,
            },
            xcode,
            swift,
            requires_full_xcode: true,
            execution_arch,
            build_arches,
            conflicts: Vec::new(),
        };
        if !contract.build_arches.contains(&execution_arch) {
            return Err(format!(
                "{context}.build_arches must include execution_arch {}",
                execution_arch.as_str()
            ));
        }
        Ok(contract)
    }
}

fn parse_config_version(
    value: Option<&str>,
    context: &str,
    field: &str,
) -> Result<Option<AppleVersion>, String> {
    value
        .map(|value| {
            AppleVersion::parse(value)
                .ok_or_else(|| format!("{context}.{field} must be a dotted Apple version"))
        })
        .transpose()
}

fn parse_constraint(
    minimum: Option<&str>,
    exact: Option<&str>,
    context: &str,
    name: &str,
) -> Result<AppleVersionConstraint, String> {
    let minimum = parse_config_version(minimum, context, &format!("{name}.minimum"))?;
    let exact = parse_config_version(exact, context, &format!("{name}.exact"))?;
    let minimum_constraint = minimum.map_or_else(
        AppleVersionConstraint::default,
        AppleVersionConstraint::minimum,
    );
    match exact {
        Some(exact) => minimum_constraint.strengthen(AppleVersionConstraint::exact(exact), name),
        None => Ok(minimum_constraint),
    }
}

/// Verified GitHub-hosted Apple image capability. Labels are accepted only
/// when this table has a primary-image-docs-backed offer; arbitrary labels are
/// never treated as proof of a native toolchain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HostedAppleOffer {
    pub(crate) label: &'static str,
    pub(crate) host_macos: AppleVersion,
    pub(crate) xcode: AppleVersion,
    pub(crate) sdk_versions: &'static [(AppleSdkFamily, AppleVersion)],
    /// The image table does not publish a Swift compiler row; preflight still
    /// verifies a source-pinned Swift toolchain on the selected host.
    pub(crate) swift: Option<AppleVersion>,
    pub(crate) execution_arch: AppleArch,
    pub(crate) build_arches: &'static [AppleArch],
}

const UNIVERSAL_BUILD_ARCHES: &[AppleArch] = &[AppleArch::Arm64, AppleArch::X86_64];
const MACOS_26_SDKS: &[(AppleSdkFamily, AppleVersion)] = &[
    (AppleSdkFamily::Macos, AppleVersion::new(26, 5, 0)),
    (AppleSdkFamily::IosDevice, AppleVersion::new(26, 5, 0)),
    (AppleSdkFamily::IosSimulator, AppleVersion::new(26, 5, 0)),
];

/// Current GitHub-hosted macOS 26 offers, verified from the public runner and
/// image readmes. The image ships Xcode 26.6 and macOS/iOS SDK 26.5 families.
pub(crate) fn hosted_apple_offer(label: &str) -> Option<HostedAppleOffer> {
    let common = |label, execution_arch, build_arches| HostedAppleOffer {
        label,
        host_macos: AppleVersion::new(26, 6, 1),
        xcode: AppleVersion::new(26, 6, 0),
        sdk_versions: MACOS_26_SDKS,
        swift: None,
        execution_arch,
        build_arches,
    };
    match label {
        "macos-26" => Some(common("macos-26", AppleArch::Arm64, UNIVERSAL_BUILD_ARCHES)),
        "macos-26-intel" => Some(common(
            "macos-26-intel",
            AppleArch::X86_64,
            UNIVERSAL_BUILD_ARCHES,
        )),
        _ => None,
    }
}

/// Check a requirement against a verified hosted image. Returns actionable
/// mismatches instead of allowing label-only success.
pub(crate) fn offer_mismatches(
    contract: &AppleNativeContract,
    offer: HostedAppleOffer,
) -> Vec<String> {
    let mut missing = Vec::new();
    missing.extend(
        contract
            .conflicts
            .iter()
            .map(|conflict| format!("unresolved source contract: {conflict}")),
    );
    if !contract.macos.accepts(offer.host_macos) {
        missing.push(format!(
            "macOS {} does not satisfy {}",
            offer.host_macos,
            format_constraint(contract.macos)
        ));
    }
    if !contract.xcode.accepts(offer.xcode) {
        missing.push(format!(
            "Xcode {} does not satisfy {}",
            offer.xcode,
            format_constraint(contract.xcode)
        ));
    }
    if !contract
        .build_arches
        .iter()
        .all(|arch| offer.build_arches.contains(arch))
    {
        missing.push(format!(
            "build architectures {:?} are unavailable (offer has {:?})",
            contract.build_arches, offer.build_arches
        ));
    }
    if contract.execution_arch != offer.execution_arch {
        missing.push(format!(
            "execution architecture {} is unavailable (host executes {})",
            contract.execution_arch.as_str(),
            offer.execution_arch.as_str()
        ));
    }
    let offered_sdk = offer
        .sdk_versions
        .iter()
        .find(|(family, _)| *family == contract.sdk.family)
        .map(|(_, version)| *version);
    match offered_sdk {
        Some(version) if contract.sdk.version.accepts(version) => {}
        Some(version) => missing.push(format!(
            "{} SDK {} does not satisfy {}",
            contract.sdk.family.as_str(),
            version,
            format_constraint(contract.sdk.version)
        )),
        None => missing.push(format!(
            "hosted label {} does not publish the required {} SDK family",
            offer.label,
            contract.sdk.family.as_str()
        )),
    }
    if let Some(actual) = offer.swift
        && !contract.swift.accepts(actual)
    {
        missing.push(format!(
            "Swift toolchain {} does not satisfy {}",
            actual,
            format_constraint(contract.swift)
        ));
    }
    missing
}

fn format_constraint(constraint: AppleVersionConstraint) -> String {
    match (constraint.minimum, constraint.exact) {
        (_, Some(exact)) => format!("exactly {exact}"),
        (Some(minimum), None) => format!(">= {minimum}"),
        (None, None) => "any version".to_owned(),
    }
}

/// Render the hosted step that verifies the selected Apple host. The shell is
/// deliberately executable in isolation; tests can provide fake macOS/Xcode
/// probes without needing a macOS runner.
pub(crate) fn render_preflight_step(contract: &AppleNativeContract) -> String {
    let sdk_minimum = contract
        .sdk
        .version
        .minimum
        .map_or_else(String::new, |version| version.to_string());
    let sdk_exact = contract
        .sdk
        .version
        .exact
        .map_or_else(String::new, |version| version.to_string());
    let macos_minimum = contract
        .macos
        .minimum
        .map_or_else(String::new, |version| version.to_string());
    let xcode_minimum = contract
        .xcode
        .minimum
        .map_or_else(String::new, |version| version.to_string());
    let xcode_exact = contract
        .xcode
        .exact
        .map_or_else(String::new, |version| version.to_string());
    let swift_minimum = contract
        .swift
        .minimum
        .map_or_else(String::new, |version| version.to_string());
    let swift_exact = contract
        .swift
        .exact
        .map_or_else(String::new, |version| version.to_string());
    let build_arches = contract
        .build_arches
        .iter()
        .map(|arch| arch.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let sdk_name = contract.sdk.family.sdk_name();
    let family = contract.sdk.family.as_str();
    format!(
        "      - name: Verify Apple native host contract\n        shell: bash\n        env:\n          APPLE_MACOS_MINIMUM: {macos_minimum:?}\n          APPLE_SDK_FAMILY: {family:?}\n          APPLE_SDK_NAME: {sdk_name:?}\n          APPLE_SDK_MINIMUM: {sdk_minimum:?}\n          APPLE_SDK_EXACT: {sdk_exact:?}\n          APPLE_XCODE_MINIMUM: {xcode_minimum:?}\n          APPLE_XCODE_EXACT: {xcode_exact:?}\n          APPLE_SWIFT_MINIMUM: {swift_minimum:?}\n          APPLE_SWIFT_EXACT: {swift_exact:?}\n          APPLE_EXECUTION_ARCH: {execution_arch:?}\n          APPLE_BUILD_ARCHES: {build_arches:?}\n        run: |\n{script}",
        execution_arch = contract.execution_arch.as_str(),
        swift_minimum = swift_minimum,
        swift_exact = swift_exact,
        script = indent_script(&preflight_script()),
    )
}

fn indent_script(script: &str) -> String {
    script
        .lines()
        .map(|line| format!("          {line}\n"))
        .collect()
}

fn preflight_script() -> String {
    r#"set -euo pipefail
fail() {
  echo "::error::Apple native host contract: $*" >&2
  exit 1
}
version_at_least() {
  awk -F. -v actual="$1" -v required="$2" '
    BEGIN {
      for (i = 1; i <= 3; i++) {
        split(actual, av, "."); split(required, rv, ".")
        a = av[i] + 0; r = rv[i] + 0
        if (a > r) exit 0
        if (a < r) exit 1
      }
      exit 0
    }'
}
version_exact() {
  version_at_least "$1" "$2" && version_at_least "$2" "$1"
}
require_version() {
  local label="$1" actual="$2" minimum="$3" exact="$4"
  if [[ -n "$minimum" ]] && ! version_at_least "$actual" "$minimum"; then
    fail "$label $actual is below required minimum $minimum"
  fi
  if [[ -n "$exact" ]] && ! version_exact "$actual" "$exact"; then
    fail "$label $actual is not the required exact version $exact"
  fi
}
command -v uname >/dev/null || fail "uname is unavailable"
command -v sw_vers >/dev/null || fail "macOS sw_vers is unavailable; native work cannot run on Linux"
command -v xcode-select >/dev/null || fail "xcode-select is unavailable; install full Xcode"
command -v xcodebuild >/dev/null || fail "xcodebuild is unavailable; install full Xcode"
command -v xcrun >/dev/null || fail "xcrun is unavailable; install full Xcode"

actual_arch="$(uname -m)"
[[ "$actual_arch" == "$APPLE_EXECUTION_ARCH" ]] \
  || fail "host executes $actual_arch, required execution architecture is $APPLE_EXECUTION_ARCH (cross-build support does not satisfy execution)"
actual_macos="$(sw_vers -productVersion)"
require_version "macOS" "$actual_macos" "$APPLE_MACOS_MINIMUM" ""

developer_dir="${DEVELOPER_DIR:-}"
if [[ -z "$developer_dir" && -n "$APPLE_XCODE_EXACT" ]]; then
  for candidate in \
    "/Applications/Xcode_${APPLE_XCODE_EXACT}.app/Contents/Developer" \
    "/Applications/Xcode.app/Contents/Developer"; do
    if [[ -d "$candidate" ]]; then
      developer_dir="$candidate"
      break
    fi
  done
fi
if [[ -n "$developer_dir" ]]; then
  [[ -d "$developer_dir" ]] || fail "DEVELOPER_DIR=$developer_dir does not exist"
  export DEVELOPER_DIR="$developer_dir"
else
  developer_dir="$(xcode-select -p 2>/dev/null || true)"
  [[ -n "$developer_dir" && -d "$developer_dir" ]] \
    || fail "no selected full Xcode developer directory; set DEVELOPER_DIR explicitly"
fi
if [[ -n "${GITHUB_ENV:-}" ]]; then
  printf 'DEVELOPER_DIR=%s\n' "$developer_dir" >> "$GITHUB_ENV"
fi

xcode_version="$(xcodebuild -version | awk '$1 == "Xcode" { print $2; exit }')"
[[ -n "$xcode_version" ]] || fail "xcodebuild did not report an Xcode version"
require_version "Xcode" "$xcode_version" "$APPLE_XCODE_MINIMUM" "$APPLE_XCODE_EXACT"
swift_version="$(swift --version 2>/dev/null | awk '$1 == "Apple" && $2 == "Swift" && $3 == "version" { print $4; exit } $1 == "Swift" && $2 == "version" { print $3; exit }')"
if [[ -n "$APPLE_SWIFT_MINIMUM" || -n "$APPLE_SWIFT_EXACT" ]]; then
  [[ -n "$swift_version" ]] || fail "swift --version did not report a Swift toolchain version"
  require_version "Swift toolchain" "$swift_version" "$APPLE_SWIFT_MINIMUM" "$APPLE_SWIFT_EXACT"
fi
sdk_listing="$(xcodebuild -showsdks)"
grep -F "$APPLE_SDK_NAME" <<<"$sdk_listing" >/dev/null \
  || fail "selected Xcode does not list required $APPLE_SDK_FAMILY SDK ($APPLE_SDK_NAME)"
sdk_version="$(xcrun --sdk "$APPLE_SDK_NAME" --show-sdk-version)"
[[ -n "$sdk_version" ]] || fail "xcrun did not report $APPLE_SDK_NAME SDK version"
require_version "$APPLE_SDK_FAMILY SDK" "$sdk_version" "$APPLE_SDK_MINIMUM" "$APPLE_SDK_EXACT"
sdk_path="$(xcrun --sdk "$APPLE_SDK_NAME" --show-sdk-path)"
[[ -n "$sdk_path" && -d "$sdk_path" ]] || fail "required SDK path is unavailable: $sdk_path"

# This probes compiler acceptance of each output architecture only. It does
# not claim that an arm64 host can execute an x86_64 product.
for arch in $APPLE_BUILD_ARCHES; do
  xcrun --sdk "$APPLE_SDK_NAME" clang -arch "$arch" -x c -fsyntax-only - </dev/null \
    >/dev/null 2>&1 || fail "selected SDK/compiler cannot build architecture $arch"
done
echo "Apple native host verified: macOS $actual_macos, Xcode $xcode_version, $APPLE_SDK_FAMILY SDK $sdk_version, execution $actual_arch, build arches [$APPLE_BUILD_ARCHES]"
"#
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        hosted_apple_offer, offer_mismatches, render_preflight_step, AppleArch,
        AppleNativeContract, AppleSdkFamily, AppleVersion,
    };
    use std::collections::BTreeSet;

    #[test]
    fn verified_images_distinguish_execution_from_build_architecture() {
        let mut contract =
            AppleNativeContract::new(AppleSdkFamily::Macos, Some(AppleVersion::new(26, 0, 0)));
        contract.xcode = super::AppleVersionConstraint::exact(AppleVersion::new(26, 6, 0));
        contract.build_arches = BTreeSet::from([AppleArch::Arm64, AppleArch::X86_64]);
        let arm = hosted_apple_offer("macos-26").expect("verified arm image");
        assert!(offer_mismatches(&contract, arm).is_empty());
        let intel = hosted_apple_offer("macos-26-intel").expect("verified intel image");
        assert!(offer_mismatches(&contract, intel)
            .iter()
            .any(|message| { message.contains("execution architecture arm64") }));
        assert!(hosted_apple_offer("macos-15").is_none());
    }

    #[test]
    fn preflight_render_carries_sdk_family_and_architecture_contract() {
        let mut contract = AppleNativeContract::new(
            AppleSdkFamily::IosSimulator,
            Some(AppleVersion::new(26, 0, 0)),
        );
        contract.build_arches = BTreeSet::from([AppleArch::Arm64, AppleArch::X86_64]);
        let rendered = render_preflight_step(&contract);
        assert!(rendered.contains("APPLE_SDK_FAMILY: \"ios-simulator\""));
        assert!(rendered.contains("APPLE_SDK_NAME: \"iphonesimulator\""));
        assert!(rendered.contains("APPLE_EXECUTION_ARCH: \"arm64\""));
        assert!(rendered.contains("cross-build support does not satisfy execution"));
    }
}
