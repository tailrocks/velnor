# G1 bootstrap source review: `5552cb8`

- Review timestamp: 2026-09-19T21:58:30Z (bounded review; exact commit is the authority).
- Reviewed commit: `5552cb8baa7ddcb9fbd5d1368ffeb5b0077a2047`.
- Parent: `737c3b7ca6a1c6b09d758357b3275ba0da8fe196`.
- Commit subject: `fix(policy): keep push policy off PR artifact polling`.
- Tree: `66bb6fb0dfc23789708fe27a42e258f7824192c1`.
- Changed blobs: `s2/mod.rs` SHA-256 `ad16317d5a7a5a1bc3c502da5231060714814650575d8fe76570f0697efcc87b`; `s2/policy/tests.rs` SHA-256 `fd05552387368b303a5f7f1fba629f006b53326630caa0c4648efe9ef2699cda`; `s2/primitives/ir.rs` SHA-256 `369302062e37985a78290dc9a42319f0cf6eb299636a5464bf226d48e9db27cd`.
- Commit trailers: valid `Signed-off-by` and `Co-authored-by: Codex <codex@openai.com>`.

## Delta that passes

The new `PolicyJobSpec.acquire_pull_request_candidate` switch is threaded through all callers. The aggregate `render_policy` path passes `false` (`crates/velnor-workflow/src/s2/primitives/ir.rs:4674`), while the dedicated `render_policy_entrypoint` remains `true` (`s2/mod.rs:4882`). Thus generated push/main/nightly policy jobs select the declared runtime-product path instead of polling a PR run; the dedicated `pull_request_target` policy entrypoint retains the PR candidate path.

Targeted tests pass:

- `aggregate_owner_policy_does_not_poll_a_pull_request_run`: 1 passed;
- `policy_candidate_step_binds_manifest_to_head_and_exports_it`: 2 passed;
- `policy_acquire_same_closure_exit_requires_pin_render_match`: 2 passed;
- `candidate_render_rejects_a_binary_claiming_another_closure`: 2 passed;
- `cargo fmt --all -- --check` and `git diff --check`: pass.

The checked-in-workflow fixed-point test fails because this source-only checkpoint did not regenerate checked-in YAML (`ci-unit-docs.yml` drift). That failure is expected before regeneration, not evidence of a runnable source checkpoint.

## Critical contract failures

1. **The direct producer and policy consumer cannot rendezvous.** The producer remains static-name `velnor-workflow-candidate-linux-x64` (`s2/primitives/ir.rs:43-44,3145-3150`). The PR-target policy still polls `velnor-workflow-candidate-${head_candidate:0:16}-${RUNNER_OS}-${RUNNER_ARCH}` (`s2/mod.rs:4627-4628`). It also requires `.profile`, `.platform`, `.run_id`, `.revision`, `.closure`, and `.binary_sha256` (`s2/mod.rs:4659`), while the producer manifest emits only role/path/job/event/repository/head SHA/artifact name/binary SHA (`s2/primitives/ir.rs:3138-3143`). A regenerated workflow will fail candidate acquisition for every direct producer artifact. The passing binding test codifies this incompatible old dynamic contract, so it is false-green relative to the new producer.

2. **Artifact selection is not authoritative or stale-safe.** `policy_candidate_step` uses `per_page=5` without pagination (`s2/mod.rs:4632`), selects the first run with any non-expired matching name, does not require completed/success conclusion, run attempt, candidate job REST ID/name, exact numeric workflow identity, or exactly one eligible run/artifact (`s2/mod.rs:4635-4653`). A stale/failed/duplicate artifact can be selected. There is no API artifact-ID selection, artifact service digest, raw ZIP digest, expiry timestamp comparison, safe archive extraction, or duplicate-member rejection.

3. **Download path bypasses the required transport proof.** After only a name-existence check, the code calls `gh run download "$run_id" --name "$name"` (`s2/mod.rs:4655-4658`). This is not download by the independently selected numeric artifact ID and does not compare API service digest to raw ZIP SHA-256. Candidate JSON remains the source of several accepted fields. This violates the approved static-single-uploader/API provenance contract.

4. **Source identity is incomplete.** The PR policy compares `HEAD_REPOSITORY` to `$GITHUB_REPOSITORY` by name and uses local `git cat-file`/fetch (`s2/mod.rs:4601-4602,4623-4627`). It does not independently query numeric target/head repository IDs, the exact commit response/tree SHA, a fresh object database, or a non-truncated source tree. Closure equality is useful but cannot replace source object identity.

5. **No changed-closure/stale-artifact API adversarial test exists.** Existing closure tests exercise local fake binaries/manifests; no fixture mutates run status/conclusion/attempt, duplicate runs, duplicate artifacts, expired/stale timestamps, pagination, numeric job IDs, service/raw digests, or static producer artifact names. `policy_candidate_step_binds_manifest_to_head_and_exports_it` asserts the obsolete dynamic prefix and therefore cannot catch the producer/consumer mismatch.

## Verdict

The push-policy branch selection fix is directionally correct and its narrow negative test passes. Exact `5552cb8` is **not** G1-approved and does not establish a valid producer/runtime path. Before any gate or hosted execution, reconcile the static producer contract with policy acquisition, require independent full API/job/artifact/source identity and digest proofs, reject stale/failed/duplicate selections, replace `gh run download` with numeric-artifact safe transport, add adversarial changed-closure/stale-artifact fixtures, then regenerate and pass the workflow fixed-point test. Prior producer isolation/hostile-canary blockers remain unchanged.
