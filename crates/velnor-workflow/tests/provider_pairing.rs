//! Structural provider pairing: every selected unit fans out to one caller
//! per provider in the universe, comparison names stay provider-first, and
//! automatic gates do not silently drop local providers except the documented
//! fork-PR trust exclusion.

#![expect(clippy::panic, reason = "a test whose setup fails should panic loudly")]
#![expect(
    clippy::unwrap_used,
    reason = "a test whose setup fails should panic loudly"
)]
#![expect(
    clippy::expect_used,
    reason = "a test whose setup fails should panic loudly"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_yaml::Value;

struct Generated {
    output: PathBuf,
}

impl Generated {
    fn workflow(&self, name: &str) -> String {
        fs::read_to_string(self.output.join(".github/workflows").join(name)).unwrap()
    }
}

fn unique_dir(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "provider-pairing-{name}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn write_rust_fixture(root: &Path, crates: usize) {
    fs::write(
        root.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.91.1\"\n",
    )
    .unwrap();
    let mut workspace = String::from("[workspace]\nmembers = [\n");
    for index in 0..crates {
        let name = format!("crate{index:02}");
        let _ = writeln!(workspace, "  \"crates/{name}\",");
        let dir = root.join("crates").join(&name).join("src");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            root.join("crates").join(&name).join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        fs::write(dir.join("lib.rs"), "pub fn n() -> u8 { 1 }\n").unwrap();
    }
    workspace.push_str("]\n");
    fs::write(root.join("Cargo.toml"), workspace).unwrap();
    fs::write(root.join("Cargo.lock"), "version = 3\n").unwrap();
    fs::create_dir_all(root.join(".github-gen")).unwrap();
}

fn write_workflow_config(root: &Path, workflow: &str) {
    write_workflow_config_for(root, "example/monorepo", workflow);
}

fn write_workflow_config_for(root: &Path, repository: &str, workflow: &str) {
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        format!(
            "schema = 2\n\n[generator]\nrepository = \"{repository}\"\n\n[workflow]\n{workflow}"
        ),
    )
    .unwrap();
}

/// Write visibility evidence bound to the fixture slug: the scan path
/// requires it, but provider placement remains explicit config.
fn write_visibility(root: &Path, visibility: &str) {
    write_visibility_for(root, "example/monorepo", visibility);
}

fn write_visibility_for(root: &Path, repository: &str, visibility: &str) {
    fs::write(
        root.join(".github-gen/visibility.toml"),
        format!("repository = \"{repository}\"\nvisibility = \"{visibility}\"\n"),
    )
    .unwrap();
}

/// The renamed fixture's native selector is explicit and host-qualified. The
/// fixture deliberately does not borrow an estate/repository name: placement
/// is a provider contract, not repository-name inference.
fn estate_velnor_selector() -> String {
    "[workflow.selectors.velnor]\nruns_on = [\"self-hosted\", \"velnor-native\", \"local-mac\"]\n"
        .to_owned()
}

const HOSTED_SELECTOR: &str = "[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n";

fn hosted_only_config() -> String {
    format!(
        "providers = [\"github-hosted\"]\nautomatic_providers = [\"github-hosted\"]\n\n{HOSTED_SELECTOR}"
    )
}

fn velnor_only_config() -> String {
    format!(
        "providers = [\"velnor\"]\nautomatic_providers = [\"velnor\"]\n\n{}",
        estate_velnor_selector()
    )
}

fn all_providers_config() -> String {
    format!(
        "provider_mode = \"both\"\n\
         \n{HOSTED_SELECTOR}"
    )
}

fn generate(root: &Path) -> Generated {
    let output = root.parent().unwrap().join(format!(
        "{}-out",
        root.file_name().and_then(|name| name.to_str()).unwrap()
    ));
    let _ = fs::remove_dir_all(&output);
    let outcome = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        outcome.status.success(),
        "generation failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    Generated { output }
}

fn parse_jobs(yaml: &str) -> BTreeMap<String, Value> {
    let doc: Value = serde_yaml::from_str(yaml).expect("parse workflow yaml");
    doc.get("jobs")
        .and_then(Value::as_mapping)
        .expect("jobs mapping")
        .iter()
        .map(|(key, value)| (key.as_str().to_owned(), value.clone()))
        .collect()
}

fn job_name(job: &Value) -> String {
    job.get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn job_if(job: &Value) -> String {
    job.get("if")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn job_runs_on(job: &Value) -> String {
    match job.get("runs-on").expect("job runs-on") {
        Value::String(value) => value.clone(),
        Value::Sequence(values) => values
            .iter()
            .map(|value| value.as_str().expect("runs-on label").to_owned())
            .collect::<Vec<_>>()
            .join(","),
        value => panic!("unexpected runs-on value: {value:?}"),
    }
}

fn aggregate_provider_caller_id(job_id: &str) -> Option<(&str, &str)> {
    job_id
        .strip_prefix("github-self-hosted-")
        .map(|id| ("github-self-hosted", id))
        .or_else(|| {
            job_id
                .strip_prefix("github-hosted-")
                .map(|id| ("github-hosted", id))
        })
        .or_else(|| job_id.strip_prefix("velnor-").map(|id| ("velnor", id)))
        .filter(|(_, id)| !id.is_empty())
}

#[test]
fn exact_three_provider_universe_emits_all_three_callers() {
    let root = unique_dir("three-provider-universe");
    write_rust_fixture(&root, 3);
    write_workflow_config(&root, &all_providers_config());
    write_visibility(&root, "public");
    let generated = generate(&root);
    let pr = parse_jobs(&generated.workflow("ci-pr.yml"));
    for provider in ["github-hosted", "github-self-hosted", "velnor"] {
        assert!(
            pr.keys().any(|id| id.starts_with(&format!("{provider}-"))),
            "three-provider surface must emit {provider} callers: {pr:?}"
        );
    }
    let kind = parse_jobs(&generated.workflow("ci-unit-rust.yml"));
    for provider in ["github-hosted", "github-self-hosted", "velnor"] {
        assert!(
            kind.contains_key(&format!("verify-{provider}")),
            "three-provider surface must emit {provider} verify job: {kind:?}"
        );
    }
}

#[test]
fn renamed_fixture_keeps_explicit_provider_host_and_trust_boundaries() {
    let original = unique_dir("renamed-original");
    let renamed = unique_dir("renamed-copy");
    for (root, repository) in [
        (&original, "example/original"),
        (&renamed, "example/renamed-copy"),
    ] {
        write_rust_fixture(root, 1);
        write_workflow_config_for(root, repository, &all_providers_config());
        write_visibility_for(root, repository, "public");
    }

    let original_jobs = parse_jobs(&generate(&original).workflow("ci-unit-rust.yml"));
    let renamed_jobs = parse_jobs(&generate(&renamed).workflow("ci-unit-rust.yml"));
    for jobs in [&original_jobs, &renamed_jobs] {
        assert_eq!(job_runs_on(&jobs["verify-github-hosted"]), "ubuntu-24.04");
        assert!(!job_runs_on(&jobs["verify-github-hosted"]).contains("velnor-scale-set"));
        assert_eq!(
            job_runs_on(&jobs["verify-github-self-hosted"]),
            "self-hosted,velnor-scale-set,local-mac"
        );
        assert_eq!(
            job_runs_on(&jobs["verify-velnor"]),
            "self-hosted,velnor-native,local-mac"
        );
        for provider in ["github-self-hosted", "velnor"] {
            let gate = job_if(&jobs[&format!("verify-{provider}")]);
            assert!(
                gate.contains("github.event.pull_request.head.repo.fork"),
                "{provider} must be excluded before local admission: {gate}"
            );
        }
    }
    assert_eq!(
        job_runs_on(&original_jobs["verify-github-hosted"]),
        job_runs_on(&renamed_jobs["verify-github-hosted"])
    );
    assert_eq!(
        job_runs_on(&original_jobs["verify-github-self-hosted"]),
        job_runs_on(&renamed_jobs["verify-github-self-hosted"])
    );
    assert_eq!(
        job_runs_on(&original_jobs["verify-velnor"]),
        job_runs_on(&renamed_jobs["verify-velnor"])
    );

    let _ = fs::remove_dir_all(original);
    let _ = fs::remove_dir_all(renamed);
}

#[test]
fn renamed_fixtures_pair_every_unit_on_the_explicit_host() {
    for (repository, host) in [
        ("example/arbitrary-original", "local-mac"),
        ("different/renamed-project", "bastion"),
    ] {
        let root = unique_dir("explicit-host-pairing");
        write_rust_fixture(&root, 2);
        write_workflow_config_for(
            &root,
            repository,
            &format!(
                "provider_mode = \"both\"\n\n{HOSTED_SELECTOR}\n\
                 [workflow.selectors.github-self-hosted]\n\
                 runs_on = [\"self-hosted\", \"velnor-scale-set\", {host:?}]\n\
                 [workflow.selectors.velnor]\n\
                 runs_on = [\"self-hosted\", \"velnor-native\", {host:?}]\n"
            ),
        );
        write_visibility_for(&root, repository, "public");
        let generated = generate(&root);
        for file in ["ci-pr.yml", "ci-main.yml"] {
            let jobs = parse_jobs(&generated.workflow(file));
            let mut providers_by_unit: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for (id, job) in &jobs {
                if let Some((provider, unit)) = aggregate_provider_caller_id(id) {
                    providers_by_unit.entry(unit).or_default().push(provider);
                    assert_eq!(job["with"]["provider"].as_str(), Some(provider));
                    assert_eq!(job["with"]["unit"].as_str(), Some(unit));
                }
            }
            assert_eq!(providers_by_unit.len(), 2, "{repository}: {file}");
            for (unit, providers) in providers_by_unit {
                assert_eq!(
                    providers,
                    ["github-hosted", "github-self-hosted", "velnor"],
                    "{repository}: {file}: {unit} must retain every sibling"
                );
            }
        }
        let jobs = parse_jobs(&generated.workflow("ci-unit-rust.yml"));
        assert_eq!(job_runs_on(&jobs["verify-github-hosted"]), "ubuntu-24.04");
        for (provider, engine) in [
            ("github-self-hosted", "velnor-scale-set"),
            ("velnor", "velnor-native"),
        ] {
            assert_eq!(
                job_runs_on(&jobs[&format!("verify-{provider}")]),
                format!("self-hosted,{engine},{host}"),
                "{repository}: {provider} must retain its configured host"
            );
        }
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&generated.output);
    }
}

#[test]
fn hosted_only_emits_only_hosted_callers() {
    let root = unique_dir("hosted-only");
    write_rust_fixture(&root, 2);
    write_workflow_config(&root, &hosted_only_config());
    write_visibility(&root, "public");
    let pr = parse_jobs(&generate(&root).workflow("ci-pr.yml"));
    assert!(pr.keys().any(|id| id.starts_with("github-hosted-rust-")));
    assert!(!pr.keys().any(|id| id.starts_with("velnor-")));
    assert!(
        !pr.keys().any(|id| id.starts_with("github-self-hosted-")),
        "hosted-only must not emit self-hosted callers"
    );
    let kind = parse_jobs(&generate(&root).workflow("ci-unit-rust.yml"));
    assert!(kind.contains_key("verify-github-hosted"));
    assert!(!kind.contains_key("verify-github-self-hosted"));
    assert!(!kind.contains_key("verify-velnor"));
}

#[test]
fn velnor_only_emits_only_velnor_callers() {
    let root = unique_dir("velnor-only");
    write_rust_fixture(&root, 2);
    write_workflow_config(&root, &velnor_only_config());
    write_visibility(&root, "private");
    let pr = parse_jobs(&generate(&root).workflow("ci-pr.yml"));
    assert!(pr.keys().any(|id| id.starts_with("velnor-rust-")));
    assert!(!pr.keys().any(|id| aggregate_provider_caller_id(id)
        .is_some_and(|(provider, _)| provider == "github-hosted")));
    let kind = parse_jobs(&generate(&root).workflow("ci-unit-rust.yml"));
    assert!(kind.contains_key("verify-velnor"));
    assert!(!kind.contains_key("verify-github-hosted"));
}

#[test]
fn velnor_automatic_gates_verify_on_trusted_events() {
    let root = unique_dir("velnor-automatic-gates");
    write_rust_fixture(&root, 1);
    write_workflow_config(&root, &velnor_only_config());
    write_visibility(&root, "private");

    let generated = generate(&root);
    let rust = parse_jobs(&generated.workflow("ci-unit-rust.yml"));
    let velnor_if = job_if(&rust["verify-velnor"]);
    assert!(
        velnor_if.contains("inputs.provider == 'velnor'"),
        "Velnor verify job must require its provider: {velnor_if}"
    );
    // An automatic provider admits every event; the trusted-only class
    // excludes fork and bot pull requests inline.
    assert!(
        !velnor_if.contains("inputs.providers"),
        "no dispatch input selects providers: {velnor_if}"
    );
    assert!(
        velnor_if.contains(
            "!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot'))"
        ),
        "Velnor must exclude untrusted PRs: {velnor_if}"
    );

    let pr = parse_jobs(&generated.workflow("ci-pr.yml"));
    assert!(
        !pr.keys().any(|id| id.contains("admission")),
        "admission is an inline gate now, not a job: {pr:?}"
    );
    assert!(pr.contains_key("velnor-rust-crate00"));
    assert!(pr.contains_key("prepare-cargo"));
}

#[test]
fn swift_is_excluded_from_local_providers_by_platform() {
    let root = unique_dir("swift-platform-exclusion");
    write_rust_fixture(&root, 1);
    fs::create_dir_all(root.join("Sources/App")).unwrap();
    fs::write(
        root.join("Package.swift"),
        "let package = Package(targets: [.binaryTarget(name: \"WidgetFFI\", path: \"../target/xcframework/Widget.xcframework\")])\n",
    )
    .unwrap();
    write_workflow_config(&root, &hosted_only_config());
    write_visibility(&root, "public");
    let generated = generate(&root);
    let pr = parse_jobs(&generated.workflow("ci-pr.yml"));
    let swift_callers: Vec<&String> = pr
        .keys()
        .filter(|id| {
            aggregate_provider_caller_id(id).is_some_and(|(_, unit)| unit.contains("swift"))
        })
        .collect();
    assert!(
        !swift_callers.is_empty(),
        "Swift must emit aggregate callers: {pr:?}"
    );
    for caller in swift_callers {
        assert!(
            caller.starts_with("github-hosted-"),
            "Swift runs hosted-only: {caller}"
        );
    }
    let swift = parse_jobs(&generated.workflow("ci-unit-swift.yml"));
    assert!(
        swift.contains_key("verify-github-hosted"),
        "Swift keeps the collapsed hosted verify job: {swift:?}"
    );
    assert!(
        !swift.contains_key("verify-github-self-hosted"),
        "Swift must not emit a self-hosted verify job: {swift:?}"
    );
    assert!(
        !swift.contains_key("verify-velnor"),
        "Swift must not emit a Velnor verify job: {swift:?}"
    );
}

#[test]
fn swift_explicit_hosted_opt_out_stays_hosted() {
    let root = unique_dir("swift-hosted-opt-out");
    write_rust_fixture(&root, 1);
    fs::create_dir_all(root.join("Sources/App")).unwrap();
    fs::write(root.join("Package.swift"), "// swift-tools-version: 5.9\n").unwrap();
    write_workflow_config(&root, &hosted_only_config());
    write_visibility(&root, "public");
    let hosted_only = generate(&root);
    let project = fs::read_to_string(hosted_only.output.join(".github/ci/project.toml")).unwrap();
    let swift_id = project
        .lines()
        .filter_map(|line| line.strip_prefix("id = \""))
        .map(|line| line.trim_end_matches('"').to_owned())
        .find(|id| id.starts_with("swift-"))
        .expect("scanned Swift unit id");
    write_workflow_config(&root, &hosted_only_config());
    let mut config = fs::read_to_string(root.join(".github-gen/velnor-workflow.toml")).unwrap();
    let _ = write!(
        config,
        r#"

[[declare]]
primitive = "swift-package-pipeline"
units = ["{swift_id}"]

[declare.args]
jobs = ["github-hosted"]
"#
    );
    fs::write(root.join(".github-gen/velnor-workflow.toml"), config).unwrap();
    let generated = generate(&root);
    let swift = parse_jobs(&generated.workflow("ci-unit-swift.yml"));
    assert!(
        swift.contains_key("verify-github-hosted"),
        "explicit hosted opt-out must keep the collapsed hosted verify job: {swift:?}"
    );
    assert!(
        !swift.contains_key("verify-velnor"),
        "explicit hosted opt-out must not emit a Velnor verify job: {swift:?}"
    );
    assert_eq!(job_name(&swift["verify-github-hosted"]), "GitHub · hosted");
    // The collapsed job gates on the caller's `inputs.unit` membership, never
    // on an enumeration of unit ids: the callee stays O(1) in units.
    assert!(
        job_if(&swift["verify-github-hosted"]).contains(
            "contains(inputs.selected_units, format('\"unit_id\":\"{0}\"', inputs.unit))"
        ),
        "swift verify job must gate on inputs.unit membership"
    );
    assert!(
        !generated
            .workflow("ci-unit-swift.yml")
            .contains("inputs.unit == '"),
        "swift verify steps must not guard on a unit identity"
    );
    assert!(
        generated.workflow("ci-pr.yml").contains(&format!(
            "      unit: {swift_id}\n      provider: github-hosted"
        )),
        "the aggregate passes the declared unit to the swift reusable"
    );
    let pr = parse_jobs(&generated.workflow("ci-pr.yml"));
    assert!(
        pr.contains_key(&format!("github-hosted-{swift_id}")),
        "explicit hosted opt-out must keep the aggregate hosted caller: {pr:?}"
    );
}
