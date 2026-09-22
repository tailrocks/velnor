//! Shared GitHub Action container-reference parsing.
//!
//! `actions/runner` classifies a container action from the metadata value
//! before preparing it.  The workflow scanner and the Velnor runner must make
//! that same decision, and must share the same image grammar, or a scanned
//! action can reach a different runtime arm.

use std::fmt;

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
mod tests {
    use super::{ActionImageReference, ImageReference};

    #[test]
    fn action_scheme_is_case_insensitive_but_image_is_normalized() {
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
}
