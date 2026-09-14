//! Resolve the local control-plane socket directory.
//!
//! Package units set `VELNOR_STORAGE_ROOT=/var` and own `/run/velnor`. Dev and
//! user-mode hosts use the XDG runtime directory, falling back to a temp dir on
//! macOS and other platforms without `/run/velnor`.

use std::path::{Path, PathBuf};

/// Whether the process is using the packaged Linux socket layout.
#[must_use]
pub fn is_package_socket_mode() -> bool {
    storage_root_prefix().is_some_and(|prefix| prefix == Path::new("/var"))
}

/// Root directory for `<instance>/control.sock` and `<instance>/admin.sock`.
#[must_use]
pub fn socket_root() -> PathBuf {
    if let Some(prefix) = storage_root_prefix() {
        if prefix == Path::new("/var") {
            PathBuf::from("/run/velnor")
        } else {
            prefix.join("run/velnor")
        }
    } else {
        user_runtime_dir().join("velnor")
    }
}

fn storage_root_prefix() -> Option<PathBuf> {
    std::env::var_os("VELNOR_STORAGE_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn user_runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("velnor-run"))
}
