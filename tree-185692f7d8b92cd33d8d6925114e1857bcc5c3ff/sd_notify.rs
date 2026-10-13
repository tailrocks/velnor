//! Minimal systemd `sd_notify(3)` client.
//!
//! Speaks the readiness/watchdog datagram protocol over `$NOTIFY_SOCKET` so the
//! packaged unit can use `Type=notify` + `WatchdogSec=`. Every call is a no-op
//! when the daemon is not running under systemd (no socket in the
//! environment), so library code can call these unconditionally.

use std::env;
use std::os::unix::net::UnixDatagram;

/// Send one `sd_notify` state string (e.g. `READY=1`, `WATCHDOG=1`,
/// `STATUS=...`). Best-effort: failures are ignored because notification must
/// never take the daemon down.
pub fn notify(state: &str) {
    let Some(socket_path) = env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    let Some(path_str) = socket_path.to_str() else {
        return;
    };
    // Abstract-namespace sockets are addressed with a leading NUL instead of '@'.
    let path = if let Some(rest) = path_str.strip_prefix('@') {
        format!("\0{rest}")
    } else {
        path_str.to_string()
    };
    let Ok(socket) = UnixDatagram::unbound() else {
        return;
    };
    let _ = socket.send_to(state.as_bytes(), path);
}

pub fn ready() {
    notify("READY=1");
}

pub fn watchdog_ping() {
    notify("WATCHDOG=1");
}

pub fn status(message: &str) {
    // STATUS values are single-line per the protocol.
    let single_line = message.replace('\n', " ");
    notify(&format!("STATUS={single_line}"));
}

/// Interval at which the daemon should ping the watchdog, derived from
/// `WATCHDOG_USEC` (half the configured timeout, as systemd recommends).
/// `None` when no watchdog is armed.
pub fn watchdog_interval() -> Option<std::time::Duration> {
    let usec: u64 = env::var("WATCHDOG_USEC").ok()?.parse().ok()?;
    if usec == 0 {
        return None;
    }
    Some(std::time::Duration::from_micros(usec / 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_without_socket_is_noop() {
        // Must not panic or error when NOTIFY_SOCKET is unset.
        notify("READY=1");
        ready();
        watchdog_ping();
        status("multi\nline");
    }

    #[test]
    fn watchdog_interval_requires_env() {
        if env::var_os("WATCHDOG_USEC").is_none() {
            assert!(watchdog_interval().is_none());
        }
    }
}
