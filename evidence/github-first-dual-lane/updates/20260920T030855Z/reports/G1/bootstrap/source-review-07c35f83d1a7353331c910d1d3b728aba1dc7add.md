# Independent legacy-removal review — `07c35f83d1a7353331c910d1d3b728aba1dc7add`

**Verdict: CHANGES REQUIRED. No G1 approval.**

Bounded source-only review of the exact delta from `7d409afdd61a87080be4439d29563313169537`. Namespace and hosted-canary findings from the prior exact review remain open.

## Frozen provenance

- Branch: `codex/g1-bootstrap-isolation`
- Remote/local exact SHA at freeze: `07c35f83d1a7353331c910d1d3b728aba1dc7add`
- Detached clean worktree: `/private/tmp/g1-bootstrap-isolation-review-07c35f83`
- Delta: only `crates/velnor-workflow/src/primitives/ir.rs` (`+13/-551`); no generated workflow or S2 transport source changed.
- No source, generated workflow, branch, remote, GitHub, publication, or installation state was changed by this review. This report is the only external write.

## What the delta fixes

The old generic unit-job producer surface is removed from `src/primitives/ir.rs`. Its local regression test `legacy_candidate_surface_is_absent` passes and proves the rendered unit-kind fixture no longer emits `candidate_publish`, the old candidate publisher text, or the old manifest environment. The current S2 transport regression also remains green:

- `rtk cargo test --locked -p velnor-workflow --test bootstrap_transport`: **4 passed**.
- `rtk cargo test --locked -p velnor-workflow --lib primitives::ir::tests::legacy_candidate_surface_is_absent -- --exact`: **1 passed**.

This is producer-side removal only. It is not a complete legacy migration.

## Blocking finding

### P1 — Old schema-1 policy/API/source transport remains reachable

The removed producer does not remove the old consumer or dispatch path:

- `src/s2/dispatch.rs:3-11,36-45,84-101,205-231` still routes schema-1, missing-config, and remote targets to the schema-1 pipeline. The tests explicitly preserve those routes.
- `src/policy.rs:203-211,244-325,1122-1144,1250-1292` still accepts `--candidate-manifest` and `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` and loads the legacy candidate manifest.
- `src/lib.rs:120-127,4751-4854,4970-4973` still renders and invokes `policy_candidate_step`. Its acquisition loop at `src/lib.rs:4813-4840` polls only `per_page=5` without pagination, selects a run when a matching name exists, uses the dynamic prefix namespace, and downloads by `gh run download --name`. It has no exact run-attempt/job-ID/artifact-ID/service-digest/raw-ZIP/source-tree contract and no `--no-replace-objects` fresh-source proof.

The producer removal strands the generated old lane, so normal generated schema-1 output may now fail to rendezvous. That is fail-closed for that path but not migration completion: an old or hand-written producer can still publish the expected namespace, and the reachable consumer still applies the weaker API/source checks. Remove the old consumer/manifest/dispatch paths or migrate them to the exact S2 transport contract; do not rely on producer disappearance as a security gate. The narrow rendered-kind test is not a global source reachability proof.

## Unchanged findings retained from `7d409af…`

1. The S2 namespace census still parses only named `- name:` step blocks and only follows local `./.github/workflows/...` reusable edges (`src/s2/mod.rs:4650-4706`). It still misses an unnamed direct `- uses: actions/upload-artifact` step and external reusable workflow publishers. The delta does not touch this code. Named extra/dynamic uploader fixtures remain useful but do not close those forms.
2. No hosted Linux canary, real GitHub API/action archive, independent remote object proof, final image/config digest proof, hostile probe, actual Docker daemon, or Mac-host Docker/runtime execution was performed. Existing transport tests use fake local `gh`, `curl`, `git`, and `docker` commands. No G1 claim follows.

## Clippy classification

`rtk cargo clippy --locked -p velnor-workflow --lib --tests -- -D warnings` fails with exactly six errors. All are pre-existing to this delta: `07c35` changes only non-S2 `src/primitives/ir.rs`, while every diagnostic is in untouched S2/policy code.

| Location | Diagnostic | Delta classification |
|---|---|---|
| `src/s2/policy.rs:1326` | `too_many_arguments` (8/7) | pre-existing; untouched |
| `src/s2/primitives/ir.rs:3138` | `too_many_lines` (140/100) | pre-existing; untouched |
| `src/s2/mod.rs:4794` | `too_many_lines` (277/100) | pre-existing; untouched |
| `src/s2/mod.rs:5086` | `too_many_lines` (314/100) | pre-existing; untouched |
| `src/s2/mod.rs:5441` | `too_many_lines` (362/100) | pre-existing; untouched |
| `src/s2/mod.rs:5826` | `too_many_lines` (117/100) | pre-existing; untouched |

The clippy gate remains red even though this exact legacy-removal delta introduces none of the six diagnostics.

## Other checks

- `rtk cargo fmt --all -- --check` — pass.
- `rtk actionlint .github/workflows/ci-pr.yml .github/workflows/ci-policy.yml` — pass.
- `rtk git diff --check 7d409afdd61a87080be4439d29563313169537..07c35f83d1a7353331c910d1d3b728aba1dc7add` — pass.

No hostile binary/probe, real Docker, network/API request, hosted run, or Mac runtime was executed.
