//! Plan 065 fixture-integration proof: a sanitized model of a real
//! `tailrocks/velnor-actions-fixture` control-plane success run
//! (run 32724621332) is serialized and rendered through every format while
//! repository/run/job identity stays visible and no token, endpoint
//! authorization, or raw secret variable value appears anywhere.

use velnor_model::{
    Adapter, AnyResource, DurationMs, Job, RepositoryRef, ResourceMeta, Run, RunnerRegistration,
    SanitizedUrl, Slot, SlotKind, SlotPhase, Source, Timestamp,
};
use velnor_render::{ColorPolicy, OutputFormat, RenderOptions};

/// The observed fixture dispatch this model mirrors.
const RUN_ID: &str = "run-32724621332";
const REPO_FULL: &str = "tailrocks/velnor-actions-fixture";
const WORKFLOW: &str = "control-plane.yml";

/// Secret-shaped material a real upstream record would carry; none of it may
/// ever appear in rendered output.
const SECRET_MARKERS: [&str; 10] = [
    "ghp_",
    "gho_",
    "github_pat_",
    "Authorization:",
    "Bearer ",
    "client_secret",
    "BEGIN PRIVATE KEY",
    "sup3r-secret-value",
    "build-agent:",
    "DEPLOY_PAT=",
];

fn at() -> Timestamp {
    Timestamp::parse("2026-08-24T12:30:45Z").unwrap()
}

#[test]
fn fixture_control_plane_success_run_renders_all_formats_sanitized() {
    let raw_endpoint = "https://build-agent:sup3r-secret-value@github.example.com/endpoint";
    let resources = vec![
        AnyResource::Run(Run {
            meta: ResourceMeta::new(RUN_ID, Source::Github, at()),
            repository: RepositoryRef::new("tailrocks", "velnor-actions-fixture"),
            number: 85,
            head_sha: "bdb6697d0e5f4a2c9a1b8d7e6f5a4b3c2d1e0f9ab".to_owned(),
            head_branch: "main".to_owned(),
            event: "workflow_dispatch".to_owned(),
            status: "completed".to_owned(),
            conclusion: Some("success".to_owned()),
            url: Some(SanitizedUrl::project(
                "https://github.com/tailrocks/velnor-actions-fixture/actions/runs/32724621332",
            )),
        }),
        AnyResource::Job(Job {
            meta: ResourceMeta::new("job-control-plane", Source::Merged, at()),
            repository: RepositoryRef::new("tailrocks", "velnor-actions-fixture"),
            run: Some(RUN_ID.to_owned()),
            workflow: WORKFLOW.to_owned(),
            head_branch: Some("main".to_owned()),
            queued_ms: Some(DurationMs(4_200)),
            duration_ms: Some(DurationMs(96_500)),
            conclusion: Some("success".to_owned()),
        }),
        AnyResource::RunnerRegistration(RunnerRegistration {
            meta: ResourceMeta::new("velnor-fixture-slot-1", Source::Github, at())
                .with_summary("Registered", "ephemeral runner registered"),
            labels: Default::default(),
            ephemeral: true,
            online: true,
        }),
        AnyResource::Slot(Slot {
            meta: ResourceMeta::new("velnor-fixture-slot-1/job", Source::Local, at()),
            host: "sentry".to_owned(),
            index: 1,
            slot_kind: SlotKind::Stable,
            phase: SlotPhase::Idle,
            job: None,
        }),
        AnyResource::Adapter(Adapter {
            meta: ResourceMeta::new("adapter-actions-checkout", Source::Local, at()),
            adapter: "actions/checkout".to_owned(),
            version: "v6.0.2".to_owned(),
            actions: vec!["actions/checkout@v6".to_owned()],
        }),
    ];

    assert_eq!(resources.len(), 5);
    assert_eq!(
        SanitizedUrl::project(raw_endpoint).as_str(),
        "https://github.example.com/endpoint",
        "credential projection strips userinfo"
    );

    let options = RenderOptions {
        color: ColorPolicy::Never,
    };
    for format in OutputFormat::ALL {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        velnor_render::render(format, &resources, &options, &mut stdout, &mut stderr).unwrap();
        let body = String::from_utf8(stdout).unwrap();
        let warnings = String::from_utf8(stderr).unwrap();

        // Identity must survive every format.
        assert!(
            body.contains(RUN_ID),
            "{} lost run identity: {body}",
            format.as_str()
        );
        if format != OutputFormat::Name {
            assert!(
                body.contains(REPO_FULL) && body.contains(WORKFLOW),
                "{} lost repo/job identity",
                format.as_str()
            );
        }

        // A clean success run warns about nothing.
        assert!(
            warnings.is_empty(),
            "{} produced unexpected warnings: {warnings}",
            format.as_str()
        );

        // Redaction contract over body and warning streams alike.
        for marker in SECRET_MARKERS {
            assert!(
                !body.contains(marker) && !warnings.contains(marker),
                "{format} leaked marker {marker:?}: {body}{warnings}"
            );
        }
        assert!(
            !body.contains(raw_endpoint),
            "{format} echoed the credential-bearing endpoint"
        );
    }
}
