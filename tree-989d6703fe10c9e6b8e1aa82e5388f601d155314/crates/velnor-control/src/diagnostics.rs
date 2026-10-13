//! Bounded, allowlisted diagnostics collection.
//!
//! Collectors are explicit application ports. The service never sweeps an
//! environment, journald, Docker inspect output, job messages, or arbitrary
//! paths merely because a masker exists.

use sha2::{Digest, Sha256};

use crate::ports::PortError;

const MAX_MEMBER_BYTES: usize = 256 * 1024;
const MAX_TOTAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_COLLECTORS: usize = 256;
const MAX_MEMBER_NAME_BYTES: usize = 256;

/// One allowlisted diagnostic collector.
pub trait Collector: Send + Sync {
    /// Stable source/member name.
    fn name(&self) -> &str;
    /// Collect already bounded, sanitized bytes.
    fn collect(&self) -> Result<Vec<u8>, PortError>;
}

/// One archive member included in a diagnostic bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleMember {
    /// Canonical relative member path.
    pub name: String,
    /// SHA-256 digest of the member bytes.
    pub digest: String,
    /// Byte length.
    pub bytes: usize,
    /// Redaction version used.
    pub redaction_version: u32,
    /// Sanitized bytes.
    pub content: Vec<u8>,
}

/// One omitted collector and safe reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleOmission {
    /// Collector/member name.
    pub name: String,
    /// Safe failure reason.
    pub reason: String,
}

/// Deterministic diagnostic bundle model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticBundle {
    /// Stable manifest version.
    pub manifest_version: u32,
    /// Included members in canonical order.
    pub members: Vec<BundleMember>,
    /// Explicit partial-failure entries.
    pub omissions: Vec<BundleOmission>,
}

/// Bounded diagnostics collector service.
pub struct DiagnosticsService {
    collectors: Vec<Box<dyn Collector>>,
    secrets: Vec<String>,
}

impl DiagnosticsService {
    /// Build a collector set from explicitly allowlisted sources.
    #[must_use]
    pub fn new(collectors: Vec<Box<dyn Collector>>, secrets: Vec<String>) -> Self {
        Self {
            collectors,
            secrets,
        }
    }

    /// Collect, redact, validate, and deterministically order the bundle.
    pub fn collect(&self) -> Result<DiagnosticBundle, PortError> {
        if self.collectors.len() > MAX_COLLECTORS {
            return Err(PortError::Unavailable {
                resource: "diagnostic collector budget exhausted".to_owned(),
            });
        }
        let mut members = Vec::new();
        let mut omissions = Vec::new();
        let mut total = 0_usize;
        for collector in &self.collectors {
            let name = collector.name();
            if name.is_empty() || name.len() > MAX_MEMBER_NAME_BYTES {
                omissions.push(BundleOmission {
                    name: name.to_owned(),
                    reason: "collector name exceeds the bounded size".to_owned(),
                });
                continue;
            }
            let raw = match collector.collect() {
                Ok(raw) => raw,
                Err(error) => {
                    omissions.push(BundleOmission {
                        name: name.to_owned(),
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            if raw.len() > MAX_MEMBER_BYTES {
                omissions.push(BundleOmission {
                    name: name.to_owned(),
                    reason: "collector byte limit exceeded".to_owned(),
                });
                continue;
            }
            let mut content = String::from_utf8_lossy(&raw).into_owned();
            for secret in &self.secrets {
                if !secret.is_empty() {
                    content = content.replace(secret, "[REDACTED]");
                }
            }
            if content.len() > MAX_MEMBER_BYTES
                || total.saturating_add(content.len()) > MAX_TOTAL_BYTES
            {
                omissions.push(BundleOmission {
                    name: name.to_owned(),
                    reason: "sanitized collector byte limit exceeded".to_owned(),
                });
                continue;
            }
            if content.contains("<html") || content.contains("<!DOCTYPE html") {
                return Err(PortError::Operation {
                    operation: format!("diagnostic collector {name} produced prohibited HTML"),
                });
            }
            let content = content.into_bytes();
            let digest = format!("sha256:{}", hex_digest(&Sha256::digest(&content)));
            total = total.saturating_add(content.len());
            members.push(BundleMember {
                name: name.to_owned(),
                digest,
                bytes: content.len(),
                redaction_version: 1,
                content,
            });
        }
        members.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(DiagnosticBundle {
            manifest_version: 1,
            members,
            omissions,
        })
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed {
        name: &'static str,
        content: &'static [u8],
    }

    impl Collector for Fixed {
        fn name(&self) -> &str {
            self.name
        }
        fn collect(&self) -> Result<Vec<u8>, PortError> {
            Ok(self.content.to_vec())
        }
    }

    #[test]
    fn bundle_redacts_and_sorts_allowlisted_members() {
        let service = DiagnosticsService::new(
            vec![
                Box::new(Fixed {
                    name: "z.json",
                    content: br#"{"token":"secret"}"#,
                }),
                Box::new(Fixed {
                    name: "a.json",
                    content: br#"{"ok":true}"#,
                }),
            ],
            vec!["secret".to_owned()],
        );
        let bundle = service.collect().expect("bundle");
        assert_eq!(bundle.members[0].name, "a.json");
        assert!(!String::from_utf8_lossy(&bundle.members[1].content).contains("secret"));
    }
}
