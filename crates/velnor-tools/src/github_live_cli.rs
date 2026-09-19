//! Runnable local entry point for a complete read-only GitHub capture.
//!
//! The command deliberately emits the typed live collection separately from
//! the checker-owned G0 mapping.  A capture has real API provenance, but a
//! caller must still supply reviewed model/workload bindings before it can be
//! presented as authoritative G0 evidence.

use super::live_collector::{collect_live, collect_live_sample};
use super::live_transport::{GithubHttpTransport, RawObjectFileStore};
use super::AuthIdentity;
use crate::evidence_check::ManifestDocument;
use anyhow::{bail, Context, Result};
use clap::Args;
use serde::Serialize;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Args)]
pub struct G0LiveCollectArgs {
    /// Reviewed 32-repository manifest JSON.
    #[arg(long)]
    pub manifest: PathBuf,
    /// Non-empty caller-chosen capture identity.
    #[arg(long)]
    pub snapshot_id: String,
    /// New local evidence directory. Existing output files are never replaced.
    #[arg(long)]
    pub evidence_dir: PathBuf,
    /// Exact source revision of this collector binary.
    #[arg(long, env = "VELNOR_COLLECTOR_REVISION")]
    pub collector_revision: String,
}

#[derive(Debug, Args)]
pub struct G0LiveSampleArgs {
    /// GitHub owner/name to sample.
    #[arg(long)]
    pub repository: String,
    /// Non-empty caller-chosen capture identity.
    #[arg(long)]
    pub snapshot_id: String,
    /// New local evidence directory. Existing output files are never replaced.
    #[arg(long)]
    pub evidence_dir: PathBuf,
    /// Exact source revision of this collector binary.
    #[arg(long, env = "VELNOR_COLLECTOR_REVISION")]
    pub collector_revision: String,
}

#[derive(Debug, Serialize)]
struct CaptureMetadata<'a> {
    schema_version: u32,
    collector_revision: &'a str,
    snapshot_id: &'a str,
    manifest_id: &'a str,
    read_only: bool,
    collection_file: &'a str,
    raw_store_directory: &'static str,
}

pub async fn run(args: G0LiveCollectArgs) -> Result<()> {
    if args.collector_revision.trim().is_empty() {
        bail!("--collector-revision must be non-empty");
    }
    if args.snapshot_id.trim().is_empty() {
        bail!("--snapshot-id must be non-empty");
    }
    let manifest_bytes = fs::read(&args.manifest)
        .with_context(|| format!("read reviewed manifest {}", args.manifest.display()))?;
    let manifest: ManifestDocument = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parse reviewed manifest {}", args.manifest.display()))?;

    fs::create_dir_all(&args.evidence_dir)
        .with_context(|| format!("create evidence directory {}", args.evidence_dir.display()))?;
    let (transport, auth) = authenticated_transport()?;
    let mut store = RawObjectFileStore::new(args.evidence_dir.join("raw"))
        .with_context(|| format!("create raw store below {}", args.evidence_dir.display()))?;
    let collection = collect_live(
        &transport,
        &mut store,
        auth,
        &manifest,
        args.snapshot_id.clone(),
    )
    .await
    .context("collect complete read-only GitHub inventory")?;

    let metadata = CaptureMetadata {
        schema_version: 1,
        collector_revision: &args.collector_revision,
        snapshot_id: &collection.snapshot_id,
        manifest_id: &collection.manifest_id,
        read_only: true,
        collection_file: "live-collection.json",
        raw_store_directory: "raw",
    };
    write_json_new(&args.evidence_dir.join("live-collection.json"), &collection)?;
    write_json_new(&args.evidence_dir.join("capture-metadata.json"), &metadata)?;
    println!(
        "captured {} repositories, {} requests, {} raw objects into {}",
        collection.repositories.len(),
        collection.requests.len(),
        collection.raw_objects.len(),
        args.evidence_dir.display()
    );
    Ok(())
}

pub async fn run_sample(args: G0LiveSampleArgs) -> Result<()> {
    if args.collector_revision.trim().is_empty() {
        bail!("--collector-revision must be non-empty");
    }
    if args.snapshot_id.trim().is_empty() {
        bail!("--snapshot-id must be non-empty");
    }
    fs::create_dir_all(&args.evidence_dir)
        .with_context(|| format!("create evidence directory {}", args.evidence_dir.display()))?;
    let (transport, auth) = authenticated_transport()?;
    let mut store = RawObjectFileStore::new(args.evidence_dir.join("raw"))
        .with_context(|| format!("create raw store below {}", args.evidence_dir.display()))?;
    let sample = collect_live_sample(
        &transport,
        &mut store,
        auth,
        &args.repository,
        args.snapshot_id.clone(),
    )
    .await
    .context("collect bounded read-only GitHub sample")?;
    let manifest_id = format!("sample:{}", sample.repository);
    let metadata = CaptureMetadata {
        schema_version: 1,
        collector_revision: &args.collector_revision,
        snapshot_id: &sample.snapshot_id,
        manifest_id: &manifest_id,
        read_only: true,
        collection_file: "live-sample.json",
        raw_store_directory: "raw",
    };
    write_json_new(&args.evidence_dir.join("live-sample.json"), &sample)?;
    write_json_new(&args.evidence_dir.join("capture-metadata.json"), &metadata)?;
    println!(
        "sampled {} with {} requests and {} raw objects into {}",
        sample.repository,
        sample.requests.len(),
        sample.raw_objects.len(),
        args.evidence_dir.display()
    );
    Ok(())
}

fn authenticated_transport() -> Result<(GithubHttpTransport, AuthIdentity)> {
    let transport = GithubHttpTransport::from_env_or_gh()?;
    let mut auth = AuthIdentity::new("github-token", "github", None, None, BTreeSet::new());
    transport.bind_auth(&mut auth)?;
    Ok((transport, auth))
}

fn write_json_new<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).context("serialize capture evidence")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create immutable evidence file {}", path.display()))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .with_context(|| format!("write immutable evidence file {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::CaptureMetadata;

    #[test]
    fn metadata_never_contains_credentials() -> Result<(), serde_json::Error> {
        let value = serde_json::to_value(CaptureMetadata {
            schema_version: 1,
            collector_revision: "rev",
            snapshot_id: "snapshot",
            manifest_id: "manifest",
            read_only: true,
            collection_file: "live-collection.json",
            raw_store_directory: "raw",
        })?;
        let text = value.to_string();
        assert!(!text.contains("token"));
        assert!(!text.contains("Authorization"));
        Ok(())
    }
}
