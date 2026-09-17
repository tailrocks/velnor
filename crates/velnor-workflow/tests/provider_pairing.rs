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

use std::collections::{BTreeMap, BTreeSet};
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
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        format!(
            "schema = 2\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\n{workflow}"
        ),
    )
    .unwrap();
}

const HOSTED_SELECTOR: &str = "[workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n";
const SELF_HOSTED_SELECTOR: &str =
    "[workflow.selectors.github-self-hosted]\nruns_on = [\"example-scale-set\"]\n";
const VELNOR_SELECTOR: &str =
    "[workflow.selectors.velnor]\nruns_on = [\"self-hosted\", \"example-runner\"]\n";

fn all_providers_config() -> String {
    format!(
        "providers = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]\n\
         automatic_providers = [\"github-hosted\", \"github-self-hosted\", \"velnor\"]\n\
         \n{HOSTED_SELECTOR}\n{SELF_HOSTED_SELECTOR}\n{VELNOR_SELECTOR}"
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

fn name_head(name: &str) -> &str {
    name.split(" / ").next().unwrap_or(name)
}

fn assert_aggregate_callers_use_sidebar_names(jobs: &BTreeMap<String, Value>) {
    for (id, job) in jobs {
        let name = job_name(job);
        assert!(!name.is_empty(), "job `{id}` is missing a display name");
        if let Some((_, unit)) = aggregate_provider_caller_id(id) {
            assert!(
                name.starts_with("Rust · "),
                "aggregate caller `{id}` must display as a sidebar unit name, got `{name}`"
            );
            assert!(
                name.ends_with(unit),
                "aggregate caller `{id}` must keep the unit suffix in `{name}`"
            );
        } else if id == "prepare-cargo" {
            assert_eq!(
                name, "Control / Prepare Cargo",
                "prep caller `{id}` must display as Control / Prepare Cargo"
            );
        }
    }
}

#[test]
fn all_providers_emit_one_caller_per_unit_per_provider() {
    let root = unique_dir("all-trifurcation");
    write_rust_fixture(&root, 3);
    write_workflow_config(&root, &all_providers_config());
    let generated = generate(&root);
    let rust = generated.workflow("ci-unit-rust.yml");
    let kind_jobs = parse_jobs(&rust);
    assert!(kind_jobs.contains_key("verify-github-hosted"));
    assert!(kind_jobs.contains_key("verify-github-self-hosted"));
    assert!(kind_jobs.contains_key("verify-velnor"));
    assert_eq!(
        job_name(&kind_jobs["verify-github-hosted"]),
        "GitHub · hosted"
    );
    assert_eq!(
        job_name(&kind_jobs["verify-github-self-hosted"]),
        "github-self-hosted"
    );
    assert_eq!(job_name(&kind_jobs["verify-velnor"]), "velnor");

    let pr_yaml = generated.workflow("ci-pr.yml");
    let pr = parse_jobs(&pr_yaml);
    assert_aggregate_callers_use_sidebar_names(&pr);

    let mut hosted = BTreeSet::new();
    let mut self_hosted = BTreeSet::new();
    let mut velnor = BTreeSet::new();
    for id in pr.keys() {
        match aggregate_provider_caller_id(id) {
            Some(("github-hosted", unit)) => {
                hosted.insert(unit.to_owned());
            }
            Some(("github-self-hosted", unit)) => {
                self_hosted.insert(unit.to_owned());
            }
            Some(("velnor", unit)) => {
                velnor.insert(unit.to_owned());
            }
            _ => {}
        }
    }
    assert_eq!(
        hosted,
        BTreeSet::from([
            "rust-crate00".to_owned(),
            "rust-crate01".to_owned(),
            "rust-crate02".to_owned(),
        ])
    );
    assert_eq!(
        hosted, self_hosted,
        "every provider must emit the same unit set: {pr:?}"
    );
    assert_eq!(
        hosted, velnor,
        "every provider must emit the same unit set: {pr:?}"
    );

    let prep = pr.get("prepare-cargo").expect("cargo prep caller");
    assert_eq!(job_name(prep), "Control / Prepare Cargo");
    assert!(
        prep.get("with")
            .and_then(Value::as_mapping)
            .and_then(|with| with.get("provider"))
            .and_then(Value::as_str)
            == Some("control"),
        "prep caller must invoke the kind reusable on the control provider"
    );
    assert!(
        !pr.keys().any(|id| id.ends_with("-prepare-cargo-sources")),
        "cargo prep must stay one control caller, not per-provider callers"
    );

    for job in [
        "github-self-hosted-prepare-cargo-sources",
        "velnor-prepare-cargo-sources",
    ] {
        let prep_inner = kind_jobs
            .get(job)
            .unwrap_or_else(|| panic!("cargo prep stays inside the kind reusable: {job}"));
        assert_eq!(job_name(prep_inner), "prepare-cargo");
        assert!(
            job_if(prep_inner).contains("inputs.provider == 'control'"),
            "inner prep must belong to the control provider: {}",
            job_if(prep_inner)
        );
    }

    for unit in &hosted {
        assert!(pr.contains_key(&format!("github-hosted-{unit}")));
        assert!(pr.contains_key(&format!("github-self-hosted-{unit}")));
        assert!(pr.contains_key(&format!("velnor-{unit}")));
    }
    // Hostname printouts are not identity (spec §2): no verify job carries
    // the deleted runner-identity step on any provider.
    assert!(
        !rust.contains("runner identity"),
        "no verify job carries a runner-identity step"
    );

    assert_eq!(job_name(&pr["plan"]), "Control / Planning");
    assert_eq!(job_name(&pr["prepare-cargo"]), "Control / Prepare Cargo");
    assert_eq!(
        pr_yaml
            .matches("uses: ./.github/workflows/ci-unit-rust.yml")
            .count(),
        10,
        "prepare-cargo + 3 units x 3 providers must reuse the rust kind workflow"
    );

    let again = generate(&root);
    assert_eq!(
        again.workflow("ci-pr.yml"),
        pr_yaml,
        "aggregate provider callers must be deterministic"
    );
}

#[test]
fn hosted_only_emits_only_hosted_callers() {
    let root = unique_dir("hosted-only");
    write_rust_fixture(&root, 2);
    write_workflow_config(
        &root,
        &format!("providers = [\"github-hosted\"]\nautomatic_providers = [\"github-hosted\"]\n\n{HOSTED_SELECTOR}"),
    );
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
    write_workflow_config(
        &root,
        &format!(
            "providers = [\"velnor\"]\nautomatic_providers = [\"velnor\"]\n\n{VELNOR_SELECTOR}"
        ),
    );
    let pr = parse_jobs(&generate(&root).workflow("ci-pr.yml"));
    assert!(pr.keys().any(|id| id.starts_with("velnor-rust-")));
    assert!(!pr.keys().any(|id| aggregate_provider_caller_id(id)
        .is_some_and(|(provider, _)| provider == "github-hosted")));
    let kind = parse_jobs(&generate(&root).workflow("ci-unit-rust.yml"));
    assert!(kind.contains_key("verify-velnor"));
    assert!(!kind.contains_key("verify-github-hosted"));
}

#[test]
fn automatic_all_gates_pair_except_fork_pr_trust_exclusion() {
    let root = unique_dir("automatic-all-gates");
    write_rust_fixture(&root, 1);
    write_workflow_config(&root, &all_providers_config());

    let generated = generate(&root);
    let rust = parse_jobs(&generated.workflow("ci-unit-rust.yml"));
    let hosted_if = job_if(&rust["verify-github-hosted"]);
    let self_hosted_if = job_if(&rust["verify-github-self-hosted"]);
    let velnor_if = job_if(&rust["verify-velnor"]);
    assert!(
        hosted_if.contains("inputs.provider == 'github-hosted'"),
        "hosted verify job must require its provider: {hosted_if}"
    );
    assert!(
        self_hosted_if.contains("inputs.provider == 'github-self-hosted'"),
        "self-hosted verify job must require its provider: {self_hosted_if}"
    );
    assert!(
        velnor_if.contains("inputs.provider == 'velnor'"),
        "Velnor verify job must require its provider: {velnor_if}"
    );
    for (provider, gate) in [
        ("github-hosted", &hosted_if),
        ("github-self-hosted", &self_hosted_if),
        ("velnor", &velnor_if),
    ] {
        assert!(
            gate.contains("github.event_name != 'workflow_dispatch'"),
            "{provider} must admit automatic events: {gate}"
        );
        assert!(
            gate.contains(&format!(
                "contains(format(',{{0}},', github.event.inputs.providers), ',{provider},')"
            )),
            "{provider} must admit dispatches selecting it: {gate}"
        );
    }
    // Fork and bot pull requests are untrusted: local providers exclude them
    // inline, hosted executes them.
    let untrusted =
        "!(github.event_name == 'pull_request' && (github.event.pull_request.head.repo.fork || github.event.pull_request.user.type == 'Bot'))";
    assert!(
        self_hosted_if.contains(untrusted),
        "self-hosted must exclude untrusted PRs: {self_hosted_if}"
    );
    assert!(
        velnor_if.contains(untrusted),
        "Velnor must exclude untrusted PRs: {velnor_if}"
    );
    assert!(
        !hosted_if.contains("pull_request.head.repo.fork"),
        "hosted must not exclude fork PRs: {hosted_if}"
    );

    let pr = parse_jobs(&generated.workflow("ci-pr.yml"));
    assert!(
        !pr.keys().any(|id| id.contains("admission")),
        "admission is an inline gate now, not a job: {pr:?}"
    );
    assert_eq!(name_head(&job_name(&pr["plan"])), "Control");
    assert_eq!(job_name(&pr["ci-required"]), "ci-required");
    assert!(pr.contains_key("github-hosted-rust-crate00"));
    assert!(pr.contains_key("github-self-hosted-rust-crate00"));
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
    write_workflow_config(&root, &all_providers_config());
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
    write_workflow_config(
        &root,
        &format!("providers = [\"github-hosted\"]\nautomatic_providers = [\"github-hosted\"]\n\n{HOSTED_SELECTOR}"),
    );
    let hosted_only = generate(&root);
    let project = fs::read_to_string(hosted_only.output.join(".github/ci/project.toml")).unwrap();
    let swift_id = project
        .lines()
        .filter_map(|line| line.strip_prefix("id = \""))
        .map(|line| line.trim_end_matches('"').to_owned())
        .find(|id| id.starts_with("swift-"))
        .expect("scanned Swift unit id");
    write_workflow_config(&root, &all_providers_config());
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
