# G1 bootstrap implementation checkpoint

Observed: 2026-09-20 Asia/Ho_Chi_Minh. Source worktree: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-generator`, branch `codex/github-first-generator`, base `12cc87b629802c294da9840325cb21087c020df6` (PR952 source-only). Committed source-only as `122fd50a60f55c66f34bed1ab035233a0d4b5744` (`git commit -s`; Codex co-author trailer). No generated workflow files changed.

## Structural fix

- `crates/velnor-workflow/src/s2/primitives/ir.rs:3120-3194` emits one owner-only `candidate-bootstrap` aggregate job for pull-request workflows before Plan/unit/Velnor jobs. Foreign repositories and non-PR workflows emit none.
- `crates/velnor-workflow/src/s2/primitives/ir.rs:5190-5308` defines the producer: hosted `ubuntu-24.04`, `contents/actions: read`, no `needs`, base-pinned checkout of the setup action, full PR-head checkout, base pin resolution with shallow recovery/fail-closed absence, base runtime closure computation, locked debug/default-feature build, isolated Cargo state, scrubbed token/wrapper/RUSTFLAGS environment, exact head/revision/closure proof, and one-day immutable artifact upload.
- `crates/velnor-workflow/src/s2/mod.rs:4534-4646` consumes the producer independently. It filters same-repository runs by immutable repository object IDs, derives the artifact from the audited head closure, verifies run metadata and the successful `Control / Candidate bootstrap` job (`:4587-4619`), then checks the complete manifest/digest and tokenless `--closure`/`--revision` reports (`:4624-4642`). Other workflow jobs may remain queued; producer job success is the gate.
- `crates/velnor-workflow/src/s2/policy.rs:1196-1293,1506-1640` parses and checks profile/features/platform/source repository IDs/head IDs/run/job/revision/build revision/closure/digest before any candidate render. Digest verification precedes binary execution (`:1630+`).
- The old active schema-2 unit-owned `candidate_publish` flag, helper, packaging path, and tests are removed from `s2/primitives/ir.rs`; only the aggregate producer remains. `rg 'candidate_publish|candidate-base-action|unit_owns_workflow_crate|candidate_publish_steps' crates/velnor-workflow/src/s2` is empty.

## Tests

- `rtk cargo fmt --all -- --check` — pass.
- `rtk cargo clippy --locked -p velnor-workflow --lib -- -D warnings` — pass.
- `rtk cargo test --locked -p velnor-workflow --lib -- --skip checked_in_workflows_match_the_generator_byte_for_byte` — `1733 passed, 1 filtered out`.
- Focused bootstrap, policy acquire, matching-head, wrong-tree, wrong-revision, malformed-manifest tests — pass. The unskipped full suite has exactly one expected failure: `s2::tests::checked_in_workflows_match_the_generator_byte_for_byte` on stale generated `ci-unit-docs.yml`; regeneration is intentionally deferred to pin adoption.
- `rtk git diff --check` — pass.

## Integration boundary

- Schema-1 `crates/velnor-workflow/src/primitives/ir.rs` still contains the legacy unit-owned producer. This commit intentionally leaves that path and the legacy top-level policy consumer untouched; active schema-2 dispatch uses `s2/policy.rs`. A separate migration/removal must complete before claiming the repository-wide “no legacy producer” condition.
- Do not publish a runtime prerequisite before source admission. After root serializes reviewed source/config, regenerate outputs from the exact commit, publish the candidate runtime/product through the mainline producer, then adopt the generated pin and rerun clean shallow checks.

## Independent source review addendum (2026-09-20)

Reviewer checkout: `/private/tmp/g1-bootstrap-review-122fd`, detached at `122fd50a60f55c66f34bed1ab035233a0d4b5744` (parent `12cc87b629802c294da9840325cb21087c020df6`, tree `ac969f5bf9f5f124f7977da511f9d07a43d5a026`). No source or generated-output files were changed by the reviewer. Current source verdict: **REJECT gate-incomplete**.

### Material findings

- `crates/velnor-workflow/src/s2/mod.rs:4542-4543,4635-4642`: 🔴 security: the candidate executable is PR-controlled, but only its child `GH_TOKEN`/`GITHUB_TOKEN` variables are blanked; the parent shell still carries `GH_TOKEN=${{ github.token }}`, and the executable inherits GitHub command-file paths. It can inspect the parent environment (for example `/proc/$PPID/environ`), recover the read token, write `GITHUB_ENV`/`GITHUB_PATH`, or alter the checkout before the privileged policy step. Split API polling/download from untrusted execution into a token-free step/process, clear all credential/runtime variables and command-file channels for every candidate invocation, and pass only immutable data across the boundary.
- `crates/velnor-workflow/src/s2/policy.rs:1491-1498,1669-1675`: 🔴 security/correctness: candidate rendering runs with write access to `policy-checkout` and uses that same mutable tree for comparison. A malicious candidate can rewrite generated files/config during `--closure` or render; the subsequent base render then compares against the tampered files and can return `TreeComparison::Pin` before candidate validation. Execute against an immutable git-object snapshot/sandbox and compare to clean `HEAD` bytes (or fail closed on any mutation); do not let candidate code write the authoritative policy tree.
- `crates/velnor-workflow/src/s2/primitives/ir.rs:790-793`: 🔴 gate: `#[expect(clippy::too_many_lines)]` is now unfulfilled on `pull_request_owner_renders_independent_candidate_bootstrap`; `cargo clippy --locked -p velnor-workflow --lib --tests -- -D warnings` fails at line 791. Remove the stale expectation (or restore a genuinely covered lint) before source admission.

### Scope approved after fixes

The S2 design otherwise has the intended owner-only, no-`needs` producer (`ir.rs:3186-3194,5198-5307`), base-SHA setup action plus PR-head full checkout, fail-closed base-pin resolution/shallow fetch, isolated Cargo state, scrubbed producer credentials, and exact candidate closure/revision manifest. Consumer acquisition (`s2/mod.rs:4538-4642`) binds same-repository object IDs, workflow path, head SHA, run metadata, successful `Control / Candidate bootstrap` job, platform/features/profile, full manifest closure and digest; digest precedes binary probes. Full closure equality prevents a same-prefix artifact collision from being accepted. Fork, wrong-head, wrong-revision, wrong-job, wrong-workflow, and merge-SHA substitutions are rejected by the current shell gates. This is a conditional S2-source design approval only after the two security findings and lint gate are fixed; it is not merge approval.

### Exact verification

- `rtk cargo test --locked -p velnor-workflow --lib s2::policy -- --nocapture` at the detached SHA: **34 passed, 1700 filtered**.
- Source-owner exact-checkout result recorded above: `rtk cargo test --locked -p velnor-workflow --lib -- --skip checked_in_workflows_match_the_generator_byte_for_byte`: **1733 passed, 1 filtered**. A reviewer rerun under concurrent host load was SIGKILLed and is not counted as an independent pass.
- `rtk cargo fmt --all -- --check`: pass.
- `rtk git diff --check 12cc87b629802c294da9840325cb21087c020df6 HEAD`: pass.
- Unskipped full source suite at this exact SHA remains **1733 passed, 1 failed**: `s2::tests::checked_in_workflows_match_the_generator_byte_for_byte`, stale checked-in `ci-unit-docs.yml` at `crates/velnor-workflow/src/s2/mod.rs:15686`. No generated workflow outputs were regenerated or modified in this source review; do not report this as a full-suite pass.
- Active schema-1 producer remains outside this source diff: `.github/workflows/ci-pr.yml:1113`, `.github/workflows/ci-main.yml:1289`, `.github/workflows/ci-unit-rust.yml:105,560,628`, and `crates/velnor-workflow/src/primitives/ir.rs` still contain `candidate_publish`. Final migration must regenerate outputs and remove the competing legacy producer/input/helper before any repository-wide no-legacy claim.
