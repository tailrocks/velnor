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
use crate::g0_contract::G0RawObjectRef;
use anyhow::{bail, Context, Result};
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

    let reference = RawObjectRef {
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
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_acquisition::{RawObject, RawObjectStore};
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn fixture_root() -> PathBuf {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        std::env::current_dir()
            .expect("find test working directory")
            .join(".github-raw-store-fixtures")
            .join(format!("velnor-g0-raw-adapter-{}-{id}", std::process::id()))
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
        let marker = path
            .parent()
            .expect("fixture parent")
            .join(String::from_utf8(marker).expect("marker name"));
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
    fn read_g0_raw_reopens_verified_descriptor_bytes() {
        let root = fixture_root();
        fs::create_dir_all(root.parent().expect("fixture parent")).expect("create fixture parent");
        fs::create_dir_all(&root).expect("create raw store root");
        let mut store = RawObjectFileStore::new(&root).expect("open raw store");
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
            .expect("store raw response");
        let checker = checker_ref(&reference);
        assert_eq!(
            read_g0_raw(&store, &checker).expect("reopen raw response"),
            br#"{"id":1}"#
        );
        drop(store);
        remove_fixture(&root);
    }

    #[test]
    fn read_g0_raw_rejects_masked_record_until_safe_reader_exists() {
        let root = fixture_root();
        fs::create_dir_all(root.parent().expect("fixture parent")).expect("create fixture parent");
        fs::create_dir_all(&root).expect("create raw store root");
        let mut store = RawObjectFileStore::new(&root).expect("open raw store");
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
            .expect("store masked response");
        let checker = checker_ref(&reference);
        let error = read_g0_raw(&store, &checker).expect_err("masked response must fail closed");
        assert!(error.to_string().contains("safe CAS reader is unavailable"));
        drop(store);
        remove_fixture(&root);
    }
}
