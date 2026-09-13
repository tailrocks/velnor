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
            ("recipe", sorted_strings(&self.recipe)),
            ("rustflags", vec![self.rustflags.clone()]),
            ("schema", vec![self.schema.to_owned()]),
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
/// dependency inputs GitHub hashes at restore time, and the freshness segment
/// that lets a state-advancing run save.
pub(crate) fn snapshot_key(
    class_prefix: &str,
    variant: &str,
    dependency_inputs: &str,
    freshness: &str,
) -> String {
    format!("{class_prefix}-{RUNTIME_SEGMENTS}-{variant}-{dependency_inputs}-{freshness}")
}

/// The restore prefixes of a snapshot key, newest-compatible-first. Only
/// prefixes are listed — never a complete earlier key — because the cache
/// service returns the most recently created entry matching any of them, and
/// an exact old generation on the list could shadow a newer compatible
/// fallback.
pub(crate) fn snapshot_restore_keys(
    class_prefix: &str,
    variant: &str,
    dependency_inputs: &str,
) -> String {
    format!(
        "{class_prefix}-{RUNTIME_SEGMENTS}-{variant}-{dependency_inputs}-\n            \
         {class_prefix}-{RUNTIME_SEGMENTS}-{variant}-"
    )
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
    /// `ci-<runner-os>-rust-...`, never the broad `ci-` prefix.
    CiRustCargoSources,
}

impl CacheKeyMatcher {
    fn matches(self, key: &str) -> bool {
        match self {
            Self::Contains(fragment) => key.contains(fragment),
            Self::CiRustCargoSources => {
                let Some(rest) = key.strip_prefix("ci-") else {
                    return false;
                };
                let Some((runner_os, unit)) = rest.split_once('-') else {
                    return false;
                };
                !runner_os.is_empty() && unit.starts_with("rust-")
            }
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
    /// How long a freshly created entry is out of reach of eviction: a
    /// producer may have published it seconds ago or be about to publish its
    /// successor, and creation — not access — is the only producer signal the
    /// cache API offers.
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
                    ],
                    budget_bytes: 2 * GIBIBYTE,
                    generation_bound: 0,
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
                    id: "docker-seed",
                    purpose: CachePurpose::DockerSeed,
                    markers: &[CacheKeyMatcher::Contains("velnor-docker-seed-")],
                    budget_bytes: GIBIBYTE,
                    generation_bound: 1,
                },
                ClassPolicy {
                    id: "compiler-snapshots",
                    purpose: CachePurpose::Outputs,
                    markers: &[CacheKeyMatcher::Contains("-mbx-v3-")],
                    budget_bytes: 7 * GIBIBYTE / 2,
                    generation_bound: 2,
                },
            ],
        }
    }
}

const GIBIBYTE: u64 = 1_073_741_824;

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
    entries
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
                eligible: age_seconds >= 0
                    && age_seconds >= i64::try_from(policy.producer_window_seconds).unwrap_or(0),
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
        .collect()
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
/// rolling state is allocated. Entries inside the producer window, and entries
/// whose age cannot be read, are never selected.
pub(crate) fn plan_evictions(
    entries: &[CacheEntry],
    policy: &RetentionPolicy,
    now_epoch: i64,
) -> Vec<Eviction> {
    let classified = classify(entries, policy, now_epoch);
    let mut plan: Vec<Eviction> = Vec::new();
    let mut selected_ids: Vec<String> = Vec::new();

    // 1. Generations beyond the bound of their variant class: the oldest go
    //    first, so the newest trusted snapshot of every variant survives.
    let mut by_generation: Vec<(String, Vec<&Classified>)> = Vec::new();
    for candidate in &classified {
        if !candidate.eligible || candidate.generation_bound == 0 {
            continue;
        }
        let generation = format!("{}/{}", candidate.class, candidate.generation);
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
        let evicting = &group[..group.len() - bound];
        let bytes: u64 = evicting.iter().map(|c| c.entry.size_in_bytes).sum();
        let class = group[0].class.clone();
        selected(
            evicting,
            bytes,
            &class,
            EvictionReason::GenerationBeyondBound,
            &mut plan,
        );
        selected_ids.extend(evicting.iter().map(|c| c.entry.id.clone()));
    }

    // 2. Class budgets: a rolling or baseline class holding more than its
    //    budget gives up its oldest entries first. A protected class's budget
    //    is a reservation, not a cap: the space is set aside for the seeds
    //    before rolling state is allocated, so retention never trims the
    //    seeds to fit it.
    let mut by_class: Vec<(String, Vec<&Classified>)> = Vec::new();
    for candidate in &classified {
        if !candidate.eligible || candidate.budget_bytes == 0 || candidate.tier == Tier::Protected {
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
            .filter(|candidate| !selected_ids.contains(&candidate.entry.id))
            .collect();
        let before = plan.len();
        selected(
            &kept,
            held - budget,
            &class,
            EvictionReason::ClassBudget,
            &mut plan,
        );
        selected_ids.extend(plan[before..].iter().map(|eviction| eviction.id.clone()));
    }

    // 3. The global budget, paid by unprotected classes only. Protected space
    //    is reserved: the sweep cannot spend it to make room for rolling
    //    snapshots. The overage is computed against the entries retention may
    //    actually touch — entries inside the producer window stay out of reach
    //    even when they push the account over, and the enforcement step
    //    reports what remains.
    let kept_total: u64 = classified
        .iter()
        .filter(|candidate| candidate.eligible && !selected_ids.contains(&candidate.entry.id))
        .map(|candidate| candidate.entry.size_in_bytes)
        .sum();
    if kept_total > policy.total_bytes {
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
            kept_total - policy.total_bytes,
            "unprotected",
            EvictionReason::GlobalBudget,
            &mut plan,
        );
        selected_ids.extend(plan[before..].iter().map(|eviction| eviction.id.clone()));
    }
    plan
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

    /// An RFC 3339 timestamp `age_seconds` before the fixture base, in the
    /// exact shape the cache API reports.
    fn stamp(age_seconds: i64) -> String {
        let epoch = BASE_EPOCH - age_seconds;
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

    fn aged(id: &str, key: &str, size: u64, age_seconds: i64) -> CacheEntry {
        entry(id, key, size, stamp(age_seconds).as_str())
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

    #[test]
    fn purpose_is_the_retention_identity_and_markers_only_match_entries() {
        const HOUR: i64 = 3_600;
        let policy = RetentionPolicy::default_policy();
        let expected = [
            ("toolchain-seeds", CachePurpose::Toolchains, Tier::Protected),
            (
                "source-bundles",
                CachePurpose::CargoSources,
                Tier::Protected,
            ),
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

        assert!(CacheKeyMatcher::CiRustCargoSources.matches("ci-Linux-rust-unit-abc123"));
        let source = generic_class("ci-Linux-rust-unit-abc123");
        assert_eq!(source.0.as_str(), "source-bundles");
        assert_eq!(source.1, CachePurpose::CargoSources);

        for key in [
            "ci-Linux-docs-docs-abc123",
            "ci-Linux-bun-package-abc123",
            "ci-Linux-opentofu-opentofu-abc123",
            "prefix-ci-Linux-rust-unit-abc123",
            "ci-Linux-rustlike-abc123",
        ] {
            assert!(
                !CacheKeyMatcher::CiRustCargoSources.matches(key),
                "generic key must not match the Rust Cargo-source matcher: {key}"
            );
            let actual = generic_class(key);
            assert_eq!(
                actual.0.as_str(),
                "unclassified",
                "generic key must remain generic: {key}"
            );
            assert_eq!(
                actual.1,
                CachePurpose::Generic,
                "generic key must remain generic: {key}"
            );
        }
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

    #[test]
    fn snapshot_keys_carry_compatibility_and_freshness_as_distinct_segments() {
        let prefix = snapshot_class_prefix("velnor-mbx", "1a2b3c4d5e6f");
        assert_eq!(prefix, "velnor-mbx-v3-1a2b3c4d5e6f");
        let dependency = "${{ hashFiles('Cargo.lock') }}";
        let freshness = "${{ hashFiles('crates/example/**/*.rs') }}";
        let key = snapshot_key(&prefix, "rust-example", dependency, freshness);
        assert!(is_snapshot_key(&key), "{key}");
        assert_eq!(
            key,
            "velnor-mbx-v3-1a2b3c4d5e6f-${{ runner.os }}-${{ runner.arch }}-rust-example-\
             ${{ hashFiles('Cargo.lock') }}-${{ hashFiles('crates/example/**/*.rs') }}"
        );
        assert_eq!(
            snapshot_restore_keys(&prefix, "rust-example", dependency),
            "velnor-mbx-v3-1a2b3c4d5e6f-${{ runner.os }}-${{ runner.arch }}-rust-example-\
             ${{ hashFiles('Cargo.lock') }}-\n            \
             velnor-mbx-v3-1a2b3c4d5e6f-${{ runner.os }}-${{ runner.arch }}-rust-example-"
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
        );
        let restore = snapshot_restore_keys(&prefix, "metadata", dependency);
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
        );
        // Same compatibility, same state: the exact-hit run writes nothing.
        assert_eq!(
            snapshot_key(
                &class_prefix,
                "rust-example",
                dependency,
                &freshness_expression(&["crates/example/**/*.rs".to_owned()]),
            ),
            saved
        );
        // Same compatibility, advanced state: a new generation the run saves.
        let advanced = snapshot_key(
            &class_prefix,
            "rust-example",
            dependency,
            &freshness_expression(&["crates/example/src/lib.rs".to_owned()]),
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
    }
}
