# G1 scan-integrity corrected candidate — independent review

## Verdict

**Rejected.** Commit `3c46b9e83c9a0ca57be88743e49ecae27f731685` fixes the previously rejected generated-header and exact static-source cases, but the corrected tree still has two fail-closed gaps:

1. A parse-valid ownership sidecar can classify an unmanaged user workflow as stale generated output and delete it under `--force`.
2. Static source validation is not path-normalized; `./.github/...` bypasses the new `.github` rejection and can self-source a workflow.

This preserves the prior independent verdict: `7e9a2b5f...` and `6409a086...` remain rejected. This report reviews only the exact corrected commit.

## Exact review identity

- Candidate: `3c46b9e83c9a0ca57be88743e49ecae27f731685`.
- Parent: `12cc87b629802c294da9840325cb21087c020df6`.
- Tree: `921391689cf0d64adf47bd9cc9e215704b5b7ab7`.
- Detached checkout: `/tmp/velnor-g1-scan-integrity-review-exact`; clean, detached HEAD.
- Source worktree/branch: `codex/github-first-scan-integrity-corrected`.
- No source edits, merge, generated-tree regeneration, host operation, or Docker operation.

## Exact health verification

Passed in the detached tree:

- `rtk cargo test -p velnor-workflow --lib s2::scan::file_walk -- --nocapture`: **5 passed**.
- `rtk cargo test -p velnor-workflow --lib generated_output_churn_is_not_scan_provenance_but_handwritten_github_is -- --nocapture`: **1 passed**.
- `rtk cargo test -p velnor-workflow --lib static_source_cannot_hide_workflow_inputs_or_self_reference -- --nocapture`: **1 passed**.
- `rtk cargo test -p velnor-workflow --lib declared_static_output_is_excluded_before_and_after_first_generation -- --nocapture`: **1 passed**.
- `rtk cargo test -p velnor-workflow --lib -- --skip checked_in_workflows_match_the_generator_byte_for_byte`: **1,741 passed, 1 filtered**.
- `rtk cargo clippy -p velnor-workflow --lib --tests -- -D warnings`: **pass**.
- `rtk cargo fmt --all -- --check`: **pass**.
- `rtk git diff --check`: **pass**.

The filtered checked-in snapshot failure is the known pre-existing stale output noted in the handoff; it is not counted as a candidate pass.

## Capability/adversarial matrix

| Case | Exact evidence | Result |
|---|---|---|
| Forged generated/fleet headers | `/tmp/velnor-g1-scan-integrity-review-exact/crates/velnor-workflow/src/s2/scan/file_walk.rs:452-530`; `.../s2/mod.rs:20224-20310` | **Pass.** Header bytes do not suppress handwritten or unmanaged workflow inputs. |
| Recorded generated-output removal | `.../s2/mod.rs:20224-20258` | **Pass.** Missing current output remains a generation drift error. |
| Handwritten `.github` input visibility | `.../s2/scan/file_walk.rs:139-153,452-530` | **Pass for tested direct files.** `.github` is no longer globally excluded. |
| Shallow checkout boundary | `.../s2/scan/file_walk.rs:532-578` | **Pass for tested fixture.** |
| Exact static source under `.github` | `.../s2/config/mod.rs:2080-2124`; `.../s2/mod.rs:20353-20385` | **Pass only for normalized-looking exact path.** `source = ".github/workflows/self.yml"` is rejected. |
| Static source `./.github/...` | `.../s2/config/mod.rs:2097-2100` | **Fail.** Raw `Path::starts_with(".github")` does not establish normalized containment. Exact CLI adversary was accepted and generated the self-sourced workflow. |
| Sidecar path ownership | `.../s2/scan/file_walk.rs:20-23,44-48`; `.../s2/mod.rs:6936-6948` | **Fail.** Every path in a parse-valid sidecar is removed from scan provenance before it is shown to the renderer/legacy checker. |
| Sidecar path allowlist | `.../s2/mod.rs:7036-7050` | **Fail.** `managed_relative_path` permits any path under `.github/`; it does not prove that the current renderer owns that path. |
| Legacy workflow classification | `.../s2/mod.rs:7074-7111` | **Fail under forged sidecar.** `owned_by_state` trusts the sidecar path alone, so a user workflow is not reported as unmanaged. |
| Stale deletion proof | `.../s2/mod.rs:6772-6808` | **Fail under forged sidecar.** If the attacker records the deterministic digest of the user file, stale deletion accepts it as generated. |

## Reproducible sidecar attack

Temporary fixture: `/tmp/velnor-g1-sidecar-attack`.

1. Generate a minimal tracked Rust repository with the exact candidate binary.
2. Add tracked `.github/workflows/user.yml` containing user-controlled workflow behavior.
3. Add its deterministic FNV-1a digest (`4d42ab05ee8d29e4`) to `[outputs]` in `.github/ci/.github-actions-generator-state`.
4. Run:

```text
/tmp/velnor-g1-scan-integrity-review-exact/target/debug/velnor-workflow \
  --providers github-hosted --default-branch main --plain --check
```

`--check` did not classify `user.yml` as unmanaged; it reported only generated drift involving the forged stale output and state file.

5. Run the authorized replacement path:

```text
/tmp/velnor-g1-scan-integrity-review-exact/target/debug/velnor-workflow \
  --providers github-hosted --default-branch main --plain --force
```

Exit status was zero. The observed result was **`user.yml deleted`**, and the sidecar line was removed. The digest is not an authentication boundary: `content_digest` is public deterministic FNV-1a at `.../s2/mod.rs:7054-7065`.

Root cause: `generator_owned_output_paths` reads the sidecar before `git ls-files`, and `direct_legacy_workflows`/`stale_owned_files` treat its arbitrary `.github/...` path plus attacker-computed digest as ownership. The source itself documents that this state is “not a security boundary” at `.../s2/mod.rs:6708-6711`; that is incompatible with the requested scan-integrity boundary.

## Reproducible static self-source bypass

Temporary fixture: `/tmp/velnor-g1-static-self-bypass`.

Config contained:

```toml
[[static_files]]
file = ".github/workflows/self.yml"
source = "./.github/workflows/self.yml"
```

The exact candidate accepted the config and generated the output, reporting `.github/workflows/self.yml` as unchanged. It then recorded that workflow as owned output in the sidecar. The exact test only exercises the unprefixed source string and therefore misses this bypass. The check after the first pass reports scan-input drift because the first scan saw the file before the sidecar existed; `--force` can then refresh the state and preserve the hidden self-source.

Root cause: `validate_static_files` checks `Path::new(source).starts_with(".github")` at `.../s2/config/mod.rs:2097-2100`, while `is_contained_repository_path` permits `.` components at `.../s2/config/mod.rs:2119-2124`. Normalize first, then reject every normalized source beneath `.github`.

## Smallest bounded correction

1. Treat sidecar output paths as untrusted until ownership is independently bound to the current renderer/config contract. Unknown or non-renderable sidecar paths must fail closed; they must never enter `generator_owned_output_paths` merely because the sidecar parses.
2. Do not permit a forged sidecar to suppress scan inputs. Verify sidecar ownership against a trusted current-output allowlist and exact rendered bytes before exclusion; keep missing/unverified paths as scan inputs or return an ownership error.
3. Normalize static source paths with one repository-relative canonicalizer before `.github` containment checks. Reject `./.github`, repeated separators, and equivalent path spellings; retain rejection of `..`, absolute, and backslash paths.
4. Add adversarial regression tests: tracked and untracked forged sidecar, existing user workflow with exact attacker digest, wrong digest, missing sidecar-listed input, `.github/actions` user action, `./.github`, `.//.github`, and first-generation/repeat-`--check` behavior.
5. Re-run the full exact gate only after the correction; do not regenerate checked-in `.github` outputs from the current corrected branch.

The corrected candidate is not approvable for scan-integrity until both adversaries fail closed. Role/runtime/action work remains outside this review.
