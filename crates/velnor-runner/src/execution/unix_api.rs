//! Firecracker HTTP API over a Unix socket (official v1.16.1 paths).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::firecracker::FirecrackerApi;
use super::FIRECRACKER_VERSION;

/// Live Firecracker client. Tests inject [`super::RecordingFirecracker`] instead.
pub struct UnixFirecrackerClient {
    socket: PathBuf,
}

impl UnixFirecrackerClient {
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    fn put_json(&self, path: &str, body: &str) -> Result<(), String> {
        unix_json(&self.socket, "PUT", path, body)
    }

    fn patch_json(&self, path: &str, body: &str) -> Result<(), String> {
        unix_json(&self.socket, "PATCH", path, body)
    }
}

impl FirecrackerApi for UnixFirecrackerClient {
    fn put_boot_source(&mut self, kernel: &Path) -> Result<(), String> {
        let body = format!(
            r#"{{"kernel_image_path":"{}","boot_args":"console=ttyS0 reboot=k panic=1 pci=off"}}"#,
            json_escape(&kernel.display().to_string())
        );
        self.put_json("/boot-source", &body)
    }

    fn put_drive(&mut self, drive_id: &str, path: &Path, read_only: bool) -> Result<(), String> {
        let body = format!(
            r#"{{"drive_id":"{drive_id}","path_on_host":"{}","is_root_device":{},"is_read_only":{read_only}}}"#,
            json_escape(&path.display().to_string()),
            drive_id == "rootfs"
        );
        self.put_json(&format!("/drives/{drive_id}"), &body)
    }

    fn put_network_interface(&mut self, iface_id: &str, tap: &str) -> Result<(), String> {
        let body = format!(
            r#"{{"iface_id":"{iface_id}","host_dev_name":"{}"}}"#,
            json_escape(tap)
        );
        self.put_json(&format!("/network-interfaces/{iface_id}"), &body)
    }

    fn put_vsock(&mut self, guest_cid: u32, uds: &Path) -> Result<(), String> {
        let body = format!(
            r#"{{"vsock_id":"1","guest_cid":{guest_cid},"uds_path":"{}"}}"#,
            json_escape(&uds.display().to_string())
        );
        self.put_json("/vsock", &body)
    }

    fn instance_start(&mut self) -> Result<(), String> {
        self.put_json("/actions", r#"{"action_type":"InstanceStart"}"#)
    }

    fn pause_vm(&mut self) -> Result<(), String> {
        self.patch_json("/vm", r#"{"state":"Paused"}"#)
    }

    fn resume_vm(&mut self) -> Result<(), String> {
        self.patch_json("/vm", r#"{"state":"Resumed"}"#)
    }

    fn create_snapshot(&mut self, mem: &Path, vmstate: &Path) -> Result<(), String> {
        let body = format!(
            r#"{{"mem_file_path":"{}","snapshot_path":"{}","snapshot_type":"Full"}}"#,
            json_escape(&mem.display().to_string()),
            json_escape(&vmstate.display().to_string())
        );
        self.put_json("/snapshot/create", &body)
    }

    fn load_snapshot(
        &mut self,
        mem: &Path,
        vmstate: &Path,
        expected_version: &str,
    ) -> Result<(), String> {
        if expected_version != FIRECRACKER_VERSION {
            return Err(format!(
                "snapshot version {expected_version} mismatch (pinned {FIRECRACKER_VERSION}); cold boot required"
            ));
        }
        let body = format!(
            r#"{{"snapshot_path":"{}","mem_backend":{{"backend_type":"File","backend_path":"{}"}},"resume_vm":true}}"#,
            json_escape(&vmstate.display().to_string()),
            json_escape(&mem.display().to_string())
        );
        self.put_json("/snapshot/load", &body)
    }
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn unix_json(socket: &Path, method: &str, path: &str, body: &str) -> Result<(), String> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|error| format!("connect {}: {error}", socket.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| format!("write timeout: {error}"))?;
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("write {path}: {error}"))?;
    let mut buf = vec![0_u8; 8192];
    let n = stream
        .read(&mut buf)
        .map_err(|error| format!("read {path}: {error}"))?;
    let response = std::str::from_utf8(&buf[..n])
        .map_err(|_| "firecracker response is not UTF-8".to_string())?;
    parse_http_success(method, path, response)
}

fn parse_http_success(method: &str, path: &str, response: &str) -> Result<(), String> {
    let status_line = response.lines().next().unwrap_or("");
    if status_line.contains(" 204 ") || status_line.ends_with(" 204") {
        return Ok(());
    }
    if status_line.contains(" 200 ") {
        return Ok(());
    }
    Err(format!(
        "firecracker {method} {path} failed: {status_line}; the docker backend was not used"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

    #[test]
    fn unix_client_puts_official_boot_source_path() {
        let dir = std::env::temp_dir().join(format!(
            "fc-api-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("fc.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            req
        });
        let mut client = UnixFirecrackerClient::new(sock);
        client
            .put_boot_source(Path::new("/usr/share/velnor/microvm/vmlinux"))
            .unwrap();
        let req = server.join().unwrap();
        assert!(req.starts_with("PUT /boot-source HTTP/1.1"), "{req}");
        assert!(req.contains("kernel_image_path"), "{req}");
        assert!(!req.contains("virtio-fs"), "{req}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn snapshot_load_refuses_foreign_version_before_socket() {
        let mut client = UnixFirecrackerClient::new(PathBuf::from("/no/such.sock"));
        let err = client
            .load_snapshot(Path::new("/m"), Path::new("/s"), "0.0.0")
            .unwrap_err();
        assert!(err.contains("cold boot required"), "{err}");
    }

    #[test]
    fn unix_client_patches_vm_pause() {
        let dir = std::env::temp_dir().join(format!(
            "fc-pause-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("fc.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = vec![0_u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
            req
        });
        let mut client = UnixFirecrackerClient::new(sock);
        client.pause_vm().unwrap();
        let req = server.join().unwrap();
        assert!(req.starts_with("PATCH /vm HTTP/1.1"), "{req}");
        assert!(req.contains("Paused"), "{req}");
        std::fs::remove_dir_all(dir).ok();
    }
}
