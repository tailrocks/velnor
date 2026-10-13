//! Resolve the local control-plane socket directory.
//!
//! Package units set `VELNOR_STORAGE_ROOT=/var` and own `/run/velnor`. Dev and
//! user-mode hosts use `XDG_RUNTIME_DIR` when set, otherwise a user-owned state
//! directory. World-writable temp paths are not used: socket-parent inspection
//! rejects them, and they would make macOS daemon startup fail closed.

use std::io;
use std::path::{Path, PathBuf};

/// Whether the process is using the packaged Linux socket layout.
#[must_use]
pub fn is_package_socket_mode() -> bool {
    storage_root_prefix().is_some_and(|prefix| prefix == Path::new("/var"))
}

/// Root directory for `<instance>/control.sock` and `<instance>/admin.sock`,
/// for this process's own `VELNOR_STORAGE_ROOT`.
#[must_use]
pub fn socket_root() -> PathBuf {
    socket_root_for_storage_root(storage_root_prefix().as_deref())
}

/// Socket root for a daemon running under `storage_root` (its
/// `VELNOR_STORAGE_ROOT`): the storage layout's runtime root, `/run/velnor`
/// for the packaged `/var`. `None` is the user-mode runtime directory.
#[must_use]
pub fn socket_root_for_storage_root(storage_root: Option<&Path>) -> PathBuf {
    match storage_root {
        Some(prefix) if prefix == Path::new("/var") => PathBuf::from("/run/velnor"),
        Some(prefix) => prefix.join("run/velnor"),
        None => user_runtime_dir().join("velnor"),
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

fn storage_root_prefix() -> Option<PathBuf> {
    std::env::var_os("VELNOR_STORAGE_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn user_runtime_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
        return PathBuf::from(xdg);
    }
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        let home = PathBuf::from(home);
        if cfg!(target_os = "macos") {
            return home.join("Library/Application Support");
        }
        return home.join(".local").join("state");
    }
    std::env::temp_dir().join("velnor-run")
}
