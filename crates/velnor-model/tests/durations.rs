//! Machine duration contract: unsigned `*_ms` fields, `null` for
//! unavailable, typed overflow errors instead of silent wrapping.

use std::time::Duration;

use velnor_model::{DurationMs, DurationOverflowError, Lease, ResourceMeta, Source, Timestamp};

fn at() -> Timestamp {
    Timestamp::parse("2026-08-24T12:30:45Z").unwrap()
}

#[test]
fn conversion_from_duration_is_exact_in_milliseconds() {
    assert_eq!(
        DurationMs::try_from(Duration::from_millis(1_234))
            .unwrap()
            .as_u64(),
        1_234
    );
    assert_eq!(
        DurationMs::try_from(Duration::from_secs(2))
            .unwrap()
            .as_u64(),
        2_000
    );
    // Sub-millisecond remainder truncates; the machine field is whole ms.
    assert_eq!(
        DurationMs::try_from(Duration::from_micros(1_999))
            .unwrap()
            .as_u64(),
        1
    );
}

#[test]
fn overflow_is_a_typed_error_not_silent_wrapping() {
    let absurd = Duration::from_secs(u64::MAX);
    let err = DurationMs::try_from(absurd).unwrap_err();
    let typed: &DurationOverflowError = &err;
    assert!(typed.to_string().contains("exceeds"));
}

#[test]
fn unavailable_lease_ttl_serializes_null_and_never_zero() {
    let lease = Lease {
        meta: ResourceMeta::new("lease/x", Source::Local, at()),
        holder: "instance/a".to_owned(),
        ttl_ms: None,
        expires_at: at(),
    };
    let json = serde_json::to_string(&lease).unwrap();
    assert!(json.contains("\"ttlMs\":null"), "{json}");
    assert!(!json.contains("\"ttlMs\":0"), "{json}");
}

#[test]
fn granted_lease_ttl_serializes_unsigned_number() {
    let lease = Lease {
        meta: ResourceMeta::new("lease/y", Source::Local, at()),
        holder: "instance/b".to_owned(),
        ttl_ms: Some(DurationMs(30_000)),
        expires_at: at(),
    };
    let json = serde_json::to_string(&lease).unwrap();
    assert!(json.contains("\"ttlMs\":30000"), "{json}");
}
