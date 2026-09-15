//! Local, bounded diagnostics for native operator recovery.
//!
//! `diagnostics bundle` deliberately does not use the control client. A
//! disconnected local host must still be able to prove what `status`,
//! `preflight`, and `host status` observed, including the unavailable case.
//! Destructive lifecycle commands are never invoked by collection; `host
//! drain` and `host stop` remain explicit operator actions with their own
//! output.

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use velnor_control::{
    diagnostics::{Collector, DiagnosticsService},
    ports::PortError,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::{commands::DiagnosticsBundleArgs, preflight, CommandError, GlobalArgs};

const SCHEMA_VERSION: u8 = 1;
const COMMAND_TIMEOUT_SECONDS: u64 = 45;
const ARCHIVE_TEMP_ATTEMPTS: usize = 32;

static NEXT_ARCHIVE_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// Collect local operator evidence into the existing `diagnostics bundle`
/// surface. A captured child command may fail; that failure is evidence in the
/// archive, not a reason to discard the archive itself.
pub(crate) fn bundle(
    globals: &GlobalArgs,
    args: DiagnosticsBundleArgs,
) -> Result<(), CommandError> {
    let config_dir = preflight::resolve_config_dir()?;
    let generated_at_unix = unix_now();
    let captures = vec![
        (
            "commands/status.json".to_owned(),
            preflight::capture(
                &status_args(&config_dir, globals.instance.as_deref()),
                std::time::Duration::from_secs(COMMAND_TIMEOUT_SECONDS),
            ),
        ),
        (
            "commands/preflight.json".to_owned(),
            preflight::capture(
                &preflight_args(&config_dir),
                std::time::Duration::from_secs(COMMAND_TIMEOUT_SECONDS),
            ),
        ),
        (
            "commands/host-status.json".to_owned(),
            preflight::capture(
                &host_status_args(globals.instance.as_deref()),
                std::time::Duration::from_secs(COMMAND_TIMEOUT_SECONDS),
            ),
        ),
    ];

    let command_success = captures.iter().all(|(_, capture)| capture.succeeded());
    let metadata = Metadata {
        schema_version: SCHEMA_VERSION,
        generated_at_unix,
        host_os: std::env::consts::OS,
        host_arch: std::env::consts::ARCH,
        config_dir: config_dir.clone(),
        github_token_present: env::var_os("GITHUB_TOKEN").is_some_and(|value| !value.is_empty()),
        lifecycle: LifecycleCommands {
            drain: "velnorctl host drain",
            stop: "velnorctl host stop",
            note: "collection never invokes drain or stop",
        },
    };

    let mut collectors: Vec<Box<dyn Collector>> = Vec::with_capacity(captures.len() + 1);
    collectors.push(Box::new(StaticCollector::new(
        "metadata.json",
        json_bytes(&metadata)?,
    )));
    for (name, capture) in &captures {
        collectors.push(Box::new(StaticCollector::new(name, json_bytes(capture)?)));
    }

    let service = DiagnosticsService::new(collectors, secret_values());
    let collected = service
        .collect()
        .map_err(|error| CommandError::operation(format!("collect local diagnostics: {error}")))?;
    let manifest = Manifest::from_bundle(&collected, generated_at_unix, command_success);
    let mut members = Vec::with_capacity(collected.members.len() + 1);
    members.push(ArchiveMember {
        name: "manifest.json".to_owned(),
        content: json_bytes(&manifest)?,
    });
    members.extend(collected.members.into_iter().map(|member| ArchiveMember {
        name: member.name,
        content: member.content,
    }));
    write_archive(&args.archive, &members)?;

    let summary = BundleSummary {
        schema_version: SCHEMA_VERSION,
        archive: args.archive,
        members: members.len(),
        command_success,
    };
    if globals.output_format().is_machine() {
        println!(
            "{}",
            serde_json::to_string(&summary)
                .map_err(|error| CommandError::operation(format!("serialize summary: {error}")))?
        );
    } else {
        println!(
            "diagnostics archive: {} ({} members)",
            summary.archive.display(),
            summary.members
        );
        if !summary.command_success {
            println!("diagnostics note: one or more local command probes failed; inspect the archive evidence");
        }
    }
    Ok(())
}

#[derive(Debug)]
struct StaticCollector {
    name: String,
    content: Vec<u8>,
}

impl StaticCollector {
    fn new(name: impl Into<String>, content: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            content,
        }
    }
}

impl Collector for StaticCollector {
    fn name(&self) -> &str {
        &self.name
    }

    fn collect(&self) -> Result<Vec<u8>, PortError> {
        Ok(self.content.clone())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    schema_version: u8,
    generated_at_unix: u64,
    host_os: &'static str,
    host_arch: &'static str,
    config_dir: PathBuf,
    github_token_present: bool,
    lifecycle: LifecycleCommands,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleCommands {
    drain: &'static str,
    stop: &'static str,
    note: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u8,
    format: &'static str,
    generated_at_unix: u64,
    command_success: bool,
    members: Vec<MemberSummary>,
    omissions: Vec<OmissionSummary>,
}

impl Manifest {
    fn from_bundle(
        bundle: &velnor_control::diagnostics::DiagnosticBundle,
        generated_at_unix: u64,
        command_success: bool,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            format: "ustar",
            generated_at_unix,
            command_success,
            members: bundle
                .members
                .iter()
                .map(|member| MemberSummary {
                    name: member.name.clone(),
                    digest: member.digest.clone(),
                    bytes: member.bytes,
                    redaction_version: member.redaction_version,
                })
                .collect(),
            omissions: bundle
                .omissions
                .iter()
                .map(|omission| OmissionSummary {
                    name: omission.name.clone(),
                    reason: omission.reason.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberSummary {
    name: String,
    digest: String,
    bytes: usize,
    redaction_version: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OmissionSummary {
    name: String,
    reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleSummary {
    schema_version: u8,
    archive: PathBuf,
    members: usize,
    command_success: bool,
}

#[derive(Debug)]
struct ArchiveMember {
    name: String,
    content: Vec<u8>,
}

fn status_args(config_dir: &Path, instance: Option<&str>) -> Vec<std::ffi::OsString> {
    let mut args = vec!["--output".into(), "json".into()];
    if let Some(instance) = instance {
        args.extend(["--instance".into(), instance.into()]);
    }
    args.extend([
        "status".into(),
        "--config-dir".into(),
        config_dir.as_os_str().to_owned(),
        "--state-dir".into(),
        config_dir.as_os_str().to_owned(),
    ]);
    args
}

fn preflight_args(config_dir: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "--output".into(),
        "json".into(),
        "preflight".into(),
        "--config-dir".into(),
        config_dir.as_os_str().to_owned(),
    ]
}

fn host_status_args(instance: Option<&str>) -> Vec<std::ffi::OsString> {
    let mut args = Vec::new();
    if let Some(instance) = instance {
        args.extend(["--instance".into(), instance.into()]);
    }
    args.extend(["host".into(), "status".into()]);
    args
}

fn secret_values() -> Vec<String> {
    [
        "GITHUB_TOKEN",
        "VELNOR_PAT",
        "ACTIONS_RUNTIME_TOKEN",
        "RUNNER_TOKEN",
        "VELNOR_GITHUB_TOKEN",
    ]
    .into_iter()
    .filter_map(|name| env::var(name).ok())
    .filter(|value| !value.is_empty())
    .collect()
}

fn json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CommandError> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| CommandError::operation(format!("serialize diagnostics: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn write_archive(path: &Path, members: &[ArchiveMember]) -> Result<(), CommandError> {
    let (temporary, mut file) = create_archive_file(path)?;
    let result = (|| -> io::Result<()> {
        for member in members {
            write_tar_member(&mut file, &member.name, &member.content)?;
        }
        file.write_all(&[0_u8; 1024])?;
        file.sync_all()
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(CommandError::operation(format!(
            "write diagnostics archive {}: {error}",
            path.display()
        )));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(CommandError::operation(format!(
            "publish diagnostics archive {}: {error}",
            path.display()
        )));
    }
    Ok(())
}

fn create_archive_file(path: &Path) -> Result<(PathBuf, File), CommandError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        CommandError::new(
            velnor_model::ExitClass::Usage,
            "diagnostics.archive_path_invalid",
            "diagnostics archive path must name a file",
        )
    })?;
    let name = name.to_string_lossy();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    for attempt in 0..ARCHIVE_TEMP_ATTEMPTS {
        let sequence = NEXT_ARCHIVE_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{name}.{}.{}.{}.tmp",
            std::process::id(),
            now,
            sequence + u64::try_from(attempt).unwrap_or(u64::MAX)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(CommandError::operation(format!(
                    "create diagnostics archive {}: {error}",
                    path.display()
                )));
            }
        }
    }
    Err(CommandError::operation(format!(
        "create diagnostics archive {}: exhausted temporary paths",
        path.display()
    )))
}

fn write_tar_member<W: Write>(writer: &mut W, name: &str, content: &[u8]) -> io::Result<()> {
    if name.is_empty() || name.len() > 100 || name.contains('\0') || name.starts_with('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostic archive member name is unsafe",
        ));
    }
    let mut header = [0_u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    write_tar_number(&mut header[100..108], 0o600)?;
    write_tar_number(&mut header[108..116], 0)?;
    write_tar_number(&mut header[116..124], 0)?;
    write_tar_number(
        &mut header[124..136],
        u64::try_from(content.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "diagnostic member is too large",
            )
        })?,
    )?;
    write_tar_number(&mut header[136..148], 0)?;
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum = header.iter().map(|byte| u32::from(*byte)).sum::<u32>();
    let mut checksum_value = checksum;
    for index in (0..6).rev() {
        header[148 + index] = b'0' + u8::try_from(checksum_value % 8).unwrap_or(0);
        checksum_value /= 8;
    }
    header[154] = 0;
    header[155] = b' ';
    writer.write_all(&header)?;
    writer.write_all(content)?;
    let padding = (512 - (content.len() % 512)) % 512;
    if padding > 0 {
        writer.write_all(&vec![0_u8; padding])?;
    }
    Ok(())
}

fn write_tar_number(field: &mut [u8], mut value: u64) -> io::Result<()> {
    if field.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty tar numeric field",
        ));
    }
    field.fill(0);
    let last = field.len() - 1;
    for index in (0..last).rev() {
        field[index] = b'0' + u8::try_from(value % 8).unwrap_or(0);
        value /= 8;
    }
    if value != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tar numeric field overflow",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests use local scratch values"
)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "velnorctl-diagnostics-{label}-{}-{}",
            std::process::id(),
            unix_now()
        ));
        fs::create_dir_all(&path).expect("scratch");
        path
    }

    #[test]
    fn tar_writer_emits_fixed_safe_members_and_padding() {
        let root = scratch("tar");
        let archive = root.join("evidence.tar");
        let members = vec![
            ArchiveMember {
                name: "manifest.json".to_owned(),
                content: b"{}\n".to_vec(),
            },
            ArchiveMember {
                name: "commands/status.json".to_owned(),
                content: b"status\n".to_vec(),
            },
        ];
        write_archive(&archive, &members).expect("write archive");
        let bytes = fs::read(&archive).expect("archive bytes");
        assert_eq!(bytes.len() % 512, 0);
        assert!(bytes
            .windows(b"manifest.json".len())
            .any(|window| window == b"manifest.json"));
        assert!(bytes
            .windows(b"commands/status.json".len())
            .any(|window| window == b"commands/status.json"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn manifest_preserves_redaction_and_command_failure_evidence() {
        let bundle = velnor_control::diagnostics::DiagnosticBundle {
            manifest_version: 1,
            members: vec![velnor_control::diagnostics::BundleMember {
                name: "commands/status.json".to_owned(),
                digest: "sha256:test".to_owned(),
                bytes: 4,
                redaction_version: 1,
                content: b"***\n".to_vec(),
            }],
            omissions: vec![velnor_control::diagnostics::BundleOmission {
                name: "commands/preflight.json".to_owned(),
                reason: "collector byte limit exceeded".to_owned(),
            }],
        };
        let manifest = Manifest::from_bundle(&bundle, 10, false);
        let json = serde_json::to_value(manifest).expect("manifest JSON");
        assert_eq!(json["commandSuccess"], false);
        assert_eq!(json["members"][0]["redactionVersion"], 1);
        assert_eq!(json["omissions"][0]["name"], "commands/preflight.json");
    }

    #[test]
    fn command_arguments_are_local_and_never_include_a_pat() {
        let args = status_args(Path::new("/tmp/velnor"), Some("mac-host"));
        let rendered = args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(rendered.iter().any(|arg| arg == "status"));
        assert!(rendered.iter().all(|arg| !arg.contains("token")));
        assert!(rendered.iter().all(|arg| !arg.contains("pat")));
    }
}
