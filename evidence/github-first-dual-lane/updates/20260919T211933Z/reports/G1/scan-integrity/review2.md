# G1 scan-integrity independent review 2

Status: **REJECTED** for G1 integration.

Candidate under review: `2c328e1d471546a2c9b4b1f7e790d1e3562c66d3` (`codex/github-first-scan-integrity-fix`), tree `1d2c422e9e0dd949667cff90043ec5ff51e207bc`, parent `12cc87b629802c294da9840325cb21087c020df6`. Exact detached tree was clean. Review is read-only; no source, generated workflow, host, or Docker mutation.

This is a separate exact-candidate review. It does not replace `review.md`, `REPORT.md`, or `corrected-independent-review.md`, and makes no merge or gate claim.

## What the candidate fixes

- The first scan pass excludes only fixed generator artifacts. The second pass excludes sidecar paths only after `scan_target` renders the current shape and checks each recorded path against that render (`crates/velnor-workflow/src/s2/mod.rs:1308-1335`, `1421-1439`, `6973-7004`). A forged sidecar entry for an arbitrary workflow or action is therefore rejected.
- Lexical normalization rejects `./.github`, repeated separators, traversal, absolute paths, backslashes, and controls (`crates/velnor-workflow/src/s2/config/mod.rs:2083-2145`). Static-file source reads canonicalize the root and source, reject repository escapes and canonical `.github` targets (`crates/velnor-workflow/src/s2/mod.rs:2927-2981`).
- Git-index and physical walks skip symlinks; `.github` is no longer a blanket scan exclusion (`crates/velnor-workflow/src/s2/scan/file_walk.rs:26-60`, `91-147`, `149-197`). Managed output ancestors are also rejected when writing (`crates/velnor-workflow/src/s2/mod.rs:6654-6700`).

## Blocking findings

### G1-SI-2: sidecar digest remains forgeable for an allowed renderer path

`verified_recorded_output_paths` checks that a sidecar path is in the current renderer, then accepts any existing bytes whose digest equals the sidecar value (`crates/velnor-workflow/src/s2/mod.rs:6984-7003`). The sidecar is repository-writable, and the digest is an unauthenticated, deterministic FNV-1a value (`crates/velnor-workflow/src/s2/mod.rs:7152-7162`). A writer can therefore:

1. choose a path the current renderer emits, such as `.github/workflows/ci-main.yml`;
2. replace its bytes with a user workflow/action body;
3. set that path's sidecar digest to the deterministic digest of those bytes.

The current bytes then satisfy `current_digest == expected`, so the path is added to `owned_paths` and disappears from scan provenance. The write planner repeats the same trust decision: `verify_generated_ownership` treats a matching sidecar digest as owned and permits replacement (`crates/velnor-workflow/src/s2/mod.rs:6790-6825`). This lets user-controlled metadata authorize invisibility and a later overwrite. The new current-renderer path allowlist fixes arbitrary-path injection but does not establish ownership of bytes.

Required structural fix: an untrusted sidecar must never authenticate arbitrary current bytes. Accept an existing path for scan exclusion only when bytes equal the current renderer (or use a separately authenticated/trusted generation record); do not treat a digest supplied by the same writer as proof. Add an adversarial fixture that edits a current-renderer workflow/action and forges its exact sidecar digest, then asserts scan/check/`--force` fail closed and preserve the body.

### G1-SI-3: current-renderer binding blocks legitimate stale-output regeneration

The same loop rejects every recorded output absent from the *current* renderer (`crates/velnor-workflow/src/s2/mod.rs:6984-6989`). The write planner's stale-output removal is reached later (`crates/velnor-workflow/src/s2/mod.rs:6850-6883`), but `scan_target` now errors before it can plan deletion whenever a config/template change legitimately removes a previously generated path. Thus the candidate has no safe path for “old generated output, exact recorded preimage, no longer rendered”: accepting it reintroduces the forged-sidecar problem; rejecting it makes normal generator removal fail. Existing direct planner tests cover stale deletion (`stale_owned_workflows_are_removed_only_after_digest_verification`, `preflight_reports_stale_deletion_and_ownership_update_without_writing`) but do not cover the integrated `scan_target`/sidecar path. Add an integrated fixture that removes a declared generated workflow and proves exact stale deletion plus refusal after a body edit.

## Hostile and integrity checks

Ran against the exact candidate:

- `s2::scan::file_walk` focused suite: **5 passed**.
- `forged_sidecar_cannot_hide_workflow_or_action_inputs`: **passed** for tracked workflow and untracked composite action.
- `declared_static_output_is_excluded_before_and_after_first_generation`: **passed**.
- `static_source_cannot_hide_workflow_inputs_or_self_reference`: **passed** for exact, `./`, and repeated-separator spellings.
- `static_source_symlink_cannot_reach_github_inputs`: **passed**.
- `generated_output_churn_is_not_scan_provenance_but_handwritten_github_is`: **passed**.
- Full unfiltered library: **1742 passed, 2 failed**. No runtime, Docker, or host operation occurred.

These passing fixtures prove the two prior corrected gaps (arbitrary forged paths and lexical/symlink static sources), not the forged digest case above.

## Base versus candidate failures

The exact parent checkout `12cc87b629802c294da9840325cb21087c020df6` was tested in its detached review tree. Both focused parent checks passed:

- `check_fails_when_config_inputs_change_but_output_does_not`: **2 passed**.
- `checked_in_workflows_match_the_generator_byte_for_byte`: **1 passed**.

The candidate changes both outcomes:

1. `check_fails_when_config_inputs_change_but_output_does_not`: **1 passed, 1 failed**. The candidate now renders the current surface while verifying sidecar paths (`scan_target` → `rendered_files_for_scanned`), so this fixture reaches the renderer's “`rust-crate` does not render” error before reporting the intended config-input digest drift. The parent reports the expected config-input error.
2. `checked_in_workflows_match_the_generator_byte_for_byte`: **0 passed, 1 failed**; panic says `release.yml drifted from the generator` at `crates/velnor-workflow/src/s2/mod.rs:15790`. Parent passes. This is a checked-in/generated-snapshot mismatch exposed by the candidate scan/render change; do not regenerate it during this review. It requires a bounded source/config decision before any G1 claim.

Candidate quality checks:

- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed.
- `cargo clippy -p velnor-workflow --lib --tests -- -D warnings`: **failed** on the candidate's new `"" | "." => continue` at `crates/velnor-workflow/src/s2/config/mod.rs:2138` (`clippy::needless_continue`).

## Required bounded follow-up

1. Redesign ownership proof so repository-writable sidecar text cannot authorize scan exclusion, overwrite, or deletion of bytes merely by supplying a matching digest.
2. Preserve legitimate stale generated-file removal through a trusted/bound preimage mechanism; add integrated removal and forged-current-path fixtures.
3. Restore the config-input error ordering and resolve the candidate-induced `release.yml` snapshot drift without generated-output hand edits.
4. Fix the clippy error, then rerun the exact hostile matrix and full library tests on a new immutable candidate SHA. G1 remains unapproved until those tests and source review pass.
