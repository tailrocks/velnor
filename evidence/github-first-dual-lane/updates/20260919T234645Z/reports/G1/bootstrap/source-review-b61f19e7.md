# Bootstrap source checkpoint review: `b61f19e7f45358de56a77ffd18c25dce38756e5d`

- Review timestamp: 2026-09-19T23:27:20Z
- Review tree: detached clean worktree `/private/tmp/g0-bootstrap-b61-review`
- Branch identity: remote `codex/g1-bootstrap-isolation` was equal to the reviewed full SHA at review time; owner WIP was not read.
- Comparison baseline: approved design commit `de1e63bcd48e7f03156160f0eda5ea4089985510` and prior source review `source-review-5552cb8`.
- Scope: typed candidate transport, API acquisition/provenance, job isolation, manifest/handoff contract, and removal of legacy candidate paths.
- This is a source-checkpoint review only. It is not G1 approval, hosted acceptance, a hostile canary, or a release/merge approval.

## Verification

- `git diff --check b61f19e7^ b61f19e7`: PASS.
- `cargo fmt --all -- --check`: PASS.
- Focused generator assertions: PASS (1 each):
  - `s2::policy::tests::owner_entrypoint_renders_the_isolated_candidate_transport`
  - `s2::primitives::ir::tests::owner_pull_request_renders_direct_candidate_producer`
- Full package test: FAIL, `1735 passed; 1 failed`; the existing checked-in regeneration gate reports `ci-unit-docs.yml drifted from the generator` at `crates/velnor-workflow/src/s2/mod.rs:16429`.
- No hostile binary/probe, hosted canary, or Docker execution was run.

## Blocking findings

1. **Candidate product is deleted before upload (P0).** The producer creates the upload directory at `crates/velnor-workflow/src/s2/primitives/ir.rs:3169-3176`, installs the binary and manifest at `3208-3223`, and registers an EXIT trap at `3178-3185` that removes the same directory. The later upload step at `3224-3231` points at that now-deleted directory. A successful build therefore cannot publish the product.

2. **Service digest is not bound to downloaded bytes (P0).** Acquisition computes the REST service digest and raw ZIP hash at `crates/velnor-workflow/src/s2/mod.rs:4679-4682` but only asserts that both are nonempty. Result/handoff archives are likewise only checked for nonempty raw hashes at `5158-5159` and `5204-5205`. Producer verification compares its raw hash only to the handoff claim at `5261`, not to the REST artifact digest. A transport substitution can therefore survive the checks.

3. **Source identity is local-checkout authority, not independent API proof (P0).** Acquisition builds `source.tar` and closure from local `git archive`/`git ls-tree` at `4724-4732`; final verification repeats local tree derivation at `5304-5312`. The checkpoint has no target commit/tree REST proof, fresh exact-object database, hooks/tags-disabled fetch, independent source archive, or byte-for-byte archive comparison required by the approved design.

4. **Workflow contract remains PR-controllable (P0).** The acquire role hashes and greps only `git show "$BASE_SHA:.github/workflows/ci-pr.yml"` at `4618-4636`. It does not acquire and normalize the target head workflow, reject head-side alternate uploaders, or prove action archive bytes. Thus a PR can alter the producer/upload implementation while retaining the checked subset and names.

## Additional acceptance gaps

5. **Job attempt is not bound.** Run selection records `run_attempt` (`4648-4649`), but the jobs query at `4651-4654` does not require the job's attempt to equal the selected run. Final job recheck at `5299-5303` also omits this equality.

6. **Artifact freshness is under-specified.** Artifact selection at `4659-4662` checks `expired == false`, size, name, and run ID, but not creation/update timestamps, retention bounds, or stale/duplicate temporal consistency required by the approved design.

7. **Legacy manifest alias remains active.** The aggregate non-candidate path still emits `--candidate-manifest` at `crates/velnor-workflow/src/s2/mod.rs:5442-5446`; the policy implementation keeps the flag and environment fallback at `crates/velnor-workflow/src/s2/policy.rs:32-38,208-216,321-330`. The candidate-graph string test only proves the owner graph replacement, not global legacy-path removal.

8. **Candidate execute boundary diverges from approved contract.** At `crates/velnor-workflow/src/s2/mod.rs:4944-4953` it uses host-derived `uid:gid`, sets a hostname, and passes `HOSTNAME` in the container environment; the approved design requires fixed `65532:65532` and the exact seven-variable allowlist without `HOSTNAME`. No runtime hostile test establishes equivalent isolation.

9. **Builder target path is not deterministic.** The producer grants writable `/target` but invokes `cargo build` without `CARGO_TARGET_DIR=/target` (`ir.rs:3177`). The default target path is `/src/target`, under the read-only source mount, unless an ambient image environment happens to alter Cargo behavior.

10. **Producer output surface is not checked.** After `docker cp`, the producer only asserts `test -f` and hashes/chmods the binary (`ir.rs:3208-3211`). It does not reject symlinks, special files, or extra output members before packaging.

11. **Current tests can go green while these gates are false.** The added tests in `s2/policy/tests.rs:589-708` and `s2/primitives/ir.rs:797-849` assert generated-string fragments. They do not exercise API fixtures for pagination, duplicate/stale artifacts, run attempts, service/raw digest mismatch, PR workflow substitution, fresh source objects, or archive traversal.

## Verdict

**REJECT as a G1 source checkpoint; retain as partial implementation only.** The four P0 defects independently block readiness. Existing isolation controls are substantial and the focused rendering tests pass, but they do not establish typed artifact provenance, source authority, or an executable hostile boundary. Owner handoff: fix the P0 transport/source/contract gates and add adversarial fixture coverage before another exact-commit review; keep hosted/G1 approval blocked.
