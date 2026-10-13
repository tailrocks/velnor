//! The synthetic-workspace contract of the primitive registry.
//!
//! The fixture is a small polyglot repository — three Rust crates with
//! different shapes, a Bun package, a root Dockerfile, and Markdown docs. It
//! pins the two properties the registry must hold on any repository: one unit
//! is exactly one workflow file and one CI graph node, and nothing about the
//! repository's name, path, or unit ids is known to the renderer.

#![expect(
    clippy::unwrap_used,
    reason = "a test whose setup fails should panic loudly"
)]
#![expect(
    clippy::expect_used,
    reason = "a test whose setup fails should panic loudly"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A generated tree, and the fixture it came from.
struct Generated {
    output: PathBuf,
}

impl Generated {
    fn workflow_files(&self) -> Vec<String> {
        let mut names = fs::read_dir(self.output.join(".github/workflows"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn workflow(&self, name: &str) -> String {
        fs::read_to_string(self.output.join(".github/workflows").join(name)).unwrap()
    }

    fn project_toml(&self) -> String {
        fs::read_to_string(self.output.join(".github/ci/project.toml")).unwrap()
    }

    fn unit_ids(&self) -> Vec<String> {
        self.project_toml()
            .lines()
            .filter_map(|line| line.strip_prefix("id = \""))
            .map(|line| line.trim_end_matches('"').to_owned())
            .collect()
    }
}

fn copy_fixture(destination: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic-workspace");
    copy_tree(&source, destination);
    destination.to_path_buf()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn generate(root: &Path) -> Generated {
    let output = root.parent().unwrap().join(format!(
        "{}-out",
        root.file_name().and_then(|name| name.to_str()).unwrap()
    ));
    // A fresh output tree every run: the generator refuses to overwrite a
    // surface it did not write, which is exactly the behavior under test
    // elsewhere.
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
        "generation failed for {}:\n{}",
        root.display(),
        String::from_utf8_lossy(&outcome.stderr)
    );
    Generated { output }
}

/// The nested workflow file the registry renders for one unit id.
fn nested_file(unit_id: &str) -> String {
    let prefixes = [
        "bun", "docker", "docs", "gradle", "homebrew", "node", "opentofu", "rust", "swift",
    ];
    let (kind, name) = prefixes
        .iter()
        .find_map(|prefix| {
            unit_id
                .strip_prefix(&format!("{prefix}-"))
                .map(|name| (*prefix, name))
        })
        .unwrap_or_else(|| unit_id.split_once('-').unwrap_or((unit_id, unit_id)));
    format!("ci-{kind}-{name}.yml")
}

fn write_config(root: &Path, config: &str) {
    let directory = root.join(".github-gen");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("velnor-workflow.toml"), config).unwrap();
}

#[test]
fn every_unit_renders_exactly_one_workflow() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let generated = generate(&root);

    let units = generated.unit_ids();
    assert_eq!(
        units,
        vec![
            "bun-synthetic-app",
            "docker",
            "docs",
            "rust-alpha",
            "rust-beta",
            "rust-gamma",
        ]
    );
    for unit in &units {
        let file = nested_file(unit);
        let content = generated.workflow(&file);
        assert!(
            content.contains(&format!("CI_UNIT_ID: {unit}")),
            "{file} does not run {unit}"
        );
    }
    // One workflow per unit, plus the two static families this fixture
    // triggers (ci-policy.yml, maintenance.yml) and the three aggregates.
    assert_eq!(generated.workflow_files().len(), units.len() + 3 + 2);
}

#[test]
fn adding_a_crate_adds_one_workflow_and_one_graph_node() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let before = generate(&root);
    let before_files = before.workflow_files();
    let before_units = before.unit_ids();
    let before_callers = before
        .workflow("ci-pr.yml")
        .matches("uses: ./.github/workflows/")
        .count();

    let crate_dir = root.join("crates/delta/src");
    fs::create_dir_all(&crate_dir).unwrap();
    fs::write(
        root.join("crates/delta/Cargo.toml"),
        "[package]\nname = \"delta\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        crate_dir.join("lib.rs"),
        "pub fn delta() -> u32 {\n    4\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("Cargo.toml"),
        fs::read_to_string(root.join("Cargo.toml"))
            .unwrap()
            .replace(
                "  \"crates/gamma\",",
                "  \"crates/gamma\",\n  \"crates/delta\",",
            ),
    )
    .unwrap();

    let after = generate(&root);
    let after_files = after.workflow_files();
    let added = after_files
        .iter()
        .filter(|file| !before_files.contains(file))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(added, vec!["ci-rust-delta.yml".to_owned()]);
    assert_eq!(after.unit_ids().len(), before_units.len() + 1);
    // Exactly one new graph node: the aggregate composes one more caller.
    let growth = after
        .workflow("ci-pr.yml")
        .matches("uses: ./.github/workflows/")
        .count()
        - before_callers;
    assert_eq!(growth, 1);

    // Removing a crate removes exactly what adding it added.
    fs::remove_dir_all(root.join("crates/delta")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        fs::read_to_string(root.join("Cargo.toml"))
            .unwrap()
            .replace(
                "  \"crates/gamma\",\n  \"crates/delta\",",
                "  \"crates/gamma\",",
            ),
    )
    .unwrap();
    let restored = generate(&root);
    assert_eq!(restored.workflow_files(), before_files);
    assert_eq!(restored.unit_ids(), before_units);
}

#[test]
fn the_repository_name_never_reaches_the_renderer() {
    let workspace = tempfile();
    let one = generate(&copy_fixture(&workspace.join("all-the-worlds-a-repo-one")));
    let other = generate(&copy_fixture(&workspace.join("totally-different-name-two")));

    // Different fixture directories, same shape, same bytes: every generated
    // workflow and the actionlint contract are identical. The project config
    // is the one place the repository records its own identity, so only the
    // `repository` line may differ.
    assert_eq!(one.workflow_files(), other.workflow_files());
    for file in one.workflow_files() {
        assert_eq!(one.workflow(&file), other.workflow(&file), "{file}");
    }
    let actionlint = |generated: &Generated| {
        fs::read_to_string(generated.output.join(".github/actionlint.yaml")).unwrap()
    };
    assert_eq!(actionlint(&one), actionlint(&other));
    for (a, b) in one.project_toml().lines().zip(other.project_toml().lines()) {
        let same = a == b || (a.starts_with("repository = ") && b.starts_with("repository = "));
        assert!(same, "projects diverge: {a} != {b}");
    }
}

const FULL_CONFIG: &str = r#"schema = 1

[generator]
repository = "example/synthetic"

[workflow]
velnor_labels = ["self-hosted", "example-lane"]

[[declare]]
primitive = "bun-package-pipeline"
units = ["bun-synthetic-app"]

[[declare]]
primitive = "docker-image-pipeline"
units = ["docker"]

[[declare]]
primitive = "docs-lint-pipeline"
units = ["docs"]

[[declare]]
primitive = "rust-crate-pipeline"
units = ["rust-alpha"]

[[declare]]
primitive = "rust-crate-pipeline"
units = ["rust-beta"]

[[declare]]
primitive = "rust-crate-pipeline"
units = ["rust-gamma"]

[[declare]]
primitive = "lane-matrix"

[[declare]]
primitive = "cache-contract"

[[declare]]
primitive = "affected-plan"

[[declare]]
primitive = "unit-aggregation"
file = "ci-pr.yml"

[[declare]]
primitive = "unit-aggregation"
file = "ci-main.yml"

[[declare]]
primitive = "unit-aggregation"
file = "nightly.yml"
"#;

#[test]
fn a_declared_config_reproduces_the_default_surface() {
    let workspace = tempfile();
    let root = copy_fixture(&workspace.join("fixture"));
    let default = generate(&root);

    write_config(&root, FULL_CONFIG);
    let declared = generate(&root);

    assert_eq!(declared.workflow_files(), default.workflow_files());
    for file in default.workflow_files() {
        assert_eq!(
            declared.workflow(&file),
            default.workflow(&file),
            "{file} changed under a full declaration"
        );
    }
    assert_eq!(declared.unit_ids(), default.unit_ids());
}

#[test]
fn a_config_that_fails_validation_stops_generation() {
    let cases: Vec<(&str, String)> = vec![
        (
            "unknown primitive",
            format!(
                "{}\n[[declare]]\nprimitive = \"not-a-primitive\"\nfile = \"nope.yml\"\n",
                config_without_declares()
            ),
        ),
        (
            "unknown argument",
            format!(
                "{}\n[[declare]]\nprimitive = \"rust-crate-pipeline\"\nunits = [\"rust-alpha\"]\n\n[declare.args]\nno_such_argument = true\n",
                config_without_declares()
            ),
        ),
        (
            "wrong kind",
            format!(
                "[[declare]]\nprimitive = \"bun-package-pipeline\"\nunits = [\"rust-alpha\"]\nfile = \"ci-rust-alpha.yml\"\n\n{}",
                config_without_declares()
            ),
        ),
        (
            "incomplete family",
            "[[declare]]\nprimitive = \"rust-crate-pipeline\"\nunits = [\"rust-alpha\"]\n\n[[declare]]\nprimitive = \"rust-crate-pipeline\"\nunits = [\"rust-beta\"]\n\n[[declare]]\nprimitive = \"affected-plan\"\n\n[[declare]]\nprimitive = \"unit-aggregation\"\nfile = \"ci-pr.yml\"\n".to_owned(),
        ),
        (
            "non-canonical order",
            "[[declare]]\nprimitive = \"rust-crate-pipeline\"\nunits = [\"rust-beta\"]\n\n[[declare]]\nprimitive = \"rust-crate-pipeline\"\nunits = [\"rust-alpha\"]\n\n[[declare]]\nprimitive = \"rust-crate-pipeline\"\nunits = [\"rust-gamma\"]\n\n[[declare]]\nprimitive = \"affected-plan\"\n\n[[declare]]\nprimitive = \"unit-aggregation\"\nfile = \"ci-pr.yml\"\n".to_owned(),
        ),
        (
            "renamed file",
            "[[declare]]\nprimitive = \"rust-crate-pipeline\"\nunits = [\"rust-alpha\"]\nfile = \"renamed.yml\"\n\n[[declare]]\nprimitive = \"rust-crate-pipeline\"\nunits = [\"rust-beta\"]\n\n[[declare]]\nprimitive = \"rust-crate-pipeline\"\nunits = [\"rust-gamma\"]\n\n[[declare]]\nprimitive = \"affected-plan\"\n\n[[declare]]\nprimitive = \"unit-aggregation\"\nfile = \"ci-pr.yml\"\n".to_owned(),
        ),
    ];
    for (name, config) in cases {
        let workspace = tempfile();
        let root = copy_fixture(&workspace.join("fixture"));
        write_config(&root, &config);
        let status = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
            .args([
                "--plain",
                "--default-branch",
                "main",
                "--output",
                workspace.join("out").to_str().unwrap(),
                root.to_str().unwrap(),
            ])
            .output()
            .expect("run velnor-workflow");
        assert!(
            !status.status.success(),
            "`{name}` must fail closed, and did not"
        );
    }
}

/// The full config minus every row that covers a unit or the plan, so a single
/// appended row can be tested in isolation.
fn config_without_declares() -> String {
    FULL_CONFIG
        .lines()
        .filter(|line| !line.starts_with("primitive =") && !line.starts_with("units ="))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tempfile() -> PathBuf {
    // Test threads run concurrently and share a clock; a timestamp alone can
    // hand two tests the same directory and let one test's cleanup delete the
    // other's output mid-write. A process-local sequence makes the name
    // collision-free.
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join(format!(
        "velnor-primitives-test-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&base).unwrap();
    base
}

/// The synthetic release fixture: one Rust crate. The release and preview
/// lanes exist only when a config declares them, so the fixture pins that a
/// declared lane is added to — never guessed from — the scanned surface.
fn copy_release_fixture(destination: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/synthetic-release");
    copy_tree(&source, destination);
    destination.to_path_buf()
}

const RELEASE_CONFIG: &str = r#"schema = 1

[generator]
repository = "example/synthetic-release"

[workflow]
velnor_labels = ["self-hosted", "example-lane"]

[[declare]]
primitive = "release"
file = "release.yml"

[declare.args]
kind = "rust-binary"
package = "app"
binary = "app"
targets = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]

[[declare]]
primitive = "preview"
file = "preview.yml"

[declare.args]
package = "app"
binary = "app"
targets = ["x86_64-unknown-linux-gnu"]
"#;

#[test]
fn a_declared_release_lane_adds_exactly_the_release_files() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    let without = generate(&root);
    let without_count = without.workflow_files().len();
    assert!(
        !without.workflow_files().contains(&"release.yml".to_owned()),
        "release.yml must not exist before the lane is declared"
    );
    assert!(
        !without.workflow_files().contains(&"preview.yml".to_owned()),
        "preview.yml must not exist before the lane is declared"
    );

    write_config(&root, RELEASE_CONFIG);
    let with = generate(&root);
    let files = with.workflow_files();
    assert!(files.contains(&"release.yml".to_owned()), "{files:?}");
    assert!(files.contains(&"preview.yml".to_owned()), "{files:?}");
    assert_eq!(
        files.len(),
        without_count + 2,
        "declaring the lanes must add exactly the two lane files: {files:?}"
    );
    let release = with.workflow("release.yml");
    assert!(release.contains("name: Release"), "{release}");
    assert!(release.contains("Publish GitHub release"), "{release}");
    assert!(
        release.contains("target: x86_64-unknown-linux-gnu"),
        "{release}"
    );
    assert!(
        release.contains("target: aarch64-unknown-linux-gnu"),
        "{release}"
    );
    let preview = with.workflow("preview.yml");
    assert!(preview.contains("Publish rolling preview"), "{preview}");
}

/// The toolchain contract is structural, not incidental bytes: the release
/// publisher provisions the pinned toolchain but never saves it (a tag push
/// can never satisfy the trusted default-branch gate), the preview lane saves
/// exactly from that gate, and no Rust unit ever asks mise for `rust`.
#[test]
fn tool_provisioning_is_pinned_and_minimal() {
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    write_config(&root, RELEASE_CONFIG);
    let with = generate(&root);

    let release = with.workflow("release.yml");
    assert!(
        release.contains("name: Restore Rust toolchain"),
        "{release}"
    );
    assert!(
        release.contains("name: Provision Rust toolchain"),
        "{release}"
    );
    assert!(
        !release.contains("name: Save Rust toolchain"),
        "the release publisher must never save the toolchain cache: {release}"
    );

    let preview = with.workflow("preview.yml");
    assert!(
        preview.contains(
            "if: github.event_name == 'push' && github.ref == 'refs/heads/main' && steps.rustup-toolchain.outputs.cache-hit != 'true'"
        ),
        "the preview save must sit behind the trusted default-branch gate: {preview}"
    );

    for unit in with
        .unit_ids()
        .into_iter()
        .filter(|id| id.starts_with("rust-"))
    {
        let workflow = with.workflow(&nested_file(&unit));
        let install_args = workflow
            .lines()
            .filter(|line| line.trim_start().starts_with("install_args:"))
            .collect::<Vec<_>>();
        for line in &install_args {
            let tools = line
                .trim()
                .strip_prefix("install_args:")
                .unwrap()
                .split_whitespace();
            assert!(
                !tools.clone().any(|tool| tool == "rust"),
                "{unit} must never install the Rust toolchain through mise: {line}"
            );
        }
        let checks_envs = workflow
            .lines()
            .filter(|line| line.trim() == "MISE_AUTO_INSTALL: \"false\"")
            .count();
        assert!(
            checks_envs >= 2,
            "{unit} runs two lanes and both must switch mise auto-install off: {workflow}"
        );
    }
}

#[test]
fn the_declared_release_surface_is_repo_name_independent() {
    let workspace = tempfile();
    let one = copy_release_fixture(&workspace.join("release-lane-one"));
    let other = copy_release_fixture(&workspace.join("release-lane-two"));
    write_config(&one, RELEASE_CONFIG);
    write_config(&other, RELEASE_CONFIG);
    let one = generate(&one);
    let other = generate(&other);
    assert_eq!(one.workflow_files(), other.workflow_files());
    for file in one.workflow_files() {
        assert_eq!(one.workflow(&file), other.workflow(&file), "{file}");
    }
}

#[test]
fn an_incomplete_declared_release_stops_generation() {
    let incomplete = RELEASE_CONFIG
        .lines()
        .filter(|line| !line.starts_with("targets ="))
        .collect::<Vec<_>>()
        .join("\n");
    let workspace = tempfile();
    let root = copy_release_fixture(&workspace.join("fixture"));
    write_config(&root, &incomplete);
    let status = Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--default-branch",
            "main",
            "--output",
            workspace.join("out").to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow");
    assert!(
        !status.status.success(),
        "an incomplete declared release must fail closed, and did not"
    );
}
