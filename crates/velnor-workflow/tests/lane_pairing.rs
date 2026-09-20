//! Structural provider pairing for the hosted-plus-Velnor repository set.
//! Every selected unit has one caller per provider, while Cargo source prep
//! remains one control-provider caller.

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
    fs::write(
        root.join(".github-gen/velnor-workflow.toml"),
        "schema = 2\n\n[generator]\nrepository = \"example/monorepo\"\n\n\
         [workflow]\nproviders = [\"github-hosted\", \"velnor\"]\n\
         automatic_providers = [\"github-hosted\", \"velnor\"]\n\
         default_dispatch_providers = [\"github-hosted\", \"velnor\"]\n\
         default_branch = \"main\"\n\n\
         [workflow.selectors.github-hosted]\nruns_on = [\"ubuntu-24.04\"]\n\n\
         [workflow.selectors.velnor]\nruns_on = [\"self-hosted\", \"example-runner\"]\n",
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

fn aggregate_provider_caller_id(job_id: &str) -> Option<(&str, &str)> {
    job_id
        .strip_prefix("github-hosted-")
        .map(|id| ("github-hosted", id))
        .or_else(|| job_id.strip_prefix("velnor-").map(|id| ("velnor", id)))
        .filter(|(_, id)| !id.is_empty())
}

fn assert_caller_names(jobs: &BTreeMap<String, Value>) {
    for (id, job) in jobs {
        if let Some((_, unit)) = aggregate_provider_caller_id(id) {
            let name = job_name(job);
            assert!(
                name.starts_with("Rust · ") && name.ends_with(unit),
                "aggregate caller `{id}` must use the sidebar unit name, got `{name}`"
            );
        }
    }
}

#[test]
fn hosted_and_velnor_emit_one_caller_per_unit() {
    let root = unique_dir("hosted-velnor");
    write_rust_fixture(&root, 3);
    let generated = generate(&root);

    let rust = parse_jobs(&generated.workflow("ci-unit-rust.yml"));
    assert!(rust.contains_key("verify-github-hosted"));
    assert!(rust.contains_key("verify-velnor"));
    assert!(!rust.contains_key("verify-github-self-hosted"));
    assert_eq!(job_name(&rust["verify-github-hosted"]), "GitHub · hosted");
    assert_eq!(job_name(&rust["verify-velnor"]), "velnor");

    let pr_yaml = generated.workflow("ci-pr.yml");
    let pr = parse_jobs(&pr_yaml);
    assert_caller_names(&pr);
    assert!(
        !pr.keys().any(|id| id.starts_with("github-self-hosted-")),
        "the two-provider config emits no self-hosted callers"
    );
    let expected = BTreeSet::from([
        "rust-crate00".to_owned(),
        "rust-crate01".to_owned(),
        "rust-crate02".to_owned(),
    ]);
    let mut hosted = BTreeSet::new();
    let mut velnor = BTreeSet::new();
    for id in pr.keys() {
        match aggregate_provider_caller_id(id) {
            Some(("github-hosted", unit)) => {
                hosted.insert(unit.to_owned());
            }
            Some(("velnor", unit)) => {
                velnor.insert(unit.to_owned());
            }
            _ => {}
        }
    }
    assert_eq!(hosted, expected);
    assert_eq!(velnor, expected);

    let prep = pr.get("prepare-cargo").expect("cargo prep caller");
    assert_eq!(job_name(prep), "Control / Prepare Cargo");
    assert_eq!(
        prep.get("with")
            .and_then(Value::as_mapping)
            .and_then(|with| with.get("provider"))
            .and_then(Value::as_str),
        Some("control"),
        "prep caller invokes the reusable on the control provider"
    );
    assert!(
        !pr.keys().any(|id| id.ends_with("-prepare-cargo-sources")),
        "Cargo prep stays one control caller, not per-provider callers"
    );
    let prep_inner = rust
        .get("velnor-prepare-cargo-sources")
        .expect("Velnor kind reusable owns Cargo preparation");
    assert_eq!(job_name(prep_inner), "prepare-cargo");
    assert!(
        prep_inner
            .get("if")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("inputs.provider == 'control'"),
        "inner preparation belongs to the control provider"
    );

    for provider in ["github-hosted", "velnor"] {
        for unit in &expected {
            assert!(pr.contains_key(&format!("{provider}-{unit}")));
        }
    }
    assert_eq!(job_name(&pr["plan"]), "Control / Planning");
    assert_eq!(job_name(&pr["prepare-cargo"]), "Control / Prepare Cargo");
    assert_eq!(
        pr_yaml
            .matches("uses: ./.github/workflows/ci-unit-rust.yml")
            .count(),
        7,
        "prepare-cargo + 3 units × 2 providers reuse the Rust workflow"
    );

    let again = generate(&root);
    assert_eq!(
        again.workflow("ci-pr.yml"),
        pr_yaml,
        "provider callers render deterministically"
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(generated.output);
    let _ = fs::remove_dir_all(again.output);
}
