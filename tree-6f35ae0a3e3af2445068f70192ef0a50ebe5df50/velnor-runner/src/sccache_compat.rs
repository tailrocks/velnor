//! Explicit sccache compatibility mode (fixture scenario B).
//!
//! Goal 22 gives the default path one compiler-acceleration architecture:
//! transparent Mr Boxington. Explicit sccache is the supported *alternative*,
//! never a simultaneous default. Everything sccache lives behind this module,
//! and [`is_explicit`] is the single decision point: when the job explicitly
//! requests `mozilla-actions/sccache-action`, the caller wires this mode's
//! store, mounts, environment, and provisioning; otherwise sccache is
//! entirely absent from planning, mounts, env, and PATH.
//!
//! ## Provisioning
//!
//! The default job image does not ship sccache (it was removed from
//! `docker/job-mise.toml`, its lock, and the Dockerfile assertion). The
//! explicit mode provisions it at step time instead, following the same
//! pinned-release pattern as the cosign adapter: [`setup_script`] reuses a
//! PATH sccache whose version already matches [`LOCKED_VERSION`], and
//! otherwise downloads the musl tarball for the container's architecture,
//! verifies its SHA-256 against the per-arch checksums below, installs it
//! into `/usr/local/bin` (already on the explicit mode's `PATH`; a
//! home-directory fallback covers non-root lanes), and only then exports
//! `RUSTC_WRAPPER` and starts the server. The version plus checksums are
//! this tool's lock — there is deliberately no Renovate entry, because a
//! version-only bump without fresh checksums would fail closed at install
//! time. To bump: update [`LOCKED_VERSION`], both checksums (from the
//! release assets' `.sha256` files), and the `version: vX.Y.Z` input rule in
//! `crate::manifest` (enforced by test, not by `version-pins.json`).
//!
//! ## Boundaries
//!
//! * Planning (`github_adapter`) calls [`is_explicit`] once per job and
//!   resolves the store with [`store_host`]; mbx is gated on `!is_explicit`.
//! * The container renderer mounts [`CONTAINER_DIR`] and applies
//!   [`container_env`]; the default mbx branch is untouched.
//! * The native `Sccache` adapter runs [`setup_script`] and registers
//!   [`post_script`]; the guest lane runs [`guest_script`].
//! * GC still accounts the explicit-mode store through
//!   `StoreClass::Sccache` — that is cleanup of explicit-mode state, not a
//!   default presence.

use std::path::{Path, PathBuf};

use crate::job_message::AgentJobRequestMessage;

/// Workflow action that selects this mode.
pub(crate) const ACTION_REPOSITORY: &str = "mozilla-actions/sccache-action";

/// Pinned sccache release provisioned by the explicit mode. See the module
/// docs for the bump procedure.
pub(crate) const LOCKED_VERSION: &str = "0.16.0";

/// SHA-256 of `sccache-v0.16.0-x86_64-unknown-linux-musl.tar.gz`.
const LINUX_X64_SHA256: &str = "aec995a83ad3dff3d14b6314e08858b7b73d35ca85a5bcf3d3a9ec07dee35588";

/// SHA-256 of `sccache-v0.16.0-aarch64-unknown-linux-musl.tar.gz`.
const LINUX_ARM64_SHA256: &str = "f73a5c39f96bb6ebb89cc7915cf182260d4cbf30765322c5e793d0fe8bd80784";

/// Container mount point of the explicit-mode compiler store.
pub(crate) const CONTAINER_DIR: &str = "/var/cache/sccache";

/// Local-store size the explicit mode configures.
pub(crate) const CACHE_SIZE: &str = "20G";

/// Marker the setup script prints with its install directory, so the adapter
/// can add a non-default install dir to later steps' PATH (cosign pattern).
pub(crate) const PATH_MARKER: &str = "__VELNOR_SCCACHE_DIR__";

/// Single decision point for the explicit mode: true iff the job enables a
/// step referencing [`ACTION_REPOSITORY`]. Everything else — store, mounts,
/// env, PATH, provisioning — keys off this one call.
pub(crate) fn is_explicit(job: &AgentJobRequestMessage) -> bool {
    job.steps.iter().filter(|step| step.enabled).any(|step| {
        step.reference
            .as_ref()
            .and_then(|reference| reference.name.as_deref())
            .is_some_and(|name| name.eq_ignore_ascii_case(ACTION_REPOSITORY))
    })
}

/// Host path of the explicit-mode compiler store. Call only when
/// [`is_explicit`] holds: repository-namespaced under the job's admitted
/// trust scope like every compiler store, ephemeral when the repository id
/// is missing or invalid.
pub(crate) fn store_host(
    job: &AgentJobRequestMessage,
    temp_host: &Path,
    trust_scope: &str,
) -> PathBuf {
    crate::github_adapter::github_rust_store_host(job, temp_host, trust_scope, "sccache")
}

/// Container environment for the explicit mode, applied after the workflow's
/// own environment so the job cannot stack accelerators or re-enable mbx.
pub(crate) fn container_env() -> [(&'static str, &'static str); 5] {
    [
        ("MBX_DISABLE", "1"),
        ("RUSTC_WRAPPER", "sccache"),
        ("SCCACHE_DIR", CONTAINER_DIR),
        ("SCCACHE_CACHE_SIZE", CACHE_SIZE),
        ("SCCACHE_GHA_ENABLED", "false"),
    ]
}

/// Shell that provisions the pinned sccache release when PATH lacks it, then
/// mirrors `mozilla-actions/sccache-action`: export the local-backend
/// environment and start the server. Velnor selects `RUSTC_WRAPPER=sccache`
/// only when this explicit action is present, so the binary MUST exist on
/// PATH afterwards or every compile fails with "could not execute process
/// `sccache ...`: No such file or directory".
pub(crate) fn setup_script() -> String {
    format!(
        r#"set -e
{ensure}
sccache --version | grep -F 'sccache {ver}'
# Velnor provides a fast, host-shared sccache cache bind-mounted at
# /var/cache/sccache. Use that local backend instead of the GitHub Actions cache
# service: this is not a GitHub-hosted cache environment, so SCCACHE_GHA_ENABLED
# would make the server fail ("cache url for ghac not found") and every
# RUSTC_WRAPPER=sccache compile would error. Export the override via GITHUB_ENV so
# subsequent compile steps (clippy/test) pick it up, and disable the GHA backend.
SCCACHE_LOCAL_DIR=/var/cache/sccache
mkdir -p "$SCCACHE_LOCAL_DIR" 2>/dev/null || true
if [ -n "${{GITHUB_ENV:-}}" ]; then
  echo "RUSTC_WRAPPER=sccache" >> "$GITHUB_ENV"
  echo "SCCACHE_DIR=$SCCACHE_LOCAL_DIR" >> "$GITHUB_ENV"
  echo "SCCACHE_GHA_ENABLED=false" >> "$GITHUB_ENV"
fi
export RUSTC_WRAPPER=sccache
export SCCACHE_DIR="$SCCACHE_LOCAL_DIR"
export SCCACHE_GHA_ENABLED=false
export SCCACHE_CACHE_SIZE="${{SCCACHE_CACHE_SIZE:-20G}}"
if [ -n "${{GITHUB_ENV:-}}" ]; then
  echo "SCCACHE_CACHE_SIZE=$SCCACHE_CACHE_SIZE" >> "$GITHUB_ENV"
fi
# Best-effort: cargo will auto-start the server on first use anyway.
sccache --start-server 2>/dev/null || true
"#,
        ensure = ensure_installed_snippet(),
        ver = LOCKED_VERSION,
    )
}

/// Post step: show stats then stop the server. Soft-fails when not running.
pub(crate) fn post_script() -> &'static str {
    "stats=$(sccache --show-stats 2>&1 || true); printf '%s\\n' \"$stats\"; if [ -n \"${GITHUB_STEP_SUMMARY:-}\" ]; then printf '## sccache statistics\\n```text\\n%s\\n```\\n' \"$stats\" >> \"$GITHUB_STEP_SUMMARY\"; fi; sccache --stop-server 2>/dev/null || true"
}

/// Guest-lane equivalent of [`setup_script`]: ensure the binary, then start
/// the server. The guest has no host-persistent store behind it.
pub(crate) fn guest_script() -> String {
    format!(
        "set -eu\n{}\nsccache --start-server; printf 'sccache: native guest adapter started\\n'\n",
        ensure_installed_snippet()
    )
}

/// Provisioning prelude shared by [`setup_script`] and [`guest_script`]: reuse
/// a PATH sccache at [`LOCKED_VERSION`], else fetch the pinned musl release,
/// verify its checksum, and install it where later steps find it.
fn ensure_installed_snippet() -> String {
    format!(
        r#"# Explicit sccache mode: the default job image does not ship sccache,
# so provision the pinned release when PATH lacks it at the locked version.
SCCACHE_WANT='{ver}'
if ! command -v sccache >/dev/null 2>&1 || ! sccache --version 2>/dev/null | grep -F "sccache $SCCACHE_WANT"; then
  case "$(uname -m)" in
    x86_64) SCCACHE_ASSET="sccache-v$SCCACHE_WANT-x86_64-unknown-linux-musl.tar.gz"; SCCACHE_SHA="{sha_x64}";;
    aarch64|arm64) SCCACHE_ASSET="sccache-v$SCCACHE_WANT-aarch64-unknown-linux-musl.tar.gz"; SCCACHE_SHA="{sha_arm64}";;
    *) echo "sccache $SCCACHE_WANT has no pinned build for $(uname -m)" >&2; exit 1;;
  esac
  SCCACHE_TMP="$(mktemp -d)/sccache.tgz"
  curl -fsSL --retry 3 "https://github.com/mozilla/sccache/releases/download/v$SCCACHE_WANT/$SCCACHE_ASSET" -o "$SCCACHE_TMP"
  echo "$SCCACHE_SHA  $SCCACHE_TMP" | sha256sum -c -
  SCCACHE_UNPACK="$(mktemp -d)"
  tar -xzf "$SCCACHE_TMP" -C "$SCCACHE_UNPACK"
  SCCACHE_BIN_DIR=/usr/local/bin
  if ! touch "$SCCACHE_BIN_DIR/.velnor-write-test" 2>/dev/null; then
    SCCACHE_BIN_DIR="$HOME/.local/bin"
    mkdir -p "$SCCACHE_BIN_DIR"
  fi
  rm -f "$SCCACHE_BIN_DIR/.velnor-write-test"
  install -m0755 "$SCCACHE_UNPACK"/sccache-*/sccache "$SCCACHE_BIN_DIR/sccache"
  rm -rf "$SCCACHE_TMP" "$SCCACHE_UNPACK"
  export PATH="$SCCACHE_BIN_DIR:$PATH"
  if [ -n "${{GITHUB_PATH:-}}" ]; then echo "$SCCACHE_BIN_DIR" >> "$GITHUB_PATH"; fi
  echo "{marker}$SCCACHE_BIN_DIR"
  sccache --version | grep -F "sccache $SCCACHE_WANT"
  echo "sccache $SCCACHE_WANT (pinned release) installed into $SCCACHE_BIN_DIR"
fi"#,
        ver = LOCKED_VERSION,
        sha_x64 = LINUX_X64_SHA256,
        sha_arm64 = LINUX_ARM64_SHA256,
        marker = PATH_MARKER,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job_with_steps(steps: serde_json::Value) -> AgentJobRequestMessage {
        serde_json::from_value(serde_json::json!({
            "messageType": "RunnerJobRequest",
            "plan": { "planId": "plan" },
            "timeline": { "id": "timeline" },
            "jobId": "job",
            "jobDisplayName": "compat test",
            "requestId": 1,
            "variables": {
                "github.repository_id": { "value": "42" }
            },
            "steps": steps,
        }))
        .unwrap()
    }

    fn sccache_step(enabled: bool) -> serde_json::Value {
        serde_json::json!({
            "type": "Action",
            "displayName": "sccache",
            "enabled": enabled,
            "reference": {
                "type": "Repository",
                "name": ACTION_REPOSITORY,
                "ref": "fc920bf0ec8de6ee65d409111f7ec508035751ba"
            }
        })
    }

    #[test]
    fn explicit_decision_matches_enabled_sccache_action_only() {
        let plain = job_with_steps(serde_json::json!([{
            "type": "Action",
            "displayName": "checkout",
            "reference": { "type": "Repository", "name": "actions/checkout", "ref": "v7" }
        }]));
        assert!(!is_explicit(&plain));

        let disabled = job_with_steps(serde_json::json!([sccache_step(false)]));
        assert!(!is_explicit(&disabled));

        let explicit = job_with_steps(serde_json::json!([sccache_step(true)]));
        assert!(is_explicit(&explicit));
    }

    #[test]
    fn store_host_is_repo_namespaced_and_slot_shared() {
        let temp = Path::new("/var/lib/velnor/work/slot-1/job/temp");
        let job = job_with_steps(serde_json::json!([]));
        assert_eq!(
            store_host(&job, temp, "trusted"),
            PathBuf::from("/var/lib/velnor/work/_velnor_sccache/trusted/42")
        );
        // A sibling slot resolves the same daemon-shared store.
        let slot2 = Path::new("/var/lib/velnor/work/slot-2/job/temp");
        assert_eq!(
            store_host(&job, slot2, "trusted"),
            store_host(&job, temp, "trusted")
        );
    }

    #[test]
    fn setup_script_provisions_the_pinned_release() {
        let script = setup_script();
        assert!(script.contains("SCCACHE_WANT='0.16.0'"));
        assert!(script.contains(LINUX_X64_SHA256));
        assert!(script.contains(LINUX_ARM64_SHA256));
        assert!(script.contains("mozilla/sccache/releases/download"));
        assert!(script.contains("sha256sum -c -"));
        assert!(script.contains("install -m0755"));
        assert!(script.contains(PATH_MARKER));
        assert!(script.contains("sccache --start-server"));
        assert!(script.contains("RUSTC_WRAPPER=sccache"));
        assert!(!script.contains("must be preinstalled"));
    }

    #[test]
    fn post_script_shows_stats_and_stops_server() {
        assert!(post_script().contains("sccache --show-stats"));
        assert!(post_script().contains("sccache --stop-server"));
    }

    #[test]
    fn guest_script_ensures_binary_and_starts_server() {
        let script = guest_script();
        assert!(script.contains("mozilla/sccache/releases/download"));
        assert!(script.contains("sccache --start-server"));
        assert!(script.contains("native guest adapter started"));
    }

    #[test]
    fn generated_scripts_are_valid_shell() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-sccache-compat-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, script) in [
            ("setup.sh", setup_script()),
            ("post.sh", post_script().to_string()),
            ("guest.sh", guest_script()),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, script).unwrap();
            let status = std::process::Command::new("sh")
                .arg("-n")
                .arg(&path)
                .status()
                .unwrap();
            assert!(status.success(), "{name} must parse under sh -n");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
