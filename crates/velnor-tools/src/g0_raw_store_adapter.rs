//! Narrow producer adapter for checker-owned G0 raw-byte reads.
//!
//! The checker must reopen bytes from the producer's immutable store.  This
//! module never decodes `G0RawObjectRef.bytes_base64` and never opens a path
//! from a serialized evidence value.  Until the store exposes a safe-byte
//! reader, it accepts only records whose original and safe objects are the
//! same immutable object; masked records fail closed rather than returning
//! unauthenticated bytes.

use super::raw_store::RawObjectFileStore;
use super::{sha256_digest, RawObjectRef, RawObjectStore};
use crate::g0_contract::{G0InventoryEvidence, G0RawObjectRef};
use crate::live_authority::VerifiedRawStore;
use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::io::Read;

const MAX_RAW_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

/// Reopen and verify one checker raw object through the producer CAS.
///
/// The current shared store exposes a descriptor-relative original-byte
/// reader.  A safe response can be returned through this seam only when the
/// store proves that its original and safe digests/lengths are identical.
/// Credential-masked responses remain unavailable until the store supplies a
/// matching descriptor-relative safe-byte reader.
pub(crate) fn read_g0_raw(store: &RawObjectFileStore, raw: &G0RawObjectRef) -> Result<Vec<u8>> {
    if raw.byte_length > MAX_RAW_OBJECT_BYTES || raw.original_byte_length > MAX_RAW_OBJECT_BYTES {
        bail!("G0 raw object {} exceeds immutable byte bound", raw.raw_id);
    }
    if raw.sha256 != raw.original_sha256
        || raw.byte_length != raw.original_byte_length
        || raw.storage_ref != raw.original_storage_ref
    {
        bail!(
            "G0 raw object {} has distinct safe/original bytes; safe CAS reader is unavailable",
            raw.raw_id
        );
    }

    let reference = raw_reference(raw);
    store
        .verify(&reference)
        .map_err(|error| anyhow::anyhow!("verify G0 raw object {}: {error:?}", raw.raw_id))?;

    let limit = raw
        .byte_length
        .checked_add(1)
        .context("G0 raw byte bound overflow")?;
    let reader = store
        .open_original_for_checkout(&raw.original_storage_ref)
        .map_err(|error| anyhow::anyhow!("open G0 raw object {}: {error:?}", raw.raw_id))?;
    let mut bytes = Vec::new();
    reader
        .take(limit)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read G0 raw object {}", raw.raw_id))?;
    if bytes.len() as u64 != raw.byte_length || sha256_digest(&bytes) != raw.sha256 {
        bail!(
            "reopened G0 raw object {} differs from verified safe digest/length",
            raw.raw_id
        );
    }
    Ok(bytes)
}

/// Concrete implementation of the checker seam. Construction requires an
/// explicit producer-selected CAS root; no serialized evidence path reaches
/// this boundary.
pub(crate) struct VerifiedRawStoreAdapter {
    store: RawObjectFileStore,
}

impl VerifiedRawStoreAdapter {
    pub(crate) fn open(root: impl Into<std::path::PathBuf>) -> Result<Self> {
        Ok(Self {
            store: RawObjectFileStore::new(root.into()).context("open producer raw store")?,
        })
    }
}

impl VerifiedRawStore for VerifiedRawStoreAdapter {
    fn verify_g0(&self, inventory: &G0InventoryEvidence) -> Result<()> {
        let mut raw_ids = BTreeSet::new();
        for raw in &inventory.collector_snapshot.raw_objects {
            if !raw_ids.insert(raw.raw_id.clone()) {
                bail!("G0 snapshot repeats raw object {}", raw.raw_id);
            }
            let reference = raw_reference(raw);
            self.store.verify(&reference).map_err(|error| {
                anyhow::anyhow!("verify G0 raw object {}: {error:?}", raw.raw_id)
            })?;
        }
        Ok(())
    }

    fn read_g0_raw(&self, raw: &G0RawObjectRef) -> Result<Vec<u8>> {
        read_g0_raw(&self.store, raw)
    }
}

fn raw_reference(raw: &G0RawObjectRef) -> RawObjectRef {
    RawObjectRef {
        raw_id: raw.raw_id.clone(),
        request_id: raw.request_id.clone(),
        object_kind: raw.object_kind.clone(),
        canonicalization: raw.canonicalization.clone(),
        sha256: raw.sha256.clone(),
        byte_length: raw.byte_length,
        original_sha256: raw.original_sha256.clone(),
        original_byte_length: raw.original_byte_length,
        bytes_base64: raw.bytes_base64.clone(),
        media_type: raw.media_type.clone(),
        storage_ref: raw.storage_ref.clone(),
        original_storage_ref: raw.original_storage_ref.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_acquisition::{RawObject, RawObjectStore};
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn fixture_root() -> anyhow::Result<PathBuf> {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        Ok(std::env::current_dir()
            .context("find test working directory")?
            .join(".github-raw-store-fixtures")
            .join(format!("velnor-g0-raw-adapter-{}-{id}", std::process::id())))
    }

    fn remove_fixture(path: &Path) {
        let _ = fs::remove_dir_all(path);
        let Some(name) = path.file_name() else {
            return;
        };
        let mut marker = b".velnor-raw-anchor-".to_vec();
        for byte in name.as_bytes() {
            marker.extend(format!("{byte:02x}").bytes());
        }
        let Some(parent) = path.parent() else {
            return;
        };
        let Ok(marker_name) = String::from_utf8(marker) else {
            return;
        };
        let marker = parent.join(marker_name);
        let _ = fs::remove_file(marker);
    }

    fn checker_ref(reference: &RawObjectRef) -> G0RawObjectRef {
        G0RawObjectRef {
            raw_id: reference.raw_id.clone(),
            request_id: reference.request_id.clone(),
            object_kind: reference.object_kind.clone(),
            canonicalization: reference.canonicalization.clone(),
            sha256: reference.sha256.clone(),
            byte_length: reference.byte_length,
            bytes_base64: reference.bytes_base64.clone(),
            media_type: reference.media_type.clone(),
            storage_ref: reference.storage_ref.clone(),
            original_sha256: reference.original_sha256.clone(),
            original_byte_length: reference.original_byte_length,
            original_storage_ref: reference.original_storage_ref.clone(),
        }
    }

    #[test]
    fn read_g0_raw_reopens_verified_descriptor_bytes() -> anyhow::Result<()> {
        let root = fixture_root()?;
        let parent = root
            .parent()
            .ok_or_else(|| anyhow::anyhow!("fixture root has no parent"))?;
        fs::create_dir_all(parent).context("create fixture parent")?;
        fs::create_dir_all(&root).context("create raw store root")?;
        let mut store = RawObjectFileStore::new(&root).context("open raw store")?;
        let reference = store
            .store(RawObject {
                raw_id: "response-1".to_owned(),
                request_id: "request-1".to_owned(),
                object_kind: "repository".to_owned(),
                canonicalization: "raw-bytes-v1".to_owned(),
                media_type: "application/json".to_owned(),
                original_bytes: br#"{"id":1}"#.to_vec(),
                bytes: br#"{"id":1}"#.to_vec(),
            })
            .map_err(|error| anyhow::anyhow!("store raw response: {error:?}"))?;
        let checker = checker_ref(&reference);
        assert_eq!(read_g0_raw(&store, &checker)?, br#"{"id":1}"#);
        drop(store);
        remove_fixture(&root);
        Ok(())
    }

    #[test]
    fn read_g0_raw_rejects_masked_record_until_safe_reader_exists() -> anyhow::Result<()> {
        let root = fixture_root()?;
        let parent = root
            .parent()
            .ok_or_else(|| anyhow::anyhow!("fixture root has no parent"))?;
        fs::create_dir_all(parent).context("create fixture parent")?;
        fs::create_dir_all(&root).context("create raw store root")?;
        let mut store = RawObjectFileStore::new(&root).context("open raw store")?;
        let reference = store
            .store(RawObject {
                raw_id: "response-2".to_owned(),
                request_id: "request-2".to_owned(),
                object_kind: "repository".to_owned(),
                canonicalization: "raw-bytes-v1".to_owned(),
                media_type: "application/json".to_owned(),
                original_bytes: br#"{"id":1,"token":"secret"}"#.to_vec(),
                bytes: br#"{"id":1,"token":"[REDACTED]"}"#.to_vec(),
            })
            .map_err(|error| anyhow::anyhow!("store masked response: {error:?}"))?;
        let checker = checker_ref(&reference);
        let error = read_g0_raw(&store, &checker)
            .err()
            .ok_or_else(|| anyhow::anyhow!("masked response must fail closed"))?;
        assert!(error.to_string().contains("safe CAS reader is unavailable"));
        drop(store);
        remove_fixture(&root);
        Ok(())
    }
}
