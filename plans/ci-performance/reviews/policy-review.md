# Policy renderer review

Scope: the dual pin/candidate policy changes in `src/policy.rs`,
`src/s2/policy.rs`, and their emitters/tests. Read-only review of the current
shared tree; no policy source was changed for this review.

## Verdict

**HOLD pending source identity repair.** The environment-slot split is the
right structural fix and the focused policy suite passed, but
`source_candidate_contract` currently binds the running executable to
`git rev-parse HEAD` and `candidate_closure_of_tree(HEAD)`. `build.rs` also
stamps `HEAD` and its closure while compiling a dirty checkout. A binary that
contains unstaged source/build-script changes can therefore receive a
self-generated manifest claiming the clean HEAD closure. The candidate receipt
is not proof that those bytes came from the audited tree. The own-repository
`--check` path must reject a dirty checkout before creating this contract (or
change the build identity to cover the worktree and prove that identity).

This is a source identity boundary, not a reason to weaken the candidate
digest or closure checks. Do not publish a dirty source candidate as an
immutable product.

## Verified design properties

- `VELNOR_WORKFLOW_PINNED_BINARY` is parsed only as the declared-pin slot.
  `VELNOR_WORKFLOW_CANDIDATE_BINARY` and
  `VELNOR_WORKFLOW_CANDIDATE_MANIFEST` are a separate paired slot; one-sided
  bindings fail at lookup.
- An explicit pin pointer is checked before self-recognition. A candidate
  cannot be promoted to the pin merely because its process reports a matching
  closure.
- Candidate manifests bind revision, candidate closure, and binary SHA-256.
  `render_with_candidate` checks the checkout HEAD and locally computed closure,
  then hashes the binary before invoking `--closure` or rendering. Wrong
  manifest, wrong head/closure, malformed receipt, missing binary, and digest
  mismatch do not execute candidate bytes.
- The generated policy job captures an absolute base validator path in
  `VELNOR_WORKFLOW_BASE_POLICY_BINARY` before the audited renderer slot is
  exported. Enforcement invokes that captured path, so a declared pin or
  candidate cannot replace the trusted validator through `PATH`.
- Producer repository, workflow, run/attempt, source, platform, profile, and
  artifact checks remain in the workflow shell. The Rust v1 parser only trusts
  the receipt fields it can bind (`revision`, `closure`, `binary_sha256`); it
  does not pretend ignored v1 metadata is authenticated.

## Required follow-up

1. Make `source_candidate_contract` fail closed when the audited checkout has
   any tracked or untracked worktree change, and add a fixture proving a dirty
   `build.rs`/source cannot produce a candidate binding. Keep the binary digest
   and HEAD closure checks.
2. Generate the policy workflow from the final clean revision and assert step
   order: trusted setup, base capture, candidate/pin acquisition, then
   enforcement through the captured base path. Run both schema implementations.
3. Run policy with a real declared-pin product and a genuinely different,
   manifest-bound candidate. Record pin render, candidate render, and malformed
   receipt outcomes separately. A passing source `--check` alone does not prove
   the CI artifact handoff.

## Validation observed

`rtk cargo test --locked -p velnor-workflow policy::tests --lib` passed 76 tests
in the current tree. The focused strict-lock config tests passed 2/2 after the
independent Mise review. Full generated output and immutable runtime acceptance
remain parent integration work.
