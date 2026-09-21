//! macOS sleep-prevention power assertion guard.
//!
//! On macOS, idle sleep suspends the Darwin kernel and pauses the OrbStack/Docker
//! VM, causing running jobs to drop heartbeats and fail. Velnor holds an awake
//! power assertion (`kIOPMAssertionTypePreventUserIdleSystemSleep`) or an owned
//! `caffeinate -w <pid> -s -i` guard while a job container is actively executing.
//!
//! On Linux (`cfg(not(target_os = "macos"))`), this struct is a no-op.

/// The kind of power assertion currently held by [`PowerAssertionGuard`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAssertionKind {
    /// Native macOS IOKit power assertion (`kIOPMAssertionTypePreventUserIdleSystemSleep`).
    Iokit(u32),
    /// Subprocess-backed `caffeinate -w <pid> -s -i` guard.
    Caffeinate(u32),
    /// No assertion held (non-macOS or failed/disabled).
    None,
}

/// RAII sleep-prevention guard.
///
/// On macOS:
/// - Attempts to acquire an in-process IOKit assertion (`kIOPMAssertionTypePreventUserIdleSystemSleep`).
/// - If native IOKit assertion creation fails, falls back to spawning `caffeinate -w <pid> -s -i`.
/// - Releases the assertion or terminates the `caffeinate` child process on [`Drop`].
///
/// On non-macOS platforms:
/// - Compiles to a zero-sized, no-op struct.
pub struct PowerAssertionGuard {
    #[cfg(target_os = "macos")]
    inner: InnerAssertion,
}

#[cfg(target_os = "macos")]
enum InnerAssertion {
    Iokit(u32),
    Caffeinate(std::process::Child),
    None,
}

impl PowerAssertionGuard {
    /// Acquire a sleep-prevention power assertion for the duration of this guard.
    ///
    /// On macOS, first attempts native IOKit assertion creation; falls back to
    /// `caffeinate -w <pid> -s -i` on failure.
    /// On Linux, returns a no-op guard.
    #[must_use]
    pub fn acquire(name: &str) -> Self {
        #[cfg(target_os = "macos")]
        {
            if let Some(assertion_id) = acquire_iokit(name) {
                return Self {
                    inner: InnerAssertion::Iokit(assertion_id),
                };
            }
            tracing::warn!(
                assertion_name = name,
                "IOKit power assertion failed; falling back to caffeinate guard"
            );
            if let Some(child) = spawn_caffeinate_process(name) {
                return Self {
                    inner: InnerAssertion::Caffeinate(child),
                };
            }
            tracing::error!(
                assertion_name = name,
                "failed to acquire both IOKit and caffeinate power assertions"
            );
            Self {
                inner: InnerAssertion::None,
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = name;
            Self
        }
    }

    /// Spawns an owned `caffeinate -w <pid> -s -i` guard process tied to the current PID.
    ///
    /// On Linux, returns a no-op guard.
    #[must_use]
    pub fn spawn_caffeinate(name: &str) -> Self {
        #[cfg(target_os = "macos")]
        {
            if let Some(child) = spawn_caffeinate_process(name) {
                Self {
                    inner: InnerAssertion::Caffeinate(child),
                }
            } else {
                Self {
                    inner: InnerAssertion::None,
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = name;
            Self
        }
    }

    /// Query the current backing mode of the guard.
    #[must_use]
    pub fn kind(&self) -> PowerAssertionKind {
        #[cfg(target_os = "macos")]
        match &self.inner {
            InnerAssertion::Iokit(id) => PowerAssertionKind::Iokit(*id),
            InnerAssertion::Caffeinate(child) => PowerAssertionKind::Caffeinate(child.id()),
            InnerAssertion::None => PowerAssertionKind::None,
        }
        #[cfg(not(target_os = "macos"))]
        PowerAssertionKind::None
    }

    /// Returns `true` if an assertion or caffeinate child is actively held.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.kind() != PowerAssertionKind::None
    }

    /// Returns the IOKit assertion ID if backed by native IOKit.
    #[must_use]
    pub fn assertion_id(&self) -> Option<u32> {
        match self.kind() {
            PowerAssertionKind::Iokit(id) => Some(id),
            _ => None,
        }
    }
}

impl Default for PowerAssertionGuard {
    fn default() -> Self {
        Self::acquire("velnor-default-job")
    }
}

impl std::fmt::Debug for PowerAssertionGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PowerAssertionGuard")
            .field("kind", &self.kind())
            .field("is_active", &self.is_active())
            .finish()
    }
}

impl Drop for PowerAssertionGuard {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        match &mut self.inner {
            InnerAssertion::Iokit(id) => {
                // SAFETY: `*id` was created by `IOPMAssertionCreateWithName` and is released once here on drop.
                let ret = unsafe { IOPMAssertionRelease(*id) };
                if ret == K_IO_RETURN_SUCCESS {
                    tracing::debug!(assertion_id = *id, "released macOS IOKit power assertion");
                } else {
                    tracing::warn!(
                        assertion_id = *id,
                        error_code = ret,
                        "failed to release macOS IOKit power assertion"
                    );
                }
            }
            InnerAssertion::Caffeinate(child) => {
                let child_pid = child.id();
                let _ = child.kill();
                let _ = child.wait();
                tracing::debug!(
                    caffeinate_pid = child_pid,
                    "killed and reaped caffeinate power guard process"
                );
            }
            InnerAssertion::None => {}
        }
    }
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        alloc: *const std::ffi::c_void,
        c_str: *const std::ffi::c_char,
        encoding: u32,
    ) -> *const std::ffi::c_void;

    fn CFRelease(cf: *const std::ffi::c_void);
}

#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: *const std::ffi::c_void,
        assertion_level: u32,
        assertion_name: *const std::ffi::c_void,
        assertion_id: *mut u32,
    ) -> i32;

    fn IOPMAssertionRelease(assertion_id: u32) -> i32;
}

#[cfg(target_os = "macos")]
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
#[cfg(target_os = "macos")]
const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;
#[cfg(target_os = "macos")]
const K_IO_RETURN_SUCCESS: i32 = 0;
#[cfg(target_os = "macos")]
const ASSERTION_TYPE: &[u8] = b"PreventUserIdleSystemSleep\0";

#[cfg(target_os = "macos")]
fn acquire_iokit(name: &str) -> Option<u32> {
    let sanitized_name = name.replace('\0', "_");
    let c_name = std::ffi::CString::new(sanitized_name).ok()?;

    // SAFETY: FFI calls to CoreFoundation and IOKit framework functions.
    // Pointers are verified for null before passing to `IOPMAssertionCreateWithName`,
    // and created CFStringRefs are balanced with `CFRelease`.
    unsafe {
        let cf_type = CFStringCreateWithCString(
            std::ptr::null(),
            ASSERTION_TYPE.as_ptr().cast(),
            K_CF_STRING_ENCODING_UTF8,
        );
        if cf_type.is_null() {
            return None;
        }

        let cf_name =
            CFStringCreateWithCString(std::ptr::null(), c_name.as_ptr(), K_CF_STRING_ENCODING_UTF8);
        if cf_name.is_null() {
            CFRelease(cf_type);
            return None;
        }

        let mut assertion_id: u32 = 0;
        let ret = IOPMAssertionCreateWithName(
            cf_type,
            K_IOPM_ASSERTION_LEVEL_ON,
            cf_name,
            &mut assertion_id,
        );

        CFRelease(cf_type);
        CFRelease(cf_name);

        if ret == K_IO_RETURN_SUCCESS {
            tracing::info!(
                assertion_id,
                assertion_name = name,
                "created macOS IOKit PreventUserIdleSystemSleep power assertion"
            );
            Some(assertion_id)
        } else {
            tracing::warn!(
                error_code = ret,
                assertion_name = name,
                "IOPMAssertionCreateWithName returned non-zero status"
            );
            None
        }
    }
}

#[cfg(target_os = "macos")]
fn spawn_caffeinate_process(name: &str) -> Option<std::process::Child> {
    let pid = std::process::id().to_string();
    match std::process::Command::new("caffeinate")
        .args(["-w", &pid, "-s", "-i"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            tracing::info!(
                caffeinate_pid = child.id(),
                target_pid = %pid,
                guard_name = name,
                "spawned caffeinate sleep prevention guard tied to process"
            );
            Some(child)
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                guard_name = name,
                "failed to spawn caffeinate fallback guard"
            );
            None
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    reason = "tests may panic"
)]
mod tests {
    use super::*;

    #[test]
    fn test_power_assertion_acquire_and_drop() {
        let guard = PowerAssertionGuard::acquire("velnor-test-acquire");
        #[cfg(target_os = "macos")]
        {
            assert!(guard.is_active());
            match guard.kind() {
                PowerAssertionKind::Iokit(id) => {
                    assert!(id > 0);
                    assert_eq!(guard.assertion_id(), Some(id));
                }
                PowerAssertionKind::Caffeinate(pid) => {
                    assert!(pid > 0);
                }
                PowerAssertionKind::None => {
                    panic!("expected active assertion on macOS");
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
    fn test_caffeinate_spawn_and_drop() {
        let guard = PowerAssertionGuard::spawn_caffeinate("velnor-test-caffeinate");
        #[cfg(target_os = "macos")]
        {
            assert!(guard.is_active());
            match guard.kind() {
                PowerAssertionKind::Caffeinate(pid) => {
                    assert!(pid > 0);
                }
                other => panic!("expected Caffeinate kind, got {other:?}"),
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
    fn test_multiple_concurrent_assertions() {
        let guard1 = PowerAssertionGuard::acquire("velnor-test-concurrent-1");
        let guard2 = PowerAssertionGuard::acquire("velnor-test-concurrent-2");
        #[cfg(target_os = "macos")]
        {
            assert!(guard1.is_active());
            assert!(guard2.is_active());
            if let (Some(id1), Some(id2)) = (guard1.assertion_id(), guard2.assertion_id()) {
                assert_ne!(id1, id2, "concurrent assertions should have distinct IDs");
            }
        }
        drop(guard1);
        #[cfg(target_os = "macos")]
        {
            assert!(guard2.is_active());
        }
        drop(guard2);
    }

    #[test]
    fn test_name_with_null_bytes_sanitization() {
        let guard = PowerAssertionGuard::acquire("velnor\0test\0null");
        #[cfg(target_os = "macos")]
        {
            assert!(guard.is_active());
        }
        drop(guard);
    }
}
