//! One provider-set schema: the only provider vocabulary in the generator.
//!
//! Every surface — config, scan, IR, plans, validation, `runs-on`, bootstrap,
//! caching, artifacts, policy, dispatch, UI names, aggregation — stores and
//! transmits `Set<ProviderId>`, never `github|velnor|both` strings. There are
//! no aliases, no deprecation branches, and no implicit provider inference.
//!
//! The canonical provider IDs are exactly `github-hosted`,
//! `github-self-hosted`, and `velnor` (bastion spec §2).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::s2::GeneratorError;

/// The only provider vocabulary. Canonical order (sort/digest/display) is
/// declaration order: hosted, self-hosted, native.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProviderId {
    #[default]
    GithubHosted,
    GithubSelfHosted,
    Velnor,
}

impl ProviderId {
    /// All three canonical providers in canonical order.
    pub(crate) const ALL: [Self; 3] = [Self::GithubHosted, Self::GithubSelfHosted, Self::Velnor];

    /// The local providers: the two whose selectors must be disjoint.
    pub(crate) const LOCAL: [Self; 2] = [Self::GithubSelfHosted, Self::Velnor];

    #[must_use]
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "std-idiomatic &self; by-value breaks fn-pointer use over borrowed iterators"
    )]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::GithubHosted => "github-hosted",
            Self::GithubSelfHosted => "github-self-hosted",
            Self::Velnor => "velnor",
        }
    }

    /// Strict parse: any other string is a hard error. There is no `github`,
    /// `both`, or empty-string alias, and no case folding.
    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            "github-hosted" => Ok(Self::GithubHosted),
            "github-self-hosted" => Ok(Self::GithubSelfHosted),
            "velnor" => Ok(Self::Velnor),
            _ => Err(GeneratorError::usage(format!(
                "unknown provider `{value}`; expected one of: github-hosted, github-self-hosted, velnor"
            ))),
        }
    }

    /// Whether this provider runs on caller-managed (local) infrastructure.
    #[must_use]
    pub(crate) fn is_local(self) -> bool {
        !matches!(self, Self::GithubHosted)
    }
}

/// Routing labels that make local placement explicit. A provider selector is
/// not a host identity by itself: the same local provider can be deployed on
/// both physical hosts, so every local selector carries exactly one host
/// label in addition to its execution-engine label.
pub(crate) const SELF_HOSTED_LABEL: &str = "self-hosted";
pub(crate) const SCALE_SET_LABEL: &str = "velnor-scale-set";
pub(crate) const NATIVE_LABEL: &str = "velnor-native";
pub(crate) const LOCAL_MAC_HOST_LABEL: &str = "local-mac";
pub(crate) const BASTION_HOST_LABEL: &str = "bastion";
const LOCAL_HOST_LABELS: [&str; 2] = [LOCAL_MAC_HOST_LABEL, BASTION_HOST_LABEL];

/// The physical/control-plane host required for one result.
///
/// This is deliberately separate from [`ProviderId`]. The same local provider
/// can exist on both physical hosts, so provider identity alone cannot prove
/// placement.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum RequiredHost {
    GithubHosted,
    LocalMac,
    Bastion,
}

impl RequiredHost {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::GithubHosted => "github-hosted",
            Self::LocalMac => LOCAL_MAC_HOST_LABEL,
            Self::Bastion => BASTION_HOST_LABEL,
        }
    }
}

/// A named runner target used by scheduled and release jobs. `macos` is a
/// platform target, not a provider alias; all provider targets use the
/// canonical provider IDs above. In particular, `github` is intentionally not
/// accepted.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum RunnerTarget {
    Provider(ProviderId),
    GithubHostedMacos,
}

impl Default for RunnerTarget {
    fn default() -> Self {
        Self::Provider(ProviderId::GithubHosted)
    }
}

impl RunnerTarget {
    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            "macos" => Ok(Self::GithubHostedMacos),
            _ => ProviderId::parse(value).map(Self::Provider),
        }
    }

    #[must_use]
    pub(crate) fn provider(self) -> ProviderId {
        match self {
            Self::Provider(provider) => provider,
            Self::GithubHostedMacos => ProviderId::GithubHosted,
        }
    }

    #[must_use]
    pub(crate) fn is_local(self) -> bool {
        self.provider().is_local()
    }

    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Provider(provider) => provider.as_str(),
            Self::GithubHostedMacos => "macos",
        }
    }
}

impl std::fmt::Display for RunnerTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Which caller-managed host lane participates in automatic events.
///
/// GitHub-hosted is always part of the generated provider universe. It is the
/// independent recovery/control-plane lane and remains automatic in every
/// mode. The mode selects the local lane(s); manual dispatch uses the full
/// three-provider universe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProviderMode {
    NativeOnly,
    ScaleSetOnly,
    Both,
}

impl ProviderMode {
    /// The accepted mode vocabulary, in stable display order.
    pub(crate) const ALL: [Self; 3] = [Self::NativeOnly, Self::ScaleSetOnly, Self::Both];

    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NativeOnly => "native-only",
            Self::ScaleSetOnly => "scale-set-only",
            Self::Both => "both",
        }
    }

    /// Parse only the canonical mode names. Provider ids and the old runner
    /// aliases are not accepted here.
    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            "native-only" => Ok(Self::NativeOnly),
            "scale-set-only" => Ok(Self::ScaleSetOnly),
            "both" => Ok(Self::Both),
            _ => Err(GeneratorError::usage(format!(
                "unknown provider mode `{value}`; expected one of: native-only, scale-set-only, both"
            ))),
        }
    }

    /// The generated workflow universe. Dispatches always see all three
    /// canonical providers, independent of the automatic local mode.
    #[must_use]
    pub(crate) fn provider_universe(self) -> ProviderSet {
        ProviderId::ALL.into_iter().collect()
    }

    /// Providers admitted on push, pull request, and schedule events.
    /// Hosted recovery is deliberately present in every mode.
    #[must_use]
    pub(crate) fn automatic_providers(self) -> ProviderSet {
        match self {
            Self::NativeOnly => ProviderSet::from([ProviderId::GithubHosted, ProviderId::Velnor]),
            Self::ScaleSetOnly => {
                ProviderSet::from([ProviderId::GithubHosted, ProviderId::GithubSelfHosted])
            }
            Self::Both => ProviderId::ALL.into_iter().collect(),
        }
    }

    /// Resolve the typed mode represented by an effective provider universe
    /// and automatic-event set. Raw provider-set configurations that do not
    /// exactly represent one of the three supported modes stay untyped.
    #[must_use]
    pub(crate) fn from_effective_sets(
        providers: &ProviderSet,
        automatic_providers: &ProviderSet,
    ) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| {
            mode.provider_universe() == *providers
                && mode.automatic_providers() == *automatic_providers
        })
    }
}

impl std::fmt::Display for ProviderMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A set of providers with canonical ordering. The in-memory form everywhere;
/// TOML/YAML/JSON arrays are the wire form.
pub(crate) type ProviderSet = BTreeSet<ProviderId>;

/// Parse a provider array with set semantics: unknown IDs and duplicates are
/// hard errors, and the result is canonically ordered.
pub(crate) fn parse_provider_set(
    values: &[String],
    field: &str,
) -> Result<ProviderSet, GeneratorError> {
    let mut set = ProviderSet::new();
    for value in values {
        let provider = ProviderId::parse(value).map_err(|_| {
            GeneratorError::usage(format!(
                "{field} has unknown provider `{value}`; expected one of: github-hosted, github-self-hosted, velnor"
            ))
        })?;
        if !set.insert(provider) {
            return Err(GeneratorError::usage(format!(
                "{field} lists provider `{value}` more than once; provider sets must not repeat an id"
            )));
        }
    }
    Ok(set)
}

/// Require a non-empty provider set where the schema needs at least one.
pub(crate) fn require_non_empty(set: &ProviderSet, field: &str) -> Result<(), GeneratorError> {
    if set.is_empty() {
        return Err(GeneratorError::usage(format!(
            "{field} must name at least one provider"
        )));
    }
    Ok(())
}

/// Require `subset ⊆ universe`, naming the field that violates the bound.
pub(crate) fn require_subset(
    subset: &ProviderSet,
    universe: &ProviderSet,
    field: &str,
    universe_field: &str,
) -> Result<(), GeneratorError> {
    for provider in subset {
        if !universe.contains(provider) {
            return Err(GeneratorError::usage(format!(
                "{field} names provider `{provider}` outside {universe_field}; dispatch and automatic selections narrow the repo universe, they never widen it"
            )));
        }
    }
    Ok(())
}

/// The `runs-on` routing for one provider: the only place labels live.
/// Routing only — never authorization, never a fanout instruction.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderSelector {
    #[serde(default)]
    pub(crate) runs_on: Vec<String>,
}

/// Per-provider selectors keyed by provider ID.
pub(crate) type SelectorMap = BTreeMap<ProviderId, ProviderSelector>;

/// Parse `[workflow.selectors.<id>]` tables keyed by strict provider ID.
pub(crate) fn parse_selectors(
    tables: &BTreeMap<String, ProviderSelector>,
) -> Result<SelectorMap, GeneratorError> {
    let mut selectors = SelectorMap::new();
    for (key, selector) in tables {
        let provider = ProviderId::parse(key).map_err(|_| {
            GeneratorError::usage(format!(
                "[workflow.selectors] has unknown provider `{key}`; expected one of: github-hosted, github-self-hosted, velnor"
            ))
        })?;
        if selector.runs_on.is_empty() {
            return Err(GeneratorError::usage(format!(
                "[workflow.selectors.{key}] runs_on must name at least one label"
            )));
        }
        for label in &selector.runs_on {
            if label.is_empty() || label.chars().any(char::is_control) {
                return Err(GeneratorError::usage(format!(
                    "[workflow.selectors.{key}] runs_on labels must be non-empty and contain no control characters"
                )));
            }
        }
        selectors.insert(provider, selector.clone());
    }
    Ok(selectors)
}

/// Every provider in the universe needs a selector; disjointness between the
/// two local providers is enforced separately by [`validate_selector_disjointness`].
pub(crate) fn require_selectors_for(
    selectors: &SelectorMap,
    universe: &ProviderSet,
) -> Result<(), GeneratorError> {
    for provider in universe {
        if !selectors.contains_key(provider) {
            return Err(GeneratorError::usage(format!(
                "[workflow.selectors.{provider}] is required: every provider in [workflow] providers needs its runs-on routing"
            )));
        }
    }
    Ok(())
}

/// The two local providers must use disjoint dedicated selectors: any shared
/// label is a routing ambiguity and a hard error.
pub(crate) fn validate_selector_disjointness(
    selectors: &SelectorMap,
) -> Result<(), GeneratorError> {
    let mut claimed: BTreeMap<&str, ProviderId> = BTreeMap::new();
    for provider in ProviderId::LOCAL {
        let Some(selector) = selectors.get(&provider) else {
            continue;
        };
        for label in &selector.runs_on {
            // GitHub's conventional marker identifies a runner as
            // self-hosted; it is intentionally shared by both local engines.
            // Disjointness is about the dedicated engine/host labels that
            // decide ownership, not this common class marker.
            if label == SELF_HOSTED_LABEL || LOCAL_HOST_LABELS.contains(&label.as_str()) {
                continue;
            }
            if let Some(owner) = claimed.insert(label.as_str(), provider) {
                return Err(GeneratorError::usage(format!(
                    "[workflow.selectors] label `{label}` is claimed by {owner} and {provider}; local providers need disjoint dedicated selectors"
                )));
            }
        }
    }
    Ok(())
}

/// Validate the provider identity encoded by a selector. This is intentionally
/// independent of visibility evidence so direct IR/render callers cannot turn
/// a hosted logical lane into a local runner by supplying local labels.
pub(crate) fn validate_selector_identity(
    provider: ProviderId,
    selector: &ProviderSelector,
) -> Result<(), GeneratorError> {
    if selector.runs_on.is_empty() {
        return Err(GeneratorError::usage(format!(
            "[workflow.selectors.{provider}] runs_on must name at least one label"
        )));
    }
    match provider {
        ProviderId::GithubHosted => {
            for label in &selector.runs_on {
                if !is_github_owned_label(label) {
                    return Err(GeneratorError::usage(format!(
                        "[workflow.selectors.github-hosted] runs_on carries {label:?}, which is not a GitHub-hosted label (ubuntu-*|macos-*|windows-*)"
                    )));
                }
            }
        }
        ProviderId::GithubSelfHosted | ProviderId::Velnor => {
            let required_engine = match provider {
                ProviderId::GithubSelfHosted => SCALE_SET_LABEL,
                ProviderId::Velnor => NATIVE_LABEL,
                ProviderId::GithubHosted => unreachable!("hosted handled above"),
            };
            if !selector
                .runs_on
                .iter()
                .any(|label| label == SELF_HOSTED_LABEL)
            {
                return Err(GeneratorError::usage(format!(
                    "[workflow.selectors.{provider}] runs_on must include `{SELF_HOSTED_LABEL}`"
                )));
            }
            if !selector
                .runs_on
                .iter()
                .any(|label| label == required_engine)
            {
                return Err(GeneratorError::usage(format!(
                    "[workflow.selectors.{provider}] runs_on must include its dedicated `{required_engine}` engine label"
                )));
            }
            let hosts = selector
                .runs_on
                .iter()
                .filter(|label| LOCAL_HOST_LABELS.contains(&label.as_str()))
                .count();
            if hosts != 1 {
                return Err(GeneratorError::usage(format!(
                    "[workflow.selectors.{provider}] runs_on must include exactly one host label: `{LOCAL_MAC_HOST_LABEL}` or `{BASTION_HOST_LABEL}`"
                )));
            }
            if let Some(label) = selector
                .runs_on
                .iter()
                .find(|label| is_github_owned_label(label))
            {
                return Err(GeneratorError::usage(format!(
                    "[workflow.selectors.{provider}] runs_on cannot carry GitHub-hosted label {label:?}"
                )));
            }
        }
    }
    Ok(())
}

/// Validate every selector used by a provider universe. This is called both
/// after scanning and immediately before IR rendering because callers can
/// construct `ProjectConfig` directly in tests and integrations.
pub(crate) fn validate_selector_identities(
    selectors: &SelectorMap,
    universe: &ProviderSet,
) -> Result<(), GeneratorError> {
    require_selectors_for(selectors, universe)?;
    for provider in universe {
        let selector = selectors
            .get(provider)
            .expect("require_selectors_for checked the provider selector");
        validate_selector_identity(*provider, selector)?;
    }
    Ok(())
}

/// Pure selector lookup: the `runs-on` labels for one provider. Unknown
/// providers cannot reach this function; a missing selector is a caller bug
/// that fails here instead of rendering an empty `runs-on`.
pub(crate) fn runs_on_for(
    selectors: &SelectorMap,
    provider: ProviderId,
) -> Result<&[String], GeneratorError> {
    selectors
        .get(&provider)
        .map(|selector| selector.runs_on.as_slice())
        .ok_or_else(|| {
            GeneratorError::usage(format!(
                "no [workflow.selectors.{provider}] routing for a selected provider"
            ))
        })
}

/// Execution platform, typed. Platforms are never label strings.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Platform {
    LinuxX64,
    LinuxArm64,
    MacosArm64,
}

impl Platform {
    #[must_use]
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "std-idiomatic &self; by-value breaks fn-pointer use over borrowed iterators"
    )]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::LinuxX64 => "linux-x64",
            Self::LinuxArm64 => "linux-arm64",
            Self::MacosArm64 => "macos-arm64",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            "linux-x64" => Ok(Self::LinuxX64),
            "linux-arm64" => Ok(Self::LinuxArm64),
            "macos-arm64" => Ok(Self::MacosArm64),
            _ => Err(GeneratorError::usage(format!(
                "unknown platform `{value}`; expected one of: linux-x64, linux-arm64, macos-arm64"
            ))),
        }
    }

    /// The architecture portion of this execution platform, carried as a
    /// separate required-result identity dimension.
    #[must_use]
    pub(crate) fn target_architecture(self) -> TargetArchitecture {
        match self {
            Self::LinuxX64 => TargetArchitecture::X86_64,
            Self::LinuxArm64 | Self::MacosArm64 => TargetArchitecture::Aarch64,
        }
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Workload architecture required by a result. OS belongs to [`Platform`];
/// architecture is repeated explicitly so a result cannot substitute a
/// different ABI while retaining the same platform-shaped label.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum TargetArchitecture {
    X86_64,
    Aarch64,
}

impl TargetArchitecture {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
}

/// What trust tier a unit needs. Typed; never a label appended to `runs-on`.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TrustReq {
    /// The unit runs on any event, including untrusted forks (hosted only).
    #[default]
    UntrustedOk,
    /// The unit runs only where the controller authorizes trusted execution.
    TrustedOnly,
}

impl TrustReq {
    #[must_use]
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "std-idiomatic &self; by-value breaks fn-pointer use over borrowed iterators"
    )]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::UntrustedOk => "untrusted-ok",
            Self::TrustedOnly => "trusted-only",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, GeneratorError> {
        match value {
            "untrusted-ok" => Ok(Self::UntrustedOk),
            "trusted-only" => Ok(Self::TrustedOnly),
            _ => Err(GeneratorError::usage(format!(
                "unknown trust requirement `{value}`; expected one of: untrusted-ok, trusted-only"
            ))),
        }
    }
}

/// The spec §2 capability list, typed. No CPU/RAM resource classes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "d2a shape: one bool per spec capability"
)]
pub(crate) struct Capabilities {
    pub(crate) docker: bool,
    pub(crate) nested_privileged_docker: bool,
    pub(crate) buildx_compose: bool,
    pub(crate) testcontainers: bool,
    pub(crate) services_with_readiness: bool,
    pub(crate) browser_binaries: bool,
    pub(crate) native_macos_arm64: bool,
}

impl Capabilities {
    /// Names of the capabilities `self` requires that `offered` lacks.
    pub(crate) fn missing_in(self, offered: Self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.docker && !offered.docker {
            missing.push("docker");
        }
        if self.nested_privileged_docker && !offered.nested_privileged_docker {
            missing.push("nested-privileged-docker");
        }
        if self.buildx_compose && !offered.buildx_compose {
            missing.push("buildx-compose");
        }
        if self.testcontainers && !offered.testcontainers {
            missing.push("testcontainers");
        }
        if self.services_with_readiness && !offered.services_with_readiness {
            missing.push("services-with-readiness");
        }
        if self.browser_binaries && !offered.browser_binaries {
            missing.push("browser-binaries");
        }
        if self.native_macos_arm64 && !offered.native_macos_arm64 {
            missing.push("native-macos-arm64");
        }
        missing
    }
}

/// What one provider offers: platforms, trust tier, and capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderCaps {
    pub(crate) platforms: BTreeSet<Platform>,
    /// Whether the provider is authorized for trusted-only units.
    pub(crate) trusted: bool,
    pub(crate) caps: Capabilities,
}

/// The static provider × platform capability matrix. Matrix changes are
/// explicit edits with tests, never inference.
#[must_use]
pub(crate) fn provider_caps(provider: ProviderId) -> ProviderCaps {
    match provider {
        ProviderId::GithubHosted => ProviderCaps {
            platforms: BTreeSet::from([Platform::LinuxX64, Platform::MacosArm64]),
            trusted: true,
            caps: Capabilities {
                docker: true,
                nested_privileged_docker: false,
                buildx_compose: true,
                testcontainers: true,
                services_with_readiness: true,
                browser_binaries: true,
                native_macos_arm64: true,
            },
        },
        ProviderId::GithubSelfHosted | ProviderId::Velnor => ProviderCaps {
            platforms: BTreeSet::from([Platform::LinuxX64]),
            trusted: true,
            caps: Capabilities {
                docker: true,
                nested_privileged_docker: true,
                buildx_compose: true,
                testcontainers: true,
                services_with_readiness: true,
                browser_binaries: true,
                native_macos_arm64: false,
            },
        },
    }
}

/// Why the planner excluded one (unit, provider) pair before expansion.
/// Exclusions are pre-declared, never reclassified after failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ExclusionReason {
    Platform,
    Trust,
    GenuinelyUnaffected,
    CapabilityUnsupported,
}

impl ExclusionReason {
    #[must_use]
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "std-idiomatic &self; by-value breaks fn-pointer use over borrowed iterators"
    )]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Trust => "trust",
            Self::GenuinelyUnaffected => "genuinely-unaffected",
            Self::CapabilityUnsupported => "capability-unsupported",
        }
    }
}

/// Platform/trust eligibility of one (unit, provider) pair on an event.
/// `event_trusted` is the controller-side verdict for the event (fork and bot
/// PRs are untrusted); same-repo origin or a requested label is never enough.
/// Platform/trust mismatch is a planner-declared exclusion, never silent.
pub(crate) fn eligibility(
    platform: Platform,
    trust: TrustReq,
    provider: ProviderId,
    event_trusted: bool,
) -> Result<(), ExclusionReason> {
    let caps = provider_caps(provider);
    if !caps.platforms.contains(&platform) {
        return Err(ExclusionReason::Platform);
    }
    if matches!(trust, TrustReq::TrustedOnly) && !event_trusted {
        return Err(ExclusionReason::Trust);
    }
    // Untrusted events execute on github-hosted only; local providers are
    // excluded with an explicit trust reason.
    if !event_trusted && provider.is_local() {
        return Err(ExclusionReason::Trust);
    }
    if matches!(trust, TrustReq::TrustedOnly) && !caps.trusted {
        return Err(ExclusionReason::Trust);
    }
    Ok(())
}

/// Capability check for an in-scope unit: an unsupported capability is a hard
/// error naming unit + provider + capability. Out-of-scope-via-exclusion is
/// the only non-error path, and exclusions are planner-declared.
pub(crate) fn check_capabilities(
    unit_id: &str,
    required: Capabilities,
    provider: ProviderId,
) -> Result<(), GeneratorError> {
    let offered = provider_caps(provider).caps;
    let missing = required.missing_in(offered);
    if missing.is_empty() {
        return Ok(());
    }
    Err(GeneratorError::usage(format!(
        "unit `{unit_id}` requires unsupported capabilities on provider `{provider}`: {}; declare the unit out of scope or drop the requirement",
        missing.join(", ")
    )))
}

/// The visibility-based runner policy: a public repository runs on
/// GitHub-hosted runners only, a private repository on Velnor runners only.
/// The singleton universe for one visibility.
#[must_use]
pub(crate) fn singleton_for_visibility(visibility: crate::visibility::Visibility) -> ProviderSet {
    ProviderSet::from([if visibility.is_public() {
        ProviderId::GithubHosted
    } else {
        ProviderId::Velnor
    }])
}

/// The control plane (planning, policy, recovery, required-result monitoring,
/// watchdog) runs on hosted when the universe contains it, otherwise on the
/// canonical-first local provider — so no hosted `runs-on` leaks into a
/// local-only surface. Under the visibility policy the universe is always a
/// singleton, which makes this exactly the visibility provider.
#[must_use]
pub(crate) fn control_plane_provider(universe: &ProviderSet) -> ProviderId {
    if universe.contains(&ProviderId::GithubHosted) {
        ProviderId::GithubHosted
    } else {
        universe
            .iter()
            .next()
            .copied()
            .unwrap_or(ProviderId::GithubHosted)
    }
}

/// A GitHub-owned execution label: inherently hosted, never a trust fact.
/// Hosted selectors must use these exclusively; anything else in a hosted
/// selector could route a hosted-only tree onto caller-managed runners.
#[must_use]
pub(crate) fn is_github_owned_label(label: &str) -> bool {
    label.starts_with("ubuntu-") || label.starts_with("macos-") || label.starts_with("windows-")
}

/// Stable plan digest over sorted unit IDs × sorted providers × exclusion
/// declarations × command/profile/features/fixture digests.
pub(crate) fn plan_digest(
    units: &[(String, ProviderSet, String)],
    exclusions: &[(String, ProviderId, ExclusionReason)],
) -> String {
    let mut digest_input = String::new();
    for (unit_id, providers, command_digest) in units {
        let _ = write!(digest_input, "unit:{unit_id}");
        for provider in providers {
            let _ = write!(digest_input, ":{provider}");
        }
        let _ = writeln!(digest_input, ":{command_digest}");
    }
    for (unit_id, provider, reason) in exclusions {
        let _ = writeln!(
            digest_input,
            "excluded:{unit_id}:{provider}:{}",
            reason.as_str()
        );
    }
    let digest = crate::s2::content_digest_bytes(digest_input.as_bytes());
    format!("{digest:016x}")
}

/// Bind the typed execution platform to the planner's full command/profile/
/// features/fixtures digest. The result record carries both fields, but the
/// digest is the run-level binding available to the strict verdict without a
/// second per-unit platform table.
pub(crate) fn execution_identity_digest(platform: Platform, payload_digest: &str) -> String {
    format!("platform={};payload={payload_digest}", platform.as_str())
}

/// The execution-specific portion of a required-result identity.
///
/// It is frozen from the plan before work starts. In particular, `required_host`
/// is not inferred from a job-reported hostname after execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResultBinding {
    pub(crate) required_host: RequiredHost,
    pub(crate) platform: Platform,
    pub(crate) target_architecture: TargetArchitecture,
    pub(crate) command_digest: String,
    pub(crate) profile_digest: String,
    pub(crate) fixture_digest: String,
}

/// Full required-result identity (spec §2): repository + source + run +
/// attempt + plan + unit + provider + required host + target platform and
/// architecture + command/profile/fixture digests.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "D2 part-A strict-results API; no schema-2 caller yet"
)]
pub(crate) struct ResultIdentity {
    pub(crate) repository_id: String,
    pub(crate) source_sha: String,
    pub(crate) run_id: String,
    pub(crate) run_attempt: String,
    pub(crate) plan_digest: String,
    pub(crate) unit_id: String,
    pub(crate) provider: ProviderId,
    pub(crate) required_host: RequiredHost,
    pub(crate) platform: Platform,
    pub(crate) target_architecture: TargetArchitecture,
    pub(crate) command_digest: String,
    pub(crate) profile_digest: String,
    pub(crate) fixture_digest: String,
}

impl ResultIdentity {
    #[must_use]
    pub(crate) fn binding(&self) -> ResultBinding {
        ResultBinding {
            required_host: self.required_host,
            platform: self.platform,
            target_architecture: self.target_architecture,
            command_digest: self.command_digest.clone(),
            profile_digest: self.profile_digest.clone(),
            fixture_digest: self.fixture_digest.clone(),
        }
    }
}

/// One observed result record keyed by its identity tuple.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "D2 part-A strict-results API; no schema-2 caller yet"
)]
pub(crate) struct ObservedResult {
    pub(crate) identity: ResultIdentity,
    pub(crate) outcome: ObservedOutcome,
}

/// The outcome an observed record carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "D2 part-A strict-results API; no schema-2 caller yet"
)]
pub(crate) enum ObservedOutcome {
    Success,
    Failed,
    Cancelled,
    TimedOut,
    Skipped,
}

/// Why one expected result failed the strict verdict.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "D2 part-A strict-results API; no schema-2 caller yet"
)]
pub(crate) enum VerdictFailure {
    Missing {
        unit_id: String,
        provider: ProviderId,
    },
    Skipped {
        unit_id: String,
        provider: ProviderId,
    },
    Cancelled {
        unit_id: String,
        provider: ProviderId,
    },
    TimedOut {
        unit_id: String,
        provider: ProviderId,
    },
    Failed {
        unit_id: String,
        provider: ProviderId,
    },
    DuplicateConflicting {
        unit_id: String,
        provider: ProviderId,
    },
    IdentityMismatch {
        unit_id: String,
        provider: ProviderId,
        reason: String,
    },
    StaleAttempt {
        unit_id: String,
        provider: ProviderId,
        attempt: String,
    },
    WrongProvider {
        unit_id: String,
        expected: ProviderId,
        claimed: ProviderId,
    },
    WrongHost {
        unit_id: String,
        provider: ProviderId,
        expected: RequiredHost,
        claimed: RequiredHost,
    },
}

impl VerdictFailure {
    #[must_use]
    #[allow(
        dead_code,
        reason = "D2 part-A strict-results API; no schema-2 caller yet"
    )]
    pub(crate) fn class(&self) -> &'static str {
        match self {
            Self::Missing { .. } => "missing",
            Self::Skipped { .. } => "skipped",
            Self::Cancelled { .. } => "cancelled",
            Self::TimedOut { .. } => "timed-out",
            Self::Failed { .. } => "failed",
            Self::DuplicateConflicting { .. } => "duplicate-conflicting",
            Self::IdentityMismatch { .. } => "identity-mismatch",
            Self::StaleAttempt { .. } => "stale-attempt",
            Self::WrongProvider { .. } => "wrong-provider",
            Self::WrongHost { .. } => "wrong-host",
        }
    }
}

/// Strict expected-set verdict (hosted, per run/attempt).
///
/// `expected` is the frozen plan set of (unit, provider) pairs plus their
/// execution bindings. Every member must be exactly `success` with matching
/// identity; anything else fails.
/// Records whose identity tuple does not match the run fail as
/// identity-mismatch; two records with the same identity and conflicting
/// outcomes fail as duplicate-conflicting. Excluded pairs are not in the
/// expected set and are never success.
#[allow(
    dead_code,
    reason = "D2 part-A strict-results API; no schema-2 caller yet"
)]
#[allow(
    clippy::too_many_lines,
    reason = "d2a shape: one complete verdict evaluator"
)]
pub(crate) fn evaluate_verdict(
    expected: &BTreeMap<(String, ProviderId), ResultBinding>,
    observed: &[ObservedResult],
    run: &RunIdentity,
) -> Vec<VerdictFailure> {
    let mut failures = Vec::new();
    let mut by_key: BTreeMap<(String, ProviderId), Vec<&ObservedResult>> = BTreeMap::new();
    for record in observed {
        let identity = &record.identity;
        let key = (identity.unit_id.clone(), identity.provider);
        if identity.repository_id != run.repository_id
            || identity.source_sha != run.source_sha
            || identity.run_id != run.run_id
        {
            failures.push(VerdictFailure::IdentityMismatch {
                unit_id: identity.unit_id.clone(),
                provider: identity.provider,
                reason: "repository, sha, or run id does not match this run".to_owned(),
            });
            continue;
        }
        if identity.run_attempt != run.run_attempt {
            failures.push(VerdictFailure::StaleAttempt {
                unit_id: identity.unit_id.clone(),
                provider: identity.provider,
                attempt: identity.run_attempt.clone(),
            });
            continue;
        }
        if identity.plan_digest != run.plan_digest {
            failures.push(VerdictFailure::IdentityMismatch {
                unit_id: identity.unit_id.clone(),
                provider: identity.provider,
                reason: "plan digest does not match the frozen plan".to_owned(),
            });
            continue;
        }
        let Some(expected_binding) = expected.get(&key) else {
            // A record for a pair outside the expected set is either a claim
            // for another provider's work or an unselected unit: both fail.
            let wrong_provider = expected.keys().any(|(unit, _)| unit == &identity.unit_id);
            if wrong_provider {
                let expected_provider = expected
                    .keys()
                    .find(|(unit, _)| unit == &identity.unit_id)
                    .map_or(identity.provider, |(_, provider)| *provider);
                failures.push(VerdictFailure::WrongProvider {
                    unit_id: identity.unit_id.clone(),
                    expected: expected_provider,
                    claimed: identity.provider,
                });
            } else {
                failures.push(VerdictFailure::IdentityMismatch {
                    unit_id: identity.unit_id.clone(),
                    provider: identity.provider,
                    reason: "unit is not in the expected set".to_owned(),
                });
            }
            continue;
        };
        if identity.required_host != expected_binding.required_host {
            failures.push(VerdictFailure::WrongHost {
                unit_id: identity.unit_id.clone(),
                provider: identity.provider,
                expected: expected_binding.required_host,
                claimed: identity.required_host,
            });
            continue;
        }
        if identity.platform != expected_binding.platform {
            failures.push(VerdictFailure::IdentityMismatch {
                unit_id: identity.unit_id.clone(),
                provider: identity.provider,
                reason: "target platform does not match the frozen result identity".to_owned(),
            });
            continue;
        }
        if identity.target_architecture != expected_binding.target_architecture {
            failures.push(VerdictFailure::IdentityMismatch {
                unit_id: identity.unit_id.clone(),
                provider: identity.provider,
                reason: "target architecture does not match the frozen result identity".to_owned(),
            });
            continue;
        }
        if identity.command_digest != expected_binding.command_digest {
            failures.push(VerdictFailure::IdentityMismatch {
                unit_id: identity.unit_id.clone(),
                provider: identity.provider,
                reason: "command digest does not match the frozen result identity".to_owned(),
            });
            continue;
        }
        if identity.profile_digest != expected_binding.profile_digest {
            failures.push(VerdictFailure::IdentityMismatch {
                unit_id: identity.unit_id.clone(),
                provider: identity.provider,
                reason: "profile digest does not match the frozen result identity".to_owned(),
            });
            continue;
        }
        if identity.fixture_digest != expected_binding.fixture_digest {
            failures.push(VerdictFailure::IdentityMismatch {
                unit_id: identity.unit_id.clone(),
                provider: identity.provider,
                reason: "fixture digest does not match the frozen result identity".to_owned(),
            });
            continue;
        }
        by_key.entry(key).or_default().push(record);
    }
    for (unit_id, provider) in expected.keys() {
        match by_key.get(&(unit_id.clone(), *provider)) {
            None => failures.push(VerdictFailure::Missing {
                unit_id: unit_id.clone(),
                provider: *provider,
            }),
            Some(records) => {
                let outcomes: BTreeSet<u8> = records
                    .iter()
                    .map(|record| match record.outcome {
                        ObservedOutcome::Success => 0,
                        ObservedOutcome::Failed => 1,
                        ObservedOutcome::Cancelled => 2,
                        ObservedOutcome::TimedOut => 3,
                        ObservedOutcome::Skipped => 4,
                    })
                    .collect();
                if records.len() != 1 || outcomes.len() > 1 {
                    failures.push(VerdictFailure::DuplicateConflicting {
                        unit_id: unit_id.clone(),
                        provider: *provider,
                    });
                    continue;
                }
                match records[0].outcome {
                    ObservedOutcome::Success => {}
                    ObservedOutcome::Failed => failures.push(VerdictFailure::Failed {
                        unit_id: unit_id.clone(),
                        provider: *provider,
                    }),
                    ObservedOutcome::Cancelled => failures.push(VerdictFailure::Cancelled {
                        unit_id: unit_id.clone(),
                        provider: *provider,
                    }),
                    ObservedOutcome::TimedOut => failures.push(VerdictFailure::TimedOut {
                        unit_id: unit_id.clone(),
                        provider: *provider,
                    }),
                    ObservedOutcome::Skipped => failures.push(VerdictFailure::Skipped {
                        unit_id: unit_id.clone(),
                        provider: *provider,
                    }),
                }
            }
        }
    }
    failures
}

/// The run the verdict binds every observed record to.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "D2 part-A strict-results API; no schema-2 caller yet"
)]
pub(crate) struct RunIdentity {
    pub(crate) repository_id: String,
    pub(crate) source_sha: String,
    pub(crate) run_id: String,
    pub(crate) run_attempt: String,
    pub(crate) plan_digest: String,
}

/// Per-(unit, provider) job id: `{provider}-{unit}`.
#[must_use]
pub(crate) fn unit_job_id(provider: ProviderId, unit_id: &str) -> String {
    format!("{}-{unit_id}", provider.as_str())
}

/// Job display name: the provider ID appears verbatim (grep-able invariant).
#[must_use]
pub(crate) fn unit_job_display_name(
    unit_label: &str,
    provider: ProviderId,
    unit_id: &str,
) -> String {
    format!("{unit_label} · {} — {unit_id}", provider.as_str())
}

/// Cache key grammar: repo / trust / provider / platform /
/// image-toolchain-ABI / options / dep-source-compat / unit / purpose.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "the key grammar has nine fixed segments"
)]
#[allow(
    dead_code,
    reason = "D2 part-A strict-results API; no schema-2 caller yet"
)]
pub(crate) fn cache_key(
    repo_id: &str,
    trust: TrustReq,
    provider: ProviderId,
    platform: Platform,
    toolchain_digest: &str,
    options_digest: &str,
    dep_source_digest: &str,
    unit_id: &str,
    purpose: &str,
) -> String {
    format!(
        "{repo_id}/{}/{}/{}/{toolchain_digest}/{options_digest}/{dep_source_digest}/{unit_id}/{purpose}",
        trust.as_str(),
        provider.as_str(),
        platform.as_str(),
    )
}

/// Artifact name: revision + os-arch + provider + plan digest segments.
#[must_use]
#[allow(
    dead_code,
    reason = "D2 part-A strict-results API; no schema-2 caller yet"
)]
pub(crate) fn artifact_name(
    revision: &str,
    os_arch: &str,
    provider: ProviderId,
    plan_digest: &str,
    unit_id: &str,
) -> String {
    format!(
        "{revision}-{os_arch}-{}-{plan_digest}-{unit_id}",
        provider.as_str()
    )
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "a test whose setup fails should panic loudly"
    )]
    #![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]

    use super::*;

    fn must_fail<T>(result: Result<T, GeneratorError>, context: &str) -> String {
        match result {
            Ok(_) => panic!("{context}: expected a failure, got success"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn provider_ids_parse_exactly_and_reject_aliases() {
        assert_eq!(
            ProviderId::parse("github-hosted").unwrap(),
            ProviderId::GithubHosted
        );
        assert_eq!(
            ProviderId::parse("github-self-hosted").unwrap(),
            ProviderId::GithubSelfHosted
        );
        assert_eq!(ProviderId::parse("velnor").unwrap(), ProviderId::Velnor);
        // No aliases, no inference, no case folding, no whitespace trimming.
        for alias in [
            "github",
            "both",
            "local",
            "self-hosted",
            "GitHub-Hosted",
            "VELNOR",
            "",
            " velnor",
            "velnor ",
            "github_hosted",
        ] {
            let error = must_fail(ProviderId::parse(alias), "legacy provider alias");
            assert!(
                error.contains("unknown provider") && error.contains("expected one of"),
                "unexpected error for `{alias}`: {error}"
            );
        }
    }

    #[test]
    fn provider_sets_reject_duplicates_and_empty_and_unbounded() {
        let field = "providers";
        let dupes = vec!["velnor".to_owned(), "velnor".to_owned()];
        let error = must_fail(parse_provider_set(&dupes, field), "duplicate provider");
        assert!(error.contains("more than once"), "{error}");
        let error = must_fail(
            require_non_empty(&ProviderSet::new(), field),
            "empty provider set",
        );
        assert!(error.contains("at least one provider"), "{error}");
        let universe = ProviderSet::from([ProviderId::GithubHosted]);
        let subset = ProviderSet::from([ProviderId::Velnor]);
        let error = must_fail(
            require_subset(&subset, &universe, "automatic_providers", "providers"),
            "unbounded subset",
        );
        assert!(error.contains("automatic_providers"), "{error}");
    }

    #[test]
    fn provider_modes_are_exact_and_keep_the_three_provider_universe() {
        assert_eq!(ProviderMode::ALL.len(), 3);
        assert_eq!(
            ProviderMode::parse("native-only").unwrap(),
            ProviderMode::NativeOnly
        );
        assert_eq!(
            ProviderMode::parse("scale-set-only").unwrap(),
            ProviderMode::ScaleSetOnly
        );
        assert_eq!(ProviderMode::parse("both").unwrap(), ProviderMode::Both);
        for alias in ["github", "velnor", "native", "scale-set", ""] {
            let error = must_fail(ProviderMode::parse(alias), "provider mode alias");
            assert!(error.contains("unknown provider mode"), "{alias}: {error}");
        }
        for mode in ProviderMode::ALL {
            assert_eq!(mode.provider_universe().len(), ProviderId::ALL.len());
            assert!(mode
                .automatic_providers()
                .contains(&ProviderId::GithubHosted));
        }
    }

    #[test]
    fn platform_and_trust_parse_is_strict() {
        assert_eq!(Platform::parse("linux-x64").unwrap(), Platform::LinuxX64);
        assert_eq!(
            Platform::parse("macos-arm64").unwrap(),
            Platform::MacosArm64
        );
        for unknown in ["macos-15", "ubuntu-24.04", "linux", ""] {
            let error = must_fail(Platform::parse(unknown), "unknown platform");
            assert!(error.contains("unknown platform"), "{error}");
        }
        assert_eq!(
            TrustReq::parse("trusted-only").unwrap(),
            TrustReq::TrustedOnly
        );
        for unknown in ["trusted", "example-trusted", "untrusted", ""] {
            let error = must_fail(TrustReq::parse(unknown), "unknown trust");
            assert!(error.contains("unknown trust requirement"), "{error}");
        }
    }

    #[test]
    fn missing_selector_is_a_hard_error_naming_the_provider() {
        let selectors = SelectorMap::new();
        let error = must_fail(
            runs_on_for(&selectors, ProviderId::Velnor).map(|_| ()),
            "missing selector",
        );
        assert!(error.contains("[workflow.selectors.velnor]"), "{error}");
    }

    #[test]
    fn selector_identity_is_provider_and_host_qualified() {
        let selectors = SelectorMap::from([
            (
                ProviderId::GithubHosted,
                ProviderSelector {
                    runs_on: vec!["ubuntu-24.04".to_owned()],
                },
            ),
            (
                ProviderId::GithubSelfHosted,
                ProviderSelector {
                    runs_on: vec![
                        SELF_HOSTED_LABEL.to_owned(),
                        SCALE_SET_LABEL.to_owned(),
                        LOCAL_MAC_HOST_LABEL.to_owned(),
                    ],
                },
            ),
            (
                ProviderId::Velnor,
                ProviderSelector {
                    runs_on: vec![
                        SELF_HOSTED_LABEL.to_owned(),
                        NATIVE_LABEL.to_owned(),
                        LOCAL_MAC_HOST_LABEL.to_owned(),
                    ],
                },
            ),
        ]);
        let universe = ProviderId::ALL.into_iter().collect();
        assert!(validate_selector_identities(&selectors, &universe).is_ok());

        let mut hosted_spoof = selectors.clone();
        hosted_spoof
            .get_mut(&ProviderId::GithubHosted)
            .unwrap()
            .runs_on = vec![SCALE_SET_LABEL.to_owned()];
        let error = must_fail(
            validate_selector_identities(&hosted_spoof, &universe),
            "hosted selector spoof",
        );
        assert!(
            error.contains("github-hosted") && error.contains("GitHub-hosted"),
            "{error}"
        );

        let mut unqualified_native = selectors;
        unqualified_native
            .get_mut(&ProviderId::Velnor)
            .unwrap()
            .runs_on = vec![SELF_HOSTED_LABEL.to_owned(), NATIVE_LABEL.to_owned()];
        let error = must_fail(
            validate_selector_identities(&unqualified_native, &universe),
            "unqualified native selector",
        );
        assert!(error.contains("exactly one host label"), "{error}");
    }

    #[test]
    fn runner_targets_parse_only_canonical_providers_and_explicit_macos() {
        assert_eq!(
            RunnerTarget::parse("github-hosted").unwrap(),
            RunnerTarget::Provider(ProviderId::GithubHosted)
        );
        assert_eq!(
            RunnerTarget::parse("github-self-hosted").unwrap(),
            RunnerTarget::Provider(ProviderId::GithubSelfHosted)
        );
        assert_eq!(
            RunnerTarget::parse("velnor").unwrap(),
            RunnerTarget::Provider(ProviderId::Velnor)
        );
        assert_eq!(
            RunnerTarget::parse("macos").unwrap(),
            RunnerTarget::GithubHostedMacos
        );
        for alias in ["github", "both", "native", "self-hosted"] {
            assert!(
                RunnerTarget::parse(alias).is_err(),
                "alias accepted: {alias}"
            );
        }
    }

    #[test]
    fn unsupported_capability_is_a_hard_error_naming_unit_provider_and_capability() {
        let required = Capabilities {
            nested_privileged_docker: true,
            ..Capabilities::default()
        };
        let error = must_fail(
            check_capabilities("rust-docker", required, ProviderId::GithubHosted),
            "unsupported capability",
        );
        assert!(
            error.contains("rust-docker")
                && error.contains("github-hosted")
                && error.contains("nested-privileged-docker"),
            "{error}"
        );
        assert!(check_capabilities("rust-docker", required, ProviderId::Velnor).is_ok());
    }

    #[test]
    fn eligibility_excludes_by_platform_and_trust_with_named_reasons() {
        assert_eq!(
            eligibility(
                Platform::MacosArm64,
                TrustReq::UntrustedOk,
                ProviderId::Velnor,
                true
            ),
            Err(ExclusionReason::Platform)
        );
        assert_eq!(
            eligibility(
                Platform::LinuxX64,
                TrustReq::TrustedOnly,
                ProviderId::GithubHosted,
                false
            ),
            Err(ExclusionReason::Trust)
        );
        assert_eq!(
            eligibility(
                Platform::LinuxX64,
                TrustReq::UntrustedOk,
                ProviderId::Velnor,
                false
            ),
            Err(ExclusionReason::Trust)
        );
        assert!(eligibility(
            Platform::LinuxX64,
            TrustReq::UntrustedOk,
            ProviderId::GithubHosted,
            false
        )
        .is_ok());
    }

    #[test]
    fn strict_verdict_binds_platform_and_requires_one_record() {
        let provider = ProviderId::GithubHosted;
        let unit_id = "rust-a".to_owned();
        let command_digest = execution_identity_digest(Platform::LinuxX64, "payload");
        let run = RunIdentity {
            repository_id: "repo".to_owned(),
            source_sha: "sha".to_owned(),
            run_id: "run".to_owned(),
            run_attempt: "1".to_owned(),
            plan_digest: "plan".to_owned(),
        };
        let binding = ResultBinding {
            required_host: RequiredHost::GithubHosted,
            platform: Platform::LinuxX64,
            target_architecture: TargetArchitecture::X86_64,
            command_digest: command_digest.clone(),
            profile_digest: "profile".to_owned(),
            fixture_digest: "fixture".to_owned(),
        };
        let identity = || ResultIdentity {
            repository_id: run.repository_id.clone(),
            source_sha: run.source_sha.clone(),
            run_id: run.run_id.clone(),
            run_attempt: run.run_attempt.clone(),
            plan_digest: run.plan_digest.clone(),
            unit_id: unit_id.clone(),
            provider,
            required_host: RequiredHost::GithubHosted,
            platform: Platform::LinuxX64,
            target_architecture: TargetArchitecture::X86_64,
            command_digest: command_digest.clone(),
            profile_digest: "profile".to_owned(),
            fixture_digest: "fixture".to_owned(),
        };
        let expected = BTreeMap::from([((unit_id.clone(), provider), binding)]);
        let success = || ObservedResult {
            identity: identity(),
            outcome: ObservedOutcome::Success,
        };
        assert!(evaluate_verdict(&expected, &[success()], &run).is_empty());

        let mut wrong_platform = success();
        wrong_platform.identity.platform = Platform::MacosArm64;
        let failures = evaluate_verdict(&expected, &[wrong_platform], &run);
        assert!(failures.iter().any(|failure| matches!(
            failure,
            VerdictFailure::IdentityMismatch { reason, .. }
                if reason.contains("target platform")
        )));

        let duplicate_failures = evaluate_verdict(&expected, &[success(), success()], &run);
        assert!(duplicate_failures
            .iter()
            .any(|failure| matches!(failure, VerdictFailure::DuplicateConflicting { .. })));
    }
}
