//! Intentionally hostile, diagnostic-only G1 bootstrap candidate.
//!
//! This program must run only as PID 1 in the fixed hosted sandbox. It never
//! emits or persists environment values, tokens, URLs, response bodies, source
//! bytes, or OS error strings. Its JSON line is untrusted evidence only.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixStream;
use std::path::{Component, Path, PathBuf};
use std::process::{self, Child, Command, Stdio};
use std::thread;
use std::time::Duration;

const MAX_PROC_ENV_BYTES: u64 = 1024 * 1024;
const MAX_VISIBLE_PROCS: u64 = 128;
const H7_FILE_ATTEMPTS: u64 = 4096;
const H8_CHILD_ATTEMPTS: u64 = 160;
const H8_CHILD_LIFETIME: Duration = Duration::from_millis(750);

type Status = &'static str;

#[derive(Clone, Copy)]
enum JsonValue {
    Bool(bool),
    Number(u64),
    Text(Status),
}

fn status_for_error(error: &io::Error) -> Status {
    match error.kind() {
        io::ErrorKind::NotFound => "missing",
        io::ErrorKind::PermissionDenied => "denied",
        io::ErrorKind::AlreadyExists => "exists",
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => "invalid",
        io::ErrorKind::TimedOut => "timeout",
        io::ErrorKind::WouldBlock => "blocked",
        _ => "error",
    }
}

fn bounded_read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bounded read"));
    }
    Ok(bytes)
}

fn forbidden_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    const PREFIXES: &[&str] = &[
        "GITHUB_",
        "GH_",
        "ACTIONS_",
        "RUNNER_",
        "ARTIFACT",
        "AWS_",
        "AZURE_",
        "GOOGLE_",
        "CLOUD_",
        "REGISTRY_",
        "DOCKER_",
        "NPM_",
        "CARGO_",
        "MISE_",
        "MBX_",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
    ];
    const WORDS: &[&str] = &["TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "AUTH"];
    PREFIXES.iter().any(|prefix| upper.starts_with(prefix))
        || WORDS.iter().any(|word| upper.contains(word))
}

fn forbidden_names_from_bytes(bytes: &[u8]) -> u64 {
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.split(|byte| *byte == b'=').next())
        .filter_map(|name| std::str::from_utf8(name).ok())
        .filter(|name| forbidden_name(name))
        .count() as u64
}

fn current_forbidden_env_names() -> u64 {
    env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .filter(|name| forbidden_name(name))
        .count() as u64
}

fn proc_environ(path: &Path) -> (Status, u64) {
    match bounded_read(path, MAX_PROC_ENV_BYTES) {
        Ok(bytes) => ("read", forbidden_names_from_bytes(&bytes)),
        Err(error) => (status_for_error(&error), 0),
    }
}

fn visible_proc_env_scan() -> (Status, u64, u64) {
    let Ok(entries) = fs::read_dir("/proc") else {
        return ("unreadable", 0, 0);
    };
    let mut scanned = 0;
    let mut unreadable = 0;
    let mut forbidden = 0;
    for entry in entries.filter_map(Result::ok).filter(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    }) {
        if scanned + unreadable >= MAX_VISIBLE_PROCS {
            return ("over-limit", scanned, forbidden);
        }
        match bounded_read(&entry.path().join("environ"), MAX_PROC_ENV_BYTES) {
            Ok(bytes) => {
                scanned += 1;
                forbidden += forbidden_names_from_bytes(&bytes);
            }
            Err(_) => unreadable += 1,
        }
    }
    let status = if unreadable == 0 {
        "read"
    } else if scanned != 0 {
        "partial"
    } else {
        "unreadable"
    };
    (status, scanned, forbidden)
}

fn visible_pid_count() -> u64 {
    let Ok(entries) = fs::read_dir("/proc") else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        })
        .count() as u64
}

fn path_is_safe_child(path: &Path, root: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| component == Component::ParentDir)
        && path.starts_with(root)
}

fn disposable_root() -> Option<PathBuf> {
    let root = PathBuf::from(env::var_os("HOSTILE_PROBE_WRITE_ROOT")?);
    if path_is_safe_child(&root, Path::new("/")) {
        Some(root)
    } else {
        None
    }
}

fn write_under_root(path: &Path, root: &Path, bytes: &[u8]) -> Status {
    if !path_is_safe_child(path, root) {
        return "guarded";
    }
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => match file.write_all(bytes) {
            Ok(()) => "wrote",
            Err(error) => status_for_error(&error),
        },
        Err(error) => status_for_error(&error),
    }
}

fn inspect_outside_root(path: &Path, root: Option<&Path>) -> Status {
    if let Some(root) = root
        && path_is_safe_child(path, root)
    {
        return write_under_root(path, root, b"velnor-hostile-command-file\n");
    }
    match fs::metadata(path) {
        Ok(_) => "present-guarded",
        Err(error) => status_for_error(&error),
    }
}

fn command_file_status(name: &str, root: Option<&Path>) -> Status {
    let Some(path) = env::var_os(name).map(PathBuf::from) else {
        return "absent";
    };
    inspect_outside_root(&path, root)
}

fn fixed_command_path_statuses(root: Option<&Path>) -> (u64, u64) {
    let paths = [
        "/github_env",
        "/github_path",
        "/github_output",
        "/github_state",
        "/github_step_summary",
        "/__w/_temp/_runner_file_commands",
    ];
    let mut present = 0;
    let mut guarded = 0;
    for path in paths {
        match inspect_outside_root(Path::new(path), root) {
            "absent" | "missing" => {}
            "present-guarded" => present += 1,
            _ => guarded += 1,
        }
    }
    (present, guarded)
}

fn parse_endpoint(raw: &str) -> Option<(String, u16)> {
    if raw.len() > 4096 || raw.contains(['\r', '\n']) {
        return None;
    }
    let (scheme, rest) = raw.split_once("://")?;
    let default_port = match scheme {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let (authority, _suffix) = rest.split_once('/').unwrap_or((rest, ""));
    let authority = authority.rsplit('@').next()?;
    let authority = authority.split(['?', '#']).next()?;
    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let (host, port_text) = stripped.split_once(']')?;
        let port = port_text
            .strip_prefix(':')
            .and_then(|value| value.parse().ok())
            .unwrap_or(default_port);
        (host.to_owned(), port)
    } else if let Some((host, port_text)) = authority.rsplit_once(':') {
        if port_text.is_empty() {
            (authority.to_owned(), default_port)
        } else {
            (host.to_owned(), port_text.parse().ok()?)
        }
    } else {
        (authority.to_owned(), default_port)
    };
    if host.is_empty()
        || host.contains(['\r', '\n'])
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'-'))
    {
        return None;
    }
    Some((host, port))
}

fn artifact_upload_status() -> (Status, bool, bool, bool, bool) {
    let Some(endpoint) = env::var("ACTIONS_RUNTIME_URL")
        .ok()
        .or_else(|| env::var("ACTIONS_RESULTS_URL").ok())
    else {
        return ("absent", false, false, false, false);
    };
    let token = env::var("ACTIONS_RUNTIME_TOKEN").ok();
    let token_name_present = token.is_some();
    let synthetic_token = token.filter(|value| {
        value == "velnor-synthetic-canary-token"
            && env::var("HOSTILE_PROBE_SYNTHETIC_UPLOAD").ok().as_deref() == Some("1")
    });
    if token_name_present && synthetic_token.is_none() {
        return ("real-token-refused", true, true, false, false);
    }
    let Some((host, port)) = parse_endpoint(&endpoint) else {
        return ("invalid", true, token_name_present, false, false);
    };
    let Ok(mut addresses) = (host.as_str(), port).to_socket_addrs() else {
        return ("resolve-failed", true, token_name_present, false, false);
    };
    let Some(address) = addresses.next() else {
        return ("resolve-empty", true, token_name_present, false, false);
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(250)) else {
        if token_name_present && synthetic_token.is_none() {
            return ("real-token-refused", true, true, false, false);
        }
        return ("connect-failed", true, token_name_present, false, false);
    };
    let body = br#"{"fixture":"velnor-hostile-probe","bytes":"synthetic"}"#;
    let authorization = synthetic_token
        .as_deref()
        .map(|value| format!("Authorization: Bearer {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /_apis/artifactcache/cache HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n{authorization}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        String::from_utf8_lossy(body)
    );
    let sent = stream.write_all(request.as_bytes()).is_ok();
    let _ = stream.shutdown(Shutdown::Write);
    if synthetic_token.is_some() && sent {
        ("sent-with-synthetic-auth", true, true, false, true)
    } else if sent {
        ("sent-without-auth", true, token_name_present, true, false)
    } else if token_name_present && synthetic_token.is_none() {
        ("real-token-refused", true, true, false, false)
    } else {
        ("write-failed", true, token_name_present, false, false)
    }
}

fn connect_status(host: &str, port: u16) -> Status {
    let Ok(mut addresses) = (host, port).to_socket_addrs() else {
        return "resolve-failed";
    };
    let Some(address) = addresses.next() else {
        return "resolve-empty";
    };
    match TcpStream::connect_timeout(&address, Duration::from_millis(250)) {
        Ok(stream) => {
            let _ = stream.shutdown(Shutdown::Both);
            "connected"
        }
        Err(error) => status_for_error(&error),
    }
}

fn network_statuses() -> (Status, Status, Status, Status, Status) {
    (
        connect_status("bootstrap-hostile-probe.invalid", 80),
        connect_status("127.0.0.1", 9),
        connect_status("::1", 9),
        connect_status("169.254.169.254", 80),
        if ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"]
            .iter()
            .any(|name| env::var_os(name).is_some())
        {
            "present-guarded"
        } else {
            "absent"
        },
    )
}

fn unix_socket_status(path: &Path) -> Status {
    match UnixStream::connect(path) {
        Ok(stream) => {
            let _ = stream.shutdown(Shutdown::Both);
            "connected"
        }
        Err(error) => status_for_error(&error),
    }
}

fn cache_statuses(active: bool) -> (u64, u64) {
    let paths = ["/cache", "/root/.cache", "/opt/mise", "/mbx", "/var/cache"];
    let mut present = 0;
    let mut wrote = 0;
    for path in paths {
        if !matches!(
            inspect_outside_root(Path::new(path), None),
            "absent" | "missing"
        ) {
            present += 1;
        }
        if active
            && mount_write_status(&PathBuf::from(path).join(".velnor-hostile-cache"), active)
                == "wrote"
        {
            wrote += 1;
        }
    }
    (present, wrote)
}

fn mount_write_status(path: &Path, active: bool) -> Status {
    if !active {
        return "not-run";
    }
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(mut file) => match file.write_all(b"velnor-hostile-source-write\n") {
            Ok(()) => "wrote",
            Err(error) => status_for_error(&error),
        },
        Err(error) => status_for_error(&error),
    }
}

fn symlink_status(output: &Path) -> Status {
    match symlink("../input/.secret", output.join(".hostile-symlink")) {
        Ok(()) => "created",
        Err(error) => status_for_error(&error),
    }
}

fn hardlink_status(output: &Path, root: &Path) -> Status {
    let source = output.join(".hostile-hardlink-source");
    let target = output.join(".hostile-hardlink");
    let source_status = write_under_root(&source, root, b"hardlink-source\n");
    if source_status != "wrote" && source_status != "exists" {
        return source_status;
    }
    match fs::hard_link(source, target) {
        Ok(()) => "created",
        Err(error) => status_for_error(&error),
    }
}

fn sparse_status(output: &Path) -> Status {
    let path = output.join(".hostile-sparse-80m");
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(file) => match file.set_len(80 * 1024 * 1024) {
            Ok(()) => "created",
            Err(error) => status_for_error(&error),
        },
        Err(error) => status_for_error(&error),
    }
}

fn file_flood_status(output: &Path) -> (u64, Status) {
    let mut created = 0;
    let mut first_error = "none";
    for index in 0..H7_FILE_ATTEMPTS {
        let path = output.join(format!(".hostile-file-{index:04}"));
        match OpenOptions::new().create_new(true).write(true).open(path) {
            Ok(_) => created += 1,
            Err(error) => {
                first_error = status_for_error(&error);
                break;
            }
        }
    }
    (created, first_error)
}

fn traversal_status(output: &Path, active: bool) -> Status {
    if !active {
        return "not-run";
    }
    let path = output.join("..").join("input").join(".hostile-traversal");
    match OpenOptions::new().create_new(true).write(true).open(path) {
        Ok(_) => "wrote",
        Err(error) => status_for_error(&error),
    }
}

fn fake_contract_status(output: &Path, root: &Path) -> Status {
    let path = output.join("handoff.json");
    write_under_root(
        &path,
        root,
        br#"{"schema":"forged","head_sha":"deadbeef","closure":"deadbeef"}
"#,
    )
}

fn fake_manifest_status(output: &Path, root: &Path) -> Status {
    let path = output.join("manifest.json");
    write_under_root(
        &path,
        root,
        br#"{"schema":"forged","head_sha":"deadbeef","closure":"deadbeef"}
"#,
    )
}

fn rewrite_existing_status(path: &Path, active: bool) -> Status {
    if !active {
        return "not-run";
    }
    if !path.is_file() {
        return "missing";
    }
    match OpenOptions::new().append(true).open(path) {
        Ok(mut file) => match file.write_all(b"velnor-hostile-source-rewrite\n") {
            Ok(()) => "wrote",
            Err(error) => status_for_error(&error),
        },
        Err(error) => status_for_error(&error),
    }
}

fn delete_existing_status(path: &Path, active: bool) -> Status {
    if !active {
        return "not-run";
    }
    match fs::remove_file(path) {
        Ok(()) => "deleted",
        Err(error) => status_for_error(&error),
    }
}

fn proc_status_flags() -> (Status, bool, bool, bool, bool) {
    let Ok(bytes) = bounded_read(Path::new("/proc/1/status"), 64 * 1024) else {
        return ("unreadable", false, false, false, false);
    };
    let text = String::from_utf8_lossy(&bytes);
    let uid_nonzero = text
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|uid| uid != 0);
    let capabilities_zero = text
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .map(|line| line.trim().chars().all(|character| character == '0'))
        .unwrap_or(false);
    let no_new_privileges = text
        .lines()
        .find_map(|line| line.strip_prefix("NoNewPrivs:"))
        .is_some_and(|line| line.trim() == "1");
    let seccomp = text
        .lines()
        .find_map(|line| line.strip_prefix("Seccomp:"))
        .is_some_and(|line| line.trim() == "2");
    (
        "read",
        uid_nonzero,
        capabilities_zero,
        no_new_privileges,
        seccomp,
    )
}

fn child_pressure() -> (u64, u64, Status) {
    let executable = match env::current_exe() {
        Ok(path) => path,
        Err(_) => return (0, H8_CHILD_ATTEMPTS, "current-exe-failed"),
    };
    let mut children: Vec<Child> = Vec::with_capacity(H8_CHILD_ATTEMPTS as usize);
    let mut spawned = 0;
    let mut first_error = "none";
    for _ in 0..H8_CHILD_ATTEMPTS {
        match Command::new(&executable)
            .arg("--child-sleep")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                spawned += 1;
                children.push(child);
            }
            Err(error) => {
                first_error = status_for_error(&error);
                break;
            }
        }
    }
    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }
    (spawned, H8_CHILD_ATTEMPTS, first_error)
}

fn json_escape(text: &str) -> String {
    let mut escaped = String::new();
    for character in text.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => escaped.push('?'),
            character => escaped.push(character),
        }
    }
    escaped
}

fn render_json(fields: &[(&str, JsonValue)]) -> String {
    let mut json = String::from("{");
    for (index, (name, value)) in fields.iter().enumerate() {
        if index != 0 {
            json.push(',');
        }
        json.push('"');
        json.push_str(name);
        json.push_str("\":");
        match value {
            JsonValue::Bool(value) => json.push_str(if *value { "true" } else { "false" }),
            JsonValue::Number(value) => json.push_str(&value.to_string()),
            JsonValue::Text(value) => {
                json.push('"');
                json.push_str(&json_escape(value));
                json.push('"');
            }
        }
    }
    json.push('}');
    json
}

fn parse_output() -> Result<PathBuf, Status> {
    let mut canary = false;
    let mut output = None;
    let mut args = env::args_os().skip(1);
    while let Some(argument) = args.next() {
        match argument.to_string_lossy().as_ref() {
            "--canary" => canary = true,
            "--output" => output = args.next().map(PathBuf::from),
            _ => return Err("unknown-argument"),
        }
    }
    if !canary {
        return Err("canary-flag-required");
    }
    if env::var("HOSTILE_PROBE_ISOLATED").ok().as_deref() != Some("1") {
        return Err("isolated-opt-in-required");
    }
    if process::id() != 1 {
        return Err("pid-one-required");
    }
    let output = output.ok_or("output-required")?;
    if output != Path::new("/output") {
        return Err("output-must-be-disposable-mount");
    }
    Ok(output)
}

fn main() {
    if env::args_os().any(|argument| argument == "--child-sleep") {
        thread::sleep(H8_CHILD_LIFETIME);
        return;
    }
    let output = match parse_output() {
        Ok(output) => output,
        Err(reason) => {
            eprintln!("hostile probe refused: {reason}");
            process::exit(2);
        }
    };
    let Some(root) = disposable_root() else {
        eprintln!("hostile probe refused: disposable root required");
        process::exit(2);
    };
    if root != output {
        eprintln!("hostile probe refused: disposable root mismatch");
        process::exit(2);
    }

    let (proc1_status, proc1_forbidden) = proc_environ(Path::new("/proc/1/environ"));
    let (proc_self_status, proc_self_forbidden) = proc_environ(Path::new("/proc/self/environ"));
    let (proc_scan_status, proc_scanned, proc_forbidden) = visible_proc_env_scan();
    let proc_visible = visible_pid_count();
    let (status_status, uid_nonzero, capabilities_zero, no_new_privileges, seccomp) =
        proc_status_flags();
    let h2_github_env = command_file_status("GITHUB_ENV", Some(&root));
    let h2_github_path = command_file_status("GITHUB_PATH", Some(&root));
    let h2_github_output = command_file_status("GITHUB_OUTPUT", Some(&root));
    let h2_github_state = command_file_status("GITHUB_STATE", Some(&root));
    let h2_step_summary = command_file_status("GITHUB_STEP_SUMMARY", Some(&root));
    let h3_token_name_present = [
        "ACTIONS_RUNTIME_TOKEN",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        "GITHUB_TOKEN",
        "GH_TOKEN",
    ]
    .iter()
    .any(|name| env::var_os(name).is_some());
    let (
        h3_upload,
        h3_endpoint_present,
        h3_artifact_token_name_present,
        h3_sent_without_auth,
        h3_sent_with_synthetic_auth,
    ) = artifact_upload_status();
    let (h2_fixed_command_paths_present, h2_fixed_command_paths_guarded) =
        fixed_command_path_statuses(Some(&root));
    let h4_github_workspace = env::var_os("GITHUB_WORKSPACE")
        .map(|path| inspect_outside_root(&PathBuf::from(path), Some(&root)))
        .unwrap_or("absent");
    let h4_runner_temp = env::var_os("RUNNER_TEMP")
        .map(|path| inspect_outside_root(&PathBuf::from(path), Some(&root)))
        .unwrap_or("absent");
    let h4_workspace_fixed = inspect_outside_root(Path::new("/workspace"), Some(&root));
    let h4_runner_workspace_fixed = inspect_outside_root(Path::new("/__w"), Some(&root));
    let h4_input_write = mount_write_status(Path::new("/input/.velnor-hostile-source"), true);
    let h4_candidate_write =
        mount_write_status(Path::new("/candidate/.velnor-hostile-binary"), true);
    let h4_workspace_write =
        mount_write_status(Path::new("/workspace/.velnor-hostile-workspace"), true);
    let h4_runner_workspace_write =
        mount_write_status(Path::new("/__w/.velnor-hostile-runner"), true);
    let (h5_dns, h5_loopback_v4, h5_loopback_v6, h5_metadata, h5_proxy) = network_statuses();
    let h5_unix_socket = unix_socket_status(Path::new("/tmp/velnor-hostile-network.sock"));
    let h4_docker_socket = unix_socket_status(Path::new("/var/run/docker.sock"));
    let h4_docker_socket_run = unix_socket_status(Path::new("/run/docker.sock"));
    let (h9_cache_paths_present, h9_cache_paths_wrote) = cache_statuses(true);
    let h6_symlink = symlink_status(&output);
    let h6_hardlink = hardlink_status(&output, &root);
    let h7_traversal = traversal_status(&output, true);
    let h7_fake_contract = fake_contract_status(&output, &root);
    let h7_fake_manifest = fake_manifest_status(&output, &root);
    let h9_source_write = mount_write_status(Path::new("/input/.git/.velnor-hostile-git"), true);
    let h9_source_rewrite = rewrite_existing_status(Path::new("/input/Cargo.toml"), true);
    let h9_source_delete = delete_existing_status(Path::new("/input/Cargo.toml"), true);
    let h7_sparse = sparse_status(&output);
    let (h7_files_created, h7_file_error) = file_flood_status(&output);
    let (h8_children_spawned, h8_children_attempted, h8_child_error) = child_pressure();

    let fields = [
        (
            "schema",
            JsonValue::Text("velnor.bootstrap-hostile-probe.v1"),
        ),
        ("fixture", JsonValue::Text("bootstrap-hostile-producer")),
        (
            "h1_env_forbidden_names",
            JsonValue::Number(current_forbidden_env_names()),
        ),
        ("h1_proc1_status", JsonValue::Text(proc1_status)),
        (
            "h1_proc1_forbidden_names",
            JsonValue::Number(proc1_forbidden),
        ),
        ("h1_proc_self_status", JsonValue::Text(proc_self_status)),
        (
            "h1_proc_self_forbidden_names",
            JsonValue::Number(proc_self_forbidden),
        ),
        ("h1_proc_scan_status", JsonValue::Text(proc_scan_status)),
        ("h1_proc_scanned", JsonValue::Number(proc_scanned)),
        ("h1_proc_forbidden_names", JsonValue::Number(proc_forbidden)),
        ("h1_visible_pid_count", JsonValue::Number(proc_visible)),
        ("h2_github_env", JsonValue::Text(h2_github_env)),
        ("h2_github_path", JsonValue::Text(h2_github_path)),
        ("h2_github_output", JsonValue::Text(h2_github_output)),
        ("h2_github_state", JsonValue::Text(h2_github_state)),
        ("h2_step_summary", JsonValue::Text(h2_step_summary)),
        (
            "h2_fixed_command_paths_present",
            JsonValue::Number(h2_fixed_command_paths_present),
        ),
        (
            "h2_fixed_command_paths_guarded",
            JsonValue::Number(h2_fixed_command_paths_guarded),
        ),
        ("h3_endpoint_present", JsonValue::Bool(h3_endpoint_present)),
        (
            "h3_token_name_present",
            JsonValue::Bool(h3_token_name_present || h3_artifact_token_name_present),
        ),
        ("h3_upload_status", JsonValue::Text(h3_upload)),
        (
            "h3_sent_without_auth",
            JsonValue::Bool(h3_sent_without_auth),
        ),
        (
            "h3_sent_with_synthetic_auth",
            JsonValue::Bool(h3_sent_with_synthetic_auth),
        ),
        ("h4_github_workspace", JsonValue::Text(h4_github_workspace)),
        ("h4_runner_temp", JsonValue::Text(h4_runner_temp)),
        ("h4_workspace_fixed", JsonValue::Text(h4_workspace_fixed)),
        (
            "h4_runner_workspace_fixed",
            JsonValue::Text(h4_runner_workspace_fixed),
        ),
        ("h4_input_write", JsonValue::Text(h4_input_write)),
        ("h4_candidate_write", JsonValue::Text(h4_candidate_write)),
        ("h4_workspace_write", JsonValue::Text(h4_workspace_write)),
        (
            "h4_runner_workspace_write",
            JsonValue::Text(h4_runner_workspace_write),
        ),
        ("h4_docker_socket", JsonValue::Text(h4_docker_socket)),
        (
            "h4_docker_socket_run",
            JsonValue::Text(h4_docker_socket_run),
        ),
        ("h5_dns", JsonValue::Text(h5_dns)),
        ("h5_loopback_v4", JsonValue::Text(h5_loopback_v4)),
        ("h5_loopback_v6", JsonValue::Text(h5_loopback_v6)),
        ("h5_metadata", JsonValue::Text(h5_metadata)),
        ("h5_proxy", JsonValue::Text(h5_proxy)),
        ("h5_unix_socket", JsonValue::Text(h5_unix_socket)),
        ("h6_symlink", JsonValue::Text(h6_symlink)),
        ("h6_hardlink", JsonValue::Text(h6_hardlink)),
        ("h7_sparse_80m", JsonValue::Text(h7_sparse)),
        ("h7_file_attempts", JsonValue::Number(H7_FILE_ATTEMPTS)),
        ("h7_files_created", JsonValue::Number(h7_files_created)),
        ("h7_file_error", JsonValue::Text(h7_file_error)),
        ("h7_traversal", JsonValue::Text(h7_traversal)),
        ("h7_fake_contract", JsonValue::Text(h7_fake_contract)),
        ("h7_fake_manifest", JsonValue::Text(h7_fake_manifest)),
        ("h8_proc1_status", JsonValue::Text(status_status)),
        ("h8_pid_is_one", JsonValue::Bool(process::id() == 1)),
        ("h8_uid_nonzero", JsonValue::Bool(uid_nonzero)),
        ("h8_capabilities_zero", JsonValue::Bool(capabilities_zero)),
        ("h8_no_new_privileges", JsonValue::Bool(no_new_privileges)),
        ("h8_seccomp_two", JsonValue::Bool(seccomp)),
        (
            "h8_child_attempts",
            JsonValue::Number(h8_children_attempted),
        ),
        (
            "h8_children_spawned",
            JsonValue::Number(h8_children_spawned),
        ),
        ("h8_child_error", JsonValue::Text(h8_child_error)),
        ("h9_source_write", JsonValue::Text(h9_source_write)),
        ("h9_source_rewrite", JsonValue::Text(h9_source_rewrite)),
        ("h9_source_delete", JsonValue::Text(h9_source_delete)),
        (
            "h9_cache_paths_present",
            JsonValue::Number(h9_cache_paths_present),
        ),
        (
            "h9_cache_paths_wrote",
            JsonValue::Number(h9_cache_paths_wrote),
        ),
        ("h9_contract_minting", JsonValue::Text(h7_fake_contract)),
        ("authority", JsonValue::Text("diagnostic-only")),
    ];
    println!("VELNOR_HOSTILE_RESULT {}", render_json(&fields));
}
