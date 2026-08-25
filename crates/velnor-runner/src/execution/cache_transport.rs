//! Digest-verified cache import and success-only publish. No host bind mounts.

use super::hex_sha256;

/// One content-addressed blob for vsock import/export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheBlob {
    pub digest_sha256: String,
    pub bytes: Vec<u8>,
}

/// Cache transport errors. Never fall back to host directory passthrough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheTransportError {
    pub requirement: &'static str,
    pub detail: String,
}

impl std::fmt::Display for CacheTransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cache transport {}: {}; virtio-fs/host bind was not used",
            self.requirement, self.detail
        )
    }
}

impl std::error::Error for CacheTransportError {}

impl CacheBlob {
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            digest_sha256: hex_sha256(&bytes),
            bytes,
        }
    }

    /// Import only when the digest matches.
    ///
    /// # Errors
    /// Digest mismatch.
    pub fn import(&self) -> Result<&[u8], CacheTransportError> {
        let actual = hex_sha256(&self.bytes);
        if actual != self.digest_sha256 {
            return Err(CacheTransportError {
                requirement: "cache.digest",
                detail: format!("declared {} != actual {actual}", self.digest_sha256),
            });
        }
        Ok(&self.bytes)
    }
}

/// Publish only after a successful job. Failure returns no blob.
///
/// # Errors
/// Digest mismatch on the candidate.
pub fn publish_on_success(
    job_succeeded: bool,
    blob: CacheBlob,
) -> Result<Option<CacheBlob>, CacheTransportError> {
    if !job_succeeded {
        return Ok(None);
    }
    blob.import()?;
    Ok(Some(blob))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mismatch_and_failed_job_do_not_publish() {
        let mut blob = CacheBlob::from_bytes(b"warm-cache".to_vec());
        blob.digest_sha256 = "deadbeef".into();
        assert!(blob.import().is_err());
        let blob = CacheBlob::from_bytes(b"warm-cache".to_vec());
        assert!(publish_on_success(false, blob.clone()).unwrap().is_none());
        let published = publish_on_success(true, blob).unwrap().unwrap();
        assert_eq!(published.bytes, b"warm-cache");
    }
}
