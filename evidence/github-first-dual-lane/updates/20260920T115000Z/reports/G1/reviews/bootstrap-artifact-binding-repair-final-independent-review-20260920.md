# Independent review: run-isolated artifact binding repair

Observed 2026-09-20. Read-only review. No workflow, ruleset, check, App,
branch, artifact, token, signing authority, or remote state was changed.

Reviewed proposal:

* `G1/bootstrap/artifact-binding-repair-final-20260920.md`
* SHA-256 `5a59f867177de9e53275ff9fd4da43381a7ddea7aa262609a956f34fb4587fd0`
* selected mechanism: `static-single-uploader-run-isolated-v2`

Reviewed source:

* commit `3ed0023b038335d7b22dfa2758457e3808f777ee`
* tree `45be601efb57e8d9da424a07e9115beee93a1564`
* checkout `/private/tmp/velnor-g1-bootstrap`

This is an independent design review, not an adoption, ruleset, check, merge,
publication, or execution approval.

## Verdict

Run isolation is the correct bounded direction. It removes the already-proven
same-run static-name race **if** the candidate run is genuinely a singleton
producer and the ordinary run has no delete or cross-run artifact capability.

The proposal is **not implementation-ready**. Full old behavior is not yet
preserved or proven, and several security claims depend on implementation or
provider evidence that is absent. The exact result is: **conditional design
acceptance for implementation work; proof gap blocks adoption and check/trust
cutover**.

The repair must not fall back to the current mixed run, a name/timestamp
correlation, a manifest assertion, synthetic uploader ownership, or a new
signing/OIDC authority without a separate approval.

## What the split does close

The current `.github/workflows/ci-pr.yml` has `candidate_producer` plus plan,
runtime-artifact, unit/provider, and aggregate jobs. An ordinary job can race
the static candidate artifact name: upload first and make the fixed producer
fail, or delete/recreate later and replace the producer output. The pinned
`upload-artifact` documentation confirms same-name replacement through
`overwrite: true`:

* <https://github.com/actions/upload-artifact#overwriting-an-artifact>

The proposed shape—one `candidate_producer` in `ci-pr.yml`, ordinary checks in
another workflow/run, and policy selecting `workflow_run.id`—removes that
same-run producer ambiguity. The artifact REST object has ID, name, digest, and
workflow-run fields but no uploader-job field, so the remaining ownership proof
must be structural: exactly one eligible job in the exact candidate run plus
the exact numeric artifact ID and digest comparison:

* <https://docs.github.com/en/rest/actions/artifacts>

The fixed build container is also a sound boundary for the narrower claim that
the build cannot upload at runtime: the reviewed candidate step clears runtime
token variables, uses `--network=none`, read-only source/root mounts, no
socket, dropped capabilities, and a non-root user. The fixed host-side uploader
is the only intended upload path. This remains conditional on a semantic
closed-graph contract; a post-run policy rejection is not a pre-execution
proof that PR code was never run.

## Blocking findings

### 1. Cross-run crafted backend IDs are not proven (P0)

The official artifact toolkit sends `workflowRunBackendId` and
`workflowJobRunBackendId` when creating/finalizing an artifact, and derives
those IDs from the runtime token:

* <https://github.com/actions/toolkit/blob/main/packages/artifact/src/internal/upload/upload-artifact.ts>
* <https://raw.githubusercontent.com/actions/toolkit/main/packages/artifact/src/internal/shared/util.ts>

This proves that run/job backend identity participates in the protocol. It does
not prove that the provider rejects a request made with an ordinary job's
validly signed token but candidate-run/job backend IDs. The proposal currently
states the cross-run overwrite cannot occur without provider evidence.

Required before cutover:

1. Obtain an approved hosted negative canary or provider contract evidence. An
   ordinary job must attempt candidate backend IDs; create/finalize must be
   rejected and candidate artifact `id`, digest, and bytes must remain intact.
2. Also test ordinary same-name upload, candidate artifact deletion, and
   retry/late-finalize behavior. A protocol fixture alone cannot establish
   provider enforcement.
3. If the provider accepts crafted cross-run IDs, this repair fails under the
   no-new-signing/OIDC-authority constraint. Do not rename that failure into a
   warning or add a manifest/check output as a signature.

### 2. Existing concurrency group can cancel across the new workflows (P0)

The source uses this group for `ci-pr.yml`:

```text
velnor-${{ github.repository }}-${{ github.event.pull_request.number || github.ref }}-pr-${{ github.event.pull_request.number || github.ref }}
```

It does not include the workflow name. GitHub concurrency groups are
repository-wide; equal groups in different workflows cancel one another when
`cancel-in-progress: true`:

* <https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#concurrency>

Copying the old group into `ci-pr-checks.yml` lets an ordinary run cancel the
candidate producer (or vice versa), defeating the promised isolated producer
and making the policy observe zero/failed candidate runs. This is a red result,
but not an executable valid transition.

The generated candidate and ordinary workflows need distinct, fixed,
non-PR-controlled groups. The ordinary group must preserve cancellation among
ordinary runs; the candidate group must not share it. The exact expressions and
their rendered bytes belong in the frozen generator contract and hostile
concurrency fixtures.

### 3. Required-check source and workflow collision are unspecified (P0)

The current renderer/policy assumes `.github/workflows/ci-pr.yml` is the PR
aggregate (`PULL_REQUEST_AGGREGATE`), and the source emits `ci-required` plus a
`required` mirror from that graph. Moving the ordinary graph to
`ci-pr-checks.yml` requires distinct candidate/ordinary roles in policy,
generator, config, tests, and generated state. Merely changing the path while
leaving the constants unchanged makes admission inspect the wrong workflow.

The ruleset must bind required contexts to their expected source/integration
identity, not only display text. GitHub required-status rules support a context
and an optional integration ID:

* <https://docs.github.com/en/rest/repos/rules>
* <https://docs.github.com/en/rest/branches/branch-protection>

The policy must live-read and bind the exact `ci-required`, `Control / Required`,
`Policy`, DCO, and other retained contexts (as applicable to the actual current
ruleset), including head SHA, check-run source/App/integration ID, and duplicate
source detection. A PR can add another `pull_request` workflow that emits the
same display context unless the global workflow set is scanned or the ruleset
source is constrained. Candidate-path-only scanning is insufficient.

No check/ruleset mutation is authorized in this review.

### 4. Generator cannot safely render the proposed two-role shape yet (P0)

Current source facts:

* `default_workflow_files()` lists `ci-pr.yml`, not `ci-pr-checks.yml`.
* `primitives/aggregate.rs` maps `ci-pr.yml` to the one `PullRequest` kind.
* `ir.rs::render_nested` renders plan, candidate, ordinary callers, and
  required nodes together for that kind.
* `.github/ci/project.toml` has no `ci-pr-checks.yml` workflow entry.
* candidate and policy scanners/policy constants contain hardcoded `ci-pr.yml`
  assumptions.

Adding `ci-pr-checks.yml` as a filename alias to `WorkflowKind::PullRequest`
would render another candidate producer in the ordinary run, recreating the
authority race. A safe implementation needs an explicit role/kind or dedicated
renderers:

1. candidate `ci-pr.yml`: exactly one candidate job, no plan, ordinary callers,
   runtime artifact, reusable calls, or normal required job;
2. ordinary `ci-pr-checks.yml`: old plan/unit/provider/runtime/aggregate graph,
   with the candidate removed and required display names/edges preserved; and
3. policy: candidate acquisition scans only candidate role; ordinary scanner
   separately enforces no candidate delete/cross-bind capability.

Generator output, source closure, generated state, tests, and all policy
references must be updated together. No partial alias or hand-edited generated
workflow is acceptable.

### 5. “Full normalized contract” is a requirement, not present evidence (P0)

The reviewed source's `normalized_capability_contract` is line/indent
normalization. It is not YAML-semantic normalization and the current scan is
rooted at the candidate `ci-pr.yml` graph. It cannot by itself prove effective
permissions or all reachable behavior.

The implementation must parse the complete base and head workflow graphs with
duplicate-key rejection and explicit allowlists. At minimum reject or bind:

* added jobs, matrices, services, local actions, reusable workflows, action
  pre/post hooks, dynamic artifact names, hidden upload/delete paths, and
  expression-controlled protected fields;
* top-level/job `permissions`, `env`, defaults, triggers, concurrency, runner,
  checkout/ref, action refs, and mutable/unpinned reachable action manifests;
* `read-all`/`write-all`, `actions: write`, `artifact-metadata: write`, PAT/App
  or secret transport, and equivalent delete/write capability through
  reusable callees; and
* YAML aliases/merges, duplicate keys, unknown fields, or archive members that
  evade the declared graph.

The scanner must compare effective permissions through reusable-workflow
callers/callees. A string-normalized candidate block cannot establish that
ordinary jobs cannot call the artifact delete API.

### 6. Executed workflow identity is not the same as PR-head source (P1)

For `pull_request`, GitHub evaluates the workflow from the associated merge
commit/ref and sets `GITHUB_SHA` to the merge commit; the PR head SHA is a
separate event field. The candidate checkout explicitly uses
`github.event.pull_request.head.sha`.

* <https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows>

The acquisition tuple must bind, separately and immutably:

```text
base_sha
pr_head_sha
merge/executed_sha
workflow path + workflow ID + run ID + run attempt
executed workflow blob/tree identity
candidate source tree/closure identity
repository IDs and PR number
```

Comparing only the PR-head workflow file, then trusting a run generated from a
different merge tree, is not exact-resulting-main provenance. The policy must
reject an execution/path/ref mismatch before artifact admission. The policy's
post-run check can fail closed; it cannot retroactively make a PR-controlled
workflow immutable before it ran.

### 7. Builder pin is empty in the reviewed source (P1)

The candidate build currently sets `CANDIDATE_BUILD_IMAGE_DIGEST: ""` and
fails closed unless a `sha256:<64 hex>` digest is supplied. This is a useful
fail-closed state, not execution evidence. A concrete pinned image digest,
hosted canary, platform digest, and builder closure hash remain execution
prerequisites. Do not treat the proposal's fixed-container description as a
live builder proof.

## Direct claim check

| Claim | Independent result |
| --- | --- |
| PR-controlled workflow cannot alter the admitted normalized contract | **Not proven.** Base/head closed-graph semantic scanner and executed-workflow identity are still unimplemented. A post-run rejection is fail-closed admission, not pre-dispatch prevention. |
| Candidate build/output cannot runtime-upload | **Conditionally sound.** Reviewed container flags and cleared token env support it; only a complete immutable reachable graph plus pinned builder can make the claim. Host uploader remains intentional. |
| Ordinary token cannot cross-run overwrite by crafted backend IDs | **Unproven provider property.** Toolkit carries backend IDs, but public docs/source inspected do not establish server rejection of mismatched IDs. Requires approved live negative canary/provider evidence. |
| Ordinary token cannot delete candidate artifact | **Contractually enforceable, not yet shown.** Effective permissions through all reusable callees must exclude `actions: write`, `artifact-metadata: write`, PAT/App/secret paths; live read/delete behavior still needs proof. |
| Old CI behavior survives the split | **Not shown.** Generator and policy are currently single-file assumptions; graph/dispatch/check/concurrency equivalence fixtures are absent. |

## Old-behavior preservation matrix

Before any adoption proposal, compare the old ordinary subgraph with
`ci-pr-checks.yml`, not merely rendered text:

* `pull_request` activity and `workflow_dispatch` inputs (`providers`, `scope`,
  `base_sha`), defaults, and trusted provider admission;
* plan outputs/digest and every unit/provider caller, job ID, display name,
  `needs`, `if`, reusable-workflow inputs, and terminal-result handling;
* runtime artifact publication, artifact names, action pins, retention, and
  cleanup behavior;
* `ci-required`, `required`, DCO, Policy, and all retained check contexts,
  including success/neutral/skipped semantics;
* ordinary-run concurrency/cancel behavior and the new candidate group's
  noncollision; and
* no ordinary `needs` or output dependency on candidate, and no candidate
  path that can publish ordinary runtime artifacts.

The candidate path must not retain manual dispatch, plan, unit/provider,
runtime, or normal required jobs. `ci-policy` remains a trusted
`pull_request_target` workflow and must not execute PR bytes.

## Required evidence before separate approval

1. Freeze generator/source commit, generated candidate/checks bytes, reachable
   action manifests, and all base/head/tree/closure hashes.
2. Implement the semantic closed-graph/effective-permission scanner and run
   positive plus hostile fixtures: duplicate keys, aliases, unknown keys,
   added no-upload job, local/reusable uploader, dynamic name, changed trigger,
   changed concurrency, runtime upload, write permissions, PAT/App/secret, and
   arbitrary workflow check-name spoof.
3. Generate a positive ordinary workflow and machine-compare its graph with the
   old `ci-pr.yml` ordinary graph. Include a two-run artifact fixture where
   ordinary and candidate use the same name but distinct run/backend IDs.
4. Obtain the provider/live negative canary for crafted ordinary backend IDs,
   same-name upload, delete, and late-finalize/retry. Preserve candidate bytes
   and digest on every rejected attempt.
5. Live-read ruleset/protection/check-run data and bind required contexts to
   exact source/integration identity, head SHA, workflow path/run, and no
   duplicate source. Do not mutate these controls in this phase.
6. Supply the real builder digest and separately approved hosted isolation
   canary. Current empty digest must continue to fail closed.
7. Obtain explicit user/root approval for generator merge, workflow shape,
   ruleset/check cutover, and any future signing/OIDC authority. This review
   authorizes none of them.

## Final security disposition

`static-single-uploader-run-isolated-v2` is a defensible implementation target
under the accepted no-new-authority constraints. It is not a green gate and
not ready for implementation adoption until findings 1–5 are closed with
immutable generated output and provider evidence. If cross-run backend-ID
binding cannot be proved or enforced, the selected bounded repair has no safe
fallback under current constraints; retain the gate rather than restoring the
known-racy mixed workflow.
