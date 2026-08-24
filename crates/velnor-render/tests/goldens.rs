//! Golden renderer matrix: every approved resource noun through every
//! output format, byte-compared against committed goldens, plus
//! stdout/stderr separation and unversioned-`name` guarantees.
//!
//! Regenerate committed goldens with `UPDATE_GOLDENS=1 cargo nextest run
//! -p velnor-render` after reviewing diffs by eye.

use std::collections::BTreeMap;

use velnor_model::{
    Adapter, AnyResource, Capability, Condition, DurationMs, Event, Host, Instance, Job, Lease,
    QueueEntry, RepositoryRef, Reservation, ResourceMeta, Run, RunnerRegistration, SanitizedUrl,
    Slot, SlotKind, SlotPhase, Source, Timestamp,
};
use velnor_render::{collect_warnings, human_ms, ColorPolicy, OutputFormat, RenderOptions};

fn at() -> Timestamp {
    Timestamp::parse("2026-08-24T12:30:45Z").unwrap()
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

/// One deterministic fixture per approved noun, in plan order. Mirrors the
/// model crate's golden fixtures so both suites describe the same corpus.
#[must_use]
pub fn fixture_of_every_noun() -> Vec<AnyResource> {
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
            occurred_at: at(),
            event_kind: "slot.phase-changed".to_owned(),
            subject: "slot/sentry-0".to_owned(),
            detail: Some("idle -> acquiring".to_owned()),
        }),
        AnyResource::Reservation(Reservation {
            meta: meta("reservation-job-42", Source::Local, &[]),
            slot: "slot/sentry-1".to_owned(),
            purpose: "job-acquire".to_owned(),
            expires_at: at(),
        }),
        AnyResource::Lease(Lease {
            meta: meta("lease/target-store", Source::Local, &[]),
            holder: "instance/sentry/main".to_owned(),
            ttl_ms: Some(DurationMs(30_000)),
            expires_at: at(),
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

fn render_to_string(format: OutputFormat, resources: &[AnyResource]) -> (String, String) {
    let options = RenderOptions::default();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    velnor_render::render(format, resources, &options, &mut stdout, &mut stderr).unwrap();
    (
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

fn golden_path(format: OutputFormat) -> String {
    format!("goldens/{}.golden", format.as_str())
}

fn update_or_compare(path: &str, body: &str) {
    let full = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/").to_owned() + path;
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::write(&full, body).unwrap();
    }
    let expected = std::fs::read_to_string(&full).unwrap_or_else(|error| {
        panic!("{error}: missing golden {path}; rerun with UPDATE_GOLDENS=1")
    });
    assert_eq!(expected, body, "golden drift in {path}");
}

#[test]
fn every_resource_through_every_format_matches_goldens() {
    let resources = fixture_of_every_noun();
    assert_eq!(resources.len(), 12);
    for format in OutputFormat::ALL {
        let (stdout, stderr) = render_to_string(format, &resources);
        assert!(stderr.is_empty(), "{format} emitted stderr: {stderr}");
        assert!(!stdout.is_empty(), "{format} produced no output");
        update_or_compare(&golden_path(format), &stdout);
    }
}

#[test]
fn machine_formats_are_byte_deterministic() {
    let resources = fixture_of_every_noun();
    for format in [OutputFormat::Json, OutputFormat::Jsonl, OutputFormat::Yaml] {
        let first = render_to_string(format, &resources).0;
        let second = render_to_string(format, &resources).0;
        assert_eq!(first, second, "{format} nondeterministic");
    }
}

#[test]
fn jsonl_emits_exactly_one_object_per_line() {
    let resources = fixture_of_every_noun();
    let (stdout, _) = render_to_string(OutputFormat::Jsonl, &resources);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), resources.len());
    for line in &lines {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(
            value.get("resourceKind").is_some(),
            "unversioned line: {line}"
        );
    }
}

#[test]
fn name_is_the_unversioned_identity_projection() {
    let resources = fixture_of_every_noun();
    let (stdout, _) = render_to_string(OutputFormat::Name, &resources);
    let expected: Vec<String> = resources.iter().map(|r| r.identity()).collect();
    assert_eq!(stdout.lines().collect::<Vec<_>>(), expected);
    assert!(!stdout.contains("schemaVersion"));
    assert!(!stdout.contains("resourceKind"));
}

#[test]
fn json_and_yaml_emit_versioned_resources() {
    let resources = fixture_of_every_noun();
    for format in [OutputFormat::Json, OutputFormat::Yaml] {
        let (stdout, _) = render_to_string(format, &resources);
        assert!(stdout.contains("schemaVersion"), "{format} unversioned");
        assert!(stdout.contains("resourceKind"), "{format} missing kind tag");
    }
}

#[test]
fn warnings_go_to_stderr_never_stdout() {
    let mut degraded = Run {
        meta: ResourceMeta::new("run-degraded", Source::Github, at()).with_conditions(vec![
            Condition::degraded(
                "Ready",
                "TokenRejected",
                "upstream rejected the registration token",
                at(),
            ),
        ]),
        repository: RepositoryRef::new("tailrocks", "velnor-actions-fixture"),
        number: 91,
        head_sha: "abc".to_owned(),
        head_branch: "main".to_owned(),
        event: "push".to_owned(),
        status: "completed".to_owned(),
        conclusion: Some("failure".to_owned()),
        url: None,
    };
    degraded.meta.reason = Some("TokenRejected".to_owned());
    let error_slot = Slot {
        meta: ResourceMeta::new("sentry-9", Source::Local, at()),
        host: "sentry".to_owned(),
        index: 9,
        slot_kind: SlotKind::Stable,
        phase: SlotPhase::Error,
        job: None,
    };
    let resources = vec![AnyResource::Run(degraded), AnyResource::Slot(error_slot)];
    assert_eq!(
        collect_warnings(&resources).len(),
        2,
        "one condition plus one phase warning"
    );
    for format in [
        OutputFormat::Table,
        OutputFormat::Wide,
        OutputFormat::Json,
        OutputFormat::Jsonl,
    ] {
        let (stdout, stderr) = render_to_string(format, &resources);
        assert!(stderr.contains("warning:"), "{format} lost warnings");
        assert!(!stdout.contains("warning:"), "{format} leaked warnings");
        assert!(!stdout.contains("401"), "{format} echoed detail into body");
    }
}

#[test]
fn color_respects_policy_in_tables_and_warnings() {
    let resources = vec![AnyResource::Slot(Slot {
        meta: ResourceMeta::new("sentry-err", Source::Local, at()),
        host: "sentry".to_owned(),
        index: 1,
        slot_kind: SlotKind::Stable,
        phase: SlotPhase::Error,
        job: None,
    })];
    let styled_options = RenderOptions {
        color: ColorPolicy::Always,
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    velnor_render::render(
        OutputFormat::Table,
        &resources,
        &styled_options,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    let stdout = String::from_utf8(stdout).unwrap();
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(stdout.contains("\u{1b}[31m"), "phase not painted red");
    assert!(stderr.contains("\u{1b}[33m"), "warning not painted yellow");

    let plain_options = RenderOptions::default();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    velnor_render::render(
        OutputFormat::Table,
        &resources,
        &plain_options,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    let stdout = String::from_utf8(stdout).unwrap();
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(!stdout.contains('\u{1b}'));
    assert!(!stderr.contains('\u{1b}'));
}

#[test]
fn human_durations_are_table_only_projections() {
    assert_eq!(human_ms(250), "250ms");
    assert_eq!(human_ms(96_500), "1m36s");
    assert_eq!(human_ms(65_000), "1m5s");
    assert_eq!(human_ms(3_723_000), "1h02m");
    assert_eq!(human_ms(97_884_000), "1d03h");
}

#[test]
fn wide_adds_provenance_columns_without_breaking_alignment() {
    let resources = fixture_of_every_noun();
    let (narrow, _) = render_to_string(OutputFormat::Table, &resources);
    let (wide, _) = render_to_string(OutputFormat::Wide, &resources);
    assert!(wide.contains("SOURCE"));
    assert!(wide.contains("REASON"));
    assert!(wide.contains("LAST-TRANSITION"));
    assert!(!narrow.contains("SOURCE"));
    for line in wide.lines().filter(|line| !line.trim().is_empty()) {
        assert!(!line.ends_with(' '), "ragged wide row: {line:?}");
    }
}
