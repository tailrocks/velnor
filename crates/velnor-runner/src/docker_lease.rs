//! Job-scoped ownership of host Docker objects.
//!
//! Trusted jobs used to receive `/var/run/docker.sock` directly. Guest tools
//! (Testcontainers, compose, `docker run`) then created unlabeled host objects
//! that Velnor teardown never named, so they outlived the job on a persistent
//! host. The lease is the missing ownership boundary: the job talks to a
//! per-job proxy that injects `velnor.job-id` / `velnor.daemon-id` on every
//! create, and every terminal path deletes by that label.

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

pub const JOB_ID_LABEL: &str = "velnor.job-id";
pub const DAEMON_ID_LABEL: &str = "velnor.daemon-id";
pub const TESTCONTAINERS_LABEL: &str = "org.testcontainers.managed-by=testcontainers";
pub const HOST_DOCKER_SOCKET: &str = "/var/run/docker.sock";
/// Host-visible runtime dir. systemd `PrivateTmp=yes` remaps daemon `/tmp`, so
/// a lease socket there is invisible to host dockerd and the guest bind-mount
/// of `/tmp/vdl-*.sock` is not the proxy.
pub const LEASE_SOCKET_DIR: &str = "/run/velnor";
pub const JOB_IMAGE_ANCESTOR: &str = "velnor/job-ubuntu";

const MAX_PROXY_BODY: usize = 32 * 1024 * 1024;

pub fn guest_docker_socket_host(job_id: &str, unique: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(job_id.as_bytes());
    hasher.update(unique.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let mut short = [0_u8; 8];
    short.copy_from_slice(&digest[..8]);
    lease_socket_dir().join(format!("vdl-{:016x}.sock", u64::from_be_bytes(short)))
}

/// Prefer systemd `RuntimeDirectory` (`/run/velnor`): host-visible, not
/// remapped by `PrivateTmp`. Tests and unprivileged checkouts cannot create
/// that dir; they fall back to `$TMPDIR/velnor-lease`, still outside the
/// daemon's private `/tmp/vdl-*` path that dockerd never sees.
fn lease_socket_dir() -> PathBuf {
    let runtime = PathBuf::from(LEASE_SOCKET_DIR);
    if std::fs::create_dir_all(&runtime).is_ok() {
        return runtime;
    }
    let fallback = std::env::temp_dir().join("velnor-lease");
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

pub fn list_owned_containers_args(job_id: &str) -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--quiet".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
    ]
}

pub fn list_owned_networks_args(job_id: &str) -> Vec<String> {
    vec![
        "network".into(),
        "ls".into(),
        "--quiet".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
    ]
}

pub fn list_owned_volumes_args(job_id: &str) -> Vec<String> {
    vec![
        "volume".into(),
        "ls".into(),
        "--quiet".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}={job_id}"),
    ]
}

pub fn list_owned_job_format_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("label={JOB_ID_LABEL}"),
        "--format".into(),
        "{{.Names}}\t{{.Label \"velnor.job-id\"}}\t{{.State}}".into(),
    ]
}

pub fn list_testcontainers_format_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("label={TESTCONTAINERS_LABEL}"),
        "--format".into(),
        "{{.ID}}\t{{.Label \"velnor.job-id\"}}".into(),
    ]
}

pub fn list_job_image_format_args() -> Vec<String> {
    vec![
        "ps".into(),
        "--all".into(),
        "--filter".into(),
        format!("ancestor={JOB_IMAGE_ANCESTOR}"),
        "--format".into(),
        "{{.ID}}\t{{.Label \"velnor.job-id\"}}".into(),
    ]
}

pub fn force_remove_container_args(ids: &[String]) -> Vec<String> {
    let mut args = vec!["rm".into(), "--force".into()];
    args.extend(ids.iter().cloned());
    args
}

pub fn force_remove_network_args(ids: &[String]) -> Vec<String> {
    let mut args = vec!["network".into(), "rm".into()];
    args.extend(ids.iter().cloned());
    args
}

pub fn force_remove_volume_args(ids: &[String]) -> Vec<String> {
    let mut args = vec!["volume".into(), "rm".into(), "--force".into()];
    args.extend(ids.iter().cloned());
    args
}

pub fn parse_docker_id_list(stdout: &str) -> Vec<String> {
    let mut ids = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// Job ids whose job container is not running. Guest objects for those jobs are orphans.
pub fn orphan_job_ids(formatted: &str) -> Vec<String> {
    let mut live_jobs = std::collections::BTreeSet::new();
    let mut seen_jobs = std::collections::BTreeSet::new();
    for line in formatted.lines() {
        let mut parts = line.split('\t');
        let name = parts.next().unwrap_or("").trim();
        let job_id = parts.next().unwrap_or("").trim();
        let state = parts.next().unwrap_or("").trim();
        if job_id.is_empty() {
            continue;
        }
        seen_jobs.insert(job_id.to_string());
        if name == job_id && state.eq_ignore_ascii_case("running") {
            live_jobs.insert(job_id.to_string());
        }
    }
    seen_jobs
        .into_iter()
        .filter(|job_id| !live_jobs.contains(job_id))
        .collect()
}

/// IDs of `velnor/job-ubuntu` siblings with no job label (docker-generated names).
pub fn unlabeled_job_image_ids(formatted: &str) -> Vec<String> {
    unlabeled_testcontainer_ids(formatted)
}

/// IDs of testcontainers that were created before the lease proxy (no job label).
pub fn unlabeled_testcontainer_ids(formatted: &str) -> Vec<String> {
    let mut ids = formatted
        .lines()
        .filter_map(|line| {
            let (id, job_id) = line.split_once('\t')?;
            let id = id.trim();
            if id.is_empty() || !job_id.trim().is_empty() {
                return None;
            }
            Some(id.to_string())
        })
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

pub fn is_docker_object_create(method: &str, target: &str) -> bool {
    if !method.eq_ignore_ascii_case("POST") {
        return false;
    }
    let path = target.split('?').next().unwrap_or(target);
    path.ends_with("/containers/create")
        || path.ends_with("/networks/create")
        || path.ends_with("/volumes/create")
}

pub fn inject_ownership_labels(body: &[u8], job_id: &str, daemon_id: &str) -> Result<Vec<u8>> {
    if body.is_empty() {
        let mut object = Map::new();
        object.insert("Labels".into(), ownership_labels_value(job_id, daemon_id));
        return serde_json::to_vec(&Value::Object(object)).context("serialize empty create body");
    }
    let mut value: Value = serde_json::from_slice(body)
        .context("parse Docker create JSON so ownership labels can be injected")?;
    let Some(object) = value.as_object_mut() else {
        bail!("Docker create body must be a JSON object");
    };
    let labels = object
        .remove("Labels")
        .or_else(|| object.remove("labels"))
        .unwrap_or(Value::Object(Map::new()));
    let mut labels = match labels {
        Value::Null => Map::new(),
        Value::Object(map) => map,
        other => {
            bail!("Docker create Labels must be an object, got {other}");
        }
    };
    labels.insert(JOB_ID_LABEL.into(), Value::String(job_id.to_string()));
    labels.insert(DAEMON_ID_LABEL.into(), Value::String(daemon_id.to_string()));
    object.insert("Labels".into(), Value::Object(labels));
    serde_json::to_vec(&value).context("serialize labeled Docker create body")
}

fn ownership_labels_value(job_id: &str, daemon_id: &str) -> Value {
    let mut labels = Map::new();
    labels.insert(JOB_ID_LABEL.into(), Value::String(job_id.to_string()));
    labels.insert(DAEMON_ID_LABEL.into(), Value::String(daemon_id.to_string()));
    Value::Object(labels)
}

/// Rewrite a Docker Engine HTTP/1.1 request so object creates carry job labels.
pub fn rewrite_docker_api_request(
    request: &[u8],
    job_id: &str,
    daemon_id: &str,
) -> Result<Vec<u8>> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker API request is missing header terminator")?;
    let header_bytes = &request[..header_end];
    let body = &request[header_end..];
    let header_text =
        std::str::from_utf8(header_bytes).context("Docker API headers must be UTF-8")?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().context("Docker API request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if !is_docker_object_create(method, target) {
        return Ok(request.to_vec());
    }
    let labeled = inject_ownership_labels(body, job_id, daemon_id)?;
    let mut headers = Vec::new();
    let mut skipped_length = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.len() >= 15 && line[..15].eq_ignore_ascii_case("content-length:") {
            skipped_length = true;
            continue;
        }
        headers.push(line);
    }
    if !skipped_length
        && headers
            .iter()
            .any(|line| line.len() >= 18 && line[..18].eq_ignore_ascii_case("transfer-encoding:"))
    {
        bail!("refusing to rewrite chunked Docker create request");
    }
    let mut out = Vec::new();
    out.extend_from_slice(request_line.as_bytes());
    out.extend_from_slice(b"\r\n");
    for line in headers {
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", labeled.len()).as_bytes());
    out.extend_from_slice(&labeled);
    Ok(out)
}

pub fn reclaim_job_owned(
    job_id: &str,
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    reclaim_listed(
        &list_owned_containers_args(job_id),
        &mut docker,
        force_remove_container_args,
    )?;
    reclaim_listed(
        &list_owned_networks_args(job_id),
        &mut docker,
        force_remove_network_args,
    )?;
    reclaim_listed(
        &list_owned_volumes_args(job_id),
        &mut docker,
        force_remove_volume_args,
    )?;
    Ok(())
}

pub fn reclaim_orphan_jobs(mut docker: impl FnMut(&[String]) -> Result<String>) -> Result<()> {
    let formatted = docker(&list_owned_job_format_args())?;
    for job_id in orphan_job_ids(&formatted) {
        reclaim_job_owned(&job_id, &mut docker)?;
    }
    Ok(())
}

pub fn reclaim_unlabeled_testcontainers(
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let formatted = docker(&list_testcontainers_format_args())?;
    let ids = unlabeled_testcontainer_ids(&formatted);
    if ids.is_empty() {
        return Ok(());
    }
    docker(&force_remove_container_args(&ids)).map(|_| ())
}

pub fn reclaim_unlabeled_job_image_siblings(
    mut docker: impl FnMut(&[String]) -> Result<String>,
) -> Result<()> {
    let formatted = docker(&list_job_image_format_args())?;
    let ids = unlabeled_job_image_ids(&formatted);
    if ids.is_empty() {
        return Ok(());
    }
    docker(&force_remove_container_args(&ids)).map(|_| ())
}

pub fn run_host_docker(args: &[String]) -> Result<String> {
    let output = std::process::Command::new("docker")
        .args(args)
        .output()
        .with_context(|| format!("run docker {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "docker {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn reclaim_listed(
    list_args: &[String],
    docker: &mut impl FnMut(&[String]) -> Result<String>,
    remove_args: fn(&[String]) -> Vec<String>,
) -> Result<()> {
    let listed = docker(list_args)?;
    let ids = parse_docker_id_list(&listed);
    if ids.is_empty() {
        return Ok(());
    }
    docker(&remove_args(&ids)).map(|_| ())
}

pub struct DockerLeaseGuard {
    listen_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl DockerLeaseGuard {
    pub fn bind(listen_path: PathBuf, job_id: String, daemon_id: String) -> Result<Self> {
        Self::bind_to(
            listen_path,
            PathBuf::from(HOST_DOCKER_SOCKET),
            job_id,
            daemon_id,
        )
    }

    pub fn bind_to(
        listen_path: PathBuf,
        host_socket: PathBuf,
        job_id: String,
        daemon_id: String,
    ) -> Result<Self> {
        #[cfg(not(unix))]
        {
            let _ = (listen_path, host_socket, job_id, daemon_id);
            bail!("job Docker lease proxy requires unix");
        }
        #[cfg(unix)]
        {
            bind_unix_lease(listen_path, host_socket, job_id, daemon_id)
        }
    }
}

impl Drop for DockerLeaseGuard {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        #[cfg(unix)]
        {
            let _ = std::os::unix::net::UnixStream::connect(&self.listen_path);
        }
        if let Some(thread) = self.accept_thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.listen_path);
    }
}

#[cfg(unix)]
fn bind_unix_lease(
    listen_path: PathBuf,
    host_socket: PathBuf,
    job_id: String,
    daemon_id: String,
) -> Result<DockerLeaseGuard> {
    use std::os::unix::net::UnixListener;

    if let Some(parent) = listen_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create job Docker lease directory {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(&listen_path);
    let listener = UnixListener::bind(&listen_path)
        .with_context(|| format!("bind job Docker lease socket {}", listen_path.display()))?;
    listener
        .set_nonblocking(false)
        .context("configure job Docker lease socket")?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_thread = Arc::clone(&shutdown);
    let listen_path_thread = listen_path.clone();
    let accept_thread = std::thread::Builder::new()
        .name(format!("velnor-docker-lease-{}", job_id))
        .spawn(move || {
            accept_loop(
                listener,
                host_socket,
                job_id,
                daemon_id,
                shutdown_thread,
                listen_path_thread,
            );
        })
        .context("start job Docker lease proxy thread")?;
    Ok(DockerLeaseGuard {
        listen_path,
        shutdown,
        accept_thread: Some(accept_thread),
    })
}

#[cfg(unix)]
fn accept_loop(
    listener: std::os::unix::net::UnixListener,
    host_socket: PathBuf,
    job_id: String,
    daemon_id: String,
    shutdown: Arc<AtomicBool>,
    listen_path: PathBuf,
) {
    while !shutdown.load(Ordering::SeqCst) {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(_) => continue,
        };
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let host_socket = host_socket.clone();
        let job_id = job_id.clone();
        let daemon_id = daemon_id.clone();
        let _ = std::thread::Builder::new()
            .name("velnor-docker-lease-conn".into())
            .spawn(move || {
                if let Err(error) = handle_client(stream, &host_socket, &job_id, &daemon_id) {
                    eprintln!("Warning: job Docker lease proxy: {error:#}");
                }
            });
    }
    let _ = std::fs::remove_file(listen_path);
}

#[cfg(unix)]
fn handle_client(
    mut client: std::os::unix::net::UnixStream,
    host_socket: &Path,
    job_id: &str,
    daemon_id: &str,
) -> Result<()> {
    use std::os::unix::net::UnixStream;

    let request = read_http_request(&mut client)?;
    let forwarded = rewrite_docker_api_request(&request, job_id, daemon_id)?;
    let mut host = UnixStream::connect(host_socket).with_context(|| {
        format!(
            "connect job Docker lease to host engine {}",
            host_socket.display()
        )
    })?;
    host.write_all(&forwarded)
        .context("forward Docker API request through job lease")?;
    let upgrade = std::str::from_utf8(&request).is_ok_and(|header| {
        header.lines().any(|line| {
            (line.len() >= 8 && line[..8].eq_ignore_ascii_case("upgrade:"))
                || (line.len() >= 11
                    && line[..11].eq_ignore_ascii_case("connection:")
                    && line.to_ascii_lowercase().contains("upgrade"))
        })
    });
    if upgrade {
        let mut host_reader = host.try_clone().context("clone host Docker lease stream")?;
        let mut client_writer = client
            .try_clone()
            .context("clone job Docker lease stream")?;
        let pump = std::thread::spawn(move || io::copy(&mut host_reader, &mut client_writer));
        let _ = io::copy(&mut client, &mut host);
        let _ = pump.join();
    } else {
        io::copy(&mut host, &mut client)
            .context("forward Docker API response through job lease")?;
    }
    Ok(())
}

#[cfg(unix)]
fn read_http_request(stream: &mut std::os::unix::net::UnixStream) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        let read = stream
            .read(&mut byte)
            .context("read Docker API request header")?;
        if read == 0 {
            bail!("client closed Docker API request before headers finished");
        }
        buf.push(byte[0]);
        if buf.len() > MAX_PROXY_BODY {
            bail!("Docker API request headers exceed lease proxy limit");
        }
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let header_end = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("Docker API request is missing header terminator")?;
    let header_text =
        std::str::from_utf8(&buf[..header_end]).context("Docker API headers must be UTF-8")?;
    let mut content_length = 0_usize;
    for line in header_text.split("\r\n").skip(1) {
        if line.len() >= 15 && line[..15].eq_ignore_ascii_case("content-length:") {
            content_length = line[15..]
                .trim()
                .parse()
                .context("parse Docker API Content-Length")?;
        }
    }
    if content_length > MAX_PROXY_BODY {
        bail!("Docker API request body exceeds lease proxy limit");
    }
    while buf.len() < header_end + content_length {
        let read = stream
            .read(&mut byte)
            .context("read Docker API request body")?;
        if read == 0 {
            bail!("client closed Docker API request before body finished");
        }
        buf.push(byte[0]);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn guest_socket_path_fits_unix_sun_len() {
        let path = guest_docker_socket_host(
            "velnor-job-fa461ac2-8b9f-5ef8-9754-a1ffe47774f1",
            Path::new("/var/lib/velnor-jackin-project/work/slot-1/job/temp"),
        );
        let rendered = path.to_string_lossy();
        assert!(
            rendered.len() < 100,
            "unix socket path must fit sockaddr_un, got {rendered}"
        );
        let name = path.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("vdl-") && name.ends_with(".sock"),
            "got {rendered}"
        );
        let parent = path.parent().unwrap().to_string_lossy();
        assert!(
            parent == LEASE_SOCKET_DIR || parent.ends_with("velnor-lease"),
            "lease socket must live in /run/velnor or test fallback velnor-lease, not PrivateTmp /tmp/vdl-*; got {rendered}"
        );
        assert!(
            !rendered.starts_with("/tmp/vdl-"),
            "PrivateTmp remaps daemon /tmp; dockerd would not see {rendered}"
        );
    }

    #[test]
    fn injects_job_and_daemon_labels_into_container_create() {
        let body = br#"{"Image":"postgres:18-alpine","Labels":{"org.testcontainers.managed-by":"testcontainers"}}"#;
        let labeled =
            inject_ownership_labels(body, "velnor-job-1", "/var/lib/velnor/work/slot-1").unwrap();
        let value: Value = serde_json::from_slice(&labeled).unwrap();
        let labels = value["Labels"].as_object().unwrap();
        assert_eq!(labels["org.testcontainers.managed-by"], "testcontainers");
        assert_eq!(labels[JOB_ID_LABEL], "velnor-job-1");
        assert_eq!(labels[DAEMON_ID_LABEL], "/var/lib/velnor/work/slot-1");
    }

    #[test]
    fn injects_labels_when_create_body_omits_them() {
        let labeled = inject_ownership_labels(br#"{"Name":"net"}"#, "job-a", "daemon-a").unwrap();
        let value: Value = serde_json::from_slice(&labeled).unwrap();
        assert_eq!(value["Labels"][JOB_ID_LABEL], "job-a");
        assert_eq!(value["Labels"][DAEMON_ID_LABEL], "daemon-a");
    }

    #[test]
    fn rewrite_labels_container_create_and_passes_other_methods_through() {
        let body = br#"{"Image":"redis:5.0"}"#;
        let request = format!(
            "POST /v1.43/containers/create?name=goofy HTTP/1.1\r\nHost: docker\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let rewritten =
            rewrite_docker_api_request(request.as_bytes(), "velnor-job-9", "daemon-9").unwrap();
        let text = String::from_utf8(rewritten).unwrap();
        assert!(text.contains("\"velnor.job-id\":\"velnor-job-9\""));
        assert!(text.contains("Content-Length:"));

        let ping = b"GET /_ping HTTP/1.1\r\nHost: docker\r\n\r\n";
        assert_eq!(
            rewrite_docker_api_request(ping, "job", "daemon").unwrap(),
            ping
        );
    }

    #[test]
    fn reclaim_job_owned_removes_guest_container_network_and_volume() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            "guest-postgres\nbuildkit-one\n".to_string(),
            String::new(),
            "guest-net\n".to_string(),
            String::new(),
            "guest-vol\n".to_string(),
            String::new(),
        ];
        reclaim_job_owned("velnor-job-1", |args| {
            calls.push(args.to_vec());
            if outputs.is_empty() {
                return Err(anyhow!("unexpected docker call {args:?}"));
            }
            Ok(outputs.remove(0))
        })
        .unwrap();

        assert_eq!(calls[0], list_owned_containers_args("velnor-job-1"));
        assert_eq!(
            calls[1],
            force_remove_container_args(&["buildkit-one".into(), "guest-postgres".into()])
        );
        assert_eq!(calls[2], list_owned_networks_args("velnor-job-1"));
        assert_eq!(calls[3], force_remove_network_args(&["guest-net".into()]));
        assert_eq!(calls[4], list_owned_volumes_args("velnor-job-1"));
        assert_eq!(calls[5], force_remove_volume_args(&["guest-vol".into()]));
        assert!(outputs.is_empty());
    }

    #[test]
    fn orphan_job_ids_keep_live_jobs_and_reclaim_finished_guest_siblings() {
        let formatted = "\
velnor-job-live\tvelnor-job-live\trunning
guest-pg\tvelnor-job-live\trunning
guest-old\tvelnor-job-dead\trunning
velnor-job-dead\tvelnor-job-dead\texited
";
        assert_eq!(
            orphan_job_ids(formatted),
            vec!["velnor-job-dead".to_string()]
        );
    }

    #[test]
    fn reclaim_orphan_jobs_deletes_finished_job_objects_and_keeps_live_jobs() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            "velnor-job-live\tvelnor-job-live\trunning\nguest-old\tvelnor-job-dead\trunning\n"
                .to_string(),
            "guest-old\n".to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ];
        reclaim_orphan_jobs(|args| {
            calls.push(args.to_vec());
            if outputs.is_empty() {
                return Err(anyhow!("unexpected docker call {args:?}"));
            }
            Ok(outputs.remove(0))
        })
        .unwrap();
        assert_eq!(calls[0], list_owned_job_format_args());
        assert_eq!(calls[1], list_owned_containers_args("velnor-job-dead"));
        assert_eq!(calls[2], force_remove_container_args(&["guest-old".into()]));
    }

    #[test]
    fn unlabeled_testcontainer_ids_keep_only_pre_lease_orphans() {
        let formatted = "aaa\t\nbbb\tvelnor-job-live\nccc\t\n";
        assert_eq!(
            unlabeled_testcontainer_ids(formatted),
            vec!["aaa".to_string(), "ccc".to_string()]
        );
    }

    #[test]
    fn reclaim_unlabeled_testcontainers_force_removes_orphans_only() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            "dead1\t\nlive1\tvelnor-job-now\ndead2\t\n".to_string(),
            String::new(),
        ];
        reclaim_unlabeled_testcontainers(|args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();
        assert_eq!(calls[0], list_testcontainers_format_args());
        assert_eq!(
            calls[1],
            force_remove_container_args(&["dead1".into(), "dead2".into()])
        );
    }

    #[test]
    fn reclaim_unlabeled_job_image_siblings_force_removes_orphans_only() {
        let mut calls = Vec::new();
        let mut outputs = vec![
            "gagarin\t\nvelnor-job-live\tvelnor-job-live\nride\t\n".to_string(),
            String::new(),
        ];
        reclaim_unlabeled_job_image_siblings(|args| {
            calls.push(args.to_vec());
            Ok(outputs.remove(0))
        })
        .unwrap();
        assert_eq!(calls[0], list_job_image_format_args());
        assert_eq!(
            calls[1],
            force_remove_container_args(&["gagarin".into(), "ride".into()])
        );
    }
}
