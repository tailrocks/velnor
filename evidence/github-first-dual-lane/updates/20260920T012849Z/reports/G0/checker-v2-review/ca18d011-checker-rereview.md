# Exact checker rereview: ca18d01166681269b6eb5fce8d0f6175fc17aad4

## Bounded verdict

ca18 closes the 27eb recursive child-matrix blocker. A reusable child plan
with multiple concrete instances sharing one logical job ID now fails before
`child_plan.jobs.first()` can select a representative. The exact public-CLI
fixture with recomputed source bytes, outer snapshot digest, raw objects, and
CAS now emits the intended `g0-workflow-derivation` finding.

The live authority remains intentionally unavailable. No authenticated
collector is wired or approved by this review. This is a bounded checker
source rereview only, not G0/G3 or live producer approval.

## Exact source and checks

- Detached tree: `/private/tmp/velnor-checker-review-ca18`
- Exact commit: `ca18d01166681269b6eb5fce8d0f6175fc17aad4`
- Ancestor containing the reviewed 27eb blocker: `27eb094ccd545b642206ea3d52336e0ac74d6abd`
- Product source was not edited.

Fresh exact-tree checks:

- `cargo test --locked --all-features --package velnor-tools -- --nocapture` — **258 passed**.
- `cargo fmt --all -- --check` — pass.
- `cargo clippy --locked --all-features --package velnor-tools --all-targets -- -D warnings` — pass.
- `cargo build --locked --all-features --package velnor-tools --bin velnor-tools` — pass.
- `git diff --check 27eb094c..ca18d011` — pass.

## Recursive child-matrix regression

Harness:

`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/27eb-reusable-matrix-probe/run_probe.py`

Exact result:

`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/27eb-reusable-matrix-probe/result.json`

The synthetic fixture has a pinned root `uses:` workflow, child
`on.workflow_call`, and two same-target matrix assignments. All mutated raw
bytes, source digests, outer snapshot bytes, and CAS objects are regenerated;
the child graph node/edge and source-job binding are also present. It contacts
no network.

ca18 returns exactly:

- `offline-validation-only`
- `g0-workflow-derivation`: `reusable workflow ... expands a logical child
  job into multiple concrete matrix instances; the child graph has no
  concrete matrix identity`

The source fix checks duplicate child logical IDs immediately after recursive
derivation and before selecting the first child. The dedicated unit test
`reusable_child_matrix_instances_are_rejected_before_representative_selection`
and the public fixture agree.

## Live boundary remains closed

Public hostile harness:

`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/a2bca6e-public-cli-harness/ca18-harness-results.json`

All offline cases remain failed with `offline-validation-only`. A synthetic
`--live` input fails before caller-file access with:

`trusted authenticated collector/current-API reconciliation is unavailable:
producer capability velnor.authenticated-closing/v1 is not wired`

`main.rs` contains no call to `install_collector`; `rg` finds only the seam,
tests, and documentation. The `OnceLock` registration is process-local and
immutable, but it is only a future producer hook. No authenticated collector,
closing reconciliation, or live gate is present in ca18. `from_producer` and
`install_collector` must receive separate producer-authority and raw-store
reviews when wired; comments/types alone do not establish authentication.

## Other delta checks

- `reusable_source_without_workflow_call_is_rejected` remains covered.
- `manifest_job_target_must_match_workload_map` remains covered.
- `continue-on-error` is now rejected for jobs and steps.
- Conditional trigger filters are rejected rather than approximated.
- Prior c032 artifact residuals (request `response_raw_ref` equality and
  parsing/binding typed artifact identity to verified response bytes) remain
  outside this delta and unresolved.

Bounded checker rereview: pass for the 27eb matrix blocker and formatting;
no live-authority or producer-integration approval.
