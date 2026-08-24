//! Golden serde tests: prove exact wire field names, fail-closed unknown
//! enum handling, RFC 3339 timestamps, and byte-identical round trips for a
//! representative fixture of every resource noun.

use std::collections::BTreeMap;

use velnor_model::{
    Adapter, AnyResource, Capability, Condition, ConditionStatus, DurationMs, Event, Host,
    Instance, Job, Lease, QueueEntry, RepositoryRef, Reservation, ResourceMeta, Run,
    RunnerRegistration, SanitizedUrl, Slot, SlotKind, SlotPhase, Source, Timestamp, SCHEMA_VERSION,
};

fn at() -> Timestamp {
    Timestamp::parse("2026-08-24T12:30:45Z").unwrap()
}

/// One deterministic fixture per approved noun, in plan order.
#[must_use]
pub fn fixture_of_every_noun() -> Vec<AnyResource> {
    let at = at();
    let mut labels = BTreeMap::new();
    labels.insert("pool".to_owned(), "warm".to_owned());
    vec![
        AnyResource::Host(Host {
            meta: meta("sentry", Source::Local, &["Ready"]),
            hostname: "sentry.tailrocks.internal".to_owned(),
            agent_version: Some("0.1.98".to_owned()),
            labels: labels.clone(),
        }),
        AnyResource::Instance(Instance {
            meta: meta("sentry/main", Source::Local, &[]),
            host: "sentry".to_owned(),
            version: "0.1.98".to_owned(),
            uptime_ms: Some(DurationMs(3_723_000)),
            slots_configured: 4,
            slots_busy: 2,
        }),
        AnyResource::Slot(Slot {
            meta: meta("sentry-0", Source::Local, &[]),
            host: "sentry".to_owned(),
            index: 0,
            slot_kind: SlotKind::Stable,
            phase: SlotPhase::WaitingForCapacity,
            job: None,
        }),
        AnyResource::RunnerRegistration(RunnerRegistration {
            meta: meta("velnor-sentry-slot-0", Source::Github, &["Registered"]),
            labels: labels.clone(),
            ephemeral: false,
            online: true,
        }),
        AnyResource::Job(Job {
            meta: meta("job-42", Source::Merged, &[]),
            repository: RepositoryRef::new("tailrocks", "velnor-actions-fixture"),
            run: Some("run-32714994603".to_owned()),
            workflow: "control-plane.yml".to_owned(),
            head_branch: Some("main".to_owned()),
            queued_ms: Some(DurationMs(4_200)),
            duration_ms: Some(DurationMs(96_500)),
            conclusion: Some("success".to_owned()),
        }),
        AnyResource::Run(Run {
            meta: meta("run-32714994603", Source::Github, &[]),
            repository: RepositoryRef::new("tailrocks", "velnor-actions-fixture"),
            number: 84,
            head_sha: "b211326d0e5f4a2c9a1b8d7e6f5a4b3c2d1e0f9a".to_owned(),
            head_branch: "main".to_owned(),
            event: "workflow_dispatch".to_owned(),
            status: "completed".to_owned(),
            conclusion: Some("success".to_owned()),
            url: Some(SanitizedUrl::project(
                "https://github.com/tailrocks/velnor-actions-fixture/actions/runs/32714994603",
            )),
        }),
        AnyResource::QueueEntry(QueueEntry {
            meta: meta("queue/job-43", Source::Local, &[]),
            position: 0,
            job: "job-43".to_owned(),
            wait_ms: None,
        }),
        AnyResource::Event(Event {
            meta: meta("event-000123", Source::Local, &[]),
            sequence: 123,
            occurred_at: at,
            event_kind: "slot.phase-changed".to_owned(),
            subject: "slot/sentry-0".to_owned(),
            detail: Some("idle -> acquiring".to_owned()),
        }),
        AnyResource::Reservation(Reservation {
            meta: meta("reservation-job-42", Source::Local, &[]),
            slot: "slot/sentry-1".to_owned(),
            purpose: "job-acquire".to_owned(),
            expires_at: at,
        }),
        AnyResource::Lease(Lease {
            meta: meta("lease/target-store", Source::Local, &[]),
            holder: "instance/sentry/main".to_owned(),
            ttl_ms: Some(DurationMs(30_000)),
            expires_at: at,
        }),
        AnyResource::Capability(Capability {
            meta: meta("capability/actions.checkout", Source::Local, &[]),
            key: "actions.checkout".to_owned(),
            supported: true,
            details: None,
        }),
        AnyResource::Adapter(Adapter {
            meta: meta("adapter/actions-checkout", Source::Local, &[]),
            adapter: "actions/checkout".to_owned(),
            version: "v6.0.2".to_owned(),
            actions: vec!["actions/checkout@v6".to_owned()],
        }),
    ]
}

fn meta(name: &str, source: Source, ready: &[&str]) -> ResourceMeta {
    let mut meta = ResourceMeta::new(name, source, at());
    if !ready.is_empty() {
        meta = meta.with_conditions(
            ready
                .iter()
                .map(|kind| Condition::ready(kind, at()))
                .collect(),
        );
    }
    meta
}

#[test]
fn every_noun_round_trips_byte_identically_through_json() {
    for resource in fixture_of_every_noun() {
        let json = serde_json::to_string(&resource).unwrap();
        let parsed: AnyResource = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, resource, "{} diverged", resource.identity());
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
    }
}

#[test]
fn every_noun_round_trips_through_yaml() {
    for resource in fixture_of_every_noun() {
        let yaml = serde_yaml::to_string(&resource).unwrap();
        let parsed: AnyResource = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed, resource, "{} yaml diverged", resource.identity());
    }
}

#[test]
fn schema_version_is_stamped_on_every_resource() {
    for resource in fixture_of_every_noun() {
        assert_eq!(resource.meta().schema_version, SCHEMA_VERSION);
        let json = serde_json::to_string(&resource).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schemaVersion"], SCHEMA_VERSION, "{json}");
    }
}

#[test]
fn timestamps_are_rfc3339_strings_on_the_wire() {
    let run_json = serde_json::to_string(&fixture_of_every_noun()[5]).unwrap();
    let value: serde_json::Value = serde_json::from_str(&run_json).unwrap();
    let stamp = value["lastTransitionTime"].as_str().unwrap();
    assert_eq!(stamp, "2026-08-24T12:30:45Z");
    // Event carries its own instant too.
    let event_json = serde_json::to_string(&fixture_of_every_noun()[7]).unwrap();
    let value: serde_json::Value = serde_json::from_str(&event_json).unwrap();
    assert_eq!(
        value["occurredAt"].as_str().unwrap(),
        "2026-08-24T12:30:45Z"
    );
}

#[test]
fn unknown_enum_values_are_fail_closed_for_every_closed_choice() {
    for bad in [
        r#"{"schemaVersion":1,"name":"x","source":"ORBIT","lastTransitionTime":"2026-08-24T12:30:45Z"}"#,
        r#"{"schemaVersion":1,"name":"x","source":"LOCAL","lastTransitionTime":"2026-08-24T12:30:45Z","host":"h","index":0,"slotKind":"stable","phase":"queued"}"#,
    ] {
        assert!(
            serde_json::from_str::<AnyResource>(bad).is_err(),
            "accepted {bad}"
        );
    }
    let condition_bad = "{\"kind\":\"Ready\",\"status\":\"PROBABLY\",\"reason\":null,\
         \"message\":null,\"lastTransitionTime\":\"2026-08-24T12:30:45Z\"}";
    assert!(serde_json::from_str::<Condition>(condition_bad).is_err());
    assert_eq!(ConditionStatus::ALL.len(), 3);
}

#[test]
fn unavailable_durations_serialize_as_null_never_zero() {
    let queue = match &fixture_of_every_noun()[6] {
        AnyResource::QueueEntry(entry) => entry.clone(),
        _ => unreachable!("fixture order"),
    };
    let json = serde_json::to_string(&queue).unwrap();
    assert!(
        json.contains("\"waitMs\":null"),
        "unavailable duration must be null: {json}"
    );
}
