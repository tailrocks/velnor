# Authority-transition plan review v1

- Review UTC: 2026-09-19T23:36:55Z
- Reviewed plan: `AUTHORITY-CHANGE-PLAN-2026-09-20-v1.md`
- Plan SHA-256: `42af4b1be699ab3aa49c5e038729f95923cfbe9035725f1952fc4299dd95a64c`
- Design inputs: `VALIDATOR-ONLY-DESIGN-2026-09-20.md` and `.json`, exact hashes recorded by the plan.
- Scope: operator/App permissions, prospective-main source binding, ruleset/check cutover, native-job preservation, rollback, and cleanup.
- No source, GitHub, ruleset, App, release, or workflow mutation was performed.

## Verdict

**NOT READY / HARD BLOCKED.** The plan is materially more concrete than the prior design and correctly forbids bypasses, old macOS, candidate authority, mutable releases, and unresolved placeholders. It is still not executable as written, and no authority change is approved.

## Blocking findings

### 1. Producer cannot be dispatched with the selected App contract

The plan grants `Actions:read` and explicitly forbids `Actions:write` (`AUTHORITY-CHANGE-PLAN...md:102-116`), but the runbook says the App dispatches the exact pre-main producer (`354-365`). GitHub's workflow-dispatch endpoint requires Actions repository write permission and accepts a branch/tag `ref`, not a raw prospective merge SHA:

<https://docs.github.com/en/rest/actions/workflows#create-a-workflow-dispatch-event>

Moreover, `workflow_dispatch` only receives events when the workflow file is on the default branch:

<https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#onworkflow_dispatch>

The new producer workflow exists only in the prospective transition tree, while the tree is not yet main. Staging refs are forbidden by the plan. Therefore no real producer run/job/artifact can satisfy the required binding. Select one explicit fix before approval: a protected external builder with its own exact job/provenance contract, or an authorized Actions-write/default-branch/ref protocol with a proof that it still binds the exact prospective tree. Do not silently infer either mechanism.

### 2. Prospective-main race is detected after mutation

`PR_HEAD_SHA`, `EXPECTED_MAIN_SHA`, and `EXPECTED_TREE_SHA` remain placeholders (`125-138`). The preflight snapshots main and checks the expected merge, but the merge command only supplies the PR head SHA (`320-347`, `374-383`). No compare-and-swap base SHA, branch lease, merge-queue transaction, or API conditional version is specified. Main can move after preflight; the API may then create a different merge commit, and the plan notices only after the merge has happened. “Abort on response mismatch” is not pre-mutation safety.

The ruleset update similarly snapshots/hashes JSON but has no ETag/version conditional update or lease (`178-198`, `369-383`). A concurrent ruleset edit could be overwritten by the patch or full-JSON restore. Require a proven atomic/CAS main transaction and conditional ruleset update in a disposable repository before arming this plan. If GitHub cannot provide them, record an external blocker and do not mutate.

### 3. Required-check identity/equivalence is underspecified

The cutover replaces `{DCO, ci-required, Policy}` with `{DCO, ci-required, Policy-bootstrap-B}`, but only names are specified (`171-208`). The plan does not bind the ruleset entry to the bootstrap App's integration ID, nor define the exact check-run App/provider, `head_sha`, tested prospective tree, run attempt, and terminal conclusion readback. A different integration producing the same context name must not satisfy the gate.

The runbook also does not require a complete terminal pre-merge census for DCO/`ci-required` and every child. “Enumerate” is insufficient (`330-342`). Require exact workflow/run/attempt/job/check IDs, supplying integration, tested SHA/tree, non-neutral success, and complete expected child set before merge. Resulting-main verification similarly lists labels only (`385-390`); require the exact closure, publisher run, Linux-X64, Linux-ARM64, xcode-27 jobs, publish job, all native children, and no skipped/empty/neutral work.

### 4. Source-admission and pin-adoption sequence conflicts

The plan requires final `ci-policy.yml` to use B and calls the final tree typed/B-backed (`147-169`), yet postpones the separate typed B pin-adoption PR until after cleanup (`392-404`). This combines source admission, validator consumer adoption, and xcode-27/native transition in one pre-main PR. It does not preserve the required sequence “source admission/main merge → immutable verified B publication → separate PR957 pin adoption.” Resolve the exact tree/epoch first; no implementation may choose between these contradictory paths.

### 5. Rollback/recovery has no live failure owner

If the App dies after ruleset replacement but before merge/readback, there is no watchdog, lease expiry, or authorized recovery actor. Revoking the App cannot restore a ruleset already requiring a dead context. Full saved-JSON restore can also erase legitimate concurrent changes. Define a bounded transaction lease, conditional restore, recovery credential/owner procedure, and audit evidence; retain a meaningful required gate throughout. “Restore and stop” is a procedure, not an executable failure guarantee.

### 6. App authority is broader than the stated one-transaction scope

`Contents:write`, `Pull requests:write`, and temporary `Administration:write` are repository-wide capabilities (`102-121`); GitHub App permissions do not enforce “only release assets,” “only one PR,” or “only ruleset 19573071.” The plan needs an independently reviewed transaction state machine, immutable allowlist, conditional API writes, and audit checks proving no arbitrary ref, workflow, ruleset, bypass actor, or PR mutation. The unresolved App identity and key fields (`92-100`) are a hard stop, as the plan itself states.

### 7. Durable trust-root/removal boundary is incomplete

The App public key is kept outside source until a permanent verifier review (`308-311`), but the App is later uninstalled (`392-404`). The B consumer and post-cleanup verifier need a durable, exact public-key trust root. Define its typed source location, key ID/rotation epoch, and verification evidence before App revocation.

## Accepted constraints already present

- Exact current main and transition-critical blobs are recorded (`16-29`).
- Existing ruleset/context and bypass actor are identified (`30-35`); no bypass is authorized.
- Validator/application namespaces, Linux-X64 scope, draft-first release, artifact service/raw/inner digests, numeric job/run/artifact binding, and signed predicates are explicit (`210-286`).
- Full native matrix and no-old-macOS/no-skip obligations are explicit (`147-162`, `385-390`).
- Failure paths forbid mutable tags, old-macOS rollback, wrappers, skipped children, and parser aliases (`406-421`).
- Plan status correctly remains approval-required and mutation-free (`3-10`).

## Required readiness evidence before any mutation

1. Fill every identity/SHA/key placeholder; independently hash the final plan.
2. Demonstrate the actual pre-main producer mechanism, permissions, workflow source ref, and exact source SHA/tree binding in a disposable repository.
3. Demonstrate merge base/head compare-and-swap and ruleset conditional update; record failure behavior before production use.
4. Record ruleset required-context provider/integration IDs and a complete terminal pre-merge and resulting-main child census.
5. Resolve source-admission/main-merge/B-publication/PR957 ordering and exact permanent-vs-temporary files.
6. Demonstrate watchdog/recovery/conditional-restore and durable trust-root handling; then obtain independent authority-transition review and explicit owner approval.

Until these exist: **plan only, no source approval, no trust change, no merge, no release, no G1/G7 claim.**
