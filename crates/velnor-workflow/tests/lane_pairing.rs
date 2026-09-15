//! Structural `runners` pairing: every selected unit has matching lane callers,
//! comparison names stay lane-first, and automatic gates do not silently drop
//! Velnor except the documented fork-PR admission failure.

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
    let root =
        std::env::temp_dir().join(format!("lane-pairing-{name}-{}-{id}", std::process::id()));
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
            "schema = 1\n\n[generator]\nrepository = \"example/monorepo\"\n\n[workflow]\n{workflow}"
        ),
    )
    .unwrap();
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

fn generate_fail(root: &Path) -> String {
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
        !outcome.status.success(),
        "generation should have failed:\n{}",
        String::from_utf8_lossy(&outcome.stdout)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&outcome.stderr),
        String::from_utf8_lossy(&outcome.stdout)
    )
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

fn aggregate_lane_caller_id(job_id: &str) -> Option<(&str, &str)> {
    job_id
        .strip_prefix("github-")
        .map(|id| ("github", id))
        .or_else(|| job_id.strip_prefix("velnor-").map(|id| ("velnor", id)))
        .filter(|(_, id)| *id != "lane-admission" && !id.is_empty())
}

fn lane_token(name: &str) -> &str {
    name.split(" / ").next().unwrap_or(name)
}

fn assert_aggregate_lane_callers_use_sidebar_names(jobs: &BTreeMap<String, Value>) {
    for (id, job) in jobs {
        let name = job_name(job);
        assert!(!name.is_empty(), "job `{id}` is missing a display name");
        if let Some((_, unit)) = aggregate_lane_caller_id(id) {
            assert!(
                name.starts_with("Rust · "),
                "aggregate lane caller `{id}` must display as a sidebar unit name, got `{name}`"
            );
            assert!(
                name.ends_with(unit.strip_prefix("rust-").unwrap_or(unit)),
                "aggregate lane caller `{id}` must keep the unit suffix in `{name}`"
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
fn both_emits_one_github_and_one_velnor_job_per_unit() {
    let root = unique_dir("both-bijection");
    write_rust_fixture(&root, 3);
    write_workflow_config(
        &root,
        "runners = \"both\"\nautomatic = \"both\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\n",
    );
    let generated = generate(&root);
    let rust = generated.workflow("ci-unit-rust.yml");
    let kind_jobs = parse_jobs(&rust);
    assert!(kind_jobs.contains_key("verify-github"));
    assert!(kind_jobs.contains_key("verify-velnor"));
    assert_eq!(job_name(&kind_jobs["verify-github"]), "GitHub");
    assert_eq!(job_name(&kind_jobs["verify-velnor"]), "Velnor");

    let pr_yaml = generated.workflow("ci-pr.yml");
    let pr = parse_jobs(&pr_yaml);
    assert_aggregate_lane_callers_use_sidebar_names(&pr);

    let mut github = BTreeSet::new();
    let mut velnor = BTreeSet::new();
    for id in pr.keys() {
        match aggregate_lane_caller_id(id) {
            Some(("github", unit)) => {
                github.insert(unit.to_owned());
            }
            Some(("velnor", unit)) => {
                velnor.insert(unit.to_owned());
            }
            _ => {}
        }
    }
    assert_eq!(
        github,
        BTreeSet::from([
            "rust-crate00".to_owned(),
            "rust-crate01".to_owned(),
            "rust-crate02".to_owned(),
        ])
    );
    assert_eq!(github, velnor, "both must emit a lane bijection: {pr:?}");

    let prep = pr.get("prepare-cargo").expect("cargo prep caller");
    assert_eq!(job_name(prep), "Control / Prepare Cargo");
    assert!(
        prep.get("with")
            .and_then(Value::as_mapping)
            .and_then(|with| with.get("lane"))
            .and_then(Value::as_str)
            == Some("control"),
        "prep caller must invoke the kind reusable on the control lane"
    );
    assert!(
        !pr.contains_key("github-prepare-cargo-sources"),
        "cargo prep must not invent a GitHub counterpart caller"
    );

    let prep_inner = kind_jobs
        .get("velnor-prepare-cargo-sources")
        .expect("cargo prep stays inside the kind reusable");
    assert_eq!(job_name(prep_inner), "prepare-cargo");
    assert!(
        job_if(prep_inner).contains("inputs.lane == 'control'"),
        "inner prep must belong to the control lane: {}",
        job_if(prep_inner)
    );

    for unit in &github {
        assert!(pr.contains_key(&format!("github-{unit}")));
        assert!(pr.contains_key(&format!("velnor-{unit}")));
        assert!(
            kind_jobs["verify-velnor"]
                .get("steps")
                .and_then(Value::as_sequence)
                .into_iter()
                .flatten()
                .any(|step| step.get("name").and_then(Value::as_str)
                    == Some("Velnor runner identity")),
            "Velnor verify job must carry runner identity"
        );
        assert!(
            kind_jobs["verify-github"]
                .get("steps")
                .and_then(Value::as_sequence)
                .into_iter()
                .flatten()
                .all(|step| step.get("name").and_then(Value::as_str)
                    != Some("Velnor runner identity")),
            "GitHub verify job must not carry Velnor identity"
        );
    }

    assert_eq!(job_name(&pr["plan"]), "Control / Planning");
    assert_eq!(job_name(&pr["prepare-cargo"]), "Control / Prepare Cargo");
    assert_eq!(
        pr_yaml
            .matches("uses: ./.github/workflows/ci-unit-rust.yml")
            .count(),
        7,
        "plan + prepare-cargo + 3 github + 3 velnor callers must reuse the rust kind workflow"
    );

    let again = generate(&root);
    assert_eq!(
        again.workflow("ci-pr.yml"),
        pr_yaml,
        "aggregate lane callers must be deterministic"
    );
}

#[test]
fn github_mode_emits_only_github_comparison_jobs() {
    let root = unique_dir("github-only");
    write_rust_fixture(&root, 2);
    write_workflow_config(
        &root,
        "runners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n",
    );
    let pr = parse_jobs(&generate(&root).workflow("ci-pr.yml"));
    assert!(pr.keys().any(|id| id.starts_with("github-rust-")));
    assert!(!pr.keys().any(|id| id.starts_with("velnor-")));
    let kind = parse_jobs(&generate(&root).workflow("ci-unit-rust.yml"));
    assert!(kind.contains_key("verify-github"));
    assert!(!kind.contains_key("verify-velnor"));
}

#[test]
fn velnor_mode_emits_only_velnor_comparison_jobs() {
    let root = unique_dir("velnor-only");
    write_rust_fixture(&root, 2);
    write_workflow_config(
        &root,
        "runners = \"velnor\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\n",
    );
    let pr = parse_jobs(&generate(&root).workflow("ci-pr.yml"));
    assert!(pr.keys().any(|id| id.starts_with("velnor-rust-")));
    assert!(!pr
        .keys()
        .any(|id| aggregate_lane_caller_id(id).is_some_and(|(lane, _)| lane == "github")));
    let kind = parse_jobs(&generate(&root).workflow("ci-unit-rust.yml"));
    assert!(kind.contains_key("verify-velnor"));
    assert!(!kind.contains_key("verify-github"));
}

#[test]
fn automatic_both_gates_pair_except_fork_pr_admission() {
    let root = unique_dir("automatic-both-gates");
    write_rust_fixture(&root, 1);
    write_workflow_config(
        &root,
        "runners = \"both\"\nautomatic = \"both\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\npull_request_on_velnor = true\n",
    );
    let approved = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github-gen/velnor-workflow.toml"),
    )
    .unwrap();
    let path = root.join(".github-gen/velnor-workflow.toml");
    let current = fs::read_to_string(&path).unwrap();
    let mut lines = current
        .lines()
        .filter(|line| {
            ![
                "velnor_labels =",
                "velnor_runner_group =",
                "pull_request_on_velnor =",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for prefix in [
        "velnor_labels =",
        "velnor_runner_group =",
        "pull_request_on_velnor =",
    ] {
        if let Some(line) = approved.lines().find(|line| line.starts_with(prefix)) {
            lines.push(line.to_owned());
        }
    }
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

    let generated = generate(&root);
    let rust = parse_jobs(&generated.workflow("ci-unit-rust.yml"));
    let github_if = job_if(&rust["verify-github"]);
    let velnor_if = job_if(&rust["verify-velnor"]);
    assert!(
        github_if.contains("inputs.lane == 'github'"),
        "GitHub verify job must require the github lane: {github_if}"
    );
    assert!(
        velnor_if.contains("inputs.lane == 'velnor'"),
        "Velnor verify job must require the velnor lane: {velnor_if}"
    );
    for event in [
        "github.event_name == 'push'",
        "github.event_name == 'schedule'",
    ] {
        assert!(
            github_if.contains(event),
            "GitHub must admit {event}: {github_if}"
        );
        assert!(
            velnor_if.contains(event),
            "Velnor must admit {event} when automatic=both: {velnor_if}"
        );
    }
    assert!(
        github_if.contains("github.event_name == 'pull_request'"),
        "GitHub admits pull_request: {github_if}"
    );
    assert!(
        velnor_if.contains(
            "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository"
        ),
        "Velnor admits same-repo pull_request only: {velnor_if}"
    );
    assert!(
        !velnor_if.contains("github.event.pull_request.head.repo.full_name != github.repository"),
        "Velnor must not admit fork pull_request: {velnor_if}"
    );

    let pr = parse_jobs(&generated.workflow("ci-pr.yml"));
    let admission = pr
        .get("velnor-lane-admission")
        .expect("fork PR admission job");
    assert_eq!(job_name(admission), "Control / Velnor admission");
    assert!(job_if(admission).contains(
        "github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name != github.repository"
    ));
    assert_eq!(lane_token(&job_name(&pr["plan"])), "Control");
    assert_eq!(job_name(&pr["ci-required"]), "ci-required");
    assert!(pr.contains_key("github-rust-crate00"));
    assert!(pr.contains_key("velnor-rust-crate00"));
    assert!(pr.contains_key("prepare-cargo"));
}

#[test]
fn both_refuses_swift_without_explicit_opt_out() {
    let root = unique_dir("swift-both-refuse");
    write_rust_fixture(&root, 1);
    fs::create_dir_all(root.join("Sources/App")).unwrap();
    fs::write(root.join("Package.swift"), "// swift-tools-version: 5.9\n").unwrap();
    write_workflow_config(
        &root,
        "runners = \"both\"\nautomatic = \"both\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\n",
    );
    let error = generate_fail(&root);
    assert!(
        error.contains("runners=both cannot generate unit")
            && (error.contains("swift") || error.contains("Swift")),
        "both-mode Swift must fail generation: {error}"
    );
}

#[test]
fn both_allows_swift_with_explicit_github_jobs() {
    let root = unique_dir("swift-github-opt-out");
    write_rust_fixture(&root, 1);
    fs::create_dir_all(root.join("Sources/App")).unwrap();
    fs::write(root.join("Package.swift"), "// swift-tools-version: 5.9\n").unwrap();
    write_workflow_config(
        &root,
        "runners = \"github\"\ngithub_runner = \"ubuntu-24.04\"\n",
    );
    let github_only = generate(&root);
    let project = fs::read_to_string(github_only.output.join(".github/ci/project.toml")).unwrap();
    let swift_id = project
        .lines()
        .filter_map(|line| line.strip_prefix("id = \""))
        .map(|line| line.trim_end_matches('"').to_owned())
        .find(|id| id.starts_with("swift-"))
        .expect("scanned Swift unit id");
    write_workflow_config(
        &root,
        "runners = \"both\"\nautomatic = \"both\"\ngithub_runner = \"ubuntu-24.04\"\nvelnor_labels = [\"self-hosted\", \"example-runner\"]\n",
    );
    let mut config = fs::read_to_string(root.join(".github-gen/velnor-workflow.toml")).unwrap();
    let _ = write!(
        config,
        r#"

[[declare]]
primitive = "swift-package-pipeline"
units = ["{swift_id}"]

[declare.args]
jobs = ["github"]
"#
    );
    fs::write(root.join(".github-gen/velnor-workflow.toml"), config).unwrap();
    let generated = generate(&root);
    let swift = parse_jobs(&generated.workflow("ci-unit-swift.yml"));
    assert!(
        swift.contains_key("verify-github"),
        "explicit github opt-out must keep the collapsed hosted verify job: {swift:?}"
    );
    assert!(
        !swift.contains_key("verify-velnor"),
        "explicit github opt-out must not emit a Velnor verify job: {swift:?}"
    );
    assert_eq!(job_name(&swift["verify-github"]), "GitHub");
    // The collapsed job gates on the caller's `inputs.unit` membership, never
    // on an enumeration of unit ids: the callee stays O(1) in units.
    assert!(
        job_if(&swift["verify-github"]).contains(
            "contains(format(',{0},', inputs.selected_units), format(',{0},', inputs.unit))"
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
        generated
            .workflow("ci-pr.yml")
            .contains(&format!("      unit: {swift_id}\n      lane: github")),
        "the aggregate passes the declared unit to the swift reusable"
    );
    let pr = parse_jobs(&generated.workflow("ci-pr.yml"));
    assert!(
        pr.contains_key(&format!("github-{swift_id}")),
        "explicit github opt-out must keep the aggregate github caller: {pr:?}"
    );
}
