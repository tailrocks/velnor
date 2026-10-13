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

#[cfg(unix)]
use common::generate_ok_with_umask;
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

fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn forge_sidecar_claim(output: &Path, relative: &str, bytes: &[u8]) {
    let state_path = output.join(".github/ci/.github-actions-generator-state");
    let state = fs::read_to_string(&state_path).unwrap();
    let (prefix, output_rows) = state.split_once("[outputs]\n").unwrap();
    let mut rows = output_rows
        .lines()
        .filter(|line| !line.is_empty())
        .filter(|line| line.split_once('\t').map(|(path, _)| path) != Some(relative))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    rows.push(format!("{relative}\t{:016x}", fnv64(bytes)));
    rows.sort_by(|left, right| {
        left.split_once('\t')
            .unwrap()
            .0
            .cmp(right.split_once('\t').unwrap().0)
    });
    let forged = format!("{prefix}[outputs]\n{}\n", rows.join("\n"));
    fs::write(state_path, forged).unwrap();
}

fn remove_sidecar_claim(output: &Path, relative: &str) {
    let state_path = output.join(".github/ci/.github-actions-generator-state");
    let state = fs::read_to_string(&state_path).unwrap();
    let (prefix, output_rows) = state.split_once("[outputs]\n").unwrap();
    let rows = output_rows
        .lines()
        .filter(|line| !line.is_empty())
        .filter(|line| line.split_once('\t').map(|(path, _)| path) != Some(relative))
        .collect::<Vec<_>>();
    fs::write(
        state_path,
        format!("{prefix}[outputs]\n{}\n", rows.join("\n")),
    )
    .unwrap();
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

fn exact_digest_sidecar_claim_does_not_expand_cli_force_authority(pipeline: Pipeline) {
    let (root, output) = fixture(pipeline, "forged-sidecar-force-boundary");
    let claimed = ".github/workflows/zz-forged.yml";
    let claimed_bytes = b"name: handwritten\n";
    let readme_bytes = b"human-owned documentation\n";
    fs::write(output.join(claimed), claimed_bytes).unwrap();
    fs::write(output.join("README.md"), readme_bytes).unwrap();
    forge_sidecar_claim(&output, claimed, claimed_bytes);
    forge_sidecar_claim(&output, "README.md", readme_bytes);
    let forged_state = fs::read(output.join(".github/ci/.github-actions-generator-state")).unwrap();

    let default = run_generate(&root, &output, false);
    assert!(
        !default.status.success(),
        "exact-digest unrendered `.github` claim must remain unknown"
    );
    assert!(output.join(claimed).is_file());
    assert_eq!(fs::read(output.join("README.md")).unwrap(), readme_bytes);
    assert_eq!(
        fs::read(output.join(".github/ci/.github-actions-generator-state")).unwrap(),
        forged_state,
        "a refused default run must preserve the forged sidecar"
    );

    let forced = run_generate(&root, &output, true);
    assert!(
        forced.status.success(),
        "force should remove only the unknown `.github` file: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert!(
        fs::symlink_metadata(output.join(claimed)).is_err(),
        "force must remove the unknown `.github` file"
    );
    assert_eq!(
        fs::read(output.join("README.md")).unwrap(),
        readme_bytes,
        "forged sidecar claim must not expand force outside `.github`"
    );
    let rewritten_state =
        fs::read_to_string(output.join(".github/ci/.github-actions-generator-state")).unwrap();
    assert!(!rewritten_state.contains("zz-forged.yml\t"));
    assert!(!rewritten_state.contains("README.md\t"));
    check_ok(&root, &output);
}

#[test]
fn v1_exact_digest_sidecar_claim_does_not_expand_force_authority() {
    exact_digest_sidecar_claim_does_not_expand_cli_force_authority(Pipeline::V1);
}

#[test]
fn s2_exact_digest_sidecar_claim_does_not_expand_force_authority() {
    exact_digest_sidecar_claim_does_not_expand_cli_force_authority(Pipeline::S2);
}

fn force_rejects_modified_current_workflow(pipeline: Pipeline) {
    let (root, output) = fixture(pipeline, "force-modified-current-workflow");
    let workflow = output.join(".github/workflows/ci-pr.yml");
    assert!(workflow.is_file(), "fixture must render the PR workflow");
    fs::write(&workflow, "# manual edit\n").unwrap();

    let forced = run_generate(&root, &output, true);
    assert!(
        !forced.status.success(),
        "--force must not overwrite a modified current-renderer output"
    );
    let stderr = String::from_utf8_lossy(&forced.stderr);
    assert!(
        stderr.contains("manually modified generated file"),
        "refusal must name the ownership failure: {stderr}"
    );
    assert_eq!(fs::read(&workflow).unwrap(), b"# manual edit\n");
}

#[test]
fn v1_force_rejects_modified_current_workflow() {
    force_rejects_modified_current_workflow(Pipeline::V1);
}

#[test]
fn s2_force_rejects_modified_current_workflow() {
    force_rejects_modified_current_workflow(Pipeline::S2);
}

fn forged_digest_current_workflow_still_conflicts_without_force(pipeline: Pipeline) {
    let (root, output) = fixture(pipeline, "forged-current-workflow-no-force");
    let relative = ".github/workflows/ci-pr.yml";
    let manual = b"# manual edit with forged ownership digest\n";
    let workflow = output.join(relative);
    assert!(workflow.is_file(), "fixture must render the PR workflow");
    fs::write(&workflow, manual).unwrap();
    forge_sidecar_claim(&output, relative, manual);
    let forged_state = fs::read(output.join(".github/ci/.github-actions-generator-state")).unwrap();

    let refused = run_generate(&root, &output, false);
    assert!(
        !refused.status.success(),
        "unforced run must refuse changed current-renderer output"
    );
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains(relative),
        "refusal must identify the current output: {stderr}"
    );
    assert_eq!(fs::read(&workflow).unwrap(), manual);
    assert_eq!(
        fs::read(output.join(".github/ci/.github-actions-generator-state")).unwrap(),
        forged_state,
        "refused run must not refresh the forged claim"
    );
}

#[test]
fn v1_forged_digest_current_workflow_conflicts_without_force() {
    forged_digest_current_workflow_still_conflicts_without_force(Pipeline::V1);
}

#[test]
fn s2_forged_digest_current_workflow_conflicts_without_force() {
    forged_digest_current_workflow_still_conflicts_without_force(Pipeline::S2);
}

fn force_adopts_unowned_current_workflow(pipeline: Pipeline) {
    let (root, output) = fixture(pipeline, "force-adopt-unowned-current-workflow");
    let relative = ".github/workflows/ci-pr.yml";
    let workflow = output.join(relative);
    let generated = fs::read(&workflow).unwrap();
    fs::write(&workflow, b"# unowned workflow\n").unwrap();
    remove_sidecar_claim(&output, relative);
    let state_without_claim =
        fs::read(output.join(".github/ci/.github-actions-generator-state")).unwrap();

    let refused = run_generate(&root, &output, false);
    assert!(
        !refused.status.success(),
        "unowned current output must require explicit force"
    );
    assert_eq!(fs::read(&workflow).unwrap(), b"# unowned workflow\n");
    assert_eq!(
        fs::read(output.join(".github/ci/.github-actions-generator-state")).unwrap(),
        state_without_claim,
        "default refusal must preserve state"
    );

    let forced = run_generate(&root, &output, true);
    assert!(
        forced.status.success(),
        "force must adopt an unowned current workflow: {}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert_eq!(fs::read(&workflow).unwrap(), generated);
    let updated_state =
        fs::read_to_string(output.join(".github/ci/.github-actions-generator-state")).unwrap();
    assert!(updated_state.contains(".github/workflows/ci-pr.yml\t"));
    check_ok(&root, &output);
}

#[test]
fn v1_force_adopts_unowned_current_workflow() {
    force_adopts_unowned_current_workflow(Pipeline::V1);
}

#[test]
fn s2_force_adopts_unowned_current_workflow() {
    force_adopts_unowned_current_workflow(Pipeline::S2);
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

/// The ownership-refresh install path must not inherit the caller umask:
/// a forced repeat under `umask 077` installs the same modes as the
/// staged-tree publish, so the tree stays byte- and mode-identical.
#[cfg(unix)]
fn forced_repeat_is_noop_under_restrictive_umask(pipeline: Pipeline) {
    let (root, output) = fixture(pipeline, "repeat-noop-umask");
    let before = snapshot_tree(&output);
    generate_ok_with_umask(&root, &output, true, "077");
    assert_eq!(
        snapshot_tree(&output),
        before,
        "forced repeat under umask 077 must leave every byte, mode, and link untouched"
    );
    assert_no_staging_leftovers(&output);
    check_ok(&root, &output);
}

#[cfg(unix)]
#[test]
fn v1_forced_repeat_is_noop_under_restrictive_umask() {
    forced_repeat_is_noop_under_restrictive_umask(Pipeline::V1);
}

#[cfg(unix)]
#[test]
fn s2_forced_repeat_is_noop_under_restrictive_umask() {
    forced_repeat_is_noop_under_restrictive_umask(Pipeline::S2);
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
    // A hand edit is never repaired: the ownership proof refuses it, so the
    // tree cannot silently absorb a manual change.
    let force = run_generate(&root, &output, true);
    let refusal = match drift {
        Drift::Edit(_) => Some("manually modified"),
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

fn reserved_policy_path_spellings_are_rejected(pipeline: Pipeline) {
    for (index, file) in [
        ".github/workflows/ci-policy.yml",
        ".github//workflows/ci-policy.yml",
        ".github/workflows/./ci-policy.yml",
        ".github/workflows/CI-POLICY.yml",
        ".github/WORKFLOWS/ci-policy.yml",
        ".github/workflow\u{017f}/ci-policy.yml",
    ]
    .into_iter()
    .enumerate()
    {
        let root = minimal_root(&format!("{}-reserved-policy-{index}", pipeline.name()));
        write_config(pipeline, &root);
        let output = output_for(&root);
        let _ = fs::remove_dir_all(&output);
        fs::create_dir_all(output.join(".github/workflows")).unwrap();
        let existing_policy = output.join(".github/workflows/ci-policy.yml");
        fs::write(&existing_policy, "preserve existing policy\n").unwrap();
        let source = root.join(".github-gen/sources/policy.yml");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "name: untrusted replacement\n").unwrap();

        let config = root.join(".github-gen/velnor-workflow.toml");
        let base = fs::read_to_string(&config).unwrap();
        fs::write(
            &config,
            format!(
                "{base}\n[[static_files]]\nfile = \"{file}\"\nsource = \".github-gen/sources/policy.yml\"\n"
            ),
        )
        .unwrap();
        let outcome = run_generate(&root, &output, true);
        assert!(
            !outcome.status.success(),
            "security-owned policy path spelling `{file}` must fail"
        );
        let stderr = String::from_utf8_lossy(&outcome.stderr);
        assert!(
            stderr.contains("generator owns this path")
                && stderr.contains(".github/workflows/ci-policy.yml"),
            "refusal must identify the reserved policy workflow: {stderr}"
        );
        assert_eq!(
            fs::read_to_string(&existing_policy).unwrap(),
            "preserve existing policy\n",
            "rejection must not overwrite the existing policy entrypoint"
        );
    }
}

#[test]
fn v1_reserved_policy_path_spellings_are_rejected() {
    reserved_policy_path_spellings_are_rejected(Pipeline::V1);
}

#[test]
fn s2_reserved_policy_path_spellings_are_rejected() {
    reserved_policy_path_spellings_are_rejected(Pipeline::S2);
}

fn unicode_policy_path_alias_is_rejected_with_entrypoint_rendered(pipeline: Pipeline) {
    let root = minimal_root(&format!("{}-reserved-policy-unicode", pipeline.name()));
    write_config(pipeline, &root);
    let output = output_for(&root);
    let _ = fs::remove_dir_all(&output);
    let source = root.join(".github-gen/sources/policy.yml");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "name: untrusted replacement\n").unwrap();

    let config = root.join(".github-gen/velnor-workflow.toml");
    let base = fs::read_to_string(&config).unwrap();
    let base = base.replacen(
        "[workflow]\n",
        "[workflow]\nfiles = [\"ci-pr.yml\", \"ci-policy.yml\"]\n",
        1,
    );
    assert!(
        base.contains("files = [\"ci-pr.yml\", \"ci-policy.yml\"]"),
        "fixture workflow surface must render ci-policy.yml"
    );
    fs::write(
        &config,
        format!(
            "{base}\n[[static_files]]\nfile = \".github/workflow\u{017f}/ci-policy.yml\"\nsource = \".github-gen/sources/policy.yml\"\n"
        ),
    )
    .unwrap();

    let outcome = run_generate(&root, &output, true);
    assert!(
        !outcome.status.success(),
        "a Unicode filesystem alias must fail even when the canonical workflow is omitted"
    );
    let stderr = String::from_utf8_lossy(&outcome.stderr);
    assert!(
        stderr.contains("generator owns this path")
            && stderr.contains(".github/workflows/ci-policy.yml"),
        "refusal must identify the canonical policy workflow: {stderr}"
    );
    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(output);
}

#[test]
fn v1_unicode_policy_path_alias_is_rejected_when_entrypoint_is_omitted() {
    unicode_policy_path_alias_is_rejected_with_entrypoint_rendered(Pipeline::V1);
}

#[test]
fn s2_unicode_policy_path_alias_is_rejected_when_entrypoint_is_omitted() {
    unicode_policy_path_alias_is_rejected_with_entrypoint_rendered(Pipeline::S2);
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

fn stale_recorded_workflow_is_unknown_until_force(pipeline: Pipeline) {
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

    // The sidecar row no longer proves ownership after the renderer drops it.
    // The `.github` walk reports the file as unknown and force owns removal.
    fs::write(&config, &base).unwrap();
    let blocked = run_generate(&root, &output, false);
    assert!(
        !blocked.status.success(),
        "stale workflow must block without force"
    );
    assert!(
        String::from_utf8_lossy(&blocked.stderr).contains("will not be imported"),
        "refusal must name the unknown-content gate"
    );
    assert!(output.join(".github/note.md").is_file());
    generate_ok(&root, &output, true);
    assert!(
        fs::symlink_metadata(output.join(".github/note.md")).is_err(),
        "force removes the unknown stale workflow output"
    );
    assert_no_staging_leftovers(&output);
    check_ok(&root, &output);
}

#[test]
fn v1_stale_recorded_workflow_is_unknown_until_force() {
    stale_recorded_workflow_is_unknown_until_force(Pipeline::V1);
}

#[test]
fn s2_stale_recorded_workflow_is_unknown_until_force() {
    stale_recorded_workflow_is_unknown_until_force(Pipeline::S2);
}

fn generated_static_source_is_retained_during_output_migration(pipeline: Pipeline) {
    let root = minimal_root(&format!("{}-static-source-retention", pipeline.name()));
    write_config(pipeline, &root);
    let output = root.clone();
    let config = root.join(".github-gen/velnor-workflow.toml");
    let base = fs::read_to_string(&config).unwrap();

    // First generate and commit the exact sidecar that the renderer already
    // owns. The next config explicitly adopts that path as static input.
    fs::write(
        &config,
        format!(
            "{base}\n[cache.host]\nbudget_bytes = 53687091200\n\n[cache.host.artifact]\npath = \"state/cache.env\"\ntemplate = \"CACHE={{budget_bytes}}\\n\"\n"
        ),
    )
    .unwrap();
    generate_ok(&root, &output, false);
    let source = root.join("state/cache.env");
    assert!(
        source.is_file(),
        "cache config must generate the source sidecar"
    );
    let migrated_output = root.join(".github/migrated.env");
    fs::write(&migrated_output, fs::read(&source).unwrap()).unwrap();
    common::commit_fixture(&root);

    fs::write(
        &config,
        format!(
            "{base}\n[[static_files]]\nfile = \".github/migrated.env\"\nsource = \"state/cache.env\"\n"
        ),
    )
    .unwrap();
    generate_ok(&root, &output, false);

    let source_bytes = fs::read(&source).unwrap();
    assert_eq!(fs::read(&migrated_output).unwrap(), source_bytes);
    assert!(
        source.is_file(),
        "declared static source must survive migration"
    );
    check_ok(&root, &output);

    let before = snapshot_tree(&output.join(".github"));
    let source_before = fs::read(&source).unwrap();
    generate_ok(&root, &output, false);
    assert_eq!(
        snapshot_tree(&output.join(".github")),
        before,
        "repeat generation must leave generated outputs unchanged"
    );
    assert_eq!(fs::read(&source).unwrap(), source_before);
}

#[test]
fn v1_generated_static_source_is_retained_during_output_migration() {
    generated_static_source_is_retained_during_output_migration(Pipeline::V1);
}

#[test]
fn s2_generated_static_source_is_retained_during_output_migration() {
    generated_static_source_is_retained_during_output_migration(Pipeline::S2);
}

fn separate_output_keeps_static_source_and_unrendered_output(pipeline: Pipeline) {
    let root = minimal_root(&format!("{}-separate-static-source", pipeline.name()));
    write_config(pipeline, &root);
    let output = output_for(&root);
    let _ = fs::remove_dir_all(&output);
    let config = root.join(".github-gen/velnor-workflow.toml");
    let base = fs::read_to_string(&config).unwrap();

    fs::write(
        &config,
        format!(
            "{base}\n[cache.host]\nbudget_bytes = 53687091200\n\n[cache.host.artifact]\npath = \"state/cache.env\"\ntemplate = \"CACHE={{budget_bytes}}\\n\"\n"
        ),
    )
    .unwrap();
    generate_ok(&root, &output, false);
    let generated_sidecar = output.join("state/cache.env");
    let source = root.join("state/cache.env");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let source_bytes = fs::read(&generated_sidecar).unwrap();
    fs::write(&source, &source_bytes).unwrap();
    common::commit_fixture(&root);

    fs::write(
        &config,
        format!(
            "{base}\n[[static_files]]\nfile = \".github/migrated.env\"\nsource = \"state/cache.env\"\n"
        ),
    )
    .unwrap();
    generate_ok(&root, &output, false);

    assert_eq!(fs::read(&source).unwrap(), source_bytes);
    assert_eq!(
        fs::read(output.join(".github/migrated.env")).unwrap(),
        source_bytes
    );
    assert_eq!(
        fs::read(&generated_sidecar).unwrap(),
        source_bytes,
        "unrendered output outside `.github` remains an ordinary file"
    );
    check_ok(&root, &output);
}

#[test]
fn v1_separate_output_keeps_static_source_and_unrendered_output() {
    separate_output_keeps_static_source_and_unrendered_output(Pipeline::V1);
}

#[test]
fn s2_separate_output_keeps_static_source_and_unrendered_output() {
    separate_output_keeps_static_source_and_unrendered_output(Pipeline::S2);
}

fn static_output_case_alias_transition_is_rejected(pipeline: Pipeline) {
    let root = minimal_root(&format!("{}-static-output-case-alias", pipeline.name()));
    write_config(pipeline, &root);
    let output = output_for(&root);
    let _ = fs::remove_dir_all(&output);
    let config = root.join(".github-gen/velnor-workflow.toml");
    let base = fs::read_to_string(&config).unwrap();
    let source = root.join(".github-gen/sources/note.md");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "static bytes\n").unwrap();
    fs::write(
        &config,
        format!(
            "{base}\n[[static_files]]\nfile = \".github/migrated.env\"\nsource = \".github-gen/sources/note.md\"\n"
        ),
    )
    .unwrap();
    generate_ok(&root, &output, false);
    let before = snapshot_tree(&output.join(".github"));

    fs::write(
        &config,
        format!(
            "{base}\n[[static_files]]\nfile = \".github/MIGRATED.env\"\nsource = \".github-gen/sources/note.md\"\n"
        ),
    )
    .unwrap();
    let rejected = run_generate(&root, &output, false);
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        !rejected.status.success(),
        "case alias transition must fail: {stderr}"
    );
    assert!(
        stderr.contains("aliases") || stderr.contains("overlap"),
        "rejection must identify the output path collision: {stderr}"
    );
    assert_eq!(snapshot_tree(&output.join(".github")), before);
}

#[test]
fn v1_static_output_case_alias_transition_is_rejected() {
    static_output_case_alias_transition_is_rejected(Pipeline::V1);
}

#[test]
fn s2_static_output_case_alias_transition_is_rejected() {
    static_output_case_alias_transition_is_rejected(Pipeline::S2);
}

fn static_output_file_directory_transition_is_rejected(pipeline: Pipeline) {
    let root = minimal_root(&format!(
        "{}-static-output-shape-transition",
        pipeline.name()
    ));
    write_config(pipeline, &root);
    let output = output_for(&root);
    let _ = fs::remove_dir_all(&output);
    let config = root.join(".github-gen/velnor-workflow.toml");
    let base = fs::read_to_string(&config).unwrap();
    let source = root.join(".github-gen/sources/note.md");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "static bytes\n").unwrap();
    fs::write(
        &config,
        format!(
            "{base}\n[[static_files]]\nfile = \".github/migrated.env\"\nsource = \".github-gen/sources/note.md\"\n"
        ),
    )
    .unwrap();
    generate_ok(&root, &output, false);
    let original = fs::read(output.join(".github/migrated.env")).unwrap();
    let before = snapshot_tree(&output.join(".github"));

    fs::write(
        &config,
        format!(
            "{base}\n[[static_files]]\nfile = \".github/migrated.env/child\"\nsource = \".github-gen/sources/note.md\"\n"
        ),
    )
    .unwrap();
    let rejected = run_generate(&root, &output, false);
    assert!(
        !rejected.status.success(),
        "file-to-directory transition must fail: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(
        fs::read(output.join(".github/migrated.env")).unwrap(),
        original,
        "a failed transition must preserve the old output"
    );
    assert_eq!(snapshot_tree(&output.join(".github")), before);
}

#[test]
fn v1_static_output_file_directory_transition_is_rejected() {
    static_output_file_directory_transition_is_rejected(Pipeline::V1);
}

#[test]
fn s2_static_output_file_directory_transition_is_rejected() {
    static_output_file_directory_transition_is_rejected(Pipeline::S2);
}

fn static_source_rejects_active_fleet_cache_feedback(pipeline: Pipeline) {
    let root = minimal_root(&format!("{}-static-source-cache-feedback", pipeline.name()));
    write_config(pipeline, &root);
    let output = root.clone();
    let config = root.join(".github-gen/velnor-workflow.toml");
    let base = fs::read_to_string(&config).unwrap();
    let with_cache = format!(
        "{base}\n[cache.host]\nbudget_bytes = 53687091200\n\n[cache.host.artifact]\npath = \"state/cache.env\"\ntemplate = \"CACHE={{budget_bytes}}\\n\"\n"
    );
    fs::write(&config, &with_cache).unwrap();
    generate_ok(&root, &output, false);
    let source = root.join("state/cache.env");
    let before = fs::read(&source).unwrap();

    fs::write(
        &config,
        format!(
            "{with_cache}\n[[static_files]]\nfile = \".github/migrated.env\"\nsource = \"state/cache.env\"\n"
        ),
    )
    .unwrap();
    let rejected = run_generate(&root, &output, false);
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(
        !rejected.status.success(),
        "self-referential output is rejected"
    );
    assert!(
        stderr.contains("cannot source `state/cache.env`"),
        "the rejection names the generated source collision: {stderr}"
    );
    assert_eq!(fs::read(&source).unwrap(), before);
    assert!(
        fs::symlink_metadata(root.join(".github/migrated.env")).is_err(),
        "a rejected config does not publish static output"
    );
}

#[test]
fn v1_static_source_rejects_active_fleet_cache_feedback() {
    static_source_rejects_active_fleet_cache_feedback(Pipeline::V1);
}

#[test]
fn s2_static_source_rejects_active_fleet_cache_feedback() {
    static_source_rejects_active_fleet_cache_feedback(Pipeline::S2);
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
