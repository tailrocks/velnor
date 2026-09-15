#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests may panic"
)]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use velnor_client::{is_package_socket_mode, socket_root, UnixEndpoint};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_storage_root<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
    let _guard: MutexGuard<'_, ()> = ENV_LOCK.lock().expect("env lock");
    let previous = std::env::var_os("VELNOR_STORAGE_ROOT");
    match value {
        Some(value) => unsafe { std::env::set_var("VELNOR_STORAGE_ROOT", value) },
        None => unsafe { std::env::remove_var("VELNOR_STORAGE_ROOT") },
    }
    let result = f();
    match previous {
        Some(value) => unsafe { std::env::set_var("VELNOR_STORAGE_ROOT", value) },
        None => unsafe { std::env::remove_var("VELNOR_STORAGE_ROOT") },
    }
    result
}

#[test]
fn package_mode_uses_run_velnor() {
    with_storage_root(Some("/var"), || {
        assert!(is_package_socket_mode());
        assert_eq!(socket_root(), PathBuf::from("/run/velnor"));
        let endpoint = UnixEndpoint::from_instance("primary").expect("valid endpoint");
        assert_eq!(endpoint.uri(), "unix:///run/velnor/primary".to_owned());
    });
}

#[test]
fn dev_mode_uses_user_runtime_dir() {
    with_storage_root(None, || {
        assert!(!is_package_socket_mode());
        let root = socket_root();
        assert_ne!(root, PathBuf::from("/run/velnor"));
        assert!(root.ends_with("velnor"));
        let endpoint = UnixEndpoint::from_instance("default").expect("valid endpoint");
        assert_eq!(
            endpoint.uri(),
            format!("unix://{}", root.join("default").display())
        );
        assert!(UnixEndpoint::parse(&endpoint.uri()).is_ok());
    });
}

#[test]
fn parser_rejects_traversal_and_invalid_instance_names_in_package_mode() {
    with_storage_root(Some("/var"), || {
        for uri in [
            "unix:///run/velnor/../other",
            "unix:///run/velnor/Upper",
            "unix:///run/velnor/a/b",
        ] {
            assert!(UnixEndpoint::parse(uri).is_err(), "accepted {uri}");
        }
    });
}
