//! Fail-closed evidence for declared Cargo output files.

use crate::Freshness;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path, time::UNIX_EPOCH};

/// The observed change between two snapshots of one declared output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputChange {
    /// The output bytes and modification time are unchanged.
    Unchanged,
    /// The bytes are unchanged but the modification time changed.
    MtimeOnly,
    /// The output bytes changed.
    BytesChanged,
    /// The output was absent before and present after the observation.
    Created,
    /// The snapshots cannot establish a safe conclusion.
    Unknown,
}

/// A path-free snapshot of one declared output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum OutputSnapshot {
    /// The declared output was not present.
    Missing,
    /// The output was read successfully.
    Present {
        /// BLAKE3 digest of the output bytes.
        fingerprint: String,
        /// Modification time in nanoseconds since the Unix epoch.
        mtime_ns: u128,
    },
    /// The output could not be observed reliably.
    Unknown,
}

impl OutputSnapshot {
    /// Construct a missing-output snapshot.
    pub fn missing() -> Self {
        Self::Missing
    }

    /// Construct an unreadable or otherwise indeterminate snapshot.
    pub fn unknown() -> Self {
        Self::Unknown
    }

    /// Construct a present snapshot from caller-captured bytes and mtime.
    pub fn present(bytes: &[u8], mtime_ns: u128) -> Self {
        Self::Present {
            fingerprint: format!("blake3:{}", blake3::hash(bytes).to_hex()),
            mtime_ns,
        }
    }

    /// Read one output without launching a process or consulting human logs.
    ///
    /// Missing files are represented as [`OutputSnapshot::Missing`]. Any
    /// metadata, timestamp, or read failure is represented as
    /// [`OutputSnapshot::Unknown`] so callers cannot infer execution from
    /// incomplete evidence.
    pub fn from_path(path: &Path) -> Self {
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::Missing,
            Err(_) => return Self::Unknown,
        };
        if !metadata.is_file() {
            return Self::Unknown;
        }

        let mtime_ns = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos());
        let Some(mtime_ns) = mtime_ns else {
            return Self::Unknown;
        };
        let Ok(bytes) = fs::read(path) else {
            return Self::Unknown;
        };

        Self::present(&bytes, mtime_ns)
    }
}

/// Evidence for one output named by structured Cargo JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredOutputEvidence {
    /// Stable path-free output identity.
    pub output_id: String,
    /// Snapshot captured before the Cargo observation.
    pub before: OutputSnapshot,
    /// Snapshot captured after the Cargo observation.
    pub after: OutputSnapshot,
    /// Fail-closed comparison of the two snapshots.
    pub change: OutputChange,
}

impl DeclaredOutputEvidence {
    fn new(output_id: String, before: OutputSnapshot, after: OutputSnapshot) -> Self {
        let change = compare_snapshots(&before, &after);
        Self {
            output_id,
            before,
            after,
            change,
        }
    }

    fn unknown(output_id: String) -> Self {
        Self::new(output_id, OutputSnapshot::Unknown, OutputSnapshot::Unknown)
    }
}

/// Caller-supplied before/after snapshots keyed by declared output identity.
///
/// The manifest contains only path-free keys. Inserting contradictory evidence
/// for one path marks that path ambiguous instead of choosing one observation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputEvidenceManifest {
    entries: BTreeMap<String, ManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ManifestEntry {
    Snapshots {
        before: OutputSnapshot,
        after: OutputSnapshot,
    },
    Ambiguous,
}

impl OutputEvidenceManifest {
    /// Create an empty evidence manifest.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add before/after evidence for a structured Cargo output path.
    ///
    /// The raw path is used only to derive an in-memory key. Absolute paths are
    /// represented by a BLAKE3 identity and are never serialized.
    pub fn insert_path(&mut self, path: &str, before: OutputSnapshot, after: OutputSnapshot) {
        let output_id = stable_output_id(path);
        let incoming = ManifestEntry::Snapshots { before, after };
        match self.entries.get(&output_id) {
            None => {
                self.entries.insert(output_id, incoming);
            }
            Some(existing) if *existing == incoming => {}
            Some(ManifestEntry::Ambiguous) => {}
            Some(ManifestEntry::Snapshots { .. }) => {
                self.entries.insert(output_id, ManifestEntry::Ambiguous);
            }
        }
    }

    fn evidence_for_path(&self, path: &str) -> DeclaredOutputEvidence {
        let output_id = stable_output_id(path);
        match self.entries.get(&output_id) {
            Some(ManifestEntry::Snapshots { before, after }) => {
                DeclaredOutputEvidence::new(output_id, before.clone(), after.clone())
            }
            Some(ManifestEntry::Ambiguous) | None => DeclaredOutputEvidence::unknown(output_id),
        }
    }
}

/// Evidence attached to one parsed Cargo unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputEvidence {
    /// Every path declared by `compiler-artifact.filenames` or `executable`.
    pub declared_outputs: Vec<DeclaredOutputEvidence>,
    /// Fail-closed execution classification derived from Cargo and outputs.
    pub freshness: Freshness,
}

impl OutputEvidence {
    /// Reconcile structured Cargo freshness with declared output snapshots.
    pub fn from_paths(
        paths: &[String],
        manifest: &OutputEvidenceManifest,
        cargo_version: &CargoVersion,
        cargo_freshness: Freshness,
    ) -> Self {
        let mut declared_outputs: Vec<_> = paths
            .iter()
            .map(|path| manifest.evidence_for_path(path))
            .collect();
        declared_outputs.sort_by(|left, right| left.output_id.cmp(&right.output_id));
        declared_outputs.dedup_by(|left, right| left.output_id == right.output_id);

        let freshness = classify_evidence(cargo_version, cargo_freshness, &declared_outputs);
        Self {
            declared_outputs,
            freshness,
        }
    }
}

/// Explicit Cargo version metadata supplied by the structured caller.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "version", rename_all = "snake_case")]
pub enum CargoVersion {
    /// A validated semantic Cargo version, optionally with a prerelease suffix.
    Known(String),
    /// No valid structured version was supplied.
    #[default]
    Unknown,
}

impl CargoVersion {
    /// Parse an explicit version; absent or malformed input remains unknown.
    pub fn parse(value: Option<&str>) -> Self {
        let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Self::Unknown;
        };
        if is_valid_version(value) {
            Self::Known(value.to_owned())
        } else {
            Self::Unknown
        }
    }
}

fn compare_snapshots(before: &OutputSnapshot, after: &OutputSnapshot) -> OutputChange {
    match (before, after) {
        (
            OutputSnapshot::Present {
                fingerprint: before_fingerprint,
                ..
            },
            OutputSnapshot::Present {
                fingerprint: after_fingerprint,
                ..
            },
        ) if before_fingerprint != after_fingerprint => OutputChange::BytesChanged,
        (
            OutputSnapshot::Present {
                mtime_ns: before_mtime,
                ..
            },
            OutputSnapshot::Present {
                mtime_ns: after_mtime,
                ..
            },
        ) if before_mtime != after_mtime => OutputChange::MtimeOnly,
        (OutputSnapshot::Present { .. }, OutputSnapshot::Present { .. }) => OutputChange::Unchanged,
        (OutputSnapshot::Missing, OutputSnapshot::Present { .. }) => OutputChange::Created,
        _ => OutputChange::Unknown,
    }
}

fn classify_evidence(
    cargo_version: &CargoVersion,
    cargo_freshness: Freshness,
    outputs: &[DeclaredOutputEvidence],
) -> Freshness {
    if !matches!(cargo_version, CargoVersion::Known(_)) || outputs.is_empty() {
        return Freshness::Unknown;
    }
    if outputs
        .iter()
        .any(|output| output.change == OutputChange::Unknown)
    {
        return Freshness::Unknown;
    }

    let changed = outputs.iter().any(|output| {
        matches!(
            output.change,
            OutputChange::Created | OutputChange::MtimeOnly | OutputChange::BytesChanged
        )
    });
    match (cargo_freshness, changed) {
        (Freshness::Actual, true) => Freshness::Actual,
        (Freshness::Fresh, false) => Freshness::Fresh,
        _ => Freshness::Unknown,
    }
}

fn stable_output_id(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        return "invalid-output".to_owned();
    }
    if is_absolute_path(path) {
        return format!("absolute:{}", blake3::hash(path.as_bytes()).to_hex());
    }
    format!("relative:{}", crate::redact_absolute_paths(path))
}

fn is_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || value.starts_with("file:///")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

fn is_valid_version(value: &str) -> bool {
    let (core, suffix) = value
        .split_once('-')
        .map_or((value, None), |(core, suffix)| (core, Some(suffix)));
    let mut components = core.split('.');
    let valid_core = components.clone().count() == 3
        && components.all(|component| {
            !component.is_empty()
                && component
                    .chars()
                    .all(|character| character.is_ascii_digit())
        });
    let valid_suffix = suffix.is_none_or(|suffix| {
        !suffix.is_empty()
            && suffix
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '.')
    });
    valid_core && valid_suffix
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(bytes: &[u8], mtime_ns: u128) -> OutputSnapshot {
        OutputSnapshot::present(bytes, mtime_ns)
    }

    fn evidence(before: OutputSnapshot, after: OutputSnapshot) -> DeclaredOutputEvidence {
        let mut manifest = OutputEvidenceManifest::new();
        manifest.insert_path("/workspace/target/debug/demo", before, after);
        manifest.evidence_for_path("/workspace/target/debug/demo")
    }

    #[test]
    fn unchanged_output_is_fresh_with_matching_cargo_evidence() {
        let output = evidence(present(b"demo", 7), present(b"demo", 7));
        assert_eq!(output.change, OutputChange::Unchanged);

        let mut manifest = OutputEvidenceManifest::new();
        manifest.insert_path(
            "/workspace/target/debug/demo",
            output.before.clone(),
            output.after.clone(),
        );
        let reconciled = OutputEvidence::from_paths(
            &["/workspace/target/debug/demo".to_owned()],
            &manifest,
            &CargoVersion::Known("1.98.0".to_owned()),
            Freshness::Fresh,
        );
        assert_eq!(reconciled.freshness, Freshness::Fresh);
    }

    #[test]
    fn mtime_only_change_is_actual() {
        assert_eq!(
            evidence(present(b"demo", 7), present(b"demo", 8)).change,
            OutputChange::MtimeOnly
        );
    }

    #[test]
    fn changed_bytes_are_actual() {
        assert_eq!(
            evidence(present(b"old", 7), present(b"new", 7)).change,
            OutputChange::BytesChanged
        );
    }

    #[test]
    fn missing_before_and_present_after_is_created() {
        assert_eq!(
            evidence(OutputSnapshot::missing(), present(b"new", 7)).change,
            OutputChange::Created
        );
    }

    #[test]
    fn missing_after_is_unknown() {
        assert_eq!(
            evidence(present(b"old", 7), OutputSnapshot::missing()).change,
            OutputChange::Unknown
        );
    }

    #[test]
    fn unknown_cargo_version_fails_closed() {
        let mut manifest = OutputEvidenceManifest::new();
        manifest.insert_path(
            "/workspace/target/debug/demo",
            present(b"old", 7),
            present(b"new", 8),
        );
        let reconciled = OutputEvidence::from_paths(
            &["/workspace/target/debug/demo".to_owned()],
            &manifest,
            &CargoVersion::parse(Some("cargo 1.98.0")),
            Freshness::Actual,
        );
        assert_eq!(reconciled.freshness, Freshness::Unknown);
        assert_eq!(CargoVersion::parse(None), CargoVersion::Unknown);
    }

    #[test]
    fn conflicting_evidence_fails_closed() {
        let mut manifest = OutputEvidenceManifest::new();
        manifest.insert_path(
            "/workspace/target/debug/demo",
            present(b"old", 7),
            present(b"new", 8),
        );
        let reconciled = OutputEvidence::from_paths(
            &["/workspace/target/debug/demo".to_owned()],
            &manifest,
            &CargoVersion::Known("1.98.0".to_owned()),
            Freshness::Fresh,
        );
        assert_eq!(reconciled.freshness, Freshness::Unknown);

        manifest.insert_path(
            "/workspace/target/debug/demo",
            present(b"other", 9),
            present(b"other", 10),
        );
        let ambiguous = OutputEvidence::from_paths(
            &["/workspace/target/debug/demo".to_owned()],
            &manifest,
            &CargoVersion::Known("1.98.0".to_owned()),
            Freshness::Actual,
        );
        assert_eq!(ambiguous.freshness, Freshness::Unknown);
        assert_eq!(ambiguous.declared_outputs[0].change, OutputChange::Unknown);
    }

    #[test]
    fn filesystem_snapshot_does_not_store_path() {
        let path = std::env::temp_dir().join(format!(
            "unit-collector-evidence-{}-{}",
            std::process::id(),
            blake3::hash(b"unit-collector-evidence-test").to_hex()
        ));
        std::fs::write(&path, b"demo").expect("write test output");
        let snapshot = OutputSnapshot::from_path(&path);
        std::fs::remove_file(&path).expect("remove test output");

        assert!(matches!(snapshot, OutputSnapshot::Present { .. }));
        let serialized = serde_json::to_string(&snapshot).expect("serialize snapshot");
        assert!(!serialized.contains(path.to_string_lossy().as_ref()));
    }
}
