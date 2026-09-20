//! The public `release apt-fetch` command must reject product/control role
//! collisions before it can invoke a provider or create an incoming tree.

use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

const SOURCE: &str = "example/source";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const TARGETS: [&str; 4] = [
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];
const RESERVED_CONTROL_NAMES: [&str; 10] = [
    "discovery.json",
    "product-manifest.json",
    "product-manifest.json.sha256",
    "release-manifest.json",
    "SHA256SUMS",
    "release-record.json",
    "release-record.json.sha256",
    "manifest.json",
    "manifest.json.sha256",
    "release-attestation.json",
];

fn selection_with_artifact_name(name: &str) -> Value {
    let components = TARGETS
        .iter()
        .map(|target| target.to_owned())
        .collect::<Vec<_>>();
    let components = json!([
        {
            "name": "velnor-runner",
            "crate": "velnor-runner",
            "feature": "release-build",
            "identity": "version",
            "binary": "velnor-runner",
            "targets": components,
            "version": "1.2.3"
        },
        {
            "name": "velnor-workflow",
            "crate": "velnor-workflow",
            "feature": null,
            "identity": "revision",
            "binary": "velnor-workflow",
            "targets": TARGETS,
            "version": "1.2.3"
        },
        {
            "name": "velnorctl",
            "crate": "velnorctl",
            "feature": "release-build",
            "identity": "version",
            "binary": "velnorctl",
            "targets": TARGETS,
            "version": "1.2.3"
        }
    ]);
    let manifest = json!({
        "artifacts": [{
            "kind": "binary",
            "name": name,
            "sha256": "1".repeat(64),
            "size": 1,
            "target": TARGETS[0]
        }],
        "channel": "stable",
        "components": components,
        "product_id": "velnor",
        "release_id": "123",
        "release_tag": "v1.2.3",
        "schema": "velnor.product-manifest/v1",
        "source_commit": COMMIT,
        "source_ref": "refs/tags/v1.2.3",
        "source_repository": SOURCE,
        "version": "1.2.3"
    });
    json!({
        "channel": "stable",
        "manifest": manifest,
        "manifest_asset": "product-manifest.json",
        "manifest_schema": "velnor.product-manifest/v1",
        "manifest_sha256": "0".repeat(64),
        "package": "example",
        "product_id": "velnor",
        "provider_repository_id": 1,
        "provider_release_id": 123,
        "published_at": "2026-09-20T00:00:00Z",
        "release_assets": [],
        "release_id": "123",
        "release_tag": "v1.2.3",
        "release_url": format!("https://github.com/{SOURCE}/releases/tag/v1.2.3"),
        "source_commit": COMMIT,
        "source_ref": "refs/tags/v1.2.3",
        "source_ref_resolution": {
            "method": "github-git-ref",
            "proof_ref": "refs/tags/v1.2.3",
            "resolved_commit": COMMIT
        },
        "source_repository": SOURCE,
        "tag": "v1.2.3",
        "target_commitish": COMMIT,
        "version": "1.2.3"
    })
}

fn fixture_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "velnor-apt-selection-cli-{}-{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    ))
}

fn output_text(stdout: &[u8], stderr: &[u8]) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    )
}

#[test]
fn public_apt_fetch_rejects_every_reserved_control_artifact_name() -> Result<(), Box<dyn Error>> {
    let root = fixture_root();
    fs::create_dir_all(root.join(".github-gen"))?;
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 2\n",
    )?;
    let selection = root.join("selection.json");
    let incoming = root.join("incoming");
    for reserved_name in RESERVED_CONTROL_NAMES {
        fs::write(
            &selection,
            serde_json::to_vec(&selection_with_artifact_name(reserved_name))?,
        )?;
        let output = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
            .current_dir(&root)
            .args([
                "release",
                "apt-fetch",
                "--selection",
                selection.to_str().ok_or("selection path is not UTF-8")?,
                "--source-repo",
                SOURCE,
                "--dir",
                incoming.to_str().ok_or("incoming path is not UTF-8")?,
            ])
            .output()?;
        let output_text = output_text(&output.stdout, &output.stderr);
        assert!(
            !output.status.success(),
            "reserved artifact {reserved_name} unexpectedly passed: {output_text}"
        );
        assert!(
            output_text.contains("discovery product artifact name is reserved or duplicated"),
            "reserved artifact {reserved_name} reached the wrong gate: {output_text}"
        );
        assert!(
            !incoming.exists(),
            "reserved artifact {reserved_name} created an incoming tree"
        );
    }
    fs::remove_dir_all(root)?;
    Ok(())
}
