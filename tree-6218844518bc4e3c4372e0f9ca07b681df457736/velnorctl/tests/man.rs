//! `velnorctl man` behavior over the clap-derived page set: determinism,
//! safety refusals, atomic writes, and structural documentation contracts.

use std::path::{Path, PathBuf};

use velnor_model::ExitClass;
use velnorctl::man::{self, ManArgs};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let base = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = base.join(format!(
            "velnorctl-man-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create scratch dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn man_args(directory: Option<PathBuf>, force: bool) -> ManArgs {
    ManArgs { directory, force }
}

fn err_code(error: &velnorctl::CommandError) -> u8 {
    error.exit_code()
}

#[test]
fn cli_c005_combined_stdout_page_is_deterministic_and_structurally_complete() {
    man::run(&man_args(None, false)).expect("stdout page renders");
    let first = man::combined_page();
    let second = man::combined_page();
    assert_eq!(first, second, "combined page must be deterministic");
    assert!(first.contains(".TH velnorctl 1"), "{first}");
    assert!(first.contains(".SH NAME"));
    assert!(first.contains(".SH SYNOPSIS"));
    // Velnor semantic sections survive the migration:
    assert!(first.contains(".SH OUTPUT"));
    assert!(first.contains(".SH EXIT STATUS"));
    assert!(first.contains(".SH SAFETY"));
    // Every registered leaf appears exactly once by its own page header.
    for command in ["man", "completion"] {
        let marker = format!(".TH {command} 1");
        assert_eq!(
            first.matches(&marker).count(),
            1,
            "{command} must appear exactly once in {first}"
        );
    }
}

#[test]
fn cli_c005_directory_mode_writes_a_complete_deterministic_0644_page_set() {
    let scratch = Scratch::new("set");
    man::run(&man_args(Some(scratch.path().to_path_buf()), false)).expect("page set written");

    let mut members: Vec<PathBuf> = std::fs::read_dir(scratch.path())
        .expect("read dir")
        .map(|entry| entry.expect("entry").path())
        .collect();
    members.sort();
    let names: Vec<String> = members
        .iter()
        .map(|path| {
            path.file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        names,
        vec![
            "cache.1",
            "capabilities.1",
            "completion.1",
            "configure.1",
            "daemon.1",
            "doctor.1",
            "man.1",
            "preflight.1",
            "release.1",
            "remove.1",
            "status.1",
            "storage.1",
            "velnorctl.1"
        ]
    );

    use std::os::unix::fs::PermissionsExt;
    for member in &members {
        let mode = std::fs::metadata(member)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644, "{}", member.display());
    }
    assert!(
        std::fs::read_to_string(scratch.path().join("man.1"))
            .expect("man page")
            .contains("\\-\\-directory"),
        "leaf page carries its own options"
    );
    assert!(
        std::fs::read_to_string(scratch.path().join("completion.1"))
            .expect("completion page")
            .contains("SHELL"),
        "leaf page carries its own options"
    );

    let before: Vec<(PathBuf, [u8; 32])> = members
        .iter()
        .map(|member| {
            (
                member.clone(),
                sha256(&std::fs::read(member).expect("read")),
            )
        })
        .collect();

    // A second run without --force conflicts; with --force it rewrites
    // byte-identically (deterministic content).
    let conflict = man::run(&man_args(Some(scratch.path().to_path_buf()), false))
        .expect_err("existing members conflict");
    assert_eq!(
        err_code(&conflict),
        u8::try_from(velnor_model::exit_code_for_class(ExitClass::Conflict)).unwrap()
    );

    man::run(&man_args(Some(scratch.path().to_path_buf()), true)).expect("force rewrite");
    for (member, digest) in before {
        assert_eq!(sha256(&std::fs::read(&member).expect("read")), digest);
    }
}

#[test]
fn cli_c005_destination_symlink_is_refused_as_usage() {
    let real = Scratch::new("dest-real");
    let link = Scratch::new("dest-link");
    let link_path = link.path().join("linked");
    std::os::unix::fs::symlink(real.path(), &link_path).expect("symlink destination");

    let error = man::run(&man_args(Some(link_path.clone()), true)).expect_err("symlink refused");
    assert_eq!(
        err_code(&error),
        u8::try_from(velnor_model::exit_code_for_class(ExitClass::Usage)).unwrap()
    );
    assert_eq!(error.reason, "man.destination_symlink");
    assert!(std::fs::read_dir(real.path())
        .expect("real dir")
        .next()
        .is_none());
}

#[test]
fn cli_c005_non_directory_destination_is_refused_as_usage() {
    let scratch = Scratch::new("notdir");
    let file = scratch.path().join("plain-file");
    std::fs::write(&file, b"placeholder").expect("write file");
    let error = man::run(&man_args(Some(file.clone()), false)).expect_err("file refused");
    assert_eq!(
        err_code(&error),
        u8::try_from(velnor_model::exit_code_for_class(ExitClass::Usage)).unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(&file).expect("untouched"),
        "placeholder"
    );
}

#[test]
fn cli_c005_symlinked_member_is_refused_even_under_force() {
    let scratch = Scratch::new("sym-member");
    let victim = Scratch::new("sym-victim");
    let member = scratch.path().join("man.1");
    std::os::unix::fs::symlink(victim.path().join("nowhere"), &member).expect("symlink member");

    let error = man::run(&man_args(Some(scratch.path().to_path_buf()), true))
        .expect_err("symlinked member refused under force");
    assert_eq!(
        err_code(&error),
        u8::try_from(velnor_model::exit_code_for_class(ExitClass::Usage)).unwrap()
    );
    assert_eq!(error.reason, "man.member_symlink");
    assert!(
        std::fs::symlink_metadata(&member)
            .expect("member intact")
            .file_type()
            .is_symlink(),
        "the symlink itself must remain untouched"
    );
}

#[test]
fn cli_c005_conflict_mutates_nothing_and_leaves_no_temp_files() {
    let scratch = Scratch::new("conflict");
    man::run(&man_args(Some(scratch.path().to_path_buf()), false)).expect("initial write");
    let before: Vec<PathBuf> = std::fs::read_dir(scratch.path())
        .expect("read dir")
        .map(|entry| entry.expect("entry").path())
        .collect();

    let extra = scratch.path().join("extra.1");
    std::fs::write(&extra, b"bystander").expect("seed bystander");

    let error = man::run(&man_args(Some(scratch.path().to_path_buf()), false))
        .expect_err("second run without force conflicts");
    assert_eq!(
        err_code(&error),
        u8::try_from(velnor_model::exit_code_for_class(ExitClass::Conflict)).unwrap()
    );
    assert_eq!(error.reason, "man.member_exists");

    let after: Vec<PathBuf> = std::fs::read_dir(scratch.path())
        .expect("read dir")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(before.len() + 1, after.len(), "only the bystander is new");
    assert!(
        after.iter().all(|path| !path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .starts_with('.')),
        "no temp leftovers: {after:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&extra).expect("bystander intact"),
        "bystander"
    );
}

#[test]
fn cli_c005_unwritable_destination_maps_to_operation_io_failed() {
    if std::env::var("VELNORCTL_TEST_AS_ROOT").is_ok() {
        return;
    }
    let scratch = Scratch::new("iofail");
    make_read_only(scratch.path());
    let probe = scratch.path().join(".probe");
    if std::fs::write(&probe, b"p").is_ok() {
        let _ = std::fs::remove_file(&probe);
        return;
    }
    let error =
        man::run(&man_args(Some(scratch.path().to_path_buf()), true)).expect_err("unwritable");
    restore_writable(scratch.path());
    assert_eq!(err_code(&error), 8);
    assert_eq!(error.reason, "man.io_failed");
}

#[test]
fn cli_c005_pages_never_contain_secret_marker_material() {
    let page = man::combined_page();
    for marker in ["ghp_", "github_pat_", "-----BEGIN", "AKIA"] {
        assert!(!page.contains(marker), "secret marker {marker} leaked");
    }
}

fn make_read_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).expect("stat").permissions();
    perms.set_mode(0o500);
    std::fs::set_permissions(path, perms).expect("chmod");
}

fn restore_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).expect("stat").permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(path, perms).expect("chmod");
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}
