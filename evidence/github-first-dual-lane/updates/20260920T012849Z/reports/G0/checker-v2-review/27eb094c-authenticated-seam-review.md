# Exact checker review: 27eb094ccd545b642206ea3d52336e0ac74d6abd

## Bounded verdict

The new authority seam is fail-closed as shipped: no public CLI invocation can
turn caller JSON/CAS into trusted live evidence because
`current_collector()` always returns `UnavailableCollector`, and the collector
fails before reading any caller path. The private capture fields and producer
adapter constructors prevent direct deserialization at the command boundary.

This commit is not ready for integration approval. Exact `cargo fmt --check`
fails, and a source-derived reusable child matrix with two same-target
instances is accepted structurally: the child plan is reduced to its first job
before the source-job multiplicity check sees it. This is a real source-plan
completeness blocker independent of the currently closed live path.

This is a bounded exact-source review only; no G0/G3 or producer approval.

## Exact source and checks

- Detached tree: `/private/tmp/velnor-checker-review-27eb`
- Exact commit: `27eb094ccd545b642206ea3d52336e0ac74d6abd`
- Parent: `54c9b1f123852f48caebd5afee806c1b4da60adb`
- Product source was not edited.

Checks against the exact tree:

- `cargo test --locked --all-features --package velnor-tools -- --nocapture` — **254 passed**.
- `cargo clippy --locked --all-features --package velnor-tools --all-targets -- -D warnings` — pass.
- `cargo build --locked --all-features --package velnor-tools --bin velnor-tools` — pass.
- `git diff --check 54c9b1f1..27eb094c` — pass.
- `cargo fmt --all -- --check` — **failed**. rustfmt reports changes in
  `evidence_check.rs` around `check_paths_live` and the expected-job target
  guard, and module ordering in `main.rs`.

Targeted authority/workflow tests passed: 6 passed, 248 filtered, including
the unavailable collector, caller-authored live rejection, missing
`workflow_call`, manifest workload-target mismatch, and root matrix-collapse
checks.

## Live authority boundary

Public harness result:

`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/a2bca6e-public-cli-harness/27eb-harness-results.json`

The exact 27eb binary was run against the synthetic self-authored artifact,
source, digest, and CAS cases. All offline cases failed with
`offline-validation-only`. `--live` failed before caller-file access with:

`trusted authenticated collector/current-API reconciliation is unavailable:
producer capability velnor.authenticated-closing/v1 is not wired`

A direct `--stage G7 --live` invocation with nonexistent manifest/snapshot/
evidence paths produced the same unavailable-capability error. Thus there is
no current user-mode or self-attested promotion path.

The seam is appropriately typed for the current closed state: capture fields
are private, `from_producer` is not public outside the crate parent, and the
only installed collector is `UnavailableCollector`. When an adapter is added,
its review must still prove that it never treats the caller-supplied
`reviewed_manifest` path as authority, and that authenticated viewer/scopes,
request/page bindings, closing revisions, source-derived plans, and measured
raw-store bytes are producer-controlled before constructing the capture. The
interface comments are not that proof.

## Reusable child matrix blocker

Probe script and exact result:

- Script:
  `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/27eb-reusable-matrix-probe/run_probe.py`
- Result:
  `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/checker-v2-review/27eb-reusable-matrix-probe/result.json`

The synthetic fixture is otherwise complete, has recomputed outer snapshot
bytes/digest/CAS, and contacts no network. The root workflow uses a pinned
local reusable workflow; the child declares `workflow_call` and contains:

```yaml
strategy:
  matrix:
    os: [ubuntu-24.04, ubuntu-22.04]
runs-on: ${{ matrix.os }}
```

Both finite child instances resolve to the same `github/linux/amd64` target.
The manifest, source-job row, child graph node/edge, raw objects, and CAS are
all updated consistently. The public CLI returned exactly one finding:
`offline-validation-only`; it did **not** return `g0-source-job-matrix`,
`g0-workflow-plan`, or a child-plan finding.

Source cause:

- `derive_source` recursively derives `child_plan`, checks only whether child
  instances have differing provider/platform/architecture, then selects
  `child_plan.jobs.first()` and emits one parent job.
- `check_g0_source_jobs` checks duplicate logical IDs only in the root
  `DerivedWorkflowPlan`; child jobs are not present there.
- Same-target child matrix instances therefore disappear before the
  multiplicity guard. Different-target instances happen to fail the existing
  target comparison, but same-target multiplicity is silently accepted.

Required correction: preserve concrete matrix assignment identity through
recursive child obligations/source-job rows, or fail closed whenever a child
plan has more than one matrix instance before selecting a representative.
The reviewed plan and dependency graph must enumerate those instances or
explicitly reject the reusable workflow; never collapse them to the first
child row.

## Other 27eb checks

- `reusable_source_without_workflow_call_is_rejected` passes, so a job-level
  `uses` source without `on.workflow_call` fails closed.
- `manifest_job_target_must_match_workload_map` passes.
- The public harness's source-job target substitution still emits
  `g0-source-job-plan` and `g0-source-job-derivation`.
- c032 artifact exact-run/request binding remains present; its previously
  reported response-ref and response-content residuals remain unresolved.

No approval is issued until rustfmt and the recursive matrix contract are
fixed and re-reviewed at a new exact commit.
