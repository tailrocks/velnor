//! Snapshot identity and cache retention: the two halves of the compiler-state
//! contract that have to move together.
//!
//! A GitHub cache entry is immutable, so a key that only names *compatibility*
//! freezes the snapshot at its first save: every later run restores exactly
//! that entry, produces new compiler state, and is refused a save because the
//! key already exists. Identity therefore names two different things, and the
//! generator renders both:
//!
//! * **Compatibility** — everything that decides whether a snapshot may be
//!   imported at all (cache schema, Mr. Boxington version, the pinned Rust
//!   toolchain, the hosted image, the linker recipe, `RUSTFLAGS`, the Cargo
//!   configuration inputs, and the build recipe). The generator digests those
//!   facts canonically and renders the digest into the key; content that only
//!   GitHub can hash at restore time rides `hashFiles`.
//! * **Freshness** — one successful state inside a compatible class. The
//!   freshness segment is a content digest of the class's source inputs, so a
//!   source change mints a new key, the run that produced the new state is the
//!   one that saves it, and an exact hit (same source state, snapshot already
//!   saved) writes nothing.
//!
//! Freshness creates entries, so retention has to bound them: per-class
//! budgets, a generation bound per variant class, and reservations that keep
//! toolchain and source seeds alive before rolling compiler snapshots are
//! allocated. [`RetentionPolicy`] and [`plan_evictions`] are the single
//! definition of that policy; the maintenance job plans its evictions through
//! the same code (`velnor-workflow cache-plan`), so a live run and a test
//! cannot disagree about what retention means.

use std::fmt::Write as _;

use sha2::{Digest as _, Sha256};

use crate::{CachePurpose, RustToolchain};

/// The snapshot key schema. Bumping it abandons every previously saved
/// snapshot: entries saved under an older schema are unreachable by design,
/// because the schema is part of the compatibility identity.
pub(crate) const SNAPSHOT_SCHEMA: &str = "v3";

/// Hex characters of the compatibility digest rendered into a key. Twelve
/// characters are 48 bits of the SHA-256 over the canonical facts — far more
/// than the collision resistance a handful of classes per repository needs,
/// and short enough to keep keys readable.
const COMPATIBILITY_DIGEST_CHARS: usize = 12;

/// The compatibility facts a snapshot class is pinned to, in full. A fact that
/// changes compiler output and is missing here would let a snapshot compiled
/// under one recipe be imported under another, so the struct is exhaustive by
/// construction: every field participates in the digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompatibilityFacts {
    /// The snapshot key schema this generator renders.
    pub(crate) schema: &'static str,
    /// The payload family the snapshot carries: `mbx` object closures or the
    /// Docker mutable-mount seed bundle.
    pub(crate) payload: &'static str,
    /// The Mr. Boxington version that produced the snapshot.
    pub(crate) mbx_version: String,
    /// The repository's pinned Rust toolchain, as the provisioning steps
    /// install it.
    pub(crate) toolchain: Option<RustToolchain>,
    /// The hosted runner image: the host OS and its native toolchain.
    pub(crate) host_image: String,
    /// The provider the snapshot was saved from: cross-provider restores are
    /// never compatible.
    pub(crate) provider: String,
    /// The execution platform the snapshot was saved from.
    pub(crate) platform: String,
    /// The trust tier the snapshot was saved from: cross-tier restores are
    /// never compatible.
    pub(crate) trust: String,
    /// The native linker recipe the lane arranges. The linker's own version is
    /// deliberately not a fact: a snapshot's objects are linker-independent
    /// (rustc links only final artifacts), and a linker change rides the
    /// `RUSTFLAGS` fingerprint cargo already rebuilds on.
    pub(crate) linker: String,
    /// The `RUSTFLAGS` the lane exports, empty when it exports none.
    pub(crate) rustflags: String,
    /// The Cargo configuration and dependency input paths the key transports.
    pub(crate) cargo_inputs: Vec<String>,
    /// The build recipe the lane runs, normalized and ordered.
    pub(crate) recipe: Vec<String>,
}

impl CompatibilityFacts {
    /// The canonical compatibility digest rendered into snapshot keys.
    ///
    /// Canonical, not merely stable: the field set is fixed, list order is
    /// normalized away (declaration order is not a compatibility fact), and
    /// every string is JSON-escaped, so the same facts digest the same value on
    /// every machine and across generator versions.
    pub(crate) fn digest(&self) -> String {
        let mut canonical = String::new();
        canonical.push('{');
        for (key, value) in [
            ("cargo_inputs", sorted_strings(&self.cargo_inputs)),
            ("host_image", vec![self.host_image.clone()]),
            ("linker", vec![self.linker.clone()]),
            ("mbx_version", vec![self.mbx_version.clone()]),
            ("payload", vec![self.payload.to_owned()]),
            ("platform", vec![self.platform.clone()]),
            ("provider", vec![self.provider.clone()]),
            ("recipe", sorted_strings(&self.recipe)),
            ("rustflags", vec![self.rustflags.clone()]),
            ("schema", vec![self.schema.to_owned()]),
            ("trust", vec![self.trust.clone()]),
        ] {
            write_field(&mut canonical, key, &value);
        }
        write_field(
            &mut canonical,
            "toolchain",
            &self
                .toolchain
                .as_ref()
                .map_or_else(Vec::new, toolchain_fields),
        );
        canonical.push('}');
        let digest = Sha256::digest(canonical.as_bytes());
        let mut output = String::with_capacity(COMPATIBILITY_DIGEST_CHARS);
        for byte in &digest[..COMPATIBILITY_DIGEST_CHARS.div_ceil(2)] {
            let _ = write!(output, "{byte:02x}");
        }
        output.truncate(COMPATIBILITY_DIGEST_CHARS);
        output
    }
}

/// The toolchain pin as the ordered field list the digest consumes.
fn toolchain_fields(pin: &RustToolchain) -> Vec<String> {
    vec![
        format!("channel={}", pin.channel()),
        format!("components={}", sorted_strings(pin.components()).join(",")),
        format!("profile={}", pin.profile().unwrap_or_default()),
        format!("targets={}", sorted_strings(pin.targets()).join(",")),
    ]
}

fn sorted_strings(values: &[String]) -> Vec<String> {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted
}

/// One canonical `key:[values]` field. Keys are written in the fixed order the
/// caller lists them; every string is JSON-escaped, so the bytes are stable.
fn write_field(output: &mut String, key: &str, values: &[String]) {
    if !output.ends_with('{') {
        output.push(',');
    }
    let _ = write!(
        output,
        "{}:[",
        serde_json::to_string(key).unwrap_or_default()
    );
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        let _ = write!(
            output,
            "{}",
            serde_json::to_string(value).unwrap_or_default()
        );
    }
    output.push(']');
}

/// The leading, compatibility-only part of a snapshot key: the namespace the
/// transport family owns, the schema, and the compatibility digest. Everything
/// after it — runtime segments, dependency inputs, freshness — is segment
/// structure, so retention and restore-prefix ordering are expressed against
/// this prefix alone.
pub(crate) fn snapshot_class_prefix(namespace: &str, compatibility: &str) -> String {
    format!("{namespace}-{SNAPSHOT_SCHEMA}-{compatibility}")
}

/// The runtime segments every snapshot key interpolates: the same compatibility
/// class is a different snapshot on a different runner OS or architecture.
pub(crate) const RUNTIME_SEGMENTS: &str = "${{ runner.os }}-${{ runner.arch }}";

/// A rendered snapshot key: compatibility class, runtime segments, the
/// provider/platform/trust segments that make cross-tier and cross-provider
/// hits impossible, the dependency inputs GitHub hashes at restore time, and
/// the freshness segment that lets a state-advancing run save.
pub(crate) fn snapshot_key(
    class_prefix: &str,
    variant: &str,
    dependency_inputs: &str,
    freshness: &str,
    segments: &KeySegments,
) -> String {
    format!(
        "{class_prefix}-{RUNTIME_SEGMENTS}-{}-{}-{}-{variant}-{dependency_inputs}-{freshness}",
        segments.provider, segments.platform, segments.trust
    )
}

/// The restore prefixes of a snapshot key, newest-compatible-first. Only
/// prefixes are listed — never a complete earlier key — because the cache
/// service returns the most recently created entry matching any of them, and
/// an exact old generation on the list could shadow a newer compatible
/// fallback. Every prefix carries the provider/platform/trust segments, so a
/// restore can never cross providers, platforms, or trust tiers.
pub(crate) fn snapshot_restore_keys(
    class_prefix: &str,
    variant: &str,
    dependency_inputs: &str,
    segments: &KeySegments,
) -> String {
    format!(
        "{class_prefix}-{RUNTIME_SEGMENTS}-{}-{}-{}-{variant}-{dependency_inputs}-\n            \
         {class_prefix}-{RUNTIME_SEGMENTS}-{}-{}-{}-{variant}-",
        segments.provider,
        segments.platform,
        segments.trust,
        segments.provider,
        segments.platform,
        segments.trust,
    )
}

/// The provider/platform/trust segments of a snapshot key: literals where the
/// generator knows them, `${{ inputs.* }}` expressions in collapsed jobs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeySegments {
    pub(crate) provider: String,
    pub(crate) platform: String,
    pub(crate) trust: String,
}

/// The `hashFiles(...)` expression that carries a class's source inputs into
/// the freshness segment. The derivation that produces the list always names
/// at least the class's own source root; a rendered key whose freshness
/// segment hashes nothing is a generation error, which
/// [`validate_snapshot_cache_keys`] refuses.
pub(crate) fn freshness_expression(files: &[String]) -> String {
    let quoted = files
        .iter()
        .map(|path| format!("'{}'", path.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("${{{{ hashFiles({quoted}) }}}}")
}

/// Whether a rendered `cache-key:` value is a snapshot key of the current
/// schema: it names the schema, carries a compatibility digest, and transports
/// freshness (a key that names compatibility but hashes no source state is the
/// frozen-snapshot defect, so it is not accepted as a snapshot key).
pub(crate) fn is_snapshot_key(key: &str) -> bool {
    let Some(rest) = key
        .split_once(&format!("-{SNAPSHOT_SCHEMA}-"))
        .map(|(_, rest)| rest)
    else {
        return false;
    };
    let digest = rest.split('-').next().unwrap_or_default();
    digest.len() == COMPATIBILITY_DIGEST_CHARS
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        && rest.contains("hashFiles('")
}

/// Why retention selected an entry for eviction. Recorded per class in the
/// maintenance job summary so a later cold run can be correlated with the
/// eviction that caused it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum EvictionReason {
    /// A newer generation of the same variant class exists; the bound keeps a
    /// bounded number of generations per class.
    GenerationBeyondBound,
    /// The class holds more bytes than its budget reserves.
    ClassBudget,
    /// The account is over its total budget and the class is not protected.
    GlobalBudget,
}

/// How retention treats one cache class.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Tier {
    /// Reserved before rolling state is allocated: toolchain seeds and Cargo
    /// source bundles. Never selected by the global sweep.
    Protected,
    /// The Docker seed baseline: one live generation per compatibility class,
    /// reserved from the global sweep but bounded like rolling state.
    Baseline,
    /// Rolling compiler snapshots: first to be evicted, bounded per class.
    Rolling,
}

impl From<CachePurpose> for Tier {
    fn from(purpose: CachePurpose) -> Self {
        match purpose {
            CachePurpose::CargoSources | CachePurpose::Toolchains => Self::Protected,
            CachePurpose::DockerSeed => Self::Baseline,
            CachePurpose::Generic | CachePurpose::Outputs => Self::Rolling,
        }
    }
}

/// How a purpose-qualified retention class recognizes an already-saved
/// Actions-cache key. Matchers do not define the class; [`CachePurpose`] does.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CacheKeyMatcher {
    /// The key contains one generator-owned namespace fragment.
    Contains(&'static str),
    /// The anchored generic-CI namespace for Rust Cargo-source bundles:
    /// `ci-<runner-os>-rust-...` and `ci-release-<runner-os>-rust-...`, never
    /// the broad `ci-` prefix.
    CiRustCargoSources,
    /// Non-Rust unit dependency bundles under the same anchored `ci-` /
    /// `ci-release-` namespace: `bun-`, `docs-`, and `opentofu-` units.
    CiGenericUnitCaches,
}

fn ci_prefixed_unit(key: &str) -> Option<(&str, &str)> {
    const PREFIXES: [&str; 2] = ["ci-release-", "ci-"];
    const UNIT_MARKERS: [&str; 4] = ["-rust-", "-bun-", "-docs-", "-opentofu-"];
    for prefix in PREFIXES {
        let Some(rest) = key.strip_prefix(prefix) else {
            continue;
        };
        for marker in UNIT_MARKERS {
            let Some(pos) = rest.find(marker) else {
                continue;
            };
            let runner = rest.get(..pos)?;
            if runner.is_empty() {
                continue;
            }
            let unit = rest.get(pos + 1..)?;
            return Some((runner, unit));
        }
    }
    None
}

impl CacheKeyMatcher {
    fn matches(self, key: &str) -> bool {
        match self {
            Self::Contains(fragment) => key.contains(fragment),
            Self::CiRustCargoSources => {
                ci_prefixed_unit(key).is_some_and(|(_, unit)| unit.starts_with("rust-"))
            }
            Self::CiGenericUnitCaches => ci_prefixed_unit(key).is_some_and(|(_, unit)| {
                unit.starts_with("bun-")
                    || unit.starts_with("docs-")
                    || unit.starts_with("opentofu-")
            }),
        }
    }
}

/// One cache class of the retention policy.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct ClassPolicy {
    /// The class identifier the job summary records.
    pub(crate) id: &'static str,
    /// Why the class exists. This is the semantic identity that selects the
    /// retention tier; markers only recognize the already-saved account entry.
    pub(crate) purpose: CachePurpose,
    /// The account-entry matchers for this purpose-based class. First match
    /// wins; an entry matching none is `unclassified` and is treated as
    /// rolling state without a reservation, so nothing in the account is
    /// unreachable by retention.
    pub(crate) markers: &'static [CacheKeyMatcher],
    /// The bytes the class may hold. `0` reserves nothing.
    pub(crate) budget_bytes: u64,
    /// The generations of one variant class that survive. `0` bounds nothing.
    pub(crate) generation_bound: u32,
}

/// The retention policy the maintenance job enforces and the rendered workflow
/// carries verbatim.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct RetentionPolicy {
    /// The Actions cache account budget, in bytes.
    pub(crate) total_bytes: u64,
    /// How long the newest generation of a variant is treated as
    /// producer-active: a producer may have published it seconds ago or be
    /// about to publish its successor, and creation — not access — is the
    /// only producer signal the cache API offers. A superseded generation of
    /// the same variant is not producer-active: the newer save is the signal
    /// that the older entry is no longer being written.
    pub(crate) producer_window_seconds: u64,
    /// The classes, in classification order.
    pub(crate) classes: Vec<ClassPolicy>,
}

impl RetentionPolicy {
    /// The default policy for an `8 GiB` account. Reservations are declared
    /// before rolling state: toolchain seeds and Cargo source bundles are the
    /// entries every job restores, so they are priced first, the Docker seed
    /// baseline is bounded to one live generation, and what remains is the
    /// rolling compiler-snapshot budget.
    pub(crate) fn default_policy() -> Self {
        Self {
            total_bytes: 8_589_934_592,
            producer_window_seconds: 2 * 60 * 60,
            classes: vec![
                ClassPolicy {
                    id: "toolchain-seeds",
                    purpose: CachePurpose::Toolchains,
                    markers: &[
                        CacheKeyMatcher::Contains("velnor-rustup-"),
                        CacheKeyMatcher::Contains("velnor-mold-"),
                        CacheKeyMatcher::Contains("velnor-cargo-bin-"),
                        CacheKeyMatcher::Contains("mise-v1-"),
                    ],
                    budget_bytes: 2 * GIBIBYTE,
                    generation_bound: 0,
                },
                ClassPolicy {
                    id: "runtime-binary",
                    purpose: CachePurpose::Generic,
                    markers: &[CacheKeyMatcher::Contains("velnor-workflow-v1-")],
                    budget_bytes: GIBIBYTE / 4,
                    generation_bound: 1,
                },
                ClassPolicy {
                    id: "source-bundles",
                    purpose: CachePurpose::CargoSources,
                    markers: &[
                        CacheKeyMatcher::Contains("velnor-release-cargo-"),
                        CacheKeyMatcher::Contains("velnor-cargo-"),
                        CacheKeyMatcher::CiRustCargoSources,
                    ],
                    budget_bytes: 3 * GIBIBYTE / 2,
                    generation_bound: 0,
                },
                ClassPolicy {
                    id: "unit-caches",
                    purpose: CachePurpose::Generic,
                    markers: &[CacheKeyMatcher::CiGenericUnitCaches],
                    budget_bytes: GIBIBYTE / 4,
                    generation_bound: 1,
                },
                ClassPolicy {
                    id: "renovate-repository",
                    purpose: CachePurpose::Generic,
                    markers: &[CacheKeyMatcher::Contains("velnor-renovate-")],
                    budget_bytes: GIBIBYTE / 2,
                    generation_bound: 2,
                },
                ClassPolicy {
                    id: "docker-seed",
                    purpose: CachePurpose::DockerSeed,
                    markers: &[
                        CacheKeyMatcher::Contains("velnor-docker-seed-"),
                        CacheKeyMatcher::Contains("guest-seed-"),
                    ],
                    budget_bytes: GIBIBYTE,
                    generation_bound: 1,
                },
                ClassPolicy {
                    id: "compiler-snapshots",
                    purpose: CachePurpose::Outputs,
                    markers: &[
                        CacheKeyMatcher::Contains("-mbx-v3-"),
                        CacheKeyMatcher::Contains("velnor-policy-mbx-"),
                    ],
                    budget_bytes: 3 * GIBIBYTE,
                    generation_bound: 2,
                },
            ],
        }
    }

    /// Build the maintenance retention policy from `[cache.github]` overrides.
    /// Absent fields keep [`Self::default_policy`] values.
    pub(crate) fn from_config(config: &crate::config::CacheGithubSection) -> Self {
        let mut policy = Self::default_policy();
        if let Some(total_bytes) = config.budget_bytes {
            policy.total_bytes = total_bytes;
        }
        if let Some(producer_window_seconds) = config.producer_window_seconds {
            policy.producer_window_seconds = producer_window_seconds;
        }
        if let Some(bound) = config.mbx_generation_bound
            && let Some(class) = policy
                .classes
                .iter_mut()
                .find(|class| class.id == "compiler-snapshots")
        {
            class.generation_bound = bound;
        }
        policy
    }
}

const GIBIBYTE: u64 = 1_073_741_824;

/// Held bytes and entry count for one retention class in a live account
/// snapshot.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct ClassBudgetTotal {
    pub(crate) id: String,
    pub(crate) budget_bytes: u64,
    pub(crate) held_bytes: u64,
    pub(crate) entry_count: u32,
}

/// The account budget, held total, headroom, and per-class totals the
/// maintenance job uses for enforcement and low-headroom warnings.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct BudgetReport {
    pub(crate) total_budget_bytes: u64,
    pub(crate) total_held_bytes: u64,
    pub(crate) headroom_bytes: i64,
    pub(crate) classes: Vec<ClassBudgetTotal>,
}

fn classify_key<'a>(key: &str, policy: &'a RetentionPolicy) -> (&'a str, u64, u32) {
    let class = policy
        .classes
        .iter()
        .find(|class| class.markers.iter().any(|matcher| matcher.matches(key)))
        .map_or("unclassified", |class| class.id);
    let budget_bytes = policy
        .classes
        .iter()
        .find(|candidate| candidate.id == class)
        .map_or(0, |candidate| candidate.budget_bytes);
    (class, budget_bytes, 1)
}

/// Sum the live account by retention class so budget mode can expose per-class
/// totals for headroom scripting.
pub(crate) fn budget_report(entries: &[CacheEntry], policy: &RetentionPolicy) -> BudgetReport {
    let mut classes: Vec<(String, u64, u32, u64)> = Vec::new();
    for entry in entries {
        let (class_id, budget_bytes, _) = classify_key(&entry.key, policy);
        match classes.iter_mut().find(|(id, _, _, _)| id == class_id) {
            Some((_, held, count, budget)) => {
                *held = held.saturating_add(entry.size_in_bytes);
                *count += 1;
                *budget = budget_bytes;
            }
            None => classes.push((class_id.to_owned(), entry.size_in_bytes, 1, budget_bytes)),
        }
    }
    classes.sort_by(|(left, _, _, _), (right, _, _, _)| left.cmp(right));
    let total_held_bytes: u64 = entries.iter().map(|entry| entry.size_in_bytes).sum();
    let headroom_bytes = i64::try_from(policy.total_bytes)
        .unwrap_or(i64::MAX)
        .saturating_sub(i64::try_from(total_held_bytes).unwrap_or(i64::MAX));
    BudgetReport {
        total_budget_bytes: policy.total_bytes,
        total_held_bytes,
        headroom_bytes,
        classes: classes
            .into_iter()
            .map(
                |(id, held_bytes, entry_count, budget_bytes)| ClassBudgetTotal {
                    id,
                    budget_bytes,
                    held_bytes,
                    entry_count,
                },
            )
            .collect(),
    }
}

/// One entry of the Actions cache account, as the API reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CacheEntry {
    pub(crate) id: String,
    pub(crate) key: String,
    pub(crate) size_in_bytes: u64,
    /// RFC 3339 UTC timestamp of the save.
    pub(crate) created_at: String,
}

/// One selected eviction.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct Eviction {
    pub(crate) id: String,
    pub(crate) key: String,
    pub(crate) class: String,
    pub(crate) reason: EvictionReason,
    pub(crate) size_in_bytes: u64,
}

/// The classified view of one entry the planner works on.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Classified {
    entry: CacheEntry,
    class: String,
    tier: Tier,
    budget_bytes: u64,
    generation_bound: u32,
    /// The variant class a snapshot belongs to: its key without the trailing
    /// `hashFiles` segments. Empty for entries that carry none.
    generation: String,
    /// Seconds since the save, or `-1` when the timestamp cannot be read. An
    /// unreadable timestamp is treated as producer-active: retention refuses
    /// to evict what it cannot age.
    age_seconds: i64,
    purpose: CachePurpose,
    eligible: bool,
}

/// Seconds of the POSIX epoch, or `None` when the timestamp is unreadable.
///
/// After `ss`, an optional `.` and one or more ASCII digits may precede `Z`
/// or `±HH:MM`. GitHub's cache API emits that fractional form
/// (`2026-09-13T17:30:16.329079Z`); the fraction is skipped, age is whole
/// seconds, and extra non-whitespace after the timezone is refused.
fn epoch_of(created_at: &str) -> Option<i64> {
    let bytes = created_at.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| {
        created_at
            .get(range)
            .and_then(|text| text.parse::<i64>().ok())
    };
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    let hour = number(11..13)?;
    let minute = number(14..16)?;
    let second = number(17..19)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut index = 19usize;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let digits = bytes[index..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits == 0 {
            return None;
        }
        index += digits;
    }
    let (offset_seconds, rest) = match bytes.get(index).copied() {
        Some(b'Z') => (0, index + 1),
        Some(sign @ (b'+' | b'-')) => {
            let sign = if sign == b'+' { 1 } else { -1 };
            let hours = number(index + 1..index + 3)?;
            let minutes = number(index + 4..index + 6)?;
            (sign * (hours * 3600 + minutes * 60), index + 6)
        }
        _ => return None,
    };
    if bytes
        .get(rest)
        .is_some_and(|byte| *byte != b'\0' && !byte.is_ascii_whitespace())
    {
        return None;
    }
    // Days from the civil date, Howard Hinnant's `days_from_civil`.
    let year_shifted = if month <= 2 { year - 1 } else { year };
    let era = year_shifted.div_euclid(400);
    let year_of_era = year_shifted - era * 400;
    let month_shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_era = (153 * month_shifted + 2) / 5 + day - 1;
    let day_number = era * 146_097 + year_of_era * 365 + year_of_era / 4 - year_of_era / 100
        + day_of_era
        - 719_468;
    Some(day_number * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds)
}

fn classify(entries: &[CacheEntry], policy: &RetentionPolicy, now_epoch: i64) -> Vec<Classified> {
    let mut classified = entries
        .iter()
        .cloned()
        .map(|entry| {
            let class = policy.classes.iter().find(|class| {
                class
                    .markers
                    .iter()
                    .any(|matcher| matcher.matches(&entry.key))
            });
            let (class_id, purpose, budget_bytes, generation_bound) = class.map_or_else(
                || ("unclassified", CachePurpose::Generic, 0, 0),
                |class| {
                    (
                        class.id,
                        class.purpose,
                        class.budget_bytes,
                        class.generation_bound,
                    )
                },
            );
            let generation = strip_hash_segments(&entry.key);
            let age_seconds = epoch_of(&entry.created_at).map_or(-1, |created| now_epoch - created);
            Classified {
                eligible: false,
                entry,
                class: class_id.to_owned(),
                tier: Tier::from(purpose),
                purpose,
                budget_bytes,
                generation_bound,
                generation,
                age_seconds,
            }
        })
        .collect::<Vec<_>>();
    apply_producer_eligibility(&mut classified, policy);
    classified
}

fn generation_group(candidate: &Classified) -> String {
    format!("{}/{}", candidate.class, candidate.generation)
}

/// Newest readable generation of each bounded variant, by `created_at` then
/// key — the same total order [`oldest_first`] uses.
fn newest_readable_of_bounded_variants(classified: &[Classified]) -> Vec<(String, usize)> {
    let mut newest: Vec<(String, usize)> = Vec::new();
    for (index, candidate) in classified.iter().enumerate() {
        if candidate.age_seconds < 0 || candidate.generation_bound == 0 {
            continue;
        }
        let group = generation_group(candidate);
        match newest.iter_mut().find(|(key, _)| *key == group) {
            Some((_, current)) => {
                if classified
                    .get(*current)
                    .is_some_and(|existing| oldest_first(candidate, existing).is_gt())
                {
                    *current = index;
                }
            }
            None => newest.push((group, index)),
        }
    }
    newest
}

fn apply_producer_eligibility(classified: &mut [Classified], policy: &RetentionPolicy) {
    let window = i64::try_from(policy.producer_window_seconds).unwrap_or(0);
    let newest = newest_readable_of_bounded_variants(classified);
    for (index, candidate) in classified.iter_mut().enumerate() {
        if candidate.age_seconds < 0 {
            candidate.eligible = false;
            continue;
        }
        let aged_out = candidate.age_seconds >= window;
        let superseded = candidate.generation_bound > 0
            && newest.iter().any(|(group, newest_index)| {
                *newest_index != index && *group == generation_group(candidate)
            });
        candidate.eligible = aged_out || superseded;
    }
}

fn strip_hash_segments(key: &str) -> String {
    let mut current = key.as_bytes();
    let mut end = current.len();
    while let Some(stripped) = current
        .len()
        .checked_sub(65)
        .filter(|start| current[*start] == b'-')
        .filter(|start| current[start + 1..].iter().all(u8::is_ascii_hexdigit))
    {
        end = stripped;
        current = &current[..end];
    }
    String::from_utf8_lossy(&current[..end]).into_owned()
}

/// Oldest first, then by key: a total order, so the plan is reproducible.
fn oldest_first(a: &Classified, b: &Classified) -> std::cmp::Ordering {
    a.entry
        .created_at
        .cmp(&b.entry.created_at)
        .then_with(|| a.entry.key.cmp(&b.entry.key))
}

fn selected(
    candidates: &[&Classified],
    bytes_to_free: u64,
    class: &str,
    reason: EvictionReason,
    plan: &mut Vec<Eviction>,
) {
    if bytes_to_free == 0 {
        return;
    }
    let mut freed = 0u64;
    for candidate in candidates {
        if freed >= bytes_to_free {
            break;
        }
        freed = freed.saturating_add(candidate.entry.size_in_bytes);
        plan.push(Eviction {
            id: candidate.entry.id.clone(),
            key: candidate.entry.key.clone(),
            class: class.to_owned(),
            reason,
            size_in_bytes: candidate.entry.size_in_bytes,
        });
    }
}

/// The eviction plan: which entries to delete, in the order the maintenance
/// job deletes them, with the class and reason each eviction is recorded
/// under.
///
/// The order is the policy: generations beyond the per-class bound first, then
/// class budgets, then the global budget — and protected classes are never
/// selected by the global sweep, because their space is reserved before
/// rolling state is allocated. The newest generation of a variant stays out
/// of reach inside the producer window; a superseded generation of the same
/// variant is eligible even while it is still young. Entries whose age cannot
/// be read are never selected.
pub(crate) fn plan_evictions(
    entries: &[CacheEntry],
    policy: &RetentionPolicy,
    now_epoch: i64,
) -> Vec<Eviction> {
    let classified = classify(entries, policy, now_epoch);
    let mut plan: Vec<Eviction> = Vec::new();
    let mut selected_ids: Vec<String> = Vec::new();
    evict_beyond_generation_bound(&classified, &mut plan, &mut selected_ids);
    evict_over_class_budget(&classified, &mut plan, &mut selected_ids);
    evict_over_global_budget(
        &classified,
        policy.total_bytes,
        &mut plan,
        &mut selected_ids,
    );
    plan
}

fn remember(plan: &[Eviction], before: usize, selected_ids: &mut Vec<String>) {
    selected_ids.extend(plan[before..].iter().map(|eviction| eviction.id.clone()));
}

/// Generations beyond the bound of their variant class: the oldest go first,
/// so the newest trusted snapshot of every variant survives. Ineligible
/// newest gens still occupy a slot, but only eligible overflow is selected.
fn evict_beyond_generation_bound(
    classified: &[Classified],
    plan: &mut Vec<Eviction>,
    selected_ids: &mut Vec<String>,
) {
    let mut by_generation: Vec<(String, Vec<&Classified>)> = Vec::new();
    for candidate in classified {
        if candidate.generation_bound == 0 {
            continue;
        }
        let generation = generation_group(candidate);
        match by_generation.iter_mut().find(|(key, _)| *key == generation) {
            Some((_, group)) => group.push(candidate),
            None => by_generation.push((generation, vec![candidate])),
        }
    }
    for (_, mut group) in by_generation {
        group.sort_by(|a, b| oldest_first(a, b));
        let bound = usize::try_from(group[0].generation_bound).unwrap_or(usize::MAX);
        if group.len() <= bound {
            continue;
        }
        let evicting: Vec<&Classified> = group[..group.len() - bound]
            .iter()
            .copied()
            .filter(|candidate| candidate.eligible)
            .collect();
        if evicting.is_empty() {
            continue;
        }
        let bytes: u64 = evicting.iter().map(|c| c.entry.size_in_bytes).sum();
        let class = group[0].class.clone();
        let before = plan.len();
        selected(
            &evicting,
            bytes,
            &class,
            EvictionReason::GenerationBeyondBound,
            plan,
        );
        remember(plan, before, selected_ids);
    }
}

/// A rolling or baseline class holding more than its budget gives up its
/// oldest eligible entries first. Ineligible newest gens still count toward
/// held, so a class over budget because of young live snapshots still drops
/// superseded ones. A protected class's budget is a reservation, not a cap.
fn evict_over_class_budget(
    classified: &[Classified],
    plan: &mut Vec<Eviction>,
    selected_ids: &mut Vec<String>,
) {
    let mut by_class: Vec<(String, Vec<&Classified>)> = Vec::new();
    for candidate in classified {
        if candidate.budget_bytes == 0 || candidate.tier == Tier::Protected {
            continue;
        }
        match by_class
            .iter_mut()
            .find(|(class, _)| *class == candidate.class)
        {
            Some((_, group)) => group.push(candidate),
            None => by_class.push((candidate.class.clone(), vec![candidate])),
        }
    }
    for (class, mut group) in by_class {
        group.sort_by(|a, b| oldest_first(a, b));
        let budget = group[0].budget_bytes;
        let held: u64 = group.iter().map(|c| c.entry.size_in_bytes).sum();
        let held: u64 = held.saturating_sub(
            plan.iter()
                .filter(|eviction| eviction.class == class)
                .map(|eviction| eviction.size_in_bytes)
                .sum::<u64>(),
        );
        if held <= budget {
            continue;
        }
        let kept: Vec<&Classified> = group
            .iter()
            .copied()
            .filter(|candidate| candidate.eligible && !selected_ids.contains(&candidate.entry.id))
            .collect();
        let before = plan.len();
        selected(
            &kept,
            held - budget,
            &class,
            EvictionReason::ClassBudget,
            plan,
        );
        remember(plan, before, selected_ids);
    }
}

/// Global budget, paid by unprotected classes only. Ineligible rolling and
/// baseline entries still count toward the overage; only eligible
/// unprotected entries may be deleted.
fn evict_over_global_budget(
    classified: &[Classified],
    total_bytes: u64,
    plan: &mut Vec<Eviction>,
    selected_ids: &mut Vec<String>,
) {
    let kept_total: u64 = classified
        .iter()
        .filter(|candidate| {
            !selected_ids.contains(&candidate.entry.id)
                && (candidate.eligible || candidate.tier != Tier::Protected)
        })
        .map(|candidate| candidate.entry.size_in_bytes)
        .sum();
    if kept_total <= total_bytes {
        return;
    }
    let mut candidates: Vec<&Classified> = classified
        .iter()
        .filter(|candidate| {
            candidate.eligible
                && candidate.tier != Tier::Protected
                && !selected_ids.contains(&candidate.entry.id)
        })
        .collect();
    candidates.sort_by(|a, b| oldest_first(a, b));
    let before = plan.len();
    selected(
        &candidates,
        kept_total - total_bytes,
        "unprotected",
        EvictionReason::GlobalBudget,
        plan,
    );
    remember(plan, before, selected_ids);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::MR_BOXINGTON_VERSION;

    /// `2026-09-01T00:00:00Z`, the instant the fixture ages entries from.
    const BASE_EPOCH: i64 = 1_788_220_800;
    /// The instant the planner is asked about: well past every fixture age.
    const NOW: i64 = BASE_EPOCH + 90 * 86_400;

    fn entry(id: &str, key: &str, size: u64, created_at: &str) -> CacheEntry {
        CacheEntry {
            id: id.to_owned(),
            key: key.to_owned(),
            size_in_bytes: size,
            created_at: created_at.to_owned(),
        }
    }

    #[test]
    fn retention_policy_from_config_overrides_github_budget() {
        let config = crate::config::CacheGithubSection {
            budget_bytes: Some(4_294_967_296),
            producer_window_seconds: Some(3_600),
            mbx_generation_bound: Some(3),
        };
        let policy = RetentionPolicy::from_config(&config);
        assert_eq!(policy.total_bytes, 4_294_967_296);
        assert_eq!(policy.producer_window_seconds, 3_600);
        let compiler = policy_class(&policy, "compiler-snapshots");
        assert_eq!(compiler.generation_bound, 3);
    }

    /// An RFC 3339 timestamp at `epoch`, in the exact shape the cache API
    /// reports.
    fn stamp_from_epoch(epoch: i64) -> String {
        let days = epoch.div_euclid(86_400);
        let seconds = epoch.rem_euclid(86_400);
        // Civil-from-days, the inverse of the conversion the planner parses.
        let shifted = days + 719_468;
        let era = shifted.div_euclid(146_097);
        let day_of_era = shifted - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = if month <= 2 { year + 1 } else { year };
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        )
    }

    /// An RFC 3339 timestamp `age_seconds` before the fixture base.
    fn stamp(age_seconds: i64) -> String {
        stamp_from_epoch(BASE_EPOCH - age_seconds)
    }

    fn aged(id: &str, key: &str, size: u64, age_seconds: i64) -> CacheEntry {
        entry(id, key, size, stamp(age_seconds).as_str())
    }

    /// An entry `age_seconds` before `NOW`, so it can sit inside the producer
    /// window. `aged` ages from `BASE_EPOCH`, which is 90 days behind `NOW`.
    fn recent(id: &str, key: &str, size: u64, age_seconds: i64) -> CacheEntry {
        entry(id, key, size, stamp_from_epoch(NOW - age_seconds).as_str())
    }

    fn hex64(seed: u8) -> String {
        format!("{seed:064x}")
    }

    /// Live compiler-snapshot shape: namespace, schema, compatibility digest,
    /// runtime, variant, then the two 64-hex `hashFiles` segments.
    fn release_mbx_key(variant: &str, dependency: u8, freshness: u8) -> String {
        format!(
            "velnor-release-mbx-v3-58835fa41d40-Linux-X64-{variant}-{}-{}",
            hex64(dependency),
            hex64(freshness)
        )
    }

    fn plan_ids(plan: &[Eviction]) -> Vec<&str> {
        plan.iter().map(|eviction| eviction.id.as_str()).collect()
    }

    fn snapshot_key_of(variant: &str, state: u8) -> String {
        format!(
            "velnor-mbx-v3-1a2b3c4d5e6f-Linux-X64-{variant}-{}-{:064}",
            "a".repeat(64),
            state
        )
    }

    #[expect(
        clippy::panic,
        reason = "tests need missing fixture data to name its omission"
    )]
    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[expect(
        clippy::panic,
        reason = "tests need missing fixture data to name its omission"
    )]
    fn must_some<T>(value: Option<T>, context: &str) -> T {
        match value {
            Some(value) => value,
            None => panic!("{context}"),
        }
    }

    fn policy_class<'a>(policy: &'a RetentionPolicy, id: &str) -> &'a ClassPolicy {
        must_some(
            policy.classes.iter().find(|class| class.id == id),
            "retention class exists",
        )
    }

    fn report_class<'a>(report: &'a BudgetReport, id: &str) -> &'a ClassBudgetTotal {
        must_some(
            report.classes.iter().find(|class| class.id == id),
            "budget report class exists",
        )
    }

    #[test]
    fn purpose_is_the_retention_identity_and_markers_only_match_entries() {
        const HOUR: i64 = 3_600;
        let policy = RetentionPolicy::default_policy();
        let expected = [
            ("toolchain-seeds", CachePurpose::Toolchains, Tier::Protected),
            ("runtime-binary", CachePurpose::Generic, Tier::Rolling),
            (
                "source-bundles",
                CachePurpose::CargoSources,
                Tier::Protected,
            ),
            ("unit-caches", CachePurpose::Generic, Tier::Rolling),
            ("docker-seed", CachePurpose::DockerSeed, Tier::Baseline),
            ("compiler-snapshots", CachePurpose::Outputs, Tier::Rolling),
        ];
        for (id, purpose, tier) in expected {
            let class = policy_class(&policy, id);
            assert_eq!(class.purpose, purpose, "{id}");
            assert_eq!(Tier::from(class.purpose), tier, "{id}");
        }

        let entries = vec![
            aged("toolchain", "velnor-rustup-Linux-X64-seed", 1, 30 * HOUR),
            aged("sources", "ci-Linux-rust-unit-sources", 1, 30 * HOUR),
            aged(
                "docker",
                docker_seed_key(&"d".repeat(64), 3).as_str(),
                1,
                30 * HOUR,
            ),
            aged(
                "snapshots",
                snapshot_key_of("rust-example", 4).as_str(),
                1,
                30 * HOUR,
            ),
            aged("unknown", "unrecognized-cache-entry", 1, 30 * HOUR),
        ];
        let classified = classify(&entries, &policy, NOW);
        let actual = classified
            .iter()
            .map(|candidate| (candidate.class.as_str(), candidate.purpose, candidate.tier))
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            vec![
                ("toolchain-seeds", CachePurpose::Toolchains, Tier::Protected),
                (
                    "source-bundles",
                    CachePurpose::CargoSources,
                    Tier::Protected
                ),
                ("docker-seed", CachePurpose::DockerSeed, Tier::Baseline),
                ("compiler-snapshots", CachePurpose::Outputs, Tier::Rolling),
                ("unclassified", CachePurpose::Generic, Tier::Rolling),
            ]
        );
    }

    #[test]
    fn rust_cargo_source_matching_is_anchored_and_purpose_qualified() {
        let policy = RetentionPolicy::default_policy();
        let generic_class = |key: &str| {
            let entries = [aged("entry", key, 1, 0)];
            let classified = classify(&entries, &policy, NOW);
            (
                classified[0].class.as_str().to_owned(),
                classified[0].purpose,
            )
        };

        assert!(CacheKeyMatcher::CiRustCargoSources.matches("ci-Linux-X64-rust-abc123"));
        assert!(CacheKeyMatcher::CiRustCargoSources
            .matches("ci-release-Linux-rust-example-runner-abc123"));
        let source = generic_class("ci-Linux-X64-rust-abc123");
        assert_eq!(source.0.as_str(), "source-bundles");
        assert_eq!(source.1, CachePurpose::CargoSources);
        let release_source = generic_class("ci-release-Linux-rust-example-runner-abc123");
        assert_eq!(release_source.0.as_str(), "source-bundles");
        assert_eq!(release_source.1, CachePurpose::CargoSources);

        for key in [
            "ci-Linux-docs-docs-abc123",
            "ci-Linux-bun-package-abc123",
            "ci-Linux-opentofu-opentofu-abc123",
            "ci-release-Linux-bun-example-abc123",
            "prefix-ci-Linux-rust-unit-abc123",
            "ci-Linux-rustlike-abc123",
        ] {
            assert!(
                !CacheKeyMatcher::CiRustCargoSources.matches(key),
                "generic key must not match the Rust Cargo-source matcher: {key}"
            );
        }
        for key in [
            "ci-Linux-docs-docs-abc123",
            "ci-Linux-bun-package-abc123",
            "ci-Linux-opentofu-opentofu-abc123",
            "ci-release-Linux-bun-example-abc123",
        ] {
            assert!(
                CacheKeyMatcher::CiGenericUnitCaches.matches(key),
                "unit cache key must match the generic unit matcher: {key}"
            );
            let actual = generic_class(key);
            assert_eq!(
                actual.0.as_str(),
                "unit-caches",
                "unit cache key must land in unit-caches: {key}"
            );
            assert_eq!(actual.1, CachePurpose::Generic, "unit cache key: {key}");
        }
        for key in [
            "prefix-ci-Linux-rust-unit-abc123",
            "ci-Linux-rustlike-abc123",
        ] {
            let actual = generic_class(key);
            assert_eq!(
                actual.0.as_str(),
                "unclassified",
                "unknown key must remain unclassified: {key}"
            );
        }
    }

    #[test]
    fn emitted_key_families_classify_into_retention_classes() {
        const HOUR: i64 = 3_600;
        let policy = RetentionPolicy::default_policy();
        let entries = vec![
            aged("mise", "mise-v1-linux-x64-ubuntu24", 1, 30 * HOUR),
            aged(
                "runtime",
                "velnor-workflow-v1-Linux-X64-deadbeef",
                1,
                30 * HOUR,
            ),
            aged("guest", "guest-seed-x86_64-deadbeef", 1, 30 * HOUR),
            aged(
                "policy",
                "velnor-policy-mbx-1.11.1-Linux-X64-deadbeef",
                1,
                30 * HOUR,
            ),
        ];
        let classified = classify(&entries, &policy, NOW);
        let actual = classified
            .iter()
            .map(|candidate| candidate.class.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            vec![
                "toolchain-seeds",
                "runtime-binary",
                "docker-seed",
                "compiler-snapshots"
            ]
        );
    }

    #[test]
    fn budget_report_exposes_per_class_totals_and_headroom() {
        let policy = RetentionPolicy::default_policy();
        let entries = vec![
            entry(
                "a",
                "velnor-rustup-Linux-X64-seed",
                100,
                "2026-09-01T00:00:00Z",
            ),
            entry(
                "b",
                "mise-v1-linux-x64-ubuntu24",
                200,
                "2026-09-01T00:00:00Z",
            ),
            entry(
                "c",
                "velnor-workflow-v1-Linux-X64-deadbeef",
                300,
                "2026-09-01T00:00:00Z",
            ),
        ];
        let report = budget_report(&entries, &policy);
        assert_eq!(report.total_budget_bytes, policy.total_bytes);
        assert_eq!(report.total_held_bytes, 600);
        assert_eq!(report.headroom_bytes, 8_589_933_992);
        let toolchain = report_class(&report, "toolchain-seeds");
        assert_eq!(toolchain.held_bytes, 300);
        assert_eq!(toolchain.entry_count, 2);
        let runtime = report_class(&report, "runtime-binary");
        assert_eq!(runtime.held_bytes, 300);
        assert_eq!(runtime.entry_count, 1);
    }

    #[test]
    fn docker_seed_reservation_keeps_measured_evidence_headroom() {
        // Measured live Docker-seed bytes: Actions cache `7634850138`, key
        // `velnor-docker-seed-Linux-X64-37ff55...`,
        // `size_in_bytes=527831088`. This is observed evidence, not the
        // reservation; the policy retains 1 GiB so a larger valid seed need
        // not be evicted merely to match one sample.
        const MEASURED_DOCKER_SEED_BYTES: u64 = 527_831_088;
        let policy = RetentionPolicy::default_policy();
        let docker_seed = policy_class(&policy, "docker-seed");
        assert_eq!(docker_seed.budget_bytes, GIBIBYTE);
        assert!(MEASURED_DOCKER_SEED_BYTES < docker_seed.budget_bytes);
    }

    fn facts(rustflags: &str) -> CompatibilityFacts {
        CompatibilityFacts {
            schema: SNAPSHOT_SCHEMA,
            payload: "mbx",
            mbx_version: MR_BOXINGTON_VERSION.to_owned(),
            toolchain: Some(RustToolchain {
                channel: "1.98.1".to_owned(),
                components: vec!["clippy".to_owned()],
                targets: vec!["x86_64-unknown-linux-gnu".to_owned()],
                profile: None,
            }),
            host_image: "ubuntu-24.04".to_owned(),
            provider: "velnor".to_owned(),
            platform: "linux-x64".to_owned(),
            trust: "untrusted-ok".to_owned(),
            linker: "mold".to_owned(),
            rustflags: rustflags.to_owned(),
            cargo_inputs: vec![".cargo/**".to_owned()],
            recipe: vec!["cargo nextest run".to_owned()],
        }
    }

    #[test]
    fn compatibility_digest_changes_when_a_fact_changes() {
        let base = facts("-C link-arg=-fuse-ld=mold").digest();
        assert_eq!(base.len(), COMPATIBILITY_DIGEST_CHARS);
        assert_eq!(
            facts("-C link-arg=-fuse-ld=mold").digest(),
            base,
            "digest must be stable"
        );
        assert_ne!(
            facts("-C link-arg=-fuse-ld=mold -C panic=abort").digest(),
            base,
            "a flag change is a compatibility change"
        );
        let reordered = CompatibilityFacts {
            cargo_inputs: vec![".cargo/**".to_owned(), "Cargo.lock".to_owned()],
            recipe: vec!["cargo nextest run".to_owned(), "cargo check".to_owned()],
            ..facts("-C link-arg=-fuse-ld=mold")
        };
        let swapped = CompatibilityFacts {
            cargo_inputs: vec!["Cargo.lock".to_owned(), ".cargo/**".to_owned()],
            recipe: vec!["cargo check".to_owned(), "cargo nextest run".to_owned()],
            ..facts("-C link-arg=-fuse-ld=mold")
        };
        assert_eq!(
            reordered.digest(),
            swapped.digest(),
            "declaration order is not a compatibility fact"
        );
    }

    fn segments() -> KeySegments {
        KeySegments {
            provider: "velnor".to_owned(),
            platform: "linux-x64".to_owned(),
            trust: "untrusted-ok".to_owned(),
        }
    }

    #[test]
    fn snapshot_keys_carry_compatibility_and_freshness_as_distinct_segments() {
        let prefix = snapshot_class_prefix("velnor-mbx", "1a2b3c4d5e6f");
        assert_eq!(prefix, "velnor-mbx-v3-1a2b3c4d5e6f");
        let dependency = "${{ hashFiles('Cargo.lock') }}";
        let freshness = "${{ hashFiles('crates/example/**/*.rs') }}";
        let key = snapshot_key(&prefix, "rust-example", dependency, freshness, &segments());
        assert!(is_snapshot_key(&key), "{key}");
        assert_eq!(
            key,
            "velnor-mbx-v3-1a2b3c4d5e6f-${{ runner.os }}-${{ runner.arch }}-velnor-linux-x64-untrusted-ok-rust-example-\
             ${{ hashFiles('Cargo.lock') }}-${{ hashFiles('crates/example/**/*.rs') }}"
        );
        assert_eq!(
            snapshot_restore_keys(&prefix, "rust-example", dependency, &segments()),
            "velnor-mbx-v3-1a2b3c4d5e6f-${{ runner.os }}-${{ runner.arch }}-velnor-linux-x64-untrusted-ok-rust-example-\
             ${{ hashFiles('Cargo.lock') }}-\n            \
             velnor-mbx-v3-1a2b3c4d5e6f-${{ runner.os }}-${{ runner.arch }}-velnor-linux-x64-untrusted-ok-rust-example-"
        );
    }

    /// An exact old generation must not shadow newer compatible fallbacks:
    /// restore keys are prefixes only, so the newest saved generation of the
    /// class wins.
    #[test]
    fn restore_keys_never_name_a_complete_generation() {
        let prefix = snapshot_class_prefix("velnor-release-mbx", "abcdef123456");
        let dependency = "${{ hashFiles('Cargo.lock', 'rust-toolchain.toml') }}";
        let key = snapshot_key(
            &prefix,
            "metadata",
            dependency,
            "${{ hashFiles('crates/example/**/*.rs') }}",
            &segments(),
        );
        let restore = snapshot_restore_keys(&prefix, "metadata", dependency, &segments());
        for line in restore.lines() {
            assert!(
                line.ends_with('-'),
                "a restore key must stay a prefix: {line}"
            );
            assert_ne!(
                line.trim(),
                key,
                "an exact old generation on the restore list could shadow a newer fallback: {line}"
            );
            assert!(
                line.matches("hashFiles").count() <= 1,
                "a restore key carries at most the dependency inputs and never the freshness \
                 state, so the newest compatible generation wins: {line}"
            );
        }
    }

    #[test]
    fn freshness_hashes_the_inputs_it_names() {
        assert_eq!(
            freshness_expression(&["crates/example/**/*.rs".to_owned()]),
            "${{ hashFiles('crates/example/**/*.rs') }}"
        );
        assert!(
            !is_snapshot_key(
                &snapshot_key_of("rust-example", 0)
                    .replace("-${{ hashFiles('crates/example/**/*.rs') }}", "-")
            ),
            "a compatibility-only key is not a snapshot key: it is the frozen-snapshot defect"
        );
    }

    /// The save gate rests on the key grammar: an exact hit means the run
    /// holds a state already saved under this key, so re-saving is refused
    /// and nothing is written, while a run whose source state advanced mints
    /// a new key and is never refused by an earlier immutable entry. A
    /// compatibility change mints a new class entirely, never a new
    /// generation inside the old one.
    #[test]
    fn an_exact_hit_repeats_one_key_and_new_state_mints_another() {
        let dependency = "${{ hashFiles('Cargo.lock') }}";
        let class_prefix = snapshot_class_prefix("velnor-mbx", "1a2b3c4d5e6f");
        let saved = snapshot_key(
            &class_prefix,
            "rust-example",
            dependency,
            &freshness_expression(&["crates/example/**/*.rs".to_owned()]),
            &segments(),
        );
        // Same compatibility, same state: the exact-hit run writes nothing.
        assert_eq!(
            snapshot_key(
                &class_prefix,
                "rust-example",
                dependency,
                &freshness_expression(&["crates/example/**/*.rs".to_owned()]),
                &segments(),
            ),
            saved
        );
        // Same compatibility, advanced state: a new generation the run saves.
        let advanced = snapshot_key(
            &class_prefix,
            "rust-example",
            dependency,
            &freshness_expression(&["crates/example/src/lib.rs".to_owned()]),
            &segments(),
        );
        assert_ne!(advanced, saved);
        assert_ne!(
            snapshot_class_prefix("velnor-mbx", "ffffffffffff"),
            class_prefix,
            "a compatibility change is a new class, not a new generation"
        );
    }

    /// Per-class budgets are reserved per class: a rolling class is trimmed
    /// to its own budget, a protected class is never trimmed by the global
    /// sweep even when the account is over it, and the global sweep pays for
    /// the reservations first.
    #[test]
    fn class_budgets_bound_each_class_without_touching_protected_space() {
        const HOUR: i64 = 3_600;
        let policy = RetentionPolicy::default_policy();
        let class_budget = |id: &str| -> u64 {
            must_some(
                policy
                    .classes
                    .iter()
                    .find(|class| class.id == id)
                    .map(|class| class.budget_bytes),
                "retention class exists",
            )
        };
        let seed_budget = class_budget("toolchain-seeds");
        // Protected: one byte over its reservation, all older than the
        // producer window.
        let seeds = vec![aged(
            "seed-1",
            "velnor-rustup-Linux-X64-seed",
            seed_budget + 1,
            30 * HOUR,
        )];
        // Rolling: three generations of one variant class, two gigabytes
        // each - past the generation bound of two and past the class budget.
        let snapshots = vec![
            aged(
                "snapshot-1",
                snapshot_key_of("rust-example", 1).as_str(),
                2_000_000_000,
                30 * HOUR,
            ),
            aged(
                "snapshot-2",
                snapshot_key_of("rust-example", 2).as_str(),
                2_000_000_000,
                29 * HOUR,
            ),
            aged(
                "snapshot-3",
                snapshot_key_of("rust-example", 3).as_str(),
                2_000_000_000,
                28 * HOUR,
            ),
        ];
        let entries = [seeds, snapshots].concat();
        let plan = plan_evictions(&entries, &policy, NOW);
        assert!(
            plan.iter().all(|eviction| eviction.id != "seed-1"),
            "a protected class over its reservation is never trimmed: {plan:?}"
        );
        // The bound fires first, then the class budget; the protected
        // reservation is never spent on rolling state.
        assert_eq!(
            plan.iter()
                .map(|eviction| eviction.id.as_str())
                .collect::<Vec<_>>(),
            vec!["snapshot-1", "snapshot-2"],
            "the oldest generations go first, down to the class budget"
        );
        let rolling_budget = class_budget("compiler-snapshots");
        let kept: u64 = entries
            .iter()
            .filter(|entry| entry.id != "seed-1")
            .filter(|entry| !plan.iter().any(|eviction| eviction.id == entry.id))
            .map(|entry| entry.size_in_bytes)
            .sum();
        assert!(
            kept <= rolling_budget,
            "the rolling class is trimmed into its own budget: kept {kept} > {rolling_budget}"
        );
        assert_eq!(
            plan.iter()
                .map(|eviction| eviction.reason)
                .collect::<Vec<_>>(),
            vec![
                EvictionReason::GenerationBeyondBound,
                EvictionReason::ClassBudget
            ],
            "the bound is enforced before the class budget"
        );
    }

    #[test]
    fn unreadable_ages_are_producer_active() {
        must_some(epoch_of("2026-09-01T00:00:00Z"), "whole-second Z");
        must_some(epoch_of("2026-09-01T00:00:00+02:00"), "numeric offset");
        assert!(epoch_of("not-a-timestamp").is_none());
        assert!(epoch_of("2026-09-13T17:30:16.Z").is_none());
        assert!(epoch_of("2026-09-13T17:30:16.329079Zx").is_none());
    }

    /// GitHub's cache API emits fractional `created_at` (`…16.329079Z`). A
    /// parser that stops at index 19 treats `.` as unreadable, ages the entry
    /// at `-1`, and the maintenance plan stays empty.
    #[test]
    fn github_fractional_created_at_ages_mbx_generations() {
        const GITHUB: &str = "2026-09-13T17:30:16.329079Z";
        let created = must_some(epoch_of(GITHUB), "GitHub Actions cache created_at");
        assert_eq!(
            created,
            must_some(epoch_of("2026-09-13T17:30:16Z"), "whole-second form")
        );
        assert_eq!(
            created,
            must_some(
                epoch_of("2026-09-13T17:30:16.329079+00:00"),
                "fractional offset form"
            )
        );

        let policy = RetentionPolicy::default_policy();
        let window = must(
            i64::try_from(policy.producer_window_seconds),
            "producer window fits i64",
        );
        // Three generations of one `-mbx-v3-` variant, GitHub-shaped stamps,
        // all just past the 2h producer window and within two hours of each
        // other. Bound is 2: the oldest goes. Maintenance 34773651873 left
        // `plan.json` `[]` because none of these stamps parsed.
        let now = created + window + 90 * 60;
        let key = |state: u8| {
            format!(
                "velnor-release-mbx-v3-1a2b3c4d5e6f-Linux-X64-guest-x86_64-{}-{:064}",
                "a".repeat(64),
                state
            )
        };
        let entries = [
            entry("old", key(1).as_str(), 100_000_000, GITHUB),
            entry(
                "mid",
                key(2).as_str(),
                100_000_000,
                "2026-09-13T18:10:00.1Z",
            ),
            entry(
                "new",
                key(3).as_str(),
                100_000_000,
                "2026-09-13T18:45:16.999Z",
            ),
        ];
        let plan = plan_evictions(&entries, &policy, now);
        let eviction = must_some(plan.first(), "oldest generation is evicted");
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_eq!(eviction.id, "old");
        assert_eq!(eviction.reason, EvictionReason::GenerationBeyondBound);
        assert!(
            plan.iter()
                .all(|eviction| eviction.id != "mid" && eviction.id != "new"),
            "the bound keeps the two newer generations: {plan:?}"
        );
    }

    fn docker_seed_key(hash: &str, state: u64) -> String {
        format!("velnor-docker-seed-v3-1a2b3c4d5e6f-Linux-X64-{hash}-{state:064}")
    }

    /// The §9.1 test-9 fixture: more than 100 entries across every class, the
    /// rolling classes over their budget and their generation bound, and the
    /// account over its total.
    fn over_one_hundred_entries() -> Vec<CacheEntry> {
        const HOUR: i64 = 3_600;
        let mut counter = 0usize;
        let mut entries: Vec<CacheEntry> = Vec::new();
        let mut push = |key: &str, size: u64, age_seconds: i64, entries: &mut Vec<CacheEntry>| {
            counter += 1;
            entries.push(aged(&format!("id-{counter}"), key, size, age_seconds));
        };
        // Protected classes: toolchain seeds and Cargo source bundles, every
        // one older than the producer window.
        for index in 0..30 {
            push(
                &format!("velnor-rustup-Linux-X64-seed-{index}"),
                40_000_000,
                30 * HOUR,
                &mut entries,
            );
        }
        for index in 0..20 {
            push(
                &format!("ci-Linux-rust-unit-{index}-abc123"),
                30_000_000,
                30 * HOUR,
                &mut entries,
            );
        }
        // Rolling snapshots: twenty generations of each of three variant
        // classes, 250 MB each — past the bound and far past the class budget.
        for variant in ["rust-example", "rust-other", "rust-third"] {
            for generation in 0..20u8 {
                push(
                    snapshot_key_of(variant, generation).as_str(),
                    250_000_000,
                    30 * HOUR - i64::from(generation) * HOUR,
                    &mut entries,
                );
            }
        }
        // A freshly published generation inside the producer window, and one
        // whose timestamp cannot be read: both out of reach.
        push(
            snapshot_key_of("rust-example", 200).as_str(),
            250_000_000,
            30 * 60,
            &mut entries,
        );
        entries.push(entry(
            "id-unreadable",
            snapshot_key_of("rust-example", 201).as_str(),
            250_000_000,
            "not-a-timestamp",
        ));
        // The Docker seed baseline: two generations of one compatibility class,
        // one beyond its bound. The keys carry the two `hashFiles` segments the
        // grammar renders, which is what groups them into one variant class.
        push(
            docker_seed_key(&"b".repeat(64), 1).as_str(),
            400_000_000,
            40 * HOUR,
            &mut entries,
        );
        push(
            docker_seed_key(&"c".repeat(64), 2).as_str(),
            400_000_000,
            10 * HOUR,
            &mut entries,
        );
        assert!(
            entries.len() > 100,
            "the fixture must exceed one page: {}",
            entries.len()
        );
        entries
    }

    fn id_of(entries: &[CacheEntry], predicate: impl Fn(&CacheEntry) -> bool) -> String {
        entries
            .iter()
            .find(|entry| predicate(entry))
            .map(|entry| entry.id.clone())
            .unwrap_or_default()
    }

    /// The plan must be one correct set of evictions that preserves the
    /// protected classes and the newest generation of every bounded variant
    /// class.
    #[test]
    fn planning_over_one_hundred_entries_is_one_correct_total() {
        let policy = RetentionPolicy::default_policy();
        let entries = over_one_hundred_entries();
        let plan = plan_evictions(&entries, &policy, NOW);
        assert!(
            !plan.is_empty(),
            "an account over budget and past its bounds must produce evictions"
        );
        let mut ids: Vec<&str> = plan.iter().map(|eviction| eviction.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(
            ids.windows(2).filter(|pair| pair[0] == pair[1]).count(),
            0,
            "every entry is evicted once, whatever reasons apply to it"
        );
        // Protected classes survive: their space is reserved before rolling
        // state is allocated.
        assert!(plan
            .iter()
            .all(|eviction| eviction.class != "toolchain-seeds"
                && eviction.class != "source-bundles"));
        // The newest generation of every bounded variant class survives.
        for variant in ["rust-other", "rust-third"] {
            let newest = entries
                .iter()
                .find(|entry| entry.key == snapshot_key_of(variant, 19))
                .map(|entry| entry.id.as_str())
                .unwrap_or_default();
            assert!(
                !ids.contains(&newest),
                "the newest generation of {variant} must survive"
            );
        }
        // The Docker seed keeps exactly one live generation: the older goes.
        let seed_old = id_of(&entries, |entry| {
            entry.key == docker_seed_key(&"b".repeat(64), 1)
        });
        let seed_new = id_of(&entries, |entry| {
            entry.key == docker_seed_key(&"c".repeat(64), 2)
        });
        assert!(
            ids.contains(&seed_old.as_str()),
            "the older Docker seed generation must go"
        );
        assert!(
            !ids.contains(&seed_new.as_str()),
            "the live Docker seed generation must survive"
        );
        // Producer-active and unreadable entries are never selected.
        let young = id_of(&entries, |entry| {
            entry.key == snapshot_key_of("rust-example", 200)
        });
        let unreadable = id_of(&entries, |entry| entry.id == "id-unreadable");
        assert!(
            !ids.contains(&young.as_str()),
            "a producer-active entry must survive"
        );
        assert!(
            !ids.contains(&unreadable.as_str()),
            "an unreadable age must survive"
        );
        // Evictions run bound first, then class budget, then global budget.
        let rank = |reason| match reason {
            EvictionReason::GenerationBeyondBound => 0,
            EvictionReason::ClassBudget => 1,
            EvictionReason::GlobalBudget => 2,
        };
        let ranks: Vec<u8> = plan.iter().map(|eviction| rank(eviction.reason)).collect();
        let mut ordered = ranks.clone();
        ordered.sort_unstable();
        assert_eq!(
            ranks, ordered,
            "eviction order is bound, class budget, global budget"
        );
    }

    /// A class inside its budget and its bound evicts nothing.
    #[test]
    fn a_class_inside_its_policy_evicts_nothing() {
        const HOUR: i64 = 3_600;
        let policy = RetentionPolicy::default_policy();
        let entries = vec![
            aged(
                "a",
                snapshot_key_of("rust-example", 1).as_str(),
                200_000_000,
                48 * HOUR,
            ),
            aged(
                "b",
                snapshot_key_of("rust-example", 2).as_str(),
                200_000_000,
                24 * HOUR,
            ),
            aged("c", "velnor-rustup-Linux-X64-abc", 100_000_000, 48 * HOUR),
        ];
        assert!(plan_evictions(&entries, &policy, NOW).is_empty());
    }

    /// Two generations is the bound: the previous state stays restorable while
    /// the newest one is the one the next run hits.
    #[test]
    fn the_bound_keeps_the_previous_generation_and_drops_the_rest() {
        const HOUR: i64 = 3_600;
        let policy = RetentionPolicy::default_policy();
        let entries: Vec<CacheEntry> = (0..4u8)
            .map(|generation| {
                aged(
                    &format!("g{generation}"),
                    snapshot_key_of("rust-example", generation).as_str(),
                    100_000_000,
                    96 * HOUR - i64::from(generation) * 24 * HOUR,
                )
            })
            .collect();
        let plan = plan_evictions(&entries, &policy, NOW);
        let evicted: Vec<&str> = plan.iter().map(|eviction| eviction.key.as_str()).collect();
        assert_eq!(evicted.len(), 2, "{evicted:?}");
        assert!(
            evicted[0].ends_with(&format!("-{:064}", 0)),
            "oldest first: {evicted:?}"
        );
        assert!(evicted
            .iter()
            .all(|key| !key.ends_with(&format!("-{:064}", 2))
                && !key.ends_with(&format!("-{:064}", 3))));
        assert!(plan
            .iter()
            .all(|eviction| eviction.reason == EvictionReason::GenerationBeyondBound));
    }

    #[test]
    fn hash_segments_are_stripped_to_the_variant_class() {
        assert_eq!(
            strip_hash_segments(&snapshot_key_of("rust-example", 7)),
            "velnor-mbx-v3-1a2b3c4d5e6f-Linux-X64-rust-example",
            "both hashFiles segments are stripped, leaving the variant class"
        );
        assert_eq!(
            strip_hash_segments("velnor-rustup-Linux-X64-abc"),
            "velnor-rustup-Linux-X64-abc",
            "a key without hash segments stays whole"
        );
        assert_eq!(
            strip_hash_segments(&release_mbx_key("deb-x86_64-unknown-linux-gnu", 0x3d, 0xa8)),
            "velnor-release-mbx-v3-58835fa41d40-Linux-X64-deb-x86_64-unknown-linux-gnu",
            "live-shaped compiler-snapshot keys group by variant, not freshness"
        );
    }

    /// Two rolling generations of one variant, both inside the two-hour
    /// producer window: the older one is not producer-active once the newer
    /// save exists. Bound 2 would keep both; class-budget overage (held
    /// includes the ineligible newest) selects the superseded gen. A unique
    /// young generation and an unreadable timestamp stay out of the plan.
    #[test]
    fn superseded_in_window_generations_are_evicted_while_the_newest_is_not() {
        const MINUTE: i64 = 60;
        let policy = RetentionPolicy::default_policy();
        let compiler = policy_class(&policy, "compiler-snapshots");
        assert_eq!(compiler.generation_bound, 2);
        let older_key = release_mbx_key("deb-x86_64-unknown-linux-gnu", 0x3d, 0xa8);
        let newer_key = release_mbx_key("deb-x86_64-unknown-linux-gnu", 0x3d, 0x01);
        let unique_key = release_mbx_key("guest-x86_64-unknown-linux-gnu", 0x3d, 0x11);
        assert_eq!(
            strip_hash_segments(&older_key),
            strip_hash_segments(&newer_key)
        );
        assert_ne!(
            strip_hash_segments(&older_key),
            strip_hash_segments(&unique_key)
        );
        let older_size = 2_144_000_000;
        let newer_size = 2_292_000_000;
        assert!(older_size + newer_size > compiler.budget_bytes);
        assert!(newer_size <= compiler.budget_bytes);
        let entries = vec![
            recent("older", older_key.as_str(), older_size, 22 * MINUTE),
            recent("newer", newer_key.as_str(), newer_size, 3 * MINUTE),
            recent(
                "unique-young",
                unique_key.as_str(),
                100_000_000,
                30 * MINUTE,
            ),
            entry(
                "unreadable",
                snapshot_key_of("rust-example", 9).as_str(),
                100_000_000,
                "not-a-timestamp",
            ),
        ];
        let plan = plan_evictions(&entries, &policy, NOW);
        assert_eq!(plan_ids(&plan), vec!["older"]);
        let older = must_some(
            plan.iter().find(|eviction| eviction.id == "older"),
            "superseded in-window generation is planned",
        );
        assert_eq!(older.reason, EvictionReason::ClassBudget);
        assert_eq!(older.class, "compiler-snapshots");
        assert!(
            plan.iter().all(|eviction| eviction.id != "newer"),
            "the newest in-window generation must survive: {plan:?}"
        );
        assert!(
            plan.iter().all(|eviction| eviction.id != "unique-young"),
            "a unique young generation with no older sibling must survive: {plan:?}"
        );
        assert!(
            plan.iter().all(|eviction| eviction.id != "unreadable"),
            "an unreadable age must survive: {plan:?}"
        );
    }

    /// Protected toolchain seeds are never selected, inside or outside the
    /// producer window, even when they exceed the class reservation.
    #[test]
    fn protected_seeds_are_never_planned_inside_or_outside_the_producer_window() {
        const MINUTE: i64 = 60;
        const HOUR: i64 = 3_600;
        let policy = RetentionPolicy::default_policy();
        let rustup_budget = policy_class(&policy, "toolchain-seeds").budget_bytes;
        let cargo_budget = policy_class(&policy, "source-bundles").budget_bytes;
        let entries = vec![
            recent(
                "rustup-young",
                "velnor-rustup-Linux-X64-seed",
                rustup_budget + 1,
                10 * MINUTE,
            ),
            aged(
                "rustup-old",
                "velnor-rustup-Linux-X64-old",
                rustup_budget + 1,
                48 * HOUR,
            ),
            recent(
                "cargo-young",
                "velnor-cargo-Linux-X64-sources",
                cargo_budget + 1,
                15 * MINUTE,
            ),
            aged(
                "cargo-old",
                "ci-Linux-rust-unit-sources",
                cargo_budget + 1,
                48 * HOUR,
            ),
            recent(
                "older",
                release_mbx_key("deb-x86_64-unknown-linux-gnu", 0x3d, 0xa8).as_str(),
                2_144_000_000,
                22 * MINUTE,
            ),
            recent(
                "newer",
                release_mbx_key("deb-x86_64-unknown-linux-gnu", 0x3d, 0x01).as_str(),
                2_292_000_000,
                3 * MINUTE,
            ),
        ];
        let plan = plan_evictions(&entries, &policy, NOW);
        assert!(
            plan.iter().all(|eviction| {
                eviction.id != "rustup-young"
                    && eviction.id != "rustup-old"
                    && eviction.id != "cargo-young"
                    && eviction.id != "cargo-old"
            }),
            "protected rustup/cargo must never be planned: {plan:?}"
        );
        assert_eq!(plan_ids(&plan), vec!["older"]);
    }
}
