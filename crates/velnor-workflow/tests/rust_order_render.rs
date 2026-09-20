//! Generated runtime contracts keep scanner validation order for both config schemas.

#![expect(
    clippy::unwrap_used,
    reason = "fixture setup failures should identify the failing assertion"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const COMMAND_KEYS_SCHEMA_1: [&str; 4] = [
    "github_pr_commands",
    "github_full_commands",
    "velnor_pr_commands",
    "velnor_full_commands",
];
const COMMAND_KEYS_SCHEMA_2: [&str; 2] = ["pr_commands", "full_commands"];

fn temporary_root(label: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let suffix = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "velnor-rust-order-{label}-{}-{suffix}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn generated_project(schema: u8, use_nextest: bool) -> String {
    let label = format!(
        "schema-{schema}-{}",
        if use_nextest { "nextest" } else { "test" }
    );
    let workspace = temporary_root(&label);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(if schema == 1 {
        "tests/fixtures/synthetic-workspace"
    } else {
        "tests/fixtures-s2/polyglot"
    });
    let root = workspace.join("repo");
    copy_tree(&fixture, &root);
    let config_path = root.join(".github-gen/velnor-workflow.toml");
    let config = fs::read_to_string(&config_path).unwrap().replace(
        "\n[workflow]",
        "\nrevision = \"1111111111111111111111111111111111111111\"\n\n[[units]]\nid = \"rust-policy-gate\"\nkind = \"rust\"\nroot = \".\"\nworkspace_check = true\nci_tasks = [\"check-smoke\"]\n\n[workflow]",
    );
    fs::write(config_path, config).unwrap();
    fs::write(
        root.join("mise.toml"),
        "[tasks.check-smoke]\nrun = \"echo smoke\"\n",
    )
    .unwrap();
    // The fixtures deliberately omit a lockfile; add the source fact that
    // toggles the scanner's lock flag without invoking Cargo.
    fs::write(root.join("Cargo.lock"), "version = 3\n").unwrap();
    if use_nextest {
        fs::create_dir_all(root.join(".config")).unwrap();
        fs::write(root.join(".config/nextest.toml"), "").unwrap();
    }
    let output = workspace.join("generated");
    let mut arguments = vec!["--plain", "--default-branch", "main"];
    if schema == 1 {
        arguments.extend(["--runners", "github"]);
    }
    arguments.extend(["--output", output.to_str().unwrap(), root.to_str().unwrap()]);
    let result = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "schema {schema} generation failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let project = fs::read_to_string(output.join(".github/ci/project.toml")).unwrap();
    let _ = fs::remove_dir_all(workspace);
    project
}

fn command_labels(commands: &[toml::Value]) -> Vec<&'static str> {
    commands
        .iter()
        .map(|command| command.as_str().unwrap())
        .filter_map(|command| {
            if command.contains("cargo fmt ") || command.contains("mbx fmt ") {
                Some("fmt")
            } else if command.contains("cargo clippy ") || command.contains("mbx clippy ") {
                Some("clippy")
            } else if command.contains("cargo nextest ") || command.contains("mbx nextest ") {
                Some("nextest")
            } else if command.contains("cargo test ") || command.contains("mbx test ") {
                Some("test")
            } else {
                None
            }
        })
        .collect()
}

fn assert_rust_order(project: &str, schema: u8, use_nextest: bool) {
    let document: toml::Value = toml::from_str(project).unwrap();
    let units = document["unit"].as_array().unwrap();
    let keys = if schema == 1 {
        &COMMAND_KEYS_SCHEMA_1[..]
    } else {
        &COMMAND_KEYS_SCHEMA_2[..]
    };
    let policy_gate = units
        .iter()
        .find(|unit| unit["id"].as_str() == Some("rust-policy-gate"));
    assert!(
        policy_gate.is_some(),
        "schema {schema} omitted the declared workspace gate"
    );
    let policy_gate = policy_gate.unwrap();
    let expected_gate_commands = [
        "mbx check --workspace --all-targets --locked",
        "mise run check-smoke",
    ];
    let mut gate_keys_checked = 0;
    for key in keys {
        let Some(commands) = policy_gate.get(*key).and_then(toml::Value::as_array) else {
            continue;
        };
        let observed = commands
            .iter()
            .map(|command| command.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(observed, expected_gate_commands, "{key} policy gate");
        gate_keys_checked += 1;
    }
    assert!(
        gate_keys_checked > 0,
        "schema {schema} omitted gate commands"
    );
    if schema == 2 {
        assert_eq!(policy_gate["workspace_check"].as_bool(), Some(true));
    }
    let mut checked = 0;
    for unit in units {
        if unit["kind"].as_str() != Some("rust") {
            continue;
        }
        let unit_id = unit["id"].as_str();
        assert!(
            unit_id.is_some(),
            "schema {schema} emitted a Rust unit without an id"
        );
        let unit_id = unit_id.unwrap();
        if unit_id == "rust-policy-gate" {
            continue;
        }
        for key in keys {
            let commands = unit.get(*key).and_then(toml::Value::as_array);
            assert!(
                commands.is_some(),
                "{unit_id} omitted its {key} Rust command array"
            );
            let commands = commands.unwrap();
            let labels = command_labels(commands);
            let expected = if use_nextest {
                ["fmt", "clippy", "nextest"]
            } else {
                ["fmt", "clippy", "test"]
            };
            assert_eq!(
                labels.len(),
                commands.len(),
                "{unit_id} {key} contains an unknown Rust validation command"
            );
            assert_eq!(labels, expected, "{unit_id} {key}");
            let command_text = commands
                .iter()
                .map(|command| command.as_str().unwrap())
                .collect::<Vec<_>>();
            let clippy = command_text
                .iter()
                .find(|command| command.contains(" clippy "))
                .unwrap();
            assert!(clippy.contains("--profile test"), "{clippy}");
            assert!(clippy.contains("--all-targets"), "{clippy}");
            assert!(clippy.contains("--all-features"), "{clippy}");
            assert!(clippy.contains("-D warnings"), "{clippy}");
            if use_nextest {
                let nextest = command_text
                    .iter()
                    .find(|command| command.contains(" nextest "));
                assert!(nextest.is_some(), "{unit_id} {key} omitted nextest");
                let nextest = nextest.unwrap();
                assert!(nextest.contains("--no-tests pass"), "{nextest}");
            }
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "schema {schema} emitted no scanner Rust command arrays"
    );
}

#[test]
fn generated_rust_commands_keep_clippy_before_tests_in_both_schemas() {
    for schema in [1, 2] {
        for use_nextest in [false, true] {
            let project = generated_project(schema, use_nextest);
            assert_rust_order(&project, schema, use_nextest);
        }
    }
}
