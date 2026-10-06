//! Resolve the local control-plane socket directory.
//!
//! Package units set `VELNOR_STORAGE_ROOT=/var` and own `/run/velnor`. Every
//! other process — an on-demand `velnorctl host start`, the slot and job
//! children it spawns, and the `velnorctl --instance <name>` that inspects it —
//! resolves the same user-owned storage prefix and its `run/velnor` beneath,
//! so no two `velnorctl` surfaces can disagree about where a host's sockets
//! are. World-writable temp paths are not used: socket-parent inspection
//! rejects them, and they would make macOS daemon startup fail closed.

use std::io;
use std::path::{Path, PathBuf};

/// Whether the process is using the packaged Linux socket layout.
#[must_use]
pub fn is_package_socket_mode() -> bool {
    storage_root_prefix().is_some_and(|prefix| prefix == Path::new("/var"))
}

/// Root directory for `<instance>/control.sock` and `<instance>/admin.sock`,
/// for this process's own storage prefix ([`storage_root_prefix`]).
#[must_use]
pub fn socket_root() -> PathBuf {
    socket_root_for_storage_root(storage_root_prefix().as_deref())
}

/// Socket root for a daemon running under `storage_root` (its
/// `VELNOR_STORAGE_ROOT`): the storage layout's runtime root, `/run/velnor`
/// for the packaged `/var`. `None` is a daemon with no storage root at all,
/// which only a `HOME`-less process can be; its sockets live under the
/// temp-directory fallback of [`default_user_storage_root`].
#[must_use]
pub fn socket_root_for_storage_root(storage_root: Option<&Path>) -> PathBuf {
    match storage_root {
        Some(prefix) if prefix == Path::new("/var") => PathBuf::from("/run/velnor"),
        Some(prefix) => prefix.join("run/velnor"),
        None => default_user_storage_root().join("run/velnor"),
    }
}

/// Create the user-mode socket root with owner-only-writable parents.
///
/// Package mode never creates `/run/velnor`; systemd tmpfiles owns that path.
pub fn ensure_socket_root() -> io::Result<PathBuf> {
    let root = socket_root();
    if is_package_socket_mode() {
        return Ok(root);
    }
    std::fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(root)
}

/// This process's storage prefix: `VELNOR_STORAGE_ROOT` when set, otherwise
/// [`default_user_storage_root`]. `velnorctl host start` exports the same
/// value to its children, so a shell that never set the variable still
/// addresses the host it started.
#[must_use]
pub fn storage_root_prefix() -> Option<PathBuf> {
    std::env::var_os("VELNOR_STORAGE_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(default_user_storage_root()))
}

/// The user-owned storage prefix used when `VELNOR_STORAGE_ROOT` is unset:
/// `~/.velnor-store` on macOS and `~/.local/state/velnor` elsewhere.
///
/// The macOS path is a short dot-directory on purpose: Unix socket paths are
/// limited to 104 bytes there, and
/// `~/Library/Application Support/velnor/run/velnor/<name>/control.sock`
/// overflows that limit. Without `HOME` the prefix is a `velnor` directory
/// under the temp directory; socket-parent inspection rejects that, so a
/// `HOME`-less host fails closed rather than serving from a shared path.
#[must_use]
pub fn default_user_storage_root() -> PathBuf {
    match std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        Some(home) => {
            let home = PathBuf::from(home);
            if cfg!(target_os = "macos") {
                home.join(".velnor-store")
            } else {
                home.join(".local/state/velnor")
            }
        }
        None => std::env::temp_dir().join("velnor"),
    }
}
