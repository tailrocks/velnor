//! Parse structured Cargo build messages into comparable unit records.
//!
//! This first slice deliberately consumes Cargo JSON from a reader instead of
//! launching Cargo. It observes `compiler-artifact` and
//! `build-script-executed` messages only; human-readable diagnostics are not a
//! source of unit records. Cargo versions that do not provide timing fields
//! produce zero durations, which is explicit in the generated summary.

pub mod evidence;

pub use evidence::{
    CargoVersion, DeclaredOutputEvidence, OutputChange, OutputEvidence, OutputEvidenceManifest,
    OutputSnapshot,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fmt, io, io::BufRead, str::FromStr};
use thiserror::Error;

const UNKNOWN: &str = "unknown";
const ABSOLUTE_PATH_MARKER: &str = "<absolute-path>";

/// Cargo profile represented by a collected unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildMode {
    /// Cargo's check profile.
    Check,
    /// Cargo's test profile.
    Test,
    /// Cargo's optimized release profile.
    Release,
}

impl fmt::Display for BuildMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Check => "check",
            Self::Test => "test",
            Self::Release => "release",
        };
        formatter.write_str(value)
    }
}

/// Error returned when the command-line mode is not supported by the schema.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unsupported build mode '{value}'; expected check, test, or release")]
pub struct InvalidBuildMode {
    value: String,
}

impl FromStr for BuildMode {
    type Err = InvalidBuildMode;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "check" => Ok(Self::Check),
            "test" => Ok(Self::Test),
            "release" => Ok(Self::Release),
            _ => Err(InvalidBuildMode {
                value: value.to_owned(),
            }),
        }
    }
}

/// Unit category inferred from Cargo's structured target metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitKind {
    /// A normal library or binary compilation without a link artifact.
    #[serde(rename = "compilation")]
    Compilation,
    /// A Cargo build script execution.
    #[serde(rename = "build-script")]
    BuildScript,
    /// A procedural macro compilation.
    #[serde(rename = "proc-macro")]
    ProcMacro,
    /// A target for which Cargo reported an executable artifact.
    #[serde(rename = "link")]
    Link,
}

impl fmt::Display for UnitKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Compilation => "compilation",
            Self::BuildScript => "build-script",
            Self::ProcMacro => "proc-macro",
            Self::Link => "link",
        };
        formatter.write_str(value)
    }
}

/// Conservative freshness classification from Cargo's explicit `fresh` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Freshness {
    /// Cargo explicitly reported `fresh: false`.
    Actual,
    /// Cargo explicitly reported `fresh: true`.
    Fresh,
    /// The message did not provide a freshness decision.
    Unknown,
}

impl fmt::Display for Freshness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Actual => "actual",
            Self::Fresh => "fresh",
            Self::Unknown => UNKNOWN,
        };
        formatter.write_str(value)
    }
}

/// Options applied to every parsed Cargo message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectOptions {
    mode: BuildMode,
    target: Option<String>,
    cargo_version: CargoVersion,
    output_evidence: OutputEvidenceManifest,
}

impl CollectOptions {
    /// Create options for a Cargo profile and optional target triple.
    ///
    /// The target is sanitized at the boundary so custom target paths cannot
    /// enter output records.
    pub fn new(mode: BuildMode, target: Option<&str>) -> Self {
        Self {
            mode,
            target: target.map(redact_absolute_paths),
            cargo_version: CargoVersion::Unknown,
            output_evidence: OutputEvidenceManifest::default(),
        }
    }

    /// Set the explicit structured Cargo version used for evidence checks.
    ///
    /// Missing or malformed versions remain [`CargoVersion::Unknown`]. The
    /// collector never derives a version from human-readable diagnostics.
    pub fn with_cargo_version(mut self, version: Option<&str>) -> Self {
        self.cargo_version = CargoVersion::parse(version);
        self
    }

    /// Supply before/after snapshots for declared compiler outputs.
    pub fn with_output_evidence(mut self, evidence: OutputEvidenceManifest) -> Self {
        self.output_evidence = evidence;
        self
    }

    /// Return the selected Cargo profile.
    pub fn mode(&self) -> BuildMode {
        self.mode
    }

    /// Return the sanitized target override, if one was supplied.
    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    /// Return the explicit Cargo version, or unknown when it was not supplied.
    pub fn cargo_version(&self) -> &CargoVersion {
        &self.cargo_version
    }
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self::new(BuildMode::Check, None)
    }
}

/// One structured Cargo unit observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitRecord {
    /// Cargo target or package name, never an absolute source path.
    pub unit_name: String,
    /// Inferred unit category.
    pub kind: UnitKind,
    /// Profile selected for the observed invocation.
    pub mode: BuildMode,
    /// Structured wall-clock duration in milliseconds, or zero when absent.
    pub wall_ms: u64,
    /// Structured CPU duration in milliseconds, or zero when absent.
    pub cpu_ms: u64,
    /// Explicit Cargo freshness decision, or `unknown`.
    pub freshness: Freshness,
    /// Explicit Cargo version used for output evidence, or `unknown`.
    pub cargo_version: CargoVersion,
    /// Path-free declared-output evidence and its fail-closed classification.
    pub output_evidence: OutputEvidence,
    /// Structured rustc flags supplied under the additive `rwd_flags` field.
    pub rwd_flags: Vec<String>,
    /// Features reported by Cargo for this artifact.
    pub features: Vec<String>,
    /// Target triple supplied by the caller or structured input.
    pub target: String,
}

/// Input and output errors with line context for the stdin protocol.
#[derive(Debug, Error)]
pub enum CollectorError {
    /// The input stream could not be read.
    #[error("read Cargo JSON input line {line}: {source}")]
    Read {
        line: usize,
        #[source]
        source: io::Error,
    },
    /// A non-empty input line was not valid JSON.
    #[error("invalid Cargo JSON on input line {line}: {source}")]
    InvalidJson {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    /// A valid JSON value was not an object message.
    #[error("Cargo JSON input line {line} is not an object")]
    NotObject { line: usize },
}

/// Output errors for JSONL serialization and file writes.
#[derive(Debug, Error)]
pub enum OutputError {
    /// The destination rejected a write.
    #[error("write collector output: {0}")]
    Io(#[from] io::Error),
    /// A unit record could not be serialized.
    #[error("serialize unit record: {0}")]
    Json(#[from] serde_json::Error),
}

/// Parse all supported structured Cargo messages from a line-oriented reader.
pub fn collect_messages<R: BufRead>(
    reader: R,
    options: &CollectOptions,
) -> Result<Vec<UnitRecord>, CollectorError> {
    let mut records = Vec::new();

    for (line_index, line) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = line.map_err(|source| CollectorError::Read {
            line: line_number,
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }

        let message: Value =
            serde_json::from_str(&line).map_err(|source| CollectorError::InvalidJson {
                line: line_number,
                source,
            })?;
        if !message.is_object() {
            return Err(CollectorError::NotObject { line: line_number });
        }
        if let Some(record) = parse_cargo_message(&message, options) {
            records.push(record);
        }
    }

    Ok(records)
}

/// Parse one Cargo JSON object into a unit record when its reason is supported.
pub fn parse_cargo_message(message: &Value, options: &CollectOptions) -> Option<UnitRecord> {
    let object = message.as_object()?;
    match object.get("reason").and_then(Value::as_str)? {
        "compiler-artifact" => parse_compiler_artifact(object, options),
        "build-script-executed" => parse_build_script(object, options),
        _ => None,
    }
}

/// Write records as newline-delimited JSON without adding paths or log prose.
pub fn write_units_jsonl<W: io::Write>(
    mut writer: W,
    records: &[UnitRecord],
) -> Result<(), OutputError> {
    for record in records {
        serde_json::to_writer(&mut writer, record)?;
        writer.write_all(b"\n")?;
    }
    Ok(())
}

/// Render a deterministic report ranked by descending wall-clock duration.
pub fn render_summary(records: &[UnitRecord]) -> String {
    use fmt::Write as _;

    let mut ranked: Vec<&UnitRecord> = records.iter().collect();
    ranked.sort_by(|left, right| {
        right
            .wall_ms
            .cmp(&left.wall_ms)
            .then_with(|| left.unit_name.cmp(&right.unit_name))
            .then_with(|| left.kind.to_string().cmp(&right.kind.to_string()))
    });

    let mut summary = String::new();
    let _ = writeln!(summary, "# Cargo unit summary");
    let _ = writeln!(summary);
    let _ = writeln!(summary, "Observed structured units: {}.", records.len());
    let _ = writeln!(
        summary,
        "Durations come from structured `wall_ms`/`cpu_ms` fields; missing timing is recorded as zero, not inferred from human logs."
    );
    let _ = writeln!(summary);
    let _ = writeln!(summary, "## Top 10 units by wall_ms");
    let _ = writeln!(summary);
    let _ = writeln!(
        summary,
        "| Rank | Unit | Kind | Mode | Wall ms | CPU ms | Freshness | Target |"
    );
    let _ = writeln!(
        summary,
        "| ---: | --- | --- | --- | ---: | ---: | --- | --- |"
    );

    for (index, record) in ranked.into_iter().take(10).enumerate() {
        let _ = writeln!(
            summary,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            index + 1,
            markdown_cell(&record.unit_name),
            record.kind,
            record.mode,
            record.wall_ms,
            record.cpu_ms,
            record.freshness,
            markdown_cell(&record.target),
        );
    }

    summary
}

/// Replace absolute path values with a stable non-path marker.
pub fn redact_absolute_paths(value: &str) -> String {
    value
        .split_whitespace()
        .map(redact_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_compiler_artifact(
    object: &serde_json::Map<String, Value>,
    options: &CollectOptions,
) -> Option<UnitRecord> {
    let target = object.get("target")?.as_object()?;
    let unit_name = target
        .get("name")
        .and_then(Value::as_str)
        .map(redact_absolute_paths)
        .filter(|name| !name.is_empty())
        .or_else(|| package_name(object.get("package_id")))?;

    let freshness = classify_freshness(object.get("fresh").and_then(Value::as_bool));
    Some(UnitRecord {
        unit_name,
        kind: artifact_kind(object, target),
        mode: options.mode,
        wall_ms: timing_ms(object, "wall_ms"),
        cpu_ms: timing_ms(object, "cpu_ms"),
        freshness,
        cargo_version: options.cargo_version.clone(),
        output_evidence: OutputEvidence::from_paths(
            &artifact_output_paths(object),
            &options.output_evidence,
            &options.cargo_version,
            freshness,
        ),
        rwd_flags: string_values(object.get("rwd_flags")),
        features: string_values(object.get("features")),
        target: target_name(object, options),
    })
}

fn parse_build_script(
    object: &serde_json::Map<String, Value>,
    options: &CollectOptions,
) -> Option<UnitRecord> {
    let freshness = classify_freshness(object.get("fresh").and_then(Value::as_bool));
    Some(UnitRecord {
        unit_name: package_name(object.get("package_id"))?,
        kind: UnitKind::BuildScript,
        mode: options.mode,
        wall_ms: timing_ms(object, "wall_ms"),
        cpu_ms: timing_ms(object, "cpu_ms"),
        freshness,
        cargo_version: options.cargo_version.clone(),
        output_evidence: OutputEvidence::from_paths(
            &[],
            &options.output_evidence,
            &options.cargo_version,
            freshness,
        ),
        rwd_flags: string_values(object.get("rwd_flags")),
        features: string_values(object.get("features")),
        target: target_name(object, options),
    })
}

fn artifact_kind(
    object: &serde_json::Map<String, Value>,
    target: &serde_json::Map<String, Value>,
) -> UnitKind {
    let kinds = string_values(target.get("kind"));
    let crate_types = string_values(target.get("crate_types"));
    if kinds.iter().any(|kind| kind == "custom-build") {
        return UnitKind::BuildScript;
    }
    if kinds.iter().any(|kind| kind == "proc-macro")
        || crate_types.iter().any(|kind| kind == "proc-macro")
    {
        return UnitKind::ProcMacro;
    }
    if object.get("executable").and_then(Value::as_str).is_some() {
        return UnitKind::Link;
    }
    UnitKind::Compilation
}

fn classify_freshness(fresh: Option<bool>) -> Freshness {
    match fresh {
        Some(true) => Freshness::Fresh,
        Some(false) => Freshness::Actual,
        None => Freshness::Unknown,
    }
}

fn artifact_output_paths(object: &serde_json::Map<String, Value>) -> Vec<String> {
    let mut paths: Vec<String> = object
        .get("filenames")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect();
    if let Some(executable) = object.get("executable").and_then(Value::as_str) {
        paths.push(executable.to_owned());
    }
    paths.sort_unstable();
    paths.dedup();
    paths
}

fn timing_ms(object: &serde_json::Map<String, Value>, field: &str) -> u64 {
    object
        .get("timing")
        .and_then(Value::as_object)
        .and_then(|timing| timing.get(field))
        .and_then(Value::as_u64)
        .or_else(|| object.get(field).and_then(Value::as_u64))
        .unwrap_or(0)
}

fn target_name(object: &serde_json::Map<String, Value>, options: &CollectOptions) -> String {
    options
        .target
        .clone()
        .or_else(|| {
            object
                .get("target_triple")
                .and_then(Value::as_str)
                .map(redact_absolute_paths)
        })
        .unwrap_or_else(|| UNKNOWN.to_owned())
}

fn package_name(value: Option<&Value>) -> Option<String> {
    let package_id = value.and_then(Value::as_str)?;
    let identity = package_id
        .split_once('#')
        .map(|(_, identity)| identity)
        .unwrap_or(package_id);
    identity
        .split('@')
        .next()
        .and_then(|identity| identity.split_whitespace().next())
        .map(redact_absolute_paths)
        .filter(|name| !name.is_empty())
}

fn string_values(value: Option<&Value>) -> Vec<String> {
    let mut values: Vec<String> = value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(redact_absolute_paths)
        .collect();
    values.sort_unstable();
    values.dedup();
    values
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn redact_token(token: &str) -> String {
    if let Some(start) = token.find("file:///") {
        return redact_suffix(token, start);
    }
    if token.starts_with('/') {
        return redact_suffix(token, 0);
    }
    if token.starts_with("-L/") {
        return redact_suffix(token, 2);
    }
    if let Some(start) = token.find("=/") {
        return redact_suffix(token, start + 1);
    }
    if is_windows_absolute_path(token) {
        return redact_suffix(token, 0);
    }
    token.to_owned()
}

fn redact_suffix(value: &str, start: usize) -> String {
    let suffix = &value[start..];
    let end = suffix
        .find([',', ';', ']', ')', '}', '"', '\''])
        .unwrap_or(suffix.len());
    let mut redacted = String::with_capacity(value.len());
    redacted.push_str(&value[..start]);
    redacted.push_str(ABSOLUTE_PATH_MARKER);
    redacted.push_str(&suffix[end..]);
    redacted
}

fn is_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn options() -> CollectOptions {
        CollectOptions::new(BuildMode::Test, Some("x86_64-unknown-linux-gnu"))
    }

    #[test]
    fn parses_artifact_and_uses_structured_fields() {
        let message = serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": "demo 0.1.0 (path+file:///Users/alice/demo)",
            "target": {
                "name": "demo-macro",
                "kind": ["proc-macro"],
                "crate_types": ["proc-macro"]
            },
            "features": ["default", "serde"],
            "rwd_flags": ["--cfg", "feature=serde"],
            "timing": {"wall_ms": 41, "cpu_ms": 37},
            "fresh": false,
            "executable": null
        });

        let record = parse_cargo_message(&message, &options()).expect("artifact record");

        assert_eq!(record.unit_name, "demo-macro");
        assert_eq!(record.kind, UnitKind::ProcMacro);
        assert_eq!(record.mode, BuildMode::Test);
        assert_eq!(record.wall_ms, 41);
        assert_eq!(record.cpu_ms, 37);
        assert_eq!(record.freshness, Freshness::Actual);
        assert_eq!(record.features, ["default", "serde"]);
        assert_eq!(record.rwd_flags, ["--cfg", "feature=serde"]);
        assert_eq!(record.target, "x86_64-unknown-linux-gnu");
    }

    #[test]
    fn freshness_is_conservative_for_all_supported_states() {
        let mut messages = Vec::new();
        for fresh in [Some(false), Some(true), None] {
            let mut message = serde_json::json!({
                "reason": "compiler-artifact",
                "target": {"name": "demo", "kind": ["lib"], "crate_types": ["lib"]}
            });
            if let Some(fresh) = fresh {
                message["fresh"] = Value::Bool(fresh);
            }
            messages.push(serde_json::to_string(&message).expect("serialize test message"));
        }

        let input = messages.join("\n");
        let records = collect_messages(Cursor::new(input), &CollectOptions::default())
            .expect("collect messages");

        assert_eq!(
            records
                .iter()
                .map(|record| record.freshness)
                .collect::<Vec<_>>(),
            [Freshness::Actual, Freshness::Fresh, Freshness::Unknown]
        );
    }

    #[test]
    fn package_ids_are_reduced_to_stable_names() {
        for (package_id, expected) in [
            (
                "registry+https://github.com/rust-lang/crates.io-index#serde@1.0.0",
                "serde",
            ),
            ("git+https://github.com/example/demo#demo@abc123", "demo"),
            ("demo 0.1.0 (path+file:///Users/alice/demo)", "demo"),
        ] {
            let message = serde_json::json!({
                "reason": "build-script-executed",
                "package_id": package_id
            });
            assert_eq!(
                parse_cargo_message(&message, &CollectOptions::default())
                    .expect("build script record")
                    .unit_name,
                expected
            );
        }
    }

    #[test]
    fn redacts_absolute_paths_in_flags_and_custom_targets() {
        let options =
            CollectOptions::new(BuildMode::Check, Some("/Users/alice/targets/custom.json"));
        let message = serde_json::json!({
            "reason": "compiler-artifact",
            "target": {"name": "demo", "kind": ["lib"], "crate_types": ["lib"]},
            "rwd_flags": ["--remap-path-prefix=/Users/alice/demo=/workspace/demo"]
        });

        let record = parse_cargo_message(&message, &options).expect("artifact record");
        let json = serde_json::to_string(&record).expect("serialize record");

        assert!(!json.contains("/Users/alice"));
        assert!(json.contains(ABSOLUTE_PATH_MARKER));
        assert_eq!(record.target, ABSOLUTE_PATH_MARKER);
    }

    #[test]
    fn compiler_artifact_outputs_feed_path_free_evidence() {
        let output_path = "/Users/alice/target/debug/deps/libdemo.rlib";
        let mut manifest = OutputEvidenceManifest::new();
        manifest.insert_path(
            output_path,
            OutputSnapshot::present(b"old", 7),
            OutputSnapshot::present(b"old", 8),
        );
        let options = CollectOptions::default()
            .with_cargo_version(Some("1.98.0"))
            .with_output_evidence(manifest);
        let message = serde_json::json!({
            "reason": "compiler-artifact",
            "package_id": "demo 0.1.0 (path+file:///Users/alice/demo)",
            "target": {"name": "demo", "kind": ["lib"], "crate_types": ["lib"]},
            "filenames": [output_path],
            "executable": output_path,
            "fresh": false
        });

        let record = parse_cargo_message(&message, &options).expect("artifact record");
        assert_eq!(
            record.cargo_version,
            CargoVersion::Known("1.98.0".to_owned())
        );
        assert_eq!(record.output_evidence.declared_outputs.len(), 1);
        assert_eq!(
            record.output_evidence.declared_outputs[0].change,
            OutputChange::MtimeOnly
        );
        assert_eq!(record.output_evidence.freshness, Freshness::Actual);
        let serialized = serde_json::to_string(&record).expect("serialize record");
        assert!(!serialized.contains("/Users/alice"));
    }

    #[test]
    fn summary_ranks_only_top_ten_by_wall_time() {
        let records: Vec<UnitRecord> = (0_u64..12)
            .map(|index| UnitRecord {
                unit_name: format!("unit-{index:02}"),
                kind: UnitKind::Compilation,
                mode: BuildMode::Check,
                wall_ms: index,
                cpu_ms: index,
                freshness: Freshness::Actual,
                cargo_version: CargoVersion::Unknown,
                output_evidence: OutputEvidence {
                    declared_outputs: Vec::new(),
                    freshness: Freshness::Unknown,
                },
                rwd_flags: Vec::new(),
                features: Vec::new(),
                target: UNKNOWN.to_owned(),
            })
            .collect();

        let summary = render_summary(&records);

        assert!(summary.contains("| 1 | unit-11 |"));
        assert!(summary.contains("| 10 | unit-02 |"));
        assert!(!summary.contains("| unit-01 |"));
        assert!(summary.contains("| Kind |"));
    }
}
