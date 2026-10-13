//! Shared helpers for CLI-level generator tests: fixture setup, minimal
//! schema-1/schema-2 configs, generation runs, and tree snapshots that
//! prove byte-, mode-, and link-identical output.

#![expect(
    clippy::unwrap_used,
    reason = "a test whose setup fails should panic loudly"
)]
#![expect(
    clippy::expect_used,
    reason = "a test whose setup fails should panic loudly"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Which pipeline a test drives: schema-1 (`--runners`) or schema-2
/// (`--providers` via config).
#[derive(Clone, Copy)]
pub enum Pipeline {
    V1,
    S2,
}

impl Pipeline {
    pub fn name(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::S2 => "s2",
        }
    }
}

pub fn unique_dir(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "velnor-full-tree-{name}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

pub fn copy_tree(source: &Path, destination: &Path) {
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

/// The minimal-shape fixture: a single Rust crate, no generation config.
pub fn minimal_root(name: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/minimal-rust");
    let root = unique_dir(name).join("fixture");
    copy_tree(&source, &root);
    root
}

pub fn write_schema1_config(root: &Path) {
    let directory = root.join(".github-gen");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("velnor-workflow.toml"),
        "schema = 1\n\
         \n\
         [generator]\n\
         repository = \"example/minimal\"\n\
         \n\
         [workflow]\n\
         runners = \"github\"\n\
         github_runner = \"ubuntu-24.04\"\n",
    )
    .unwrap();
}

pub fn write_schema2_config(root: &Path) {
    let directory = root.join(".github-gen");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("velnor-workflow.toml"),
        "schema = 2\n\
         \n\
         [generator]\n\
         repository = \"example/minimal\"\n\
         \n\
         [workflow]\n\
         providers = [\"github-hosted\"]\n\
         automatic_providers = [\"github-hosted\"]\n\
         \n\
         [workflow.selectors.github-hosted]\n\
         runs_on = [\"ubuntu-24.04\"]\n",
    )
    .unwrap();
}

pub fn write_config(pipeline: Pipeline, root: &Path) {
    match pipeline {
        Pipeline::V1 => write_schema1_config(root),
        Pipeline::S2 => write_schema2_config(root),
    }
}

/// Run generation of `root` into `output`, returning the raw outcome so
/// tests can assert both success and failure without panicking.
pub fn run_generate(root: &Path, output: &Path, force: bool) -> Output {
    let mut args = vec!["--plain", "--default-branch", "main", "--output"];
    args.push(output.to_str().unwrap());
    if force {
        args.push("--force");
    }
    args.push(root.to_str().unwrap());
    Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args(&args)
        .output()
        .expect("run velnor-workflow")
}

/// Generate and panic on failure; returns nothing — the tree is the result.
pub fn generate_ok(root: &Path, output: &Path, force: bool) {
    let outcome = run_generate(root, output, force);
    assert!(
        outcome.status.success(),
        "generation failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
}

/// Run generation under an explicit umask, hermetically: the mask applies
/// to the generator child only, never to the test process, so parallel
/// tests cannot observe it. Generated output must be umask-independent —
/// every installed file carries deterministic permissions.
#[cfg(unix)]
pub fn run_generate_with_umask(root: &Path, output: &Path, force: bool, mask: &str) -> Output {
    let mut args = vec![
        "--plain".to_owned(),
        "--default-branch".to_owned(),
        "main".to_owned(),
        "--output".to_owned(),
        output.to_str().unwrap().to_owned(),
    ];
    if force {
        args.push("--force".to_owned());
    }
    args.push(root.to_str().unwrap().to_owned());
    Command::new("sh")
        .arg("-c")
        .arg(format!("umask {mask}; exec \"$@\""))
        .arg("sh")
        .arg(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args(&args)
        .output()
        .expect("run velnor-workflow under umask")
}

/// Generate under an explicit umask and panic on failure.
#[cfg(unix)]
pub fn generate_ok_with_umask(root: &Path, output: &Path, force: bool, mask: &str) {
    let outcome = run_generate_with_umask(root, output, force, mask);
    assert!(
        outcome.status.success(),
        "generation under umask {mask} failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
}

/// Run `--check` of `root` against `output`, returning the raw outcome.
pub fn run_check(root: &Path, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_velnor-workflow"))
        .args([
            "--plain",
            "--check",
            "--default-branch",
            "main",
            "--output",
            output.to_str().unwrap(),
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run velnor-workflow --check")
}

pub fn check_ok(root: &Path, output: &Path) {
    let outcome = run_check(root, output);
    assert!(
        outcome.status.success(),
        "check after generate failed:\n{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
}

pub fn check_fails(root: &Path, output: &Path) -> String {
    let outcome = run_check(root, output);
    assert!(!outcome.status.success(), "check must fail on drifted tree");
    String::from_utf8_lossy(&outcome.stderr).into_owned()
}

/// Every output-tree entry: regular files by bytes plus the unix mode,
/// symlinks by target, directories as markers. A repeat-generation
/// comparison over this map proves a true no-op — no quiet rewrites, no
/// mode churn, no relinking.
pub fn snapshot_tree(output: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, directory: &Path, snapshot: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries: Vec<PathBuf> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let relative = path.strip_prefix(base).unwrap().to_path_buf();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.file_type().is_symlink() {
                let mut record = b"link:".to_vec();
                record.extend(fs::read_link(&path).unwrap().as_os_str().as_encoded_bytes());
                snapshot.insert(relative, record);
            } else if metadata.is_dir() {
                snapshot.insert(relative, b"dir".to_vec());
                visit(base, &path, snapshot);
            } else {
                let mut record = b"file:".to_vec();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    record.extend(format!("{:o}:", metadata.permissions().mode() & 0o777).bytes());
                }
                record.extend(fs::read(&path).unwrap());
                snapshot.insert(relative, record);
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(output, output, &mut snapshot);
    snapshot
}

/// No staging directory or backup sibling may survive a publish —
/// success or failure.
pub fn assert_no_staging_leftovers(output: &Path) {
    if fs::symlink_metadata(output).is_err() {
        return;
    }
    let mut leftovers = Vec::new();
    for entry in fs::read_dir(output).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.starts_with(".velnor-workflow-stage-") {
            leftovers.push(name);
        }
    }
    assert!(
        leftovers.is_empty(),
        "staging leftovers survive publication: {leftovers:?}"
    );
}

#[cfg(unix)]
pub fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o111 != 0
}
