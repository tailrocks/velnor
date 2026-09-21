//! Staged full-tree replacement and the complete-tree drift check: the
//! generator owns the entire `.github` tree, publishes it from validated
//! staging with rollback, and `--check` rejects every drift class — extra,
//! missing, edited, mistyped, mode, and symlink — across all of `.github`.
//!
//! Every test drives both pipelines over the minimal-shape fixture.

#![expect(
    clippy::unwrap_used,
    reason = "a test whose setup fails should panic loudly"
)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{
    assert_no_staging_leftovers, check_fails, check_ok, generate_ok, is_executable,
    make_executable, minimal_root, run_check, run_generate, snapshot_tree, unique_dir,
    write_config, Pipeline,
};

fn output_for(root: &Path) -> PathBuf {
    root.parent().unwrap().join(format!(
        "{}-out",
        root.file_name().and_then(|name| name.to_str()).unwrap()
    ))
}

fn fixture(pipeline: Pipeline, name: &str) -> (PathBuf, PathBuf) {
    let root = minimal_root(&format!("{}-{name}", pipeline.name()));
    write_config(pipeline, &root);
    let output = output_for(&root);
    let _ = fs::remove_dir_all(&output);
    generate_ok(&root, &output, false);
    (root, output)
}

fn write_outside(dir: &Path, name: &str, content: &str) -> PathBuf {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
    path
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) {
    if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, link).unwrap();
    } else {
        std::os::windows::fs::symlink_file(target, link).unwrap();
    }
}

fn unknown_content_blocks_without_force_and_removes_with_force(pipeline: Pipeline) {
    let (root, output) = fixture(pipeline, "unknown-content");
    let outside = unique_dir("unknown-outside");
    let target = write_outside(&outside, "kept.txt", "external bytes\n");
    let before = snapshot_tree(&output);

    fs::write(output.join(".github/stray.txt"), "stray\n").unwrap();
    fs::create_dir_all(output.join(".github/nested/deep")).unwrap();
    fs::write(output.join(".github/nested/deep/file.txt"), "deep\n").unwrap();
    fs::create_dir_all(output.join(".github/evil-dir")).unwrap();
    symlink(&target, &output.join(".github/evil-dir/link-out"));
    symlink(&target, &output.join(".github/link-out"));

    // Without `--force`, unknown content blocks with the tree untouched.
    let outcome = run_generate(&root, &output, false);
    assert!(!outcome.status.success(), "unknown content must block");
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        stderr.contains("will not be imported"),
        "refusal must name the gate: {stderr}"
    );
    // Unknown directories report whole: they move aside as one unit.
    for stray in [
        ".github/stray.txt",
        ".github/nested",
        ".github/evil-dir",
        ".github/link-out",
    ] {
        assert!(
            stderr.contains(stray),
            "refusal must name {stray}: {stderr}"
        );
    }
    let mut blocked = snapshot_tree(&output);
    for stray in [
        PathBuf::from(".github/stray.txt"),
        PathBuf::from(".github/nested"),
        PathBuf::from(".github/nested/deep"),
        PathBuf::from(".github/nested/deep/file.txt"),
        PathBuf::from(".github/evil-dir"),
        PathBuf::from(".github/evil-dir/link-out"),
        PathBuf::from(".github/link-out"),
    ] {
        blocked.remove(&stray);
    }
    assert_eq!(blocked, before, "a refused run must not touch the tree");
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "external bytes\n",
        "a refused run must not touch external targets"
    );

    // `--check` reports the same extras as drift.
    let drift = check_fails(&root, &output);
    assert!(
        drift.contains(".github/stray.txt"),
        "check must report extras: {drift}"
    );

    // `--force` replaces the tree: unknowns gone, externals intact.
    generate_ok(&root, &output, true);
    for stray in [
        ".github/stray.txt",
        ".github/nested",
        ".github/evil-dir",
        ".github/link-out",
    ] {
        assert!(
            fs::symlink_metadata(output.join(stray)).is_err(),
            "{stray} must be gone after force"
        );
    }
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "external bytes\n",
        "force must unlink, never follow or delete targets"
    );
    assert_eq!(
        snapshot_tree(&output),
        before,
        "force must converge back to the owned tree"
    );
    assert_no_staging_leftovers(&output);
    check_ok(&root, &output);
}

#[test]
fn v1_unknown_content_blocks_without_force_and_removes_with_force() {
    unknown_content_blocks_without_force_and_removes_with_force(Pipeline::V1);
}

#[test]
fn s2_unknown_content_blocks_without_force_and_removes_with_force() {
    unknown_content_blocks_without_force_and_removes_with_force(Pipeline::S2);
}

fn repeat_regen_is_identical_noop(pipeline: Pipeline) {
    let (root, output) = fixture(pipeline, "repeat-noop");
    let before = snapshot_tree(&output);
    generate_ok(&root, &output, false);
    assert_eq!(
        snapshot_tree(&output),
        before,
        "repeat generation must leave every byte, mode, and link untouched"
    );
    generate_ok(&root, &output, true);
    assert_eq!(
        snapshot_tree(&output),
        before,
        "forced repeat generation must also be a no-op on a current tree"
    );
    assert_no_staging_leftovers(&output);
    check_ok(&root, &output);
}

#[test]
fn v1_repeat_regen_is_identical_noop() {
    repeat_regen_is_identical_noop(Pipeline::V1);
}

#[test]
fn s2_repeat_regen_is_identical_noop() {
    repeat_regen_is_identical_noop(Pipeline::S2);
}

/// One drift mutation plus whether `--force` repairs it. A hand edit to
/// an owned file never repairs: the ownership proof refuses it even
/// under `--force`.
enum Drift {
    Edit(&'static str),
    Delete(&'static str),
    ExtraFile(&'static str),
    ExtraDir(&'static str),
    MakeExecutable(&'static str),
    RetargetLink,
    LinkToFile,
}

fn apply_drift(output: &Path, drift: &Drift) {
    match drift {
        Drift::Edit(relative) => {
            let path = output.join(relative);
            let mut content = fs::read_to_string(&path).unwrap();
            content.push_str("# hand edit\n");
            fs::write(&path, content).unwrap();
        }
        Drift::Delete(relative) => {
            fs::remove_file(output.join(relative)).unwrap();
        }
        Drift::ExtraFile(relative) => {
            let path = output.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "extra\n").unwrap();
        }
        Drift::ExtraDir(relative) => {
            fs::create_dir_all(output.join(relative).join("inner")).unwrap();
            fs::write(output.join(relative).join("inner/file.txt"), "extra\n").unwrap();
        }
        #[cfg(unix)]
        Drift::MakeExecutable(relative) => make_executable(&output.join(relative)),
        #[cfg(not(unix))]
        Drift::MakeExecutable(_) => {}
        Drift::RetargetLink => {
            let link = output.join(".github/CLAUDE.md");
            fs::remove_file(&link).unwrap();
            symlink(&PathBuf::from("actionlint.yaml"), &link);
        }
        Drift::LinkToFile => {
            let link = output.join(".github/CLAUDE.md");
            fs::remove_file(&link).unwrap();
            fs::write(&link, "squatter\n").unwrap();
        }
    }
}

fn check_rejects_every_drift_class(pipeline: Pipeline, drift: &Drift, label: &str) {
    let (root, output) = fixture(pipeline, label);
    apply_drift(&output, drift);
    let outcome = run_check(&root, &output);
    assert!(
        !outcome.status.success(),
        "{label}: check must fail on drift"
    );
    // A hand edit, a retargeted link, and a file squatting the link are
    // the drifts `--force` must not repair: the ownership proof refuses
    // them, so the tree cannot silently absorb a manual change.
    let force = run_generate(&root, &output, true);
    let refusal = match drift {
        Drift::Edit(_) => Some("manually modified"),
        Drift::RetargetLink => Some("manually modified generator symlink"),
        Drift::LinkToFile => Some("expected generator-owned symlink"),
        Drift::Delete(_) | Drift::ExtraFile(_) | Drift::ExtraDir(_) | Drift::MakeExecutable(_) => {
            None
        }
    };
    if let Some(marker) = refusal {
        assert!(
            !force.status.success(),
            "{label}: force must not absorb the drift"
        );
        let stderr = String::from_utf8_lossy(&force.stderr);
        assert!(
            stderr.contains(marker) || stderr.contains("cannot prove ownership"),
            "{label}: refusal must name the ownership proof: {stderr}"
        );
    } else {
        generate_ok(&root, &output, true);
        check_ok(&root, &output);
    }
    assert_no_staging_leftovers(&output);
    #[cfg(not(unix))]
    let _ = drift;
}

#[test]
fn v1_check_rejects_content_edit() {
    check_rejects_every_drift_class(
        Pipeline::V1,
        &Drift::Edit(".github/actionlint.yaml"),
        "v1-edit",
    );
}

#[test]
fn s2_check_rejects_content_edit() {
    check_rejects_every_drift_class(
        Pipeline::S2,
        &Drift::Edit(".github/actionlint.yaml"),
        "s2-edit",
    );
}

#[test]
fn v1_check_rejects_missing_file() {
    check_rejects_every_drift_class(
        Pipeline::V1,
        &Drift::Delete(".github/workflows/ci-pr.yml"),
        "v1-missing",
    );
}

#[test]
fn s2_check_rejects_missing_file() {
    check_rejects_every_drift_class(
        Pipeline::S2,
        &Drift::Delete(".github/workflows/ci-pr.yml"),
        "s2-missing",
    );
}

#[test]
fn v1_check_rejects_extra_workflow() {
    check_rejects_every_drift_class(
        Pipeline::V1,
        &Drift::ExtraFile(".github/workflows/zzz-hand.yml"),
        "v1-extra-workflow",
    );
}

#[test]
fn s2_check_rejects_extra_workflow() {
    check_rejects_every_drift_class(
        Pipeline::S2,
        &Drift::ExtraFile(".github/workflows/zzz-hand.yml"),
        "s2-extra-workflow",
    );
}

#[test]
fn v1_check_rejects_extra_non_workflow_file() {
    check_rejects_every_drift_class(
        Pipeline::V1,
        &Drift::ExtraFile(".github/notes.txt"),
        "v1-extra-file",
    );
}

#[test]
fn s2_check_rejects_extra_non_workflow_file() {
    check_rejects_every_drift_class(
        Pipeline::S2,
        &Drift::ExtraFile(".github/notes.txt"),
        "s2-extra-file",
    );
}

#[test]
fn v1_check_rejects_extra_nested_dir() {
    check_rejects_every_drift_class(
        Pipeline::V1,
        &Drift::ExtraDir(".github/ci/extra"),
        "v1-extra-dir",
    );
}

#[test]
fn s2_check_rejects_extra_nested_dir() {
    check_rejects_every_drift_class(
        Pipeline::S2,
        &Drift::ExtraDir(".github/ci/extra"),
        "s2-extra-dir",
    );
}

#[test]
#[cfg(unix)]
fn v1_check_rejects_executable_mode() {
    check_rejects_every_drift_class(
        Pipeline::V1,
        &Drift::MakeExecutable(".github/workflows/nightly.yml"),
        "v1-mode",
    );
}

#[test]
#[cfg(unix)]
fn s2_check_rejects_executable_mode() {
    check_rejects_every_drift_class(
        Pipeline::S2,
        &Drift::MakeExecutable(".github/workflows/nightly.yml"),
        "s2-mode",
    );
}

#[test]
fn v1_check_rejects_retargeted_link() {
    check_rejects_every_drift_class(Pipeline::V1, &Drift::RetargetLink, "v1-retarget");
}

#[test]
fn s2_check_rejects_retargeted_link() {
    check_rejects_every_drift_class(Pipeline::S2, &Drift::RetargetLink, "s2-retarget");
}

#[test]
fn v1_check_rejects_file_squatting_link() {
    check_rejects_every_drift_class(Pipeline::V1, &Drift::LinkToFile, "v1-squatter");
}

#[test]
fn s2_check_rejects_file_squatting_link() {
    check_rejects_every_drift_class(Pipeline::S2, &Drift::LinkToFile, "s2-squatter");
}

#[cfg(unix)]
fn symlink_escape_never_touches_external_targets(pipeline: Pipeline) {
    let (root, output) = fixture(pipeline, "symlink-escape");
    let outside = unique_dir("escape-outside");
    let kept_file = write_outside(&outside, "kept.txt", "external bytes\n");
    let kept_dir = outside.join("kept-dir");
    fs::create_dir_all(&kept_dir).unwrap();
    write_outside(&kept_dir, "inner.txt", "inner\n");

    symlink(&kept_file, &output.join(".github/link-to-file"));
    symlink(&kept_dir, &output.join(".github/link-to-dir"));
    symlink(
        &PathBuf::from("link-loop"),
        &output.join(".github/link-loop"),
    );

    // Even `--force` only unlinks: targets and their bytes survive.
    generate_ok(&root, &output, true);
    for link in [
        ".github/link-to-file",
        ".github/link-to-dir",
        ".github/link-loop",
    ] {
        assert!(
            fs::symlink_metadata(output.join(link)).is_err(),
            "{link} must be unlinked"
        );
    }
    assert_eq!(fs::read_to_string(&kept_file).unwrap(), "external bytes\n");
    assert_eq!(
        fs::read_to_string(kept_dir.join("inner.txt")).unwrap(),
        "inner\n"
    );
    check_ok(&root, &output);

    // A symlinked managed directory refuses instead of redirecting the
    // publish outside the tree.
    fs::remove_dir_all(output.join(".github/workflows")).unwrap();
    symlink(&kept_dir, &output.join(".github/workflows"));
    let outcome = run_generate(&root, &output, true);
    assert!(
        !outcome.status.success(),
        "a symlinked managed directory must refuse"
    );
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        stderr.contains("refusing symlinked managed directory"),
        "refusal must name the escape: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(kept_dir.join("inner.txt")).unwrap(),
        "inner\n",
        "the refused run must not write through the link"
    );
    assert_no_staging_leftovers(&output);
}

#[test]
#[cfg(unix)]
fn v1_symlink_escape_never_touches_external_targets() {
    symlink_escape_never_touches_external_targets(Pipeline::V1);
}

#[test]
#[cfg(unix)]
fn s2_symlink_escape_never_touches_external_targets() {
    symlink_escape_never_touches_external_targets(Pipeline::S2);
}

fn generated_paths_stay_within_the_output_tree(pipeline: Pipeline) {
    let root = minimal_root(&format!("{}-boundary", pipeline.name()));
    write_config(pipeline, &root);
    let output = output_for(&root);
    let _ = fs::remove_dir_all(&output);

    // A `..` escape in a static row fails at config validation, before
    // anything is written anywhere. The source exists so the failure is
    // the boundary, not a missing read.
    fs::write(root.join("x"), "source\n").unwrap();
    let config = root.join(".github-gen/velnor-workflow.toml");
    let base = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!("{base}\n[[static_files]]\nfile = \"../escape.txt\"\nsource = \"x\"\n"),
    )
    .unwrap();
    let outcome = run_generate(&root, &output, true);
    assert!(
        !outcome.status.success(),
        "an escaping static path must fail"
    );
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        stderr.contains("inside `.github/`"),
        "refusal must name the boundary: {stderr}"
    );
    assert!(
        !root.parent().unwrap().join("escape.txt").exists(),
        "nothing may escape the output tree"
    );
    assert_no_staging_leftovers(&output);
}

#[test]
fn v1_generated_paths_stay_within_the_output_tree() {
    generated_paths_stay_within_the_output_tree(Pipeline::V1);
}

#[test]
fn s2_generated_paths_stay_within_the_output_tree() {
    generated_paths_stay_within_the_output_tree(Pipeline::S2);
}

fn reserved_agent_path_spellings_are_rejected(pipeline: Pipeline) {
    let root = minimal_root(&format!("{}-reserved", pipeline.name()));
    write_config(pipeline, &root);
    let output = output_for(&root);
    let _ = fs::remove_dir_all(&output);
    let source = root.join(".github-gen/sources/evil.md");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "evil\n").unwrap();

    // A redundant separator must not dodge the reserved-path guard: the
    // comparison is over paths, not strings.
    let config = root.join(".github-gen/velnor-workflow.toml");
    let base = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!(
            "{base}\n[[static_files]]\nfile = \".github//AGENTS.md\"\nsource = \".github-gen/sources/evil.md\"\n"
        ),
    )
    .unwrap();
    let outcome = run_generate(&root, &output, true);
    assert!(
        !outcome.status.success(),
        "a reserved-path spelling must fail"
    );
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        stderr.contains("generator owns this path"),
        "refusal must name the reservation: {stderr}"
    );
}

#[test]
fn v1_reserved_agent_path_spellings_are_rejected() {
    reserved_agent_path_spellings_are_rejected(Pipeline::V1);
}

#[test]
fn s2_reserved_agent_path_spellings_are_rejected() {
    reserved_agent_path_spellings_are_rejected(Pipeline::S2);
}

#[cfg(unix)]
fn set_readonly(path: &Path, readonly: bool) {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(if readonly { 0o555 } else { 0o755 });
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
fn replacement_recovery_preserves_tree_on_failure(pipeline: Pipeline) {
    let (root, output) = fixture(pipeline, "recovery");
    let before = snapshot_tree(&output);

    // A pre-publication failure (staging cannot even start) leaves the
    // tree byte-identical. The unknown plants drift so publication is
    // attempted instead of short-circuiting to a no-op.
    fs::write(output.join(".github/staged.txt"), "staged\n").unwrap();
    set_readonly(&output, true);
    let outcome = run_generate(&root, &output, true);
    set_readonly(&output, false);
    assert!(
        !outcome.status.success(),
        "an unwritable root must fail publication"
    );
    let mut failed = snapshot_tree(&output);
    failed.remove(&PathBuf::from(".github/staged.txt"));
    assert_eq!(
        failed, before,
        "a pre-publication failure must preserve the tree"
    );
    assert_no_staging_leftovers(&output);
    fs::remove_file(output.join(".github/staged.txt")).unwrap();

    // A publish failure before the first install also preserves the tree:
    // the unknown removal cannot start, so there is nothing to roll back.
    fs::write(output.join(".github/doomed.txt"), "doomed\n").unwrap();
    set_readonly(&output.join(".github"), true);
    let outcome = run_generate(&root, &output, true);
    set_readonly(&output.join(".github"), false);
    assert!(
        !outcome.status.success(),
        "an unwritable tree must fail publication"
    );
    let mut failed = snapshot_tree(&output);
    failed.remove(&PathBuf::from(".github/doomed.txt"));
    assert_eq!(
        failed, before,
        "a failed publish must preserve every owned byte"
    );
    assert_eq!(
        fs::read_to_string(output.join(".github/doomed.txt")).unwrap(),
        "doomed\n",
        "a failed removal must leave the unknown in place"
    );
    assert_no_staging_leftovers(&output);

    // Recovery is a retry away once the failure clears.
    generate_ok(&root, &output, true);
    assert_eq!(snapshot_tree(&output), before);
    check_ok(&root, &output);
}

#[test]
#[cfg(unix)]
fn v1_replacement_recovery_preserves_tree_on_failure() {
    replacement_recovery_preserves_tree_on_failure(Pipeline::V1);
}

#[test]
#[cfg(unix)]
fn s2_replacement_recovery_preserves_tree_on_failure() {
    replacement_recovery_preserves_tree_on_failure(Pipeline::S2);
}

fn stale_recorded_file_removes_without_force(pipeline: Pipeline) {
    let root = minimal_root(&format!("{}-stale", pipeline.name()));
    write_config(pipeline, &root);
    let output = output_for(&root);
    let _ = fs::remove_dir_all(&output);
    let source = root.join(".github-gen/sources/note.md");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "note\n").unwrap();
    let config = root.join(".github-gen/velnor-workflow.toml");
    let base = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!(
            "{base}\n[[static_files]]\nfile = \".github/note.md\"\nsource = \".github-gen/sources/note.md\"\n"
        ),
    )
    .unwrap();
    generate_ok(&root, &output, false);
    assert_eq!(
        fs::read_to_string(output.join(".github/note.md")).unwrap(),
        "note\n"
    );

    // Dropping the row makes the recorded file stale: digest-verified, so
    // plain generation removes it — no `--force` needed.
    fs::write(&config, &base).unwrap();
    generate_ok(&root, &output, false);
    assert!(
        fs::symlink_metadata(output.join(".github/note.md")).is_err(),
        "a stale recorded file must be removed without force"
    );
    assert_no_staging_leftovers(&output);
    check_ok(&root, &output);
}

#[test]
fn v1_stale_recorded_file_removes_without_force() {
    stale_recorded_file_removes_without_force(Pipeline::V1);
}

#[test]
fn s2_stale_recorded_file_removes_without_force() {
    stale_recorded_file_removes_without_force(Pipeline::S2);
}

#[test]
#[cfg(unix)]
fn force_normalizes_executable_modes() {
    for pipeline in [Pipeline::V1, Pipeline::S2] {
        let (root, output) = fixture(pipeline, "mode-repair");
        let workflow = output.join(".github/workflows/ci-pr.yml");
        make_executable(&workflow);
        assert!(is_executable(&workflow));
        generate_ok(&root, &output, true);
        assert!(
            !is_executable(&workflow),
            "force must normalize the executable bit away"
        );
        check_ok(&root, &output);
    }
}
