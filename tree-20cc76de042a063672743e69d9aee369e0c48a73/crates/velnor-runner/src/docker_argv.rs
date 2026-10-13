//! Typed construction of `docker` command lines.
//!
//! Two argument-injection classes are structurally impossible here, rather
//! than merely absent from the current call sites:
//!
//! 1. **Flag injection through an operand.** A workflow controls the image
//!    reference (`runs.image: docker://--privileged`), the service image, the
//!    container name and the action arguments. Any of those, placed on a
//!    `docker` command line as a bare word, is parsed as a flag and yields a
//!    host `docker run` of the attacker's choosing — root on a shared,
//!    persistent runner host. The builder is a typestate: flags can only be
//!    added while it is a [`DockerCommand`]/[`DockerArgv`], and the transition
//!    to the operand phase writes the `--` end-of-flags separator. Once in
//!    [`DockerOperands`]/[`DockerArgvOperands`] there is no method that can
//!    append another flag, so no future call site can put an operand where a
//!    flag would be read.
//! 2. **Environment on argv.** `/proc/<pid>/cmdline` is world-readable, so any
//!    co-tenant on the host can read a `docker run -e GITHUB_TOKEN=...`
//!    command line. The builder has no way to emit `-e NAME=VALUE`: callers
//!    use [`DockerCommand::env`], and [`DockerCommand::finish`] materializes
//!    every variable into a mode-0600 `--env-file` (values that an env file
//!    cannot represent are forwarded from the `docker` client's own process
//!    environment with a bare `-e NAME`).
//!
//! The image reference is additionally validated against the OCI distribution
//! reference grammar, so an image that is not an image is rejected at
//! construction instead of reaching the Docker CLI.

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

static ENV_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// End-of-flags separator. Every operand is emitted after this token.
const END_OF_FLAGS: &str = "--";

// ---------------------------------------------------------------------------
// Image references
// ---------------------------------------------------------------------------

/// A validated OCI image reference.
///
/// The only way to place an image on a Docker command line is to parse it
/// first, so `docker://--privileged` and every other flag-shaped value is
/// rejected before a command exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageReference(String);

/// Why a candidate string is not an image reference. The offending value is
/// never echoed: it is attacker-controlled and reaches operator logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidImageReference {
    pub reason: &'static str,
}

impl std::fmt::Display for InvalidImageReference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid image reference: {}", self.reason)
    }
}

impl std::error::Error for InvalidImageReference {}

impl From<InvalidImageReference> for io::Error {
    fn from(error: InvalidImageReference) -> Self {
        Self::new(io::ErrorKind::InvalidInput, error.to_string())
    }
}

/// Maximum length of the name portion, matching
/// `distribution/reference.NameTotalLengthMax`.
const NAME_TOTAL_LENGTH_MAX: usize = 255;

impl ImageReference {
    /// Parse `raw` as `[domain[:port]/]path[:tag][@digest]`.
    ///
    /// # Errors
    /// Any deviation from the OCI reference grammar, including a leading `-`
    /// (which the Docker CLI would parse as a flag).
    pub fn parse(raw: &str) -> Result<Self, InvalidImageReference> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(invalid("empty reference"));
        }
        if raw.starts_with('-') {
            return Err(invalid(
                "reference starts with '-' and would be read as a flag",
            ));
        }
        if raw.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
            return Err(invalid(
                "reference contains whitespace or control characters",
            ));
        }
        if !raw.is_ascii() {
            return Err(invalid("reference contains non-ASCII characters"));
        }

        // Digest first: it is the only part that may contain ':' after a '@'.
        let (remainder, digest) = match raw.split_once('@') {
            Some((remainder, digest)) => (remainder, Some(digest)),
            None => (raw, None),
        };
        if let Some(digest) = digest {
            validate_digest(digest)?;
        }

        // A ':' in the final path segment separates the tag. A ':' that is
        // followed by a '/' belongs to a registry port instead.
        let (name, tag) = match remainder.rfind(':') {
            Some(index) if !remainder[index + 1..].contains('/') => {
                (&remainder[..index], Some(&remainder[index + 1..]))
            }
            _ => (remainder, None),
        };
        if let Some(tag) = tag {
            validate_tag(tag)?;
        }
        validate_name(name)?;

        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ImageReference {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

const fn invalid(reason: &'static str) -> InvalidImageReference {
    InvalidImageReference { reason }
}

fn validate_name(name: &str) -> Result<(), InvalidImageReference> {
    if name.is_empty() {
        return Err(invalid("empty name"));
    }
    if name.len() > NAME_TOTAL_LENGTH_MAX {
        return Err(invalid("name exceeds 255 bytes"));
    }
    let mut components = name.split('/');
    let first = components.next().unwrap_or_default();
    let mut path_components: Vec<&str> = Vec::new();
    // The first component is a registry domain only when it looks like one and
    // more components follow; otherwise it is part of the path.
    let rest: Vec<&str> = components.collect();
    if !rest.is_empty() && is_domain_shaped(first) {
        validate_domain(first)?;
        path_components.extend(rest);
    } else {
        path_components.push(first);
        path_components.extend(rest);
    }
    if path_components.is_empty() {
        return Err(invalid("empty path"));
    }
    for component in path_components {
        validate_path_component(component)?;
    }
    Ok(())
}

fn is_domain_shaped(component: &str) -> bool {
    component == "localhost" || component.contains('.') || component.contains(':')
}

fn validate_domain(domain: &str) -> Result<(), InvalidImageReference> {
    let (host, port) = match domain.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (domain, None),
    };
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid("registry port is not numeric"));
        }
    }
    if host.is_empty() {
        return Err(invalid("empty registry host"));
    }
    for label in host.split('.') {
        if label.is_empty() {
            return Err(invalid("empty registry host label"));
        }
        if !label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(invalid("registry host label has an invalid character"));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(invalid("registry host label starts or ends with '-'"));
        }
    }
    Ok(())
}

/// `alpha-numeric [ separator alpha-numeric ]*` where `alpha-numeric` is
/// `[a-z0-9]+` and `separator` is one of `.`, `_`, `__` or one-or-more `-`.
fn validate_path_component(component: &str) -> Result<(), InvalidImageReference> {
    if component.is_empty() {
        return Err(invalid("empty path component"));
    }
    let bytes = component.as_bytes();
    let mut index = 0;
    loop {
        let start = index;
        while index < bytes.len()
            && (bytes[index].is_ascii_lowercase() || bytes[index].is_ascii_digit())
        {
            index += 1;
        }
        if index == start {
            return Err(invalid(
                "path component must be lowercase alphanumerics with '.', '_' or '-' separators",
            ));
        }
        if index == bytes.len() {
            return Ok(());
        }
        let separator_start = index;
        match bytes[index] {
            b'.' => index += 1,
            b'_' => {
                index += 1;
                if index < bytes.len() && bytes[index] == b'_' {
                    index += 1;
                }
            }
            b'-' => {
                while index < bytes.len() && bytes[index] == b'-' {
                    index += 1;
                }
            }
            _ => return Err(invalid("path component has an invalid character")),
        }
        if index == bytes.len() || separator_start == index {
            return Err(invalid("path component ends with a separator"));
        }
    }
}

/// `[A-Za-z0-9_][A-Za-z0-9._-]{0,127}`.
fn validate_tag(tag: &str) -> Result<(), InvalidImageReference> {
    if tag.is_empty() || tag.len() > 128 {
        return Err(invalid("tag must be 1..=128 characters"));
    }
    let mut bytes = tag.bytes();
    let first = bytes.next().unwrap_or(b'\0');
    if !(first.is_ascii_alphanumeric() || first == b'_') {
        return Err(invalid("tag must start with an alphanumeric or '_'"));
    }
    if !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')) {
        return Err(invalid("tag has an invalid character"));
    }
    Ok(())
}

/// `algorithm ":" hex`, algorithm `[a-z0-9]+([+._-][a-z0-9]+)*`, hex at least
/// 32 characters.
fn validate_digest(digest: &str) -> Result<(), InvalidImageReference> {
    let Some((algorithm, hex)) = digest.split_once(':') else {
        return Err(invalid("digest is missing its algorithm"));
    };
    if algorithm.is_empty() || hex.len() < 32 {
        return Err(invalid("digest algorithm or hex is too short"));
    }
    let mut expect_alphanumeric = true;
    for byte in algorithm.bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            expect_alphanumeric = false;
        } else if matches!(byte, b'+' | b'.' | b'_' | b'-') && !expect_alphanumeric {
            expect_alphanumeric = true;
        } else {
            return Err(invalid("digest algorithm has an invalid character"));
        }
    }
    if expect_alphanumeric {
        return Err(invalid("digest algorithm ends with a separator"));
    }
    if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("digest hex has a non-hex character"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Prepared arguments
// ---------------------------------------------------------------------------

/// A finished Docker command line plus the resources that keep its secrets off
/// argv: mode-0600 env files (unlinked on drop) and the variables the caller
/// must place in the `docker` client's own process environment.
#[derive(Debug)]
pub struct PreparedDockerArgs {
    args: Vec<String>,
    /// Held only for its `Drop`: the referenced env files are unlinked when
    /// the prepared command line is discarded.
    #[allow(dead_code)]
    env_files: Vec<EnvFile>,
    process_env: Vec<(String, String)>,
}

impl PreparedDockerArgs {
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    #[must_use]
    pub fn process_env(&self) -> &[(String, String)] {
        &self.process_env
    }
}

/// A mode-0600 env file that is unlinked when the command that referenced it
/// is done.
#[derive(Debug)]
struct EnvFile {
    path: PathBuf,
}

impl Drop for EnvFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// A value an env file cannot carry: docker's env-file parser is line
/// oriented, rejects names containing whitespace, and treats `#` as a comment
/// introducer.
fn needs_process_environment(name: &str, value: &str) -> bool {
    value.contains('\n')
        || value.contains('\r')
        || name.is_empty()
        || name.contains('=')
        || name.starts_with('#')
        || name.chars().any(char::is_whitespace)
}

fn write_env_file(dir: &Path, entries: &[(String, String)]) -> io::Result<EnvFile> {
    fs::create_dir_all(dir)?;
    let counter = ENV_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = dir.join(format!("env-{}-{counter}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&path)?;
    for (name, value) in entries {
        writeln!(file, "{name}={value}")?;
    }
    Ok(EnvFile { path })
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Flag-phase sink shared by the two builders, so a helper that appends
/// operator-controlled flags (mounts, labels, cgroup) can serve both without
/// ever gaining access to the operand phase.
pub trait FlagSink {
    fn push_flag(&mut self, arg: String);

    fn flag(&mut self, arg: impl Into<String>) -> &mut Self {
        self.push_flag(arg.into());
        self
    }

    fn flags<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for arg in args {
            self.push_flag(arg.into());
        }
        self
    }

    /// `flag value` as one unit, for readability at call sites.
    fn pair(&mut self, flag: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.push_flag(flag.into());
        self.push_flag(value.into());
        self
    }
}

#[derive(Debug, Clone)]
enum Slot {
    Arg(String),
    Env(String, String),
}

fn render(slots: Vec<Slot>, env_dir: &Path) -> io::Result<PreparedDockerArgs> {
    let mut args = Vec::with_capacity(slots.len());
    let mut env_files = Vec::new();
    let mut process_env = Vec::new();
    // Consecutive env entries become one env file placed exactly where they
    // were declared, so `docker`'s last-wins override order is unchanged.
    let mut pending: Vec<(String, String)> = Vec::new();
    let flush = |pending: &mut Vec<(String, String)>,
                 args: &mut Vec<String>,
                 env_files: &mut Vec<EnvFile>|
     -> io::Result<()> {
        if pending.is_empty() {
            return Ok(());
        }
        let file = write_env_file(env_dir, pending)?;
        args.push("--env-file".to_owned());
        args.push(file.path.display().to_string());
        env_files.push(file);
        pending.clear();
        Ok(())
    };
    for slot in slots {
        match slot {
            Slot::Arg(arg) => {
                flush(&mut pending, &mut args, &mut env_files)?;
                args.push(arg);
            }
            Slot::Env(name, value) => {
                if needs_process_environment(&name, &value) {
                    flush(&mut pending, &mut args, &mut env_files)?;
                    args.push("-e".to_owned());
                    args.push(name.clone());
                    process_env.push((name, value));
                } else {
                    pending.push((name, value));
                }
            }
        }
    }
    flush(&mut pending, &mut args, &mut env_files)?;
    Ok(PreparedDockerArgs {
        args,
        env_files,
        process_env,
    })
}

/// Flag phase of a Docker command that carries environment.
#[derive(Debug)]
pub struct DockerCommand {
    slots: Vec<Slot>,
    env_dir: PathBuf,
}

impl DockerCommand {
    /// Start `docker <subcommand...>`. The subcommand is operator-controlled
    /// by construction: it is always a literal in runner code.
    pub fn new<I, S>(env_dir: impl Into<PathBuf>, subcommand: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            slots: subcommand
                .into_iter()
                .map(|part| Slot::Arg(part.into()))
                .collect(),
            env_dir: env_dir.into(),
        }
    }

    /// Record one environment variable. It never reaches argv as
    /// `NAME=VALUE`; see [`render`].
    pub fn env(&mut self, name: impl Into<String>, value: impl Into<String>) -> &mut Self {
        self.slots.push(Slot::Env(name.into(), value.into()));
        self
    }

    pub fn envs<'a, I>(&mut self, entries: I) -> &mut Self
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        for (name, value) in entries {
            self.env(name, value);
        }
        self
    }

    /// Enter the operand phase with a validated image. Emits `--` first, so
    /// nothing after it can be read as a flag.
    #[must_use]
    pub fn image(mut self, image: &ImageReference) -> DockerOperands {
        self.slots.push(Slot::Arg(END_OF_FLAGS.to_owned()));
        self.slots.push(Slot::Arg(image.as_str().to_owned()));
        DockerOperands {
            slots: self.slots,
            env_dir: self.env_dir,
        }
    }

    /// Enter the operand phase without an image (for example `docker exec`,
    /// whose first operand is a container name). Emits `--`.
    #[must_use]
    pub fn operands(mut self) -> DockerOperands {
        self.slots.push(Slot::Arg(END_OF_FLAGS.to_owned()));
        DockerOperands {
            slots: self.slots,
            env_dir: self.env_dir,
        }
    }
}

impl FlagSink for DockerCommand {
    fn push_flag(&mut self, arg: String) {
        self.slots.push(Slot::Arg(arg));
    }
}

impl FlagSink for DockerArgv {
    fn push_flag(&mut self, arg: String) {
        self.argv.push(arg);
    }
}

/// Operand phase: no method here can append a flag.
#[derive(Debug)]
pub struct DockerOperands {
    slots: Vec<Slot>,
    env_dir: PathBuf,
}

impl DockerOperands {
    #[must_use]
    pub fn operand(mut self, value: impl Into<String>) -> Self {
        self.slots.push(Slot::Arg(value.into()));
        self
    }

    #[must_use]
    pub fn operands<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.slots
            .extend(values.into_iter().map(|value| Slot::Arg(value.into())));
        self
    }

    /// # Errors
    /// Creating or writing an env file failed.
    pub fn finish(self) -> io::Result<PreparedDockerArgs> {
        render(self.slots, &self.env_dir)
    }
}

/// Flag phase of a Docker command that carries no environment (lifecycle and
/// inspection commands). Same `--` guarantee, no env-file machinery.
#[derive(Debug, Default)]
pub struct DockerArgv {
    argv: Vec<String>,
}

impl DockerArgv {
    pub fn new<I, S>(subcommand: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            argv: subcommand.into_iter().map(Into::into).collect(),
        }
    }

    #[must_use]
    pub fn image(mut self, image: &ImageReference) -> DockerArgvOperands {
        self.argv.push(END_OF_FLAGS.to_owned());
        self.argv.push(image.as_str().to_owned());
        DockerArgvOperands { argv: self.argv }
    }

    #[must_use]
    pub fn operands(mut self) -> DockerArgvOperands {
        self.argv.push(END_OF_FLAGS.to_owned());
        DockerArgvOperands { argv: self.argv }
    }
}

/// Operand phase for env-free commands.
#[derive(Debug)]
pub struct DockerArgvOperands {
    argv: Vec<String>,
}

impl DockerArgvOperands {
    #[must_use]
    pub fn operand(mut self, value: impl Into<String>) -> Self {
        self.argv.push(value.into());
        self
    }

    #[must_use]
    pub fn operands<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.argv.extend(values.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn into_argv(self) -> Vec<String> {
        self.argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_references() {
        for raw in [
            "alpine",
            "alpine:3.22",
            "ubuntu:24.04",
            "library/postgres:16",
            "ghcr.io/owner/name:v1.2.3",
            "registry.local:5000/team/app:latest",
            "velnor-action-acme-docker-v1-root",
            "node@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(ImageReference::parse(raw).is_ok(), "{raw} should parse");
        }
    }

    /// The exact fork-PR payload: `runs.image: docker://--privileged`, whose
    /// `docker://` prefix is stripped before the reference reaches Docker.
    #[test]
    fn rejects_flag_shaped_image_references() {
        for raw in [
            "--privileged",
            "-v/:/host",
            "--user=0:0",
            "--volume=/:/mnt",
            "-",
        ] {
            let error = ImageReference::parse(raw).expect_err("{raw} must be rejected");
            assert!(!error.reason.is_empty());
        }
    }

    #[test]
    fn rejects_malformed_references() {
        for raw in [
            "",
            "   ",
            "UPPERCASE",
            "alpine:",
            "alpine::3",
            "alpine:-tag",
            "with space",
            "with\nnewline",
            "alpine@sha256:short",
            "alpine@nodigest",
            "-alpine",
            "alpine/",
            "/alpine",
            "alpine_-name",
        ] {
            assert!(
                ImageReference::parse(raw).is_err(),
                "{raw:?} must be rejected"
            );
        }
    }

    /// Without `--`, docker parses the image as a flag. With it, the operand
    /// phase is unreachable from the flag phase.
    #[test]
    fn image_is_always_preceded_by_the_end_of_flags_separator() {
        let image = ImageReference::parse("alpine:3.22").unwrap();
        let mut command = DockerArgv::new(["run"]);
        command.flags(["--rm"]);
        let argv = command
            .image(&image)
            .operand("echo")
            .operand("hi")
            .into_argv();
        assert_eq!(
            argv,
            vec!["run", "--rm", "--", "alpine:3.22", "echo", "hi"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }

    /// An action's `runs.args` are attacker-controlled. After `--` they can
    /// only ever be operands of the container entrypoint.
    #[test]
    fn operands_that_look_like_flags_stay_after_the_separator() {
        let image = ImageReference::parse("alpine:3.22").unwrap();
        let argv = DockerArgv::new(["run"])
            .image(&image)
            .operands(["--privileged", "-v", "/:/host"])
            .into_argv();
        let separator = argv.iter().position(|arg| arg == "--").unwrap();
        for (index, arg) in argv.iter().enumerate() {
            if arg.starts_with('-') && arg != "--" {
                assert!(index > separator, "{arg} escaped the separator");
            }
        }
    }

    #[test]
    fn environment_never_reaches_argv() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-docker-argv-{}-{}",
            std::process::id(),
            line!()
        ));
        let mut command = DockerCommand::new(&dir, ["run"]);
        command
            .flag("--rm")
            .env("GITHUB_TOKEN", "ghs_supersecret")
            .env("PLAIN", "value");
        let image = ImageReference::parse("alpine:3.22").unwrap();
        let prepared = command.image(&image).finish().unwrap();
        for arg in prepared.args() {
            assert!(!arg.contains("ghs_supersecret"), "secret on argv: {arg}");
            assert!(!arg.contains("PLAIN=value"), "env pair on argv: {arg}");
        }
        assert!(prepared.args().contains(&"--env-file".to_owned()));
        let file = PathBuf::from(
            &prepared.args()[prepared
                .args()
                .iter()
                .position(|arg| arg == "--env-file")
                .unwrap()
                + 1],
        );
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "GITHUB_TOKEN=ghs_supersecret\nPLAIN=value\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(prepared);
        assert!(!file.exists(), "env file must be unlinked");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn multiline_values_are_forwarded_through_the_process_environment() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-docker-argv-{}-{}",
            std::process::id(),
            line!()
        ));
        let mut command = DockerCommand::new(&dir, ["exec"]);
        command.env("KEY", "-----BEGIN\nline\n-----END");
        let prepared = command.operands().operand("job").finish().unwrap();
        assert!(prepared.args().contains(&"-e".to_owned()));
        assert!(prepared.args().contains(&"KEY".to_owned()));
        for arg in prepared.args() {
            assert!(!arg.contains("BEGIN"), "secret on argv: {arg}");
        }
        assert_eq!(
            prepared.process_env(),
            &[("KEY".to_owned(), "-----BEGIN\nline\n-----END".to_owned())]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Declaration order is docker's override order, so env files must land
    /// where the entries were declared rather than being hoisted.
    #[test]
    fn env_files_keep_declaration_order() {
        let dir = std::env::temp_dir().join(format!(
            "velnor-docker-argv-{}-{}",
            std::process::id(),
            line!()
        ));
        let mut command = DockerCommand::new(&dir, ["run"]);
        command.env("A", "1").flag("--rm").env("A", "2");
        let prepared = command.operands().finish().unwrap();
        let files: Vec<usize> = prepared
            .args()
            .iter()
            .enumerate()
            .filter(|(_, arg)| *arg == "--env-file")
            .map(|(index, _)| index)
            .collect();
        assert_eq!(files.len(), 2);
        let rm = prepared
            .args()
            .iter()
            .position(|arg| arg == "--rm")
            .unwrap();
        assert!(files[0] < rm && rm < files[1]);
        let _ = fs::remove_dir_all(&dir);
    }
}
