#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
//! Tests for macOS sleep-prevention power assertions (`PowerAssertionGuard`).

use velnor_runner::platform::{PowerAssertionGuard, PowerAssertionKind};

#[test]
fn power_assertion_guard_acquire_and_drop() {
    let guard = PowerAssertionGuard::acquire("velnor-test-guard-acquire");
    #[cfg(target_os = "macos")]
    {
        assert!(guard.is_active());
        match guard.kind() {
            PowerAssertionKind::Iokit(id) => {
                assert!(id > 0);
                assert_eq!(guard.assertion_id(), Some(id));

                // Verify assertion is visible in pmset output while held
                let output = std::process::Command::new("pmset")
                    .args(["-g", "assertions"])
                    .output()
                    .unwrap();
                let text = String::from_utf8_lossy(&output.stdout);
                assert!(
                    text.contains("velnor-test-guard-acquire"),
                    "pmset assertions should include velnor-test-guard-acquire"
                );
            }
            PowerAssertionKind::Caffeinate(pid) => {
                assert!(pid > 0);
            }
            PowerAssertionKind::None => {
                panic!("expected active power assertion on macOS");
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        assert!(!guard.is_active());
        assert_eq!(guard.kind(), PowerAssertionKind::None);
    }
    drop(guard);
}

#[test]
fn power_assertion_guard_caffeinate_fallback() {
    let guard = PowerAssertionGuard::spawn_caffeinate("velnor-test-caffeinate-direct");
    #[cfg(target_os = "macos")]
    {
        assert!(guard.is_active());
        match guard.kind() {
            PowerAssertionKind::Caffeinate(pid) => {
                assert!(pid > 0);
                // Verify caffeinate process exists
                let kill_check = unsafe { libc::kill(pid as i32, 0) };
                assert_eq!(kill_check, 0, "caffeinate child process should be alive");
            }
            other => panic!("expected Caffeinate kind, got {other:?}"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        assert!(!guard.is_active());
    }
    drop(guard);
}

#[test]
fn power_assertion_guard_concurrent() {
    let g1 = PowerAssertionGuard::acquire("velnor-test-concurrent-1");
    let g2 = PowerAssertionGuard::acquire("velnor-test-concurrent-2");
    #[cfg(target_os = "macos")]
    {
        assert!(g1.is_active());
        assert!(g2.is_active());
        if let (Some(id1), Some(id2)) = (g1.assertion_id(), g2.assertion_id()) {
            assert_ne!(id1, id2, "distinct assertions must have distinct IDs");
        }
    }
    drop(g1);
    #[cfg(target_os = "macos")]
    {
        assert!(g2.is_active());
    }
    drop(g2);
}
