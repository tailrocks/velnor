# Independent bootstrap source/security review — `b981f43e8dfd70b4c628d29b0e7e9dce679ce537`

- Observed: `2026-09-20T00:41:53Z`.
- Exact review tree: `/private/tmp/velnor-g1-bootstrap`, branch `codex/g1-bootstrap-isolation`, clean at `b981f43e8dfd70b4c628d29b0e7e9dce679ce537`.
- Compared with prior checkpoint `source-review-b61f19e7` and approved bootstrap design/contract `de1e63bcd48e7f03156160f0eda5ea4089985510`.
- Scope: trusted transport, API/source identity, attempt/freshness binding, action/uploader identity, legacy paths, fixed-point/static evidence, and the requested actual-shell fixture coverage.
- This is an independent source review. It is **not** G1 approval, hosted acceptance, a hostile canary, candidate execution, Docker evidence, merge approval, or release approval.

## Verification

All bounded local checks passed:

- `rtk cargo fmt --all -- --check`
- `rtk cargo test --locked -p velnor-workflow --lib s2::policy::tests::owner_entrypoint_renders_the_isolated_candidate_transport -- --exact`
- `rtk cargo test --locked -p velnor-workflow --lib s2::primitives::ir::tests::owner_pull_request_renders_direct_candidate_producer -- --exact`
- `rtk cargo test --locked -p velnor-workflow --lib s2::tests::checked_in_workflows_match_the_generator_byte_for_byte -- --exact` (`1 passed; 1726 filtered out`)
- `rtk actionlint .github/workflows/ci-pr.yml .github/workflows/ci-policy.yml`
- `rtk git diff --check b981f43e^ b981f43e`

No candidate, Docker, hostile-runtime, hosted-canary, or full package/clippy run was performed. Generated workflow hashes at review: `ci-pr.yml=e24c71a8a87ed963f77d2e8d91df4d02ceeea8d38ae357871b13cac9ec5b69ca`; `ci-policy.yml=4d51708c15f3fe47c6fa49c2c3ae5ce18ec13480ecf63213a994c7658b51cb94`.

## Positive evidence

- Producer staging now survives container cleanup and remains present through upload: cleanup removes only the container (`.github/workflows/ci-pr.yml:167-173`), upload follows at `:219-226`, and workspace removal is after upload at `:227-229`.
- Producer, handoff, result, and re-downloaded producer ZIPs are compared against their service digests and raw downloaded bytes (`ci-policy.yml:140-144`, `:659-662`, `:719-721`). Exact member/symlink/special-file checks are present.
- Base/head candidate producer blocks are compared byte-for-byte, with one fixed uploader/name/path and pinned checkout/upload actions (`ci-policy.yml:72-95`).
- Candidate/execute boundary statically uses UID/GID `65532`, no network, read-only root/binds, private PID, dropped capabilities, no-new-privileges, bounded resources, fixed env allowlists, `/target`, and output census (`ci-pr.yml:165-188`; `ci-policy.yml:371-425`).
- Run/job/artifact queries paginate and require exactly one successful eligible result; artifact IDs are used for downloads, not names alone (`ci-policy.yml:101-144`).

## Blocking findings

1. **P1 — verifier never independently archives the API-proven head and compares source bytes.** Acquire creates `source.tar` from its fresh repo (`ci-policy.yml:186-187`). The final verifier fetches HEAD/BASE and checks root tree IDs (`:756-771`), then recomputes an `ls-tree` closure (`:795-798`), but never runs `git archive "$HEAD_SHA"` in the verifier store or compares that archive/hash to the handoff archive. Candidate execute only re-hashes the received archive (`:318-319`); final verification does the same (`:704`). This does not satisfy the approved independent source-archive byte comparison.

2. **P1 — API tree proof is root-only, not recursive/completeness-checked.** Acquire and final verification validate `/commits/{sha}` and `.commit.tree.sha` (`ci-policy.yml:55-60`, `:756-761`) and compare local `%T` (`:69-70`, `:770-771`). There is no `/git/trees/{tree_sha}?recursive=1` query and no `.truncated` rejection. Local `git ls-tree` is not an API recursive-tree proof. A required head/base/tree object and completeness gate remains absent.

3. **P1 — attempt and artifact temporal binding remain incomplete.** Selected run `run_attempt` is recorded (`ci-policy.yml:107-109`), but candidate job selection/recheck match only run ID/head/name/status (`:111-117`, `:786-791`), not job attempt to selected run attempt. Artifact selection/recheck binds name/run/ID/digest/expiry (`:119-129`, `:707-711`) but never checks artifact `created_at`/`updated_at` against run timing. The approved stale/attempt contract therefore is not proven.

4. **P1 — provenance metadata omits required schema/action/upload identity.** `handoff.json` (`ci-policy.yml:196-230`) and `result.json` (`:444-487`) omit schema version, run status/conclusion, upload-step/binding method, action ref plus raw action archive SHA-256, and measured binary/manifest digests as first-class fields. The generated actions are commit-pinned, but `rg` finds no action-archive hash/download proof or those metadata fields in the bootstrap workflow/source. Static producer block equality does not supply these missing transport records for handoff/result. This fails the approved immutable metadata/action-identity contract.

5. **P1 admission blocker — schema-1 candidate paths still execute in the crate.** S2's generated hosted verifier rejects the old flag (`crates/velnor-workflow/src/s2/policy/tests.rs:725-736`), but the non-S2 policy still parses and binds `--candidate-manifest`/`VELNOR_WORKFLOW_CANDIDATE_MANIFEST` (`crates/velnor-workflow/src/policy.rs:203-211`, `:286-325`, `:1541-1552`). The old generated path still polls a short artifact prefix and invokes `gh run download` (`crates/velnor-workflow/src/lib.rs:4804-4854`). This violates the no-legacy admission gate; it is a separate migration blocker, not evidence that the S2 static test passed.

6. **P1 test-evidence gap — no actual transport shell/API fixture helper is integrated.** `crates/velnor-workflow/tests` contains no `.sh`/`.py` transport helper. The S2 test is generated-string inspection (`crates/velnor-workflow/src/s2/policy/tests.rs:578-715`), not execution against wrong workflow/path, wrong run/job IDs or attempts, duplicate/stale/expired artifacts, service/raw digest substitutions, recursive-tree truncation, or changed-closure fixtures. Existing evidence prose/materializer files do not establish actual shell-case execution. Add the requested base-owned fixture helper and run its negative cases before any G1 claim.

## Explicit readiness blocker

Builder and sandbox image digests are intentionally empty (`crates/velnor-workflow/src/s2/mod.rs:121-132`; generated `ci-pr.yml:127-134`, `ci-policy.yml:260-321`) and fail closed. This is preferable to an unpinned runtime, but it means no candidate path can execute at this SHA. It is a readiness blocker, not a successful runtime proof.

## Verdict

**Reject as a G1 source checkpoint.** The b61 product-staging, raw service/ZIP equality, normalized PR-head producer contract, fixed UID/target/env boundary, API run/job/artifact uniqueness, and generated fixed-point gaps are materially improved and locally pass bounded static checks. Independent source archive proof, recursive API tree proof, attempt/freshness binding, complete action/upload provenance, legacy removal, actual transport fixtures, and pinned runtime products remain unresolved. Do not claim G1 approval.
