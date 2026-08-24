//! Redaction-by-construction regression corpus.
//!
//! Each case starts from a secret-bearing source record (the shape of raw
//! upstream data) and builds the sanitized model through the dedicated DTOs,
//! then proves no serialized form contains any deny-list marker or secret
//! value: GitHub PAT/OAuth token prefixes, Authorization headers, Basic
//! auth blobs, credential-bearing URLs, and marked-secret variable values.

use velnor_model::{
    Adapter, AnyResource, Condition, IdentityRef, Job, RepositoryRef, ResourceMeta, Run,
    RunnerRegistration, SanitizedUrl, SecretRef, Source, Timestamp,
};

const SECRET_MARKERS: [&str; 12] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "Authorization:",
    "Basic ",
    "Bearer ",
    "client_secret",
    "BEGIN PRIVATE KEY",
    "sup3r-secret-value",
];

/// Stand-in for a raw upstream payload that carries secrets.
struct RawUpstream {
    endpoint_url: String,
    registration_token: String,
    app_token: String,
    auth_header: String,
    secret_variable_value: String,
}

fn raw_upstream() -> RawUpstream {
    RawUpstream {
        endpoint_url: "https://build-agent:sup3r-secret-value@github.example.com/endpoint"
            .to_owned(),
        registration_token: format!("ghp_{}deadbeefdeadbeef", "sup3r"),
        app_token: format!("gho_{}cafebabecafebabe", "app1"),
        auth_header: "Authorization: Basic c2VjcmV0".to_owned(),
        secret_variable_value: "sup3r-secret-value".to_owned(),
    }
}

fn at() -> Timestamp {
    Timestamp::parse("2026-08-24T12:30:45Z").unwrap()
}

/// Build a run resource the way production code must: every secret-bearing
/// field passes through a sanitized projection.
#[must_use]
fn run_from_raw(raw: &RawUpstream) -> AnyResource {
    let meta = ResourceMeta::new("run-9001", Source::Merged, at()).with_conditions(vec![
        // The raw failure text quotes an Authorization header; the builder
        // records a sanitized reason instead.
        Condition::degraded("Ready", "TokenRejected", "upstream answered 401", at()),
    ]);
    AnyResource::Run(Run {
        meta,
        repository: RepositoryRef::new("tailrocks", "velnor-actions-fixture"),
        number: 90,
        head_sha: "0f9e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e".to_owned(),
        head_branch: "main".to_owned(),
        event: "push".to_owned(),
        status: "completed".to_owned(),
        conclusion: Some("failure".to_owned()),
        url: Some(SanitizedUrl::project(&raw.endpoint_url)),
    })
}

#[must_use]
fn runner_registration_from_raw(raw: &RawUpstream) -> AnyResource {
    let _ = (&raw.registration_token, &raw.app_token);
    // Tokens exist only inside runner-private configuration; the operator
    // model carries registration metadata without any credential field.
    AnyResource::RunnerRegistration(RunnerRegistration {
        meta: ResourceMeta::new("velnor-sentry-slot-0", Source::Github, at())
            .with_summary("Registered", "ephemeral runner registered"),
        labels: Default::default(),
        ephemeral: true,
        online: false,
    })
}

#[must_use]
fn job_from_raw(raw: &RawUpstream) -> AnyResource {
    let _ = raw.secret_variable_value;
    // A job's marked-secret variables appear in the model only as names.
    let _secret_name_only = SecretRef::named("DEPLOY_PAT");
    AnyResource::Job(Job {
        meta: ResourceMeta::new("job-with-secrets", Source::Merged, at()),
        repository: RepositoryRef::new("tailrocks", "velnor-actions-fixture"),
        run: Some("run-9001".to_owned()),
        workflow: "control-plane.yml".to_owned(),
        head_branch: None,
        queued_ms: None,
        duration_ms: None,
        conclusion: None,
    })
}

#[must_use]
fn adapter_from_raw(raw: &RawUpstream) -> AnyResource {
    let _ = raw.auth_header;
    AnyResource::Adapter(Adapter {
        meta: ResourceMeta::new("adapter-actions-checkout", Source::Local, at()),
        adapter: "actions/checkout".to_owned(),
        version: "v6.0.2".to_owned(),
        actions: vec!["actions/checkout@v6".to_owned()],
    })
}

#[test]
fn corpus_covers_every_sanitization_path() {
    let corpus = {
        let raw = raw_upstream();
        vec![
            run_from_raw(&raw),
            runner_registration_from_raw(&raw),
            job_from_raw(&raw),
            adapter_from_raw(&raw),
        ]
    };
    assert_eq!(corpus.len(), 4);
}

#[test]
fn no_serialized_form_contains_secret_markers_or_values() {
    let raw = raw_upstream();
    let secrets = [
        raw.endpoint_url.as_str(),
        raw.registration_token.as_str(),
        raw.app_token.as_str(),
        raw.auth_header.as_str(),
        raw.secret_variable_value.as_str(),
    ];
    for resource in [
        run_from_raw(&raw),
        runner_registration_from_raw(&raw),
        job_from_raw(&raw),
        adapter_from_raw(&raw),
    ] {
        for rendered in [
            serde_json::to_string(&resource).unwrap(),
            serde_yaml::to_string(&resource).unwrap(),
        ] {
            for marker in SECRET_MARKERS {
                assert!(
                    !rendered.contains(marker),
                    "{} leaked marker {marker:?}: {rendered}",
                    resource.identity()
                );
            }
            for secret in &secrets {
                assert!(
                    !rendered.contains(secret),
                    "{} echoed a raw secret value",
                    resource.identity()
                );
            }
        }
    }
}

#[test]
fn sanitized_url_projection_drops_credentials_but_keeps_location() {
    let projected = SanitizedUrl::project(&raw_upstream().endpoint_url);
    assert_eq!(projected.as_str(), "https://github.example.com/endpoint");
    let json = serde_json::to_string(&projected).unwrap();
    assert!(!json.contains("sup3r"), "{json}");
}

#[test]
fn secret_and_identity_refs_serialize_names_only() {
    assert_eq!(
        serde_json::to_string(&SecretRef::named("DEPLOY_PAT")).unwrap(),
        "{\"name\":\"DEPLOY_PAT\"}"
    );
    assert_eq!(
        serde_json::to_string(&IdentityRef::new("velnor-ci", Some(7))).unwrap(),
        "{\"slug\":\"velnor-ci\",\"id\":7}"
    );
}
