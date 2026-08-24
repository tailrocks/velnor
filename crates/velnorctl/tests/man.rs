//! C005 `velnorctl man` tests: parser rejections, combined stdout page,
//! atomic directory mode, golden zero-leaf output, exit codes, and
//! no-secret corpus checks.

use std::path::{Path, PathBuf};
use std::process::Command;

use velnor_model::ExitClass;
use velnorctl::man::ManCommand;
use velnorctl::metadata::DocumentedCommand;
use velnorctl::Outcome;

fn dispatch_exit(args: &[&str]) -> (Outcome, u8) {
    let argv: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let registry = velnorctl::compose();
    let outcome = velnorctl::run(&registry, &argv);
    (outcome.clone(), outcome.exit_code())
}

fn expect_usage_failure(args: &[&str]) {
    let (outcome, code) = dispatch_exit(args);
    match &outcome {
        Outcome::CommandFailed { error, .. } => {
            assert_eq!(error.class, ExitClass::Usage, "{args:?} -> {error:?}");
            assert_eq!(error.reason, "cli.usage", "{args:?} -> {error:?}");
        }
        other => panic!("{args:?} should fail as command usage, got {other:?}"),
    }
    assert_eq!(code, 2, "{args:?} must map Usage to exit code 2");
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("velnor-man-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// --- parser ---

#[test]
fn command_c005_unknown_flag_is_rejected_nonzero() {
    for args in [
        vec!["man", "--bogus"],
        vec!["man", "--force=1"],
        vec!["man", "-d", "somewhere"],
        vec!["man", "--directoryx", "."],
    ] {
        expect_usage_failure(&args);
    }
}

#[test]
fn command_c005_unknown_positional_is_rejected() {
    for args in [
        vec!["man", "extra"],
        vec!["man", "--force", "extra"],
        vec!["man", "--directory", ".", "extra"],
    ] {
        expect_usage_failure(&args);
    }
}

#[test]
fn command_c005_directory_flag_requires_a_value() {
    expect_usage_failure(&["man", "--directory"]);
    // The inline form carries its own value and must parse through instead.
    let dir = TempDir::new("inline");
    let destination = dir.path().to_string_lossy().into_owned();
    let (outcome, code) = dispatch_exit(&["man", &format!("--directory={destination}")]);
    assert_eq!(
        outcome,
        Outcome::Handled {
            name: "man".to_owned()
        }
    );
    assert_eq!(code, 0);
    assert!(dir.path().join("velnorctl.1").is_file());
}

#[test]
fn command_c005_force_alone_is_valid_without_a_destination() {
    let (outcome, code) = dispatch_exit(&["man", "--force"]);
    assert_eq!(
        outcome,
        Outcome::Handled {
            name: "man".to_owned()
        }
    );
    assert_eq!(code, 0);
}

// --- stdout combined page ---

#[test]
fn command_c005_stdout_combined_page_documents_binary_globals_and_leaves() {
    let bin = env!("CARGO_BIN_EXE_velnorctl");
    let run = Command::new(bin).arg("man").output().unwrap();
    assert_eq!(run.status.code(), Some(0));
    assert!(
        run.stderr.is_empty(),
        "stderr was {:?}",
        String::from_utf8_lossy(&run.stderr)
    );
    let page = String::from_utf8_lossy(&run.stdout);
    for expected in [
        ".TH VELNORCTL 1",
        ".SH GLOBAL OPTIONS",
        "\\fB-o, --output <FORMAT>\\fR",
        ".SH OUTPUT",
        ".SH EXIT STATUS",
        "\\fBUSAGE (2)\\fR",
        ".SH SAFETY",
        "slotKind \"stable\"",
        // Every registered leaf appears exactly once; `man` is registered.
        "velnorctl-MAN",
        "\\fB--directory <PATH>\\fR",
        "\\fB--force\\fR",
    ] {
        assert!(page.contains(expected), "page missing {expected:?}");
    }
    assert_eq!(page.matches(".TH ").count(), 1, "one header per page");
    assert_eq!(
        page.matches(".SH NAME\n").count(),
        2,
        "binary NAME plus one leaf section"
    );
}

#[test]
fn command_c005_output_is_deterministic_across_runs() {
    let bin = env!("CARGO_BIN_EXE_velnorctl");
    let first = Command::new(bin).arg("man").output().unwrap();
    let second = Command::new(bin).arg("man").output().unwrap();
    assert_eq!(
        first.stdout, second.stdout,
        "stdout mode must be byte-stable"
    );

    let commands = vec![ManCommand.metadata()];
    assert_eq!(
        velnorctl::man::combined_page(velnorctl::BIN_NAME, &commands),
        velnorctl::man::combined_page(velnorctl::BIN_NAME, &commands),
        "direct rendering must be stable"
    );

    let reversed = vec![ManCommand.metadata()];
    let mut shuffled = reversed.clone();
    shuffled.reverse();
    assert_eq!(
        velnorctl::man::combined_page(velnorctl::BIN_NAME, &shuffled),
        velnorctl::man::combined_page(velnorctl::BIN_NAME, &reversed),
        "leaf sections follow name order regardless of input order"
    );
}

// --- directory mode ---

#[test]
fn command_c005_directory_mode_writes_the_complete_0644_page_set() {
    let dir = TempDir::new("pageset");
    let (outcome, code) = dispatch_exit(&["man", "--directory", &dir.path().to_string_lossy()]);
    assert_eq!(
        outcome,
        Outcome::Handled {
            name: "man".to_owned()
        },
        "{outcome:?}"
    );
    assert_eq!(code, 0);

    let combined = dir.path().join("velnorctl.1");
    let man_page_path = dir.path().join("man.1");
    assert!(combined.is_file(), "entries: {:?}", dir.entries());
    assert!(man_page_path.is_file(), "registered leaf pages are written");
    assert_eq!(dir.entries(), vec!["man.1", "velnorctl.1"]);
    #[cfg(unix)]
    {
        assert_eq!(mode_of(&combined), 0o644, "combined page mode");
        assert_eq!(mode_of(&man_page_path), 0o644, "leaf page mode");
    }
    let contents = std::fs::read_to_string(&combined).unwrap();
    assert!(contents.starts_with(".TH VELNORCTL 1 "));
    assert!(contents.contains(".SH GLOBAL OPTIONS"));
    assert!(contents.contains("velnorctl-MAN"));
    let leaf = std::fs::read_to_string(&man_page_path).unwrap();
    assert!(leaf.starts_with(".TH velnorctl 1 "));
    assert!(leaf.contains("\\fB--directory <PATH>\\fR"));
}

#[test]
fn command_c005_second_run_without_force_conflicts_and_mutates_nothing() {
    let dir = TempDir::new("conflict");
    let (first, first_code) = dispatch_exit(&["man", "--directory", &dir.path().to_string_lossy()]);
    assert_eq!(first_code, 0, "{first:?}");
    let before = dir.entries();

    let (second, second_code) =
        dispatch_exit(&["man", "--directory", &dir.path().to_string_lossy()]);
    match &second {
        Outcome::CommandFailed { error, .. } => {
            assert_eq!(error.class, ExitClass::Conflict, "{error:?}");
            assert_eq!(error.reason, "man.member_exists", "{error:?}");
            assert!(error.message.contains("--force"), "{error:?}");
        }
        other => panic!("existing members without --force must conflict, got {other:?}"),
    }
    assert_eq!(second_code, 6);
    assert_eq!(
        dir.entries(),
        before,
        "failed run must not mutate or leave temp files"
    );
}

#[test]
fn command_c005_force_overwrites_members_only() {
    let dir = TempDir::new("force");
    let destination = dir.path().to_string_lossy().into_owned();
    assert_eq!(dispatch_exit(&["man", "--directory", &destination]).1, 0);
    // A non-member file is never touched even under --force.
    let bystander = dir.path().join("unrelated.txt");
    std::fs::write(&bystander, b"keep").unwrap();

    let (forced, forced_code) = dispatch_exit(&["man", "--directory", &destination, "--force"]);
    assert_eq!(
        forced,
        Outcome::Handled {
            name: "man".to_owned()
        },
        "{forced:?}"
    );
    assert_eq!(forced_code, 0);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("velnorctl.1")).unwrap(),
        velnorctl::man::combined_page(velnorctl::BIN_NAME, &[ManCommand.metadata()])
    );
    assert_eq!(std::fs::read(&bystander).unwrap(), b"keep");
    assert_eq!(dir.entries(), vec!["man.1", "unrelated.txt", "velnorctl.1"]);
}

#[test]
fn command_c005_symlinked_destination_is_rejected() {
    let parent = TempDir::new("symlink-parent");
    let real = parent.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&real, parent.path().join("link")).unwrap();
        let (outcome, code) = dispatch_exit(&[
            "man",
            "--directory",
            &parent.path().join("link").to_string_lossy(),
        ]);
        match &outcome {
            Outcome::CommandFailed { error, .. } => {
                assert_eq!(error.class, ExitClass::Usage, "{error:?}");
                assert_eq!(error.reason, "man.destination_symlink", "{error:?}");
            }
            other => panic!("symlinked destination must be rejected, got {other:?}"),
        }
        assert_eq!(code, 2);
        assert!(
            !real.join("velnorctl.1").try_exists().unwrap(),
            "nothing may be written through a symlinked destination"
        );
    }
}

#[test]
fn command_c005_non_directory_destination_is_rejected() {
    let dir = TempDir::new("notdir");
    let file = dir.path().join("plain.txt");
    std::fs::write(&file, b"not a directory").unwrap();
    let (outcome, code) = dispatch_exit(&["man", "--directory", &file.to_string_lossy()]);
    match &outcome {
        Outcome::CommandFailed { error, .. } => {
            assert_eq!(error.class, ExitClass::Usage, "{error:?}");
            assert_eq!(error.reason, "man.destination_not_directory", "{error:?}");
        }
        other => panic!("non-directory destination must be rejected, got {other:?}"),
    }
    assert_eq!(code, 2);
}

#[test]
fn command_c005_atomicity_smoke_induced_failure_leaves_no_partial_files() {
    let dir = TempDir::new("atomic");
    let destination = dir.path().to_string_lossy().into_owned();
    assert_eq!(dispatch_exit(&["man", "--directory", &destination]).1, 0);
    // Induce a member-exists conflict mid-set: man.1 exists but the combined
    // page is removed; validation must fire before any write.
    std::fs::remove_file(dir.path().join("velnorctl.1")).unwrap();
    let before = dir.entries();
    let (outcome, code) = dispatch_exit(&["man", "--directory", &destination]);
    assert_ne!(code, 0, "{outcome:?}");
    assert_eq!(
        dir.entries(),
        before,
        "no temp files or partial pages may remain"
    );
    assert!(!dir
        .entries()
        .iter()
        .any(|name| name.starts_with(".velnorctl-man-tmp-")));
}

// --- golden output ---

#[test]
fn command_c005_golden_zero_leaf_combined_page_is_exact() {
    let rendered = velnorctl::man::combined_page(velnorctl::BIN_NAME, &[]);
    let expected = r#".TH VELNORCTL 1 "0.1.0" "velnorctl 0.1.0" "Velnor Manual"
.SH NAME
velnorctl \- Velnor operator CLI
.SH SYNOPSIS
.B velnorctl [GLOBAL FLAGS] <COMMAND> [ARGS]...
.SH GLOBAL OPTIONS
.TP
\fB--context <NAME>\fR
named connection context
.TP
\fB-o, --output <FORMAT>\fR
output format: table|wide|json|yaml|jsonl|name
.TP
\fB--instance <NAME>\fR
restrict to one daemon instance
.TP
\fB--repo <REPO>\fR
restrict to one repository (owner/name)
.TP
\fB--selector <SELECTOR>\fR
include-only filter over resource fields
.TP
\fB--field-selector <SELECTOR>\fR
field equality selector (key=value)
.TP
\fB--since <SINCE>\fR
lower time bound: RFC 3339 or relative duration
.TP
\fB--timeout <SECONDS>\fR
deadline in seconds before the command exits with TIMEOUT
.TP
\fB--no-color\fR
disable ANSI styling regardless of TTY detection
.TP
\fB-v, --verbose\fR
increase verbosity; repeatable
.SH OUTPUT
Resource data is written to stdout; warnings and diagnostics go to stderr.
Machine output modes (--output json|yaml|jsonl|name) render versioned
resources stamped with a schema version; human table/wide views render the
same types and are never the source of truth.
.PP
Slot resources carry slotKind "stable" (a persistent named slot reused
across jobs) or "ephemeral" (a single-job runner created for one job and
discarded afterwards); consumers read the distinction from the typed field,
never from labels or names.
.SH EXIT STATUS
Every command exits with exactly one class from this fixed mapping.
.TP
\fBSUCCESS (0)\fR
the requested operation completed
.TP
\fBCONDITION (1)\fR
an inspection completed and authoritatively found a degraded condition
.TP
\fBUSAGE (2)\fR
CLI syntax, selector, field, or local input was invalid
.TP
\fBAUTHORIZATION (3)\fR
authentication failed or permission was denied
.TP
\fBUNAVAILABLE (4)\fR
an authoritative resource was absent or unavailable
.TP
\fBTIMEOUT (5)\fR
the deadline elapsed before a terminal result
.TP
\fBCONFLICT (6)\fR
a state or safety precondition no longer matched
.TP
\fBTRANSPORT (7)\fR
connection, rate-limit, or ambiguous upstream outcome
.TP
\fBOPERATION (8)\fR
an accepted operation reached a definite failure
.TP
\fBINTERRUPTED (130)\fR
local interruption stopped observation
.SH SAFETY
--directory writes only into the exact destination given: system man paths
are never installed, updated, or removed implicitly. A symbolic-link or
non-directory destination is rejected outright, and an existing member is
overwritten only under --force.
"#;
    assert_eq!(rendered, expected);
}

// --- no-secret corpus ---

#[test]
fn command_c005_rendered_pages_contain_no_credential_markers() {
    const MARKERS: [&str; 3] = ["ghp_", "github_pat_", "BEGIN"];
    let man_meta = ManCommand.metadata();
    let corpus = vec![
        velnorctl::man::combined_page(velnorctl::BIN_NAME, &[]),
        velnorctl::man::combined_page(velnorctl::BIN_NAME, std::slice::from_ref(&man_meta)),
        velnorctl::metadata::man_page(velnorctl::BIN_NAME, &man_meta),
    ];
    for page in &corpus {
        for marker in MARKERS {
            assert!(
                !page.contains(marker),
                "rendered page leaked credential marker {marker:?}"
            );
        }
    }
}
