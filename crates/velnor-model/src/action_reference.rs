//! Shared GitHub Action container-reference parsing.
//!
//! `actions/runner` classifies a container action from the metadata value
//! before preparing it.  The workflow scanner and the Velnor runner must make
//! that same decision, and must share the same image grammar, or a scanned
//! action can reach a different runtime arm.

use std::fmt;
use std::path::{Path, PathBuf};

/// A validated OCI image reference without the `docker://` action scheme.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageReference(String);

/// Why an action image value was rejected. The offending value is deliberately
/// not retained because it is repository-controlled input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidImageReference {
    pub reason: &'static str,
}

impl fmt::Display for InvalidImageReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid image reference: {}", self.reason)
    }
}

impl std::error::Error for InvalidImageReference {}

/// A repository action reference pinned to an immutable commit.
///
/// This is the one strict parser for downloaded action metadata references:
/// `owner/repository[/path]@<40-hex-SHA>`. Workflow fields that carry the
/// repository, path, and ref separately use [`Self::from_parts`] so admission
/// and planning share exactly the same validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryActionReference {
    pub repository: String,
    pub source_path: Option<String>,
    pub git_ref: String,
}

/// Why a repository action reference was rejected. The received value is not
/// retained because it is repository-controlled input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidRepositoryActionReference {
    pub reason: &'static str,
}

impl fmt::Display for InvalidRepositoryActionReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid repository action reference: {}",
            self.reason
        )
    }
}

impl std::error::Error for InvalidRepositoryActionReference {}

impl RepositoryActionReference {
    /// Parse `owner/repository[/path]@<40-hex-SHA>` without normalization.
    pub fn parse(raw: &str) -> Result<Self, InvalidRepositoryActionReference> {
        let (path, git_ref) = raw
            .rsplit_once('@')
            .ok_or(invalid_repository_action("reference is missing @SHA"))?;
        if !is_full_sha(git_ref) {
            return Err(invalid_repository_action(
                "reference must end in a 40-hex commit SHA",
            ));
        }
        let parts = path.split('/').collect::<Vec<_>>();
        if parts.len() < 2 || parts.iter().any(|part| !valid_repository_path_part(part)) {
            return Err(invalid_repository_action(
                "reference must be owner/repository with an optional safe path",
            ));
        }
        Ok(Self {
            repository: format!("{}/{}", parts[0], parts[1]),
            source_path: (parts.len() > 2).then(|| parts[2..].join("/")),
            git_ref: git_ref.to_owned(),
        })
    }

    /// Validate separate workflow fields through the same strict parser.
    pub fn from_parts(
        repository: &str,
        source_path: Option<&str>,
        git_ref: &str,
    ) -> Result<Self, InvalidRepositoryActionReference> {
        let path = source_path.map_or_else(
            || repository.to_owned(),
            |source_path| format!("{repository}/{source_path}"),
        );
        Self::parse(&format!("{path}@{git_ref}"))
    }
}

fn invalid_repository_action(reason: &'static str) -> InvalidRepositoryActionReference {
    InvalidRepositoryActionReference { reason }
}

fn is_full_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_repository_path_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && !part.contains('\\')
        && !part.contains('@')
        && !part
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

/// A normalized, repository-relative action metadata path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeActionPath(PathBuf);

/// Why a metadata path could not be safely resolved below an action root.
#[derive(Debug)]
pub enum InvalidActionPath {
    Invalid(&'static str),
    Io(std::io::Error),
}

impl fmt::Display for InvalidActionPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(formatter, "invalid action path: {reason}"),
            Self::Io(error) => write!(formatter, "inspect action path: {error}"),
        }
    }
}

impl std::error::Error for InvalidActionPath {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(_) => None,
            Self::Io(error) => Some(error),
        }
    }
}

impl SafeActionPath {
    /// Parse a metadata path without accepting platform-specific escapes.
    pub fn parse(raw: &str) -> Result<Self, InvalidActionPath> {
        if raw.is_empty() {
            return Err(invalid_action_path("path is empty"));
        }
        if raw.contains('\\') {
            return Err(invalid_action_path("backslash is not allowed"));
        }
        if raw.starts_with('/') || has_windows_drive_prefix(raw) {
            return Err(invalid_action_path(
                "path must be relative to the action root",
            ));
        }
        let mut components = Vec::new();
        for component in raw.split('/') {
            match component {
                "" | "." => {}
                ".." => return Err(invalid_action_path("path traversal is not allowed")),
                component if has_windows_drive_prefix(component) => {
                    return Err(invalid_action_path(
                        "Windows drive prefixes are not allowed",
                    ));
                }
                component if component.chars().any(char::is_control) => {
                    return Err(invalid_action_path("control characters are not allowed"));
                }
                component => components.push(component),
            }
        }
        if components.is_empty() {
            return Err(invalid_action_path("path is empty"));
        }
        Ok(Self(PathBuf::from(components.join("/"))))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Resolve a metadata path below `action_root` and reject symlink traversal.
/// Missing final files are allowed so callers can report their own metadata
/// missing-file diagnostic; every existing component is checked without
/// following links.
pub fn resolve_action_path(action_root: &Path, raw: &str) -> Result<PathBuf, InvalidActionPath> {
    let relative = SafeActionPath::parse(raw)?;
    let resolved = action_root.join(relative.as_path());
    let mut existing = action_root.to_path_buf();
    for component in relative.as_path().components() {
        existing.push(component.as_os_str());
        match std::fs::symlink_metadata(&existing) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(invalid_action_path("path traverses a symlink"));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                break
            }
            Err(error) => return Err(InvalidActionPath::Io(error)),
        }
    }
    Ok(resolved)
}

fn invalid_action_path(reason: &'static str) -> InvalidActionPath {
    InvalidActionPath::Invalid(reason)
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// The runner's two supported `runs.image` classes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionImageReference {
    /// A remote image after removing the case-insensitive `docker://` scheme.
    DockerImage(ImageReference),
    /// A host-local Dockerfile path classified by the runner's basename rule.
    Dockerfile(String),
}

impl ActionImageReference {
    /// Parse the metadata `runs.image` value using the runner's classification
    /// order: an ordinal-ignore-case `docker://` scheme always means an image;
    /// otherwise only a Dockerfile basename is a local build source.
    pub fn parse(raw: &str) -> Result<Self, InvalidImageReference> {
        if raw.is_empty() {
            return Err(invalid("empty action image"));
        }
        if raw
            .get(..DOCKER_SCHEME.len())
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case(DOCKER_SCHEME))
        {
            let image = &raw[DOCKER_SCHEME.len()..];
            return ImageReference::parse(image).map(Self::DockerImage);
        }
        if is_dockerfile_reference(raw) {
            return Ok(Self::Dockerfile(raw.to_owned()));
        }
        Err(invalid(
            "action image must be a Dockerfile path or docker:// image",
        ))
    }
}

impl ImageReference {
    /// Parse `[domain[:port]/]path[:tag][@digest]`.
    pub fn parse(raw: &str) -> Result<Self, InvalidImageReference> {
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

        let (remainder, digest) = match raw.split_once('@') {
            Some((remainder, digest)) => (remainder, Some(digest)),
            None => (raw, None),
        };
        if let Some(digest) = digest {
            validate_digest(digest)?;
        }

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

impl fmt::Display for ImageReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

const DOCKER_SCHEME: &str = "docker://";
const NAME_TOTAL_LENGTH_MAX: usize = 255;

const fn invalid(reason: &'static str) -> InvalidImageReference {
    InvalidImageReference { reason }
}

fn is_dockerfile_reference(value: &str) -> bool {
    if value
        .get(..DOCKER_SCHEME.len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case(DOCKER_SCHEME))
    {
        return false;
    }
    let basename = value.rsplit('/').next().unwrap_or(value);
    let basename = basename.to_ascii_lowercase();
    basename.starts_with("dockerfile.") || basename.ends_with("dockerfile")
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
    let rest: Vec<&str> = components.collect();
    let mut path_components = Vec::new();
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
    if let Some(port) = port
        && (port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(invalid("registry port is not numeric"));
    }
    if host.is_empty() {
        return Err(invalid("empty registry host"));
    }
    for label in host.split('.') {
        if label.is_empty()
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || label.starts_with('-')
            || label.ends_with('-')
        {
            return Err(invalid("registry host label is invalid"));
        }
    }
    Ok(())
}

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
        if index == bytes.len() {
            return Err(invalid("path component ends with a separator"));
        }
    }
}

fn validate_tag(tag: &str) -> Result<(), InvalidImageReference> {
    if tag.is_empty() || tag.len() > 128 {
        return Err(invalid("tag must be 1..=128 characters"));
    }
    let mut bytes = tag.bytes();
    let first = bytes.next().unwrap_or(b'\0');
    if !(first.is_ascii_alphanumeric() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    {
        return Err(invalid("tag has an invalid character"));
    }
    Ok(())
}

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
    if expect_alphanumeric || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("digest is invalid"));
    }
    Ok(())
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
    use super::{
        resolve_action_path, ActionImageReference, ImageReference, RepositoryActionReference,
        SafeActionPath,
    };

    #[test]
    fn action_scheme_is_case_insensitive_without_image_normalization() {
        let parsed = ActionImageReference::parse("DOCKER://ubuntu:24.04");
        assert_eq!(
            parsed.as_ref().ok().and_then(|value| match value {
                ActionImageReference::DockerImage(image) => Some(image.as_str()),
                ActionImageReference::Dockerfile(_) => None,
            }),
            Some("ubuntu:24.04")
        );
    }

    #[test]
    fn malformed_or_flag_shaped_images_are_rejected() {
        for value in [
            "docker://--privileged",
            "docker://",
            "docker://ubuntu ",
            "docker:// ubuntu",
            " docker://ubuntu",
            "ubuntu",
            "éééééé",
        ] {
            assert!(ActionImageReference::parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn dockerfile_basename_is_the_only_local_image_class() {
        assert!(matches!(
            ActionImageReference::parse("./nested/Dockerfile"),
            Ok(ActionImageReference::Dockerfile(_))
        ));
        assert!(ActionImageReference::parse("./nested/Dockerfile ").is_err());
        assert!(ImageReference::parse("ubuntu:24.04").is_ok());
    }

    #[test]
    fn strict_repository_action_reference_has_one_owner() {
        let parsed = RepositoryActionReference::parse(
            "octo/example/.github/actions/tool@0123456789abcdef0123456789abcdef01234567",
        )
        .expect("valid pinned action reference");
        assert_eq!(parsed.repository, "octo/example");
        assert_eq!(parsed.source_path.as_deref(), Some(".github/actions/tool"));
        assert_eq!(
            RepositoryActionReference::from_parts(
                "octo/example",
                Some(".github/actions/tool"),
                "0123456789abcdef0123456789abcdef01234567",
            ),
            Ok(parsed)
        );
        for value in [
            "octo/example@main",
            "octo/example@0123456789abcdef0123456789abcdef0123456",
            "octo/example@0123456789abcdef0123456789abcdef012345678",
            "octo/example/../tool@0123456789abcdef0123456789abcdef01234567",
            "octo/example\\tool@0123456789abcdef0123456789abcdef01234567",
            " octo/example@0123456789abcdef0123456789abcdef01234567",
        ] {
            assert!(RepositoryActionReference::parse(value).is_err(), "{value}");
        }
    }

    #[test]
    fn safe_action_path_rejects_cross_platform_escapes() {
        for value in [
            "",
            ".",
            "/etc/passwd",
            "../outside.js",
            "nested/../../outside.js",
            "nested\\outside.js",
            "C:/outside.js",
            "C:outside.js",
            "nested/C:outside.js",
        ] {
            assert!(SafeActionPath::parse(value).is_err(), "{value:?}");
        }
        assert_eq!(
            SafeActionPath::parse("./dist/./index.js")
                .expect("safe relative path")
                .as_path(),
            std::path::Path::new("dist/index.js")
        );
    }

    #[cfg(unix)]
    #[test]
    fn safe_action_path_rejects_symlink_components() {
        use std::fs;
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("velnor-safe-action-path-{}", std::process::id()));
        let outside = root.join("outside");
        let action = root.join("action");
        let _ = fs::remove_dir_all(&root);
        assert!(fs::create_dir_all(&outside).is_ok());
        assert!(fs::create_dir_all(&action).is_ok());
        assert!(symlink(&outside, action.join("link")).is_ok());
        assert!(resolve_action_path(&action, "link/entry.js").is_err());
        let _ = fs::remove_dir_all(root);
    }
}
