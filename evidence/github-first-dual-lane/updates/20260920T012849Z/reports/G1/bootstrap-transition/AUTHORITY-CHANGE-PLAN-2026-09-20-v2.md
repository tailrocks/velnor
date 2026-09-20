# Velnor one-use bootstrap-check authority amendment v2

Status: **approval required; no mutation performed**.

This v2 supersedes v1. It deliberately does not claim that GitHub can publish
an artifact or standard attestation for a future `refs/heads/main` ref. The
selected transition is exactly three states:

* **Tree A** is the reviewed source/generator transition. It contains no
  permanent validator-B consumer pin and no pre-main B release claim. A
  one-use external semantic check admits its exact PR head plus synthetic
  merge tree.
* **Main-B** is the first actual resulting `refs/heads/main` revision. Its
  generated main workflow publishes typed Linux-X64 B from that real main
  SHA, verifies the immutable release and binding, and only then runs the
  main policy consumer. The complete Linux-X64/Linux-ARM64/xcode-27 matrix
  still runs; no native cell is suppressed.
* **Tree B** is a separate reviewed PR that adds the immutable B source/release
  pin to normal PR/preview/release consumers. It is not folded into Tree A.

This is a proposed, narrow authority amendment to the unamended ordering
requirement: current GitHub APIs do not let a future-main workflow publish a
truthful main-bound artifact before that ref exists. The amendment replaces
only Tree-A premerge `Policy` authority with the one-use semantic check; it
does not bless candidate artifacts, old macOS, skipped jobs, or a temporary
publisher. If that amendment is not approved, G1 is blocked at the external
ordering authority, not passed by relabeling a candidate.

No source edit, workflow dispatch, ruleset mutation, App installation, release,
package publication, merge, or host operation is authorized by this document.
Every placeholder must be resolved and independently reviewed before an
operator applies the change.

Observed UTC: `2026-09-20T00:18:00Z`.

## 1. Locked current facts

At this observation, authoritative `tailrocks/velnor` main is
`d20d4d1d17590cca85b501d982cbaad70d42c641`, parent
`1048337062ea625fada1b4f7c07f2feed75f60c7`. Its latest commit routes Apple
jobs to `macos-26`; that remains forbidden by the exact `xcode-27` contract.
Fresh runs prove the race: runtime-products run `35475920678` started at
`23:21:55Z`, its Apple job ran on `macos-26`, and it completed at `23:25:24Z`;
the same push's Preview run `35475920808` failed and CI/Main run `35475920826`
failed. Transition-critical current blobs are:

| Surface | Blob | Constraint |
| --- | --- | --- |
| `.github/workflows/ci-runtime-products.yml` | `c35974071e31f1cbef495776fc5167ab8748908b` | Current matrix schedules forbidden `macos-26`; final output must use exact `xcode-27`. |
| `.github/workflows/ci-policy.yml` | `c8896e9e9bb766105fe949fa668ca15e3fd73f9c` | Base `pull_request_target` still uses legacy `0dc` candidate policy. |
| `.github/workflows/ci-main.yml` | `1bfa0ce7358d9acca59cac1eb91719b1dbe56491` | Push/dispatch embeds another legacy `Policy` job and old runtime pin. |
| `.github/workflows/ci-pr.yml` | `335bebccf33f3ab1cc27176f0f4154eb52cd9f9e` | PR child path still consumes old runtime/candidate product. |
| `.github/workflows/preview.yml` | `f0a54de82479960b858bfc446ff79d53688ffeff` | Preview jobs invoke old policy runtime. |
| `.github/workflows/release.yml` | `99c0584852dbdd4beedd04102dc42bff8e0c7232` | Stable release jobs repeat old policy-runtime setup. |
| `.github/workflows/ci-unit-rust.yml` | `3f17cdbf571e32b5c19c49f83affa47cb9800970` | Reusable Rust path still consumes old runtime/policy revision. |
| `.github/actions/setup-velnor-workflow/action.yml` | `4e48bc2694af7b3d1a969cb108234a9a92b515d3` | No typed validator consumer. |
| `.github-gen/velnor-workflow.toml` | `b2bd968a484b9786c112c931dc325071ba1dff08` | Existing old-schema config; generator pin is old and has no permanent B pin. |
| `.github/ci/.github-actions-generator-state` | `cd94b8751826154fb424414a484ad39bc320caac` | Must be regenerated and byte-stable; old state is part of generated-tree proof. |
| `.github/actionlint.yaml` | `3dfcb29e220adb41ce218c511bdffb40983e07c2` | Must remain in generated-tree/source closure; no stale or unsupported scan. |
| `crates/velnor-workflow/src/s2/policy.rs` | `7ea7ccf4adba0e41ae55763a04e9de2d5a72ce65` | `xcode-27` not admitted. |
| `crates/velnor-workflow/src/s2/config/mod.rs` | `4f1097af012453d3378f67bb465f7c66a26c79da` | Old parser rejects early `[policy.validator]`. |
| `crates/velnor-workflow/src/s2/primitives/runtime_products.rs` | `b5c72fa4c37384ae7f0ce3227d723578c519b688` | Application product still includes old macOS runner. |

The seven workflow rows are all transition consumers, not optional examples:
`ci-policy.yml`, `ci-main.yml`, `ci-pr.yml`, `preview.yml`, `release.yml`,
`ci-unit-rust.yml`, and `ci-runtime-products.yml`. A generated diff is
incomplete if any one still resolves `0dc`, a candidate product, an old
runner, or a mutable alias. `nightly.yml` is only a trigger wrapper; its
child runs and terminal conclusions must be collected separately.

Ruleset `protect-main` ID `19573071` is active and scoped to the default
branch. Required contexts are exactly `DCO`, `ci-required`, and `Policy`.
The API reports repository-role bypass actor ID 5 in `always` mode. This plan
does not use it; the audit must prove no bypass merge. `/branches/main/protection`
returned 404, so the ruleset API is authoritative.

The old checker is source `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`, binary
SHA-256 `ade826fd3a27de43cfce55acfda870493b4921770199fe0afaa653b9a96f1750`.
Historical `b5` passes old check/policy `11/11`; `[policy.validator]` is
rejected as an unknown field; an old `static_files` sidecar passes only after
`--force`; current main raw old policy fails generated-tree fixed point; the
current `macos-26` run is not an accepted replacement; PR957 `xcode-27` fails
old trusted-runner semantics. The sidecar is compatibility evidence only, not
current-main admission or authority.

Evidence inputs:

* [validator design](VALIDATOR-ONLY-DESIGN-2026-09-20.md), SHA-256
  `b8ed23d26fb260e619a4f51c849fc527c0d45b62c9af8cf0f1b746cf929a139d`;
* [validator design JSON](VALIDATOR-ONLY-DESIGN-2026-09-20.json), SHA-256
  `8e7f8c5afbb751d64eb75bda67bab6bc5b9dee313f7fddaa72af651d537abcc7`;
* [admission probe](VALIDATOR-ADMISSION-PROBE-2026-09-20.md), SHA-256
  `70039e170b154db26ad595549fa8f69e02b509e62ff9d82d9eaf34f365525591`;
* [binding audit](validator-binding-audit-2026-09-20/REPORT.md), SHA-256
  `6adfc96925cfda77d6db1329c7e3592da63f541af21d68f8c53267686270b9d8`.

The binding audit has 34 hostile fixtures but no real verifier. It cannot pass
until a real verifier executes every fixture.

The GitHub API facts that constrain the transition are explicit. The ruleset
update endpoint is a full replacement `PUT`, not a documented compare-and-set
operation; this plan assumes no ETag/CAS behavior. A non-force ref update is
documented as a fast-forward check, while the pull merge endpoint's `sha`
parameter guards the PR head, not the base. Therefore the selected operation
uses an owner-controlled exclusive lease and a preflight proof of the entire
merge sequence; it never discovers a wrong resulting main SHA and calls that
recoverable after mutation. See [ruleset update](https://docs.github.com/en/enterprise-cloud@latest/rest/repos/rules),
[git refs](https://docs.github.com/en/rest/git/refs), and
[pull merge](https://docs.github.com/en/rest/pulls/pulls).

## 2. One-use authority: semantic check only

Install one owner-controlled GitHub App named `velnor-bootstrap-authority` on
`tailrocks/velnor`. It creates exactly one temporary check context,
`Policy-bootstrap-A`, for one reviewed bootstrap PR. It does not publish
releases, write refs, merge PRs, access secrets, or bypass rulesets. The normal
protected merge remains the merge authority. It is a Tree-A admission
authority only; it is never the B publisher and never signs the permanent B
trust decision.

Required identity values are unresolved and hard-stop placeholders:

```text
APP_ID=<APP_ID>
APP_INSTALLATION_ID=<APP_INSTALLATION_ID>
APP_KEY_ID=<APP_KEY_ID>
APP_PUBLIC_KEY_SHA256=<APP_PUBLIC_KEY_SHA256>
APP_OWNER_GITHUB_ID=<OWNER_GITHUB_ID>
AUTHORITY_VERIFIER_REVISION=<40-hex-reviewed-verifier-revision>
BOOTSTRAP_PR_NUMBER=<BOOTSTRAP_PR_NUMBER>
PR_HEAD_SHA=<40-hex-pr-head>
BASE_SHA=d20d4d1d17590cca85b501d982cbaad70d42c641
SYNTHETIC_MERGE_SHA=<40-hex-synthetic-merge-sha>
EXPECTED_TREE_SHA=<40-hex-synthetic-merge-tree-sha>
MERGE_TRANSACTION_ID=<unique-immutable-id>
RESULTING_MAIN_SHA=<captured-only-from-verified-protected-merge-response>
BOOTSTRAP_LEASE_ID=<unique-signed-lease-id>
BOOTSTRAP_LEASE_EXPIRY=<UTC-expiry-before-operator-token-expiry>
LEASE_STORE_URI=<append-only-external-evidence-location>
RULESET_OPERATOR_ID=<owner-controlled-ruleset-operator-identity>
MERGE_OPERATOR_ID=<short-lived-protected-merge-operator-identity>
WATCHDOG_APP_ID=<independent-recovery-watchdog-app-id>
WATCHDOG_INSTALLATION_ID=<watchdog-installation-id>
WATCHDOG_KEY_ID=<watchdog-key-id>
OIDC_TRUST_POLICY_REVISION=<reviewed-permanent-B-trust-policy-revision>
```

App permissions, exactly:

| Permission | Level | Use | Forbidden use |
| --- | --- | --- | --- |
| Metadata | read | Repository identity. | — |
| Contents | read | Read PR/base/synthetic merge trees. | Releases, tags, ref writes. |
| Actions | read | Read existing checks/runs if needed for comparison. | Dispatch, artifact publication. |
| Checks | read/write | Create/read the one semantic check. | Claiming another workflow passed. |
| Pull requests | read | Read head/base/merge facts. | Merge or review bypass. |
| Administration | none | Ruleset is operator-mutated through existing authorized process. | App policy mutation. |
| Secrets/workflows/packages/deployments | none | No privileged source execution. | Any access. |

The transition also requires three distinct owner-controlled identities. They
are placeholders until the owner names and independently audits them:

| Identity | Exact intended permissions | Scope and prohibition |
| --- | --- | --- |
| `RULESET_OPERATOR_ID` | Repository Administration write; Metadata read. | Full ruleset replacement only under the signed lease; no contents, refs, merge, release, workflow, secret, or bypass authority. |
| `MERGE_OPERATOR_ID` | Contents write and Pull requests write; Metadata read. | One normal protected PR merge during the lease; not a ruleset bypass actor; no ruleset, release, workflow, secret, or check authority. |
| `WATCHDOG_APP_ID` | Administration write; Metadata, Contents, Checks, and Pull requests read. | Conditional ruleset recovery and observation only; no ref write, merge, release, workflow dispatch, check creation, or bypass. |

The owner must prove that `MERGE_OPERATOR_ID` is not in the ruleset's
bypass actors and that no other write-capable installation, token, scheduled
job, or human writer can update `refs/heads/main` during the lease. The
existing repository-role bypass actor `5` is not used; if it cannot be
operationally frozen and audited, the transition stops before the ruleset
change. This is an explicit exclusive freeze, not an assumed API lock.

The external lease record is append-only and signed by the repository owner.
It contains the transaction ID, repository/database ID, PR number/head/base,
synthetic merge/tree SHA, main-before SHA, ruleset-before SHA, exact temporary
ruleset SHA, all allowed actors, expiry, watchdog heartbeat, and a monotonic
state sequence (`prepared`, `app-green`, `ruleset-temporary`, `merge-requested`,
`main-observed`, `recovery`, or `closed`). No source revision records the
lease. A missing, expired, duplicated, or forked sequence blocks every write.

The App's verifier is pinned by `AUTHORITY_VERIFIER_REVISION`; it reads the PR
tree and synthetic merge tree but never executes PR-supplied workflow code or
accepts a PR-authored check result. It creates a check run on `PR_HEAD_SHA`
named exactly `Policy-bootstrap-A`, with `tested_merge_sha=SYNTHETIC_MERGE_SHA`
and `tested_tree_sha=EXPECTED_TREE_SHA` in structured output. It may conclude
success only after all semantic checks below pass.

The App check is not a release attestation and does not claim
`source_ref=refs/heads/main` before merge. It binds the candidate to
`refs/pull/<BOOTSTRAP_PR_NUMBER>/merge` plus the intended future main SHA. The
actual B release is bound to `refs/heads/main` only by the post-merge publisher.

The App's public key and verifier revision are retained in the external
attestation record for history, but they are not the permanent B trust root.
Permanent B trust is anchored independently in the reviewed
`OIDC_TRUST_POLICY_REVISION`: the exact main publisher workflow identity,
GitHub OIDC/Sigstore signer identity, predicate type, repository ID, and
`refs/heads/main` source rule. Tree B must verify that trust policy and the
live binding; it must not depend on the temporary App key. The key is revoked
only after Tree B is merged and a clean checkout proves the permanent trust
path, then its immutable public fingerprint and transition evidence are
archived outside source.

## 3. Exact candidate tree and semantic equivalence

Tree A must generate, not hand-edit, these surfaces:

* typed source/config:
  `crates/velnor-workflow/src/s2/{config/mod.rs,mod.rs,policy.rs,closure.rs,
  primitives/mod.rs,primitives/policy_validator_products.rs}` plus the
  generator's negative/fixed-point tests. Tree A must not add a consumed
  `[policy.validator]` field or a permanent B pin; the source-native B
  publisher is self-contained and does not self-depend on a B release;
  matching S2 renderer tests under `src/s2/**` and crate tests for generated
  fixed-point, closure, provider, product-identity, and hostile-input
  behavior are part of the same source-bound change;
* source/config/state and actionlint inputs:
  `.github-gen/velnor-workflow.toml`,
  `.github/ci/.github-actions-generator-state`, and
  `.github/actionlint.yaml`, plus their generated state/manifest/checksum
  sidecars;
* `.github/workflows/ci-policy-validator-products.yml`, a Linux-X64-only
  reusable main publisher for the distinct validator product, called as the
  `policy-validator-B` job from `ci-main` and emitting the required
  `Policy-bootstrap-B` check only after the real main release is verified;
* `.github/workflows/ci-policy.yml`, whose Tree-A mode is an explicitly
  non-authoritative source/structure audit when no pin-adoption input exists;
  for the one Tree-B PR it may read only that PR's typed immutable-B pin and
  verify the already-published Main-B release, emitting normal `Policy` from
  the trusted base workflow. Tree A itself contains no literal permanent B
  pin or release claim; Tree B materializes the exact pin and trust policy;
* `.github/workflows/ci-main.yml`, regenerated so its `Policy` job has a real
  `needs: [policy-validator-B]` edge and cannot start before B publication;
* `.github/actions/setup-velnor-policy-validator/action.yml`, with typed
  identity/digest/source/closure/binding checks, sourced from
  `.github-gen/sources/actions/setup-velnor-policy-validator/action.yml`; and
* `.github/workflows/ci-runtime-products.yml`, with the complete matrix:
  Linux-X64 on `ubuntu-24.04`, Linux-ARM64 on `ubuntu-24.04-arm`, and
  macOS-ARM64 on exact offered `xcode-27`.

Every other generated consumer is in the same handoff. The exact active
workflow closure is `ci-main.yml`, `ci-policy.yml`, `ci-pr.yml`,
`ci-unit-rust.yml`, `ci-unit-bun.yml`, `ci-unit-docker.yml`,
`ci-unit-docs.yml`, `ci-unit-opentofu.yml`, `maintenance.yml`, `preview.yml`,
and `release.yml`. All must contain no old `0dc` setup, candidate-manifest
lookup, `--pin-build`, or obsolete runtime identity. The generated and source
actions `.github/actions/{setup-velnor-workflow,report-velnor-ci-outcomes}`
and `.github-gen/sources/actions/{setup-velnor-workflow,report-velnor-ci-outcomes}`
are included in the same renderer/test closure; they are replaced by the
typed validator setup/report path without a compatibility alias. In Tree A
these consumers run their complete ordinary workload inventory
and an explicitly non-authoritative bootstrap audit. Main-triggered Preview,
Release, and reusable children discover the just-verified Main-B release by
the exact resulting main SHA and trusted source binding; they do not require a
literal pin and do not fail merely because Tree B has not landed. A PR without
the typed Tree-B pin remains non-authoritative and cannot merge during the
transition. In Tree B, all these consumers verify the same immutable B release
by exact source SHA, release ID, producer run/attempt, artifact/binding
digests, and live API census before any policy step. `nightly.yml` may schedule
a wrapper, but its child graph must be followed and cannot count as policy
success.

The App verifier checks the candidate generated tree and source/config for:

1. no `macos-15`, old image alias, disabled/empty native cell, skipped matrix,
   `continue-on-error`, candidate binary, `--pin-build`, mutable `latest`, or
   local fallback;
2. complete existing workload/job inventory, DCO, `ci-required`, all current
   unit/security/native checks, and no missing required child;
3. Linux-only typed B publisher with one immutable product/tag namespace and no
   application runtime asset or release alias;
4. direct `needs` dependency in `ci-main`: `Policy` cannot start until the
   reusable B publisher terminally verifies the actual main release in the
   same workflow run. This is a real job dependency, not `workflow_run`, a
   dispatch wrapper, or a check-name coincidence. The consumer checks producer
   repository/workflow/source SHA/ref/run attempt, terminal success, release
   ID/tag/target, artifact ID/digest, custom binding, and exact B
   source/closure. Failed, queued, canceled, timed-out, stale, or missing
   producer runs leave Policy absent/failing, never green;
5. preview/release/child workflows consume the same verified immutable B
   release only after Tree B, with event source SHA and producer run graph
   checked before policy. Within `ci-main`, B exposes typed outputs
   (`RESULTING_MAIN_SHA`, run/attempt, producer job/check IDs, artifact ID and
   digest, release ID, and binding digest) through the reusable-workflow output
   contract. Cross-workflow Preview/Release consumers cannot use `needs`; they
   query the Actions/release/attestation APIs by that exact source SHA and
   verify the live producer graph. They do not use `workflow_run`, manual
   dispatch, a mutable latest tag, or a check-name correlation; and
6. Tree-A admission proves that no permanent B pin is being smuggled into the
   pre-main tree. Its base `ci-policy.yml` reads one exact typed
   `.github/ci/validator-pin-adoption.json` file from the Tree-B PR, queries
   the live Main-B release/binding, and emits normal `Policy` only when the
   file's reviewed transition ID, source SHA, release ID/tag, artifact digest,
   signer, and PR/API identity all match. When that exact file is absent, the
   base-owned workflow emits only a non-required audit; it never falls back to
   `0dc`, candidate-manifest, local, or mutable inputs. Tree-B review proves
   the exact pin, trust policy, and normal `Policy` output after Main-B
   evidence exists. These are separate checks and separate revisions.

The old-schema admission probe remains separate. It tests only a dormant,
old-compatible source stage with no `[policy.validator]`, B pin, xcode label,
or candidate authority. The current raw-main drift result is recorded as a
failure, not relabeled as a pass.

The one selected old-parser-safe typed input for the unpinned publisher is the
existing generic `[[static_files]]` source mapping for the **actual publisher
workflow**, not a compatibility data sidecar:

```toml
[[static_files]]
file = ".github/workflows/ci-policy-validator-products.yml"
source = ".github-gen/sources/workflows/ci-policy-validator-products.yml"

[[static_files]]
file = ".github/actions/setup-velnor-policy-validator/action.yml"
source = ".github-gen/sources/actions/setup-velnor-policy-validator/action.yml"
```

This maps the source-owned, self-contained publisher workflow byte-for-byte
through the existing typed config field. It is not `.github/ci/policy-validator.toml`,
an alias to the application product, a candidate manifest, `--pin-build`, or a
local fallback. The generic S2 crate remains estate-neutral; the product
identity and workflow contract live in the target repository's
`.github-gen/velnor-workflow.toml` and `.github-gen/sources`.

Before any authority operation, an isolated fixture must run the real old
`0dc...` parser/check with candidate input empty and this mapping present, then
run the new generator twice from the reviewed source/config. It must record
old parser acceptance, old `--plain --check` and semantic policy conclusions,
source/output bytes, and both new-render output hashes. The known current-main
raw result remains a failure when generated state or native policy differs;
that fact cannot be renamed as old semantic approval. If the old checker rejects
the full Tree-A graph, the external `Policy-bootstrap-A` authority—not a
sidecar—must carry that narrow transition amendment.

## 4. Ruleset/check transition and protected merge

The owner must approve this exact, narrow staged context replacement. The
ruleset state is:

```text
before = {DCO, ci-required, Policy}
tree_a_temporary = {DCO, ci-required, Policy-bootstrap-A}
main_b_temporary = {DCO, ci-required, Policy-bootstrap-B}
tree_b_final = {DCO, ci-required, Policy}
```

The staged contexts are deliberately distinct. `Policy-bootstrap-A` is
created only by the approved `velnor-bootstrap-authority` App on the Tree-A PR
head/synthetic merge. `Policy-bootstrap-B` is created only by the generated
main B publisher on the actual Main-B SHA. Normal `Policy` is created by the
trusted B consumer for Tree B and later. Each ruleset entry is bound to its
own concrete integration/provider ID, source SHA, and transaction/run
evidence; a matching label from a different App or SHA is not a pass.

GitHub's ruleset update is a full replacement `PUT`; no documented ETag or
conditional update is assumed. The operator therefore uses a signed,
exclusive lease and a separate recovery watchdog. The lease is valid only if
all repository write-capable actors except the named merge operator are
frozen, the existing bypass actor 5 is not used and is operationally frozen,
and the owner has an audit-log snapshot proving no competing writer. The
watchdog independently polls the main ref and ruleset, owns the recovery key,
and expires the lease before either operator token. If any of these facts
cannot be proved, no ruleset write occurs.

The chosen protected-merge mechanism is the normal PR merge API under that
exclusive lease. It is not an atomic GitHub CAS: the API's `sha` field guards
only `PR_HEAD_SHA`. The lease is the explicit base-race guard. The operator
must prove in a disposable repository with the same ruleset shape that the
lease/freeze stops every other main writer, preserves DCO/`ci-required`/review
enforcement, and that the merge response's commit is the only allowed
resulting main revision. A pre-created direct ref update is rejected because
it can lose PR merge attribution or bypass required checks; no undocumented
ETag behavior is substituted. Failure of the disposable proof is an external
authority blocker and stops before mutation.

Exact idempotent sequence, with no step skipped:

1. Create and sign `BOOTSTRAP_LEASE_ID` with the locked current main
   `BASE_SHA`, PR head, synthetic merge/tree SHA, full ruleset-before hash,
   owner, operator identities, allowed endpoints, monotonic state sequence,
   expiry, and recovery conditions. The watchdog acknowledges the lease.
2. Refresh all facts. The App creates `Policy-bootstrap-A` only after proving
   the exact PR head, base, synthetic merge/tree, integration ID, generated
   source/config, all workload IDs, and all premerge DCO/`ci-required`/provider
   checks. Every required run, attempt, child job, and check must be terminal
   `success`, non-neutral, and non-skipped. A wrapper success is insufficient.
3. Freeze every other main writer and verify the audit snapshot. Verify the
   `Policy-bootstrap-A` check is terminal success on the exact head and contains the exact
   synthetic merge/tree binding. Set lease state `app-green`.
4. Replace the ruleset through the documented full `PUT` body, changing only
   the required Policy context to `Policy-bootstrap-A`; preserve deletion,
   non-fast-forward, pull-request/review, DCO, `ci-required`, bypass actor
   records, and every unrelated rule. Read back and hash the complete object.
   If the readback is not the exact expected temporary object, the watchdog
   performs conditional recovery only when the live hash equals that exact
   temporary hash; otherwise it refuses to overwrite and escalates.
5. Re-read main, PR head/base, lease, and ruleset immediately. The merge
   operator calls the normal protected merge endpoint with `sha=PR_HEAD_SHA`
   and `merge_method=merge`. It uses no admin bypass, force push, ref update,
   App permission, or `--admin` path. The response's `merged` flag and commit
   SHA become the immutable `RESULTING_MAIN_SHA` in the lease only after the
   pre-proven freeze; the operator immediately verifies the main ref equals
   that response and that the merge commit has exactly the locked PR/base
   ancestry. Any deviation is a failed transition, never a successful
   bootstrap or a reason to publish.
6. Main-B runs the complete generated graph. `ci-main` invokes reusable job
   `policy-validator-B`; `Policy` has a direct `needs: [policy-validator-B]`
   edge in the same workflow run. B must first publish and verify the actual
   main-bound typed release, then emit the Main-B `Policy-bootstrap-B` check.
   `Policy` verifies live producer/release/attestation bindings and runs only
   after B succeeds. There is no `workflow_run` trigger, dispatch wrapper, or
   check-name shortcut.
7. In parallel, `ci-runtime-products` runs and is censused as the exact
   application closure: `build[Linux-X64]` on `ubuntu-24.04`,
   `build[Linux-ARM64]` on `ubuntu-24.04-arm`, `build[xcode-27]` on the exact
   offered macOS runner, and its sole application `publish` job. Every run,
   attempt, job, child, artifact, manifest, architecture, and digest binds to
   `RESULTING_MAIN_SHA`; no macOS-26/15 alias is accepted.
8. Collect the exact Main-B run/attempt/job/check census for B publisher,
   `Policy`, `ci-required`, DCO, all unit/security/provider/native workloads,
   all three application cells, and every reusable child. Every required
   conclusion is terminal `success`; queued, canceled, timed-out, neutral,
   skipped, empty, stale, or wrapper-only results fail the transition.
   For event-driven Preview and Release workflows, first evaluate their
   committed `on:` predicates against Main-B. Preview is required when its
   predicate matches. The current `release.yml` `v*` tag predicate does not
   match a Main-B push; record that exact non-applicability and do not invent a
   missing Release run. Its real tag-triggered run and full child census remain
   required at the first applicable stable release, not as a skipped Main-B
   job.
9. Only after that census and the live B attestation pass does the watchdog
   mark Main-B verified. The operator changes the required context from
   `{DCO, ci-required, Policy-bootstrap-A}` to
   `{DCO, ci-required, Policy-bootstrap-B}` with a full `PUT`, using the same
   conditional live-hash rule and readback. The B check is now the sole
   temporary Main-B authority; the ordinary `Policy` result is retained as
   evidence and is B-backed through the same-run dependency.
10. After the Main-B change is read back, the owner changes the required
    context to `{DCO, ci-required, Policy}` with another full `PUT`, only after
    verifying that Main-B's normal `Policy` run consumed the just-published B
    release through its direct `needs` edge. Tree-A's base policy workflow
    supports one typed pin-adoption input from the pending Tree-B PR, so Tree B
    is not deadlocked on a workflow that exists only after merge.
11. Open Tree B as a separate reviewed PR. It adds the immutable B
    source/release pin and permanent OIDC/Sigstore trust policy to
    `ci-policy.yml`, `ci-pr.yml`, `preview.yml`, `release.yml`, and
    `ci-unit-rust.yml`; it does not alter the Main-B evidence. Its PR checks
    must consume the verified B release and pass all ordinary workloads.
12. After Tree B is merged and its resulting main checks pass, remove the
    temporary context, App installation, verifier configuration, and lease
    material according to the cleanup gates. The generated B publisher remains
    the sole validator-release publisher. The temporary App never becomes a
    second publisher or a permanent trust root.

There is no unowned policy interval: Tree-A PR authority is the explicitly
temporary App check; Main-B authority is the real main B publisher plus its
direct Policy dependency; Tree-B authority is the permanent pinned B
consumer. A successful dispatch wrapper, stale check, skipped child, or
unbound check never satisfies a context.

## 5. Main publisher and validator B contract

The Tree-A temporary admission record is a separate typed contract and is
never a validator product or permanent B substitute:

```text
kind = v.formal.bootstrap-admission.v1
provider = velnor-bootstrap-authority (approved Checks App integration)
check_name = Policy-bootstrap-A
source_ref = refs/pull/<BOOTSTRAP_PR_NUMBER>/merge
source_head = PR_HEAD_SHA
base_sha = BASE_SHA
prospective_tree = EXPECTED_TREE_SHA
integration_id = <live-check-provider-integration-id>
transaction_id = MERGE_TRANSACTION_ID
required_run_census = <all-premerge-run-attempt-child-check-IDs>
product_id = none
release_id = none
artifact_id = none
attestation_subject = none
permanent_B_pin = none
```

The admission record proves only reviewed Tree-A semantics and the protected
transition facts. It intentionally contains no GitHub Actions producer run,
job, artifact, release, or permanent source-main claim. Fabricating any of
those IDs, or treating the App check as B, fails the plan.

The base-owned Tree-B adoption path reads this exact PR file from the PR tree;
it is absent from Tree A and is not a compatibility sidecar:

```json
{
  "schema": "velnor.policy-validator-pin-adoption.v1",
  "mode": "tree-b",
  "transition_id": "<owner-reviewed-transition-id>",
  "tree_b_pr_number": "<positive-integer>",
  "main_b": {
    "source_sha": "<RESULTING_MAIN_SHA>",
    "release_id": "<nonzero-id>",
    "tag": "velnor-workflow-policy-validator-v1-<B_CLOSURE16>",
    "artifact_id": "<nonzero-id>",
    "artifact_digest": "sha256:<64-hex>",
    "binding_digest": "sha256:<64-hex>",
    "signer_workflow": ".github/workflows/ci-policy-validator-products.yml"
  }
}
```

The trusted base `ci-policy.yml` validates the PR number/file/API identity,
transition ticket, immutable B release, source SHA/ref, live producer run and
attempt, numeric job/check/artifact IDs, raw and inner digests, attestation
predicate/signer, and the exact generated Tree-B pin before emitting normal
`Policy`. If this exact file is absent, it emits only the non-required audit;
it never uses a candidate, old checker, local build, alias, or mutable tag.

The generated main publisher creates exactly:

```text
product_id = velnor-workflow-policy-validator
schema = velnor.workflow-policy-validator.v1
purpose = policy-validator
platform = Linux-X64
runner = ubuntu-24.04
profile = release
features = empty
asset = velnor-workflow-policy-validator-Linux-X64
tag = velnor-workflow-policy-validator-v1-<B_CLOSURE16>
source_ref = refs/heads/main
source_sha = actual resulting main SHA
mutable_latest = false
overwrite = false
application namespace reuse = false
```

Only the generated main publisher has release write authority. Untrusted build
and verify jobs use only `contents:read` and `actions:read`; they have no OIDC
token, attestation permission, release write, package write, or secret. A
dedicated `reserve-release`/`publish` job receives `contents:write` only for
draft creation, asset upload, and final publication. A separate `attest`
job receives `contents:read`, `actions:read`, `id-token:write`, and
`attestations:write`, but no `contents:write`; it signs only after downloading
the immutable artifact by ID/digest and after the nonzero draft release ID
exists. No source-build job holds a signing token. The publisher runs no
PR-supplied build code and has no APT/Homebrew/application publish capability.
Its generated workflow path and exact workflow commit are part of the
permanent OIDC/Sigstore trust policy. The temporary App has no release or
attestation permission.

Draft-first lifecycle:

1. Build B in the unprivileged build job, upload an immutable Actions artifact with pinned
   `actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a`, and
   record artifact ID/digest. Do not claim a release ID.
2. Fetch the raw REST ZIP. Compare REST ID/name/digest/expiry/workflow run,
   upload outputs, raw ZIP SHA/size, safe extraction, inner payload
   SHA/size/architecture, and binary self-report.
3. The isolated `reserve-release` job creates a unique draft release with exact
   tag and actual main target. Require
   nonzero `release.id`, `draft=true`, exact tag/target, and no conflicting
   draft/published release.
4. Write/sign final manifest only after the real release ID exists. Reject
   `release.id=0`, stale drafts, overwrite, mutable `latest`, or tag-only
   identity.
5. The dedicated no-contents-write `attest` job uses pinned
   `actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6` for custom
   artifact and release predicates. The artifact subject is
   `velnor-workflow-policy-validator-artifact-<ARTIFACT_ID>.zip` with digest
   `sha256:<SERVICE_ZIP_SHA256>` and predicate
   `https://tailrocks.dev/velnor/attestation/validator-artifact-binding/v1`.
   Set `create-storage-record=false`; do not grant OCI artifact metadata write.
6. The binding includes repository ID, actual main SHA/ref/tree/closure,
   workflow path/SHA, run ID/attempt/database ID, producer job/check-run IDs,
   artifact ID/name, upload outputs, raw ZIP/inner payload/binary digests and
   sizes, release ID/tag/target, signer identity, predicate type, and all
   required step conclusions. Verify live Actions run/jobs/artifact/release
   APIs and raw bytes. Names and run IDs alone are insufficient. The verifier
   must cross-check producer job -> run/attempt -> artifact -> release -> raw
   ZIP -> inner payload -> manifest -> binary and source SHA, rather than trust
   a name correlation.
7. The isolated `publish` job with `contents:write` uploads
   binary/manifest/sidecars to the draft, verifies exact asset census and
   attestations, then publishes. No other workflow publishes B.

The 34 hostile fixtures must execute against the real verifier. Structural JSON
validation alone remains incomplete.

## 6. Concrete operator runbook (placeholders block)

No command below is executable until every placeholder has a reviewed value,
the disposable protected-merge proof is attached, and the owner signs the
lease. In particular, the external admission contract must never be filled
with fabricated Actions run/job/artifact/release IDs.

Read-only preflight:

```sh
gh api repos/tailrocks/velnor/rulesets/19573071 > ruleset.before.json
gh api repos/tailrocks/velnor/pulls/<BOOTSTRAP_PR_NUMBER> > pr.before.json
gh api repos/tailrocks/velnor/git/ref/heads/main > main.before.json
gh api repos/tailrocks/velnor/commits/<PR_HEAD_SHA> > head.before.json
gh api repos/tailrocks/velnor/commits/<SYNTHETIC_MERGE_SHA> > synthetic.merge.json
gh api repos/tailrocks/velnor/commits/<SYNTHETIC_MERGE_SHA>/check-runs?per_page=100 > synthetic.checks.json
gh api repos/tailrocks/velnor/actions/runs?head_sha=<PR_HEAD_SHA>\&per_page=100 > premerge.runs.json
sha256sum ruleset.before.json pr.before.json main.before.json head.before.json synthetic.merge.json synthetic.checks.json premerge.runs.json
```

The operator must prove, and store in the signed lease:

* current main equals `BASE_SHA`; PR head/base, integration ID, synthetic
  merge/tree SHA, and every premerge run/attempt/child are exact and fresh;
* all DCO, `ci-required`, provider, unit, security, and native premerge checks
  are terminal `success`, non-neutral, and non-skipped on the intended tree;
* the App ID, installation, key fingerprint, verifier revision, check-run App
  identity, and structured binding match the approved record; the App has no
  merge, release, ref-write, secret, or bypass permission;
* the disposable proof demonstrates the exclusive freeze and normal protected
  PR merge path under the same ruleset, with no competing writer, no bypass,
  and no lost PR attribution; and
* generated Tree-A output has the complete xcode-27/Linux matrix, a real
  same-run B `needs` edge in `ci-main`, nonempty expected workloads, no old
  policy/runner/candidate path, and no permanent B pin.

Ruleset replacement is blocked until the App check, lease, and disposable
proof pass. The REST endpoint is `PUT`, not `PATCH`; the body is the complete
saved object with only the required context changed from `Policy` to
`Policy-bootstrap-A` for Tree A:

```sh
gh api --method PUT repos/tailrocks/velnor/rulesets/19573071 \
  --input ruleset.tree-a-temporary.json > ruleset.temporary.response.json
gh api repos/tailrocks/velnor/rulesets/19573071 > ruleset.temporary.after.json
sha256sum ruleset.temporary.after.json
```

The signed lease records the exact response hash. If it differs, the watchdog
may restore only when the live ruleset hash equals the exact temporary hash;
it must not submit the saved JSON over an unknown concurrent update.

After a fresh lease/main/ruleset read, the protected merge operator performs
the only main mutation:

```sh
gh api --method PUT repos/tailrocks/velnor/pulls/<BOOTSTRAP_PR_NUMBER>/merge \
  -f sha=<PR_HEAD_SHA> -f merge_method=merge > merge.response.json
```

The response must say `merged=true`. Its returned commit SHA is recorded as
`RESULTING_MAIN_SHA` only under the already-proven exclusive lease. Immediately
verify its parent order (`BASE_SHA`, `PR_HEAD_SHA`), tree, PR number, and main
ref; a mismatch is a failed transition and never an artifact/policy claim.
There is no force push, direct ref update, `--admin`, or post-hoc acceptance
of an unexpected SHA.

Post-merge verification must collect the full live graph, not labels:

```sh
gh api repos/tailrocks/velnor/git/ref/heads/main > main.after.json
gh run list --repo tailrocks/velnor --branch main --limit 100 \
  --json databaseId,headSha,workflowName,status,conclusion,attempt,event,url > main.runs.json
gh api repos/tailrocks/velnor/actions/runs/<CI_MAIN_RUN_ID>/attempts/<CI_MAIN_ATTEMPT>/jobs?per_page=100 > ci-main.jobs.json
gh api repos/tailrocks/velnor/actions/runs/<B_RUN_ID>/attempts/<B_RUN_ATTEMPT>/jobs?per_page=100 > b.jobs.json
gh api repos/tailrocks/velnor/actions/artifacts/<ARTIFACT_ID> > b.artifact.json
gh api repos/tailrocks/velnor/actions/artifacts/<ARTIFACT_ID>/zip --output service-artifact.zip
gh api repos/tailrocks/velnor/releases/<B_RELEASE_ID> > b.release.json
gh attestation verify service-artifact.zip --repo tailrocks/velnor \
  --signer-workflow tailrocks/velnor/.github/workflows/ci-policy-validator-products.yml \
  --source-ref refs/heads/main --source-digest <RESULTING_MAIN_SHA> \
  --predicate-type https://tailrocks.dev/velnor/attestation/validator-artifact-binding/v1 \
  --format json > binding-verification.json
```

The census must include the B publisher's run/attempt/database ID, producer
job/check IDs, immutable Actions artifact ID/name/digest, raw ZIP and inner
payload digests/sizes, manifest, nonzero release ID/tag/target, custom
attestation subject/predicate, and every required step conclusion. It must
also include `ci-runtime-products` run/attempt plus exactly
`build[Linux-X64]`, `build[Linux-ARM64]`, `build[xcode-27]`, and `publish`,
with each artifact/manifest/architecture bound to `RESULTING_MAIN_SHA`.
Collect equivalent run/attempt/child evidence for DCO, `ci-required`, Policy,
unit/security/provider/native workloads, applicable Preview, and reusable
children. For Release, store the evaluated trigger predicate and
`not_applicable_to_main_b` evidence when its `v*` tag condition is false; then
collect its full run/attempt/child evidence at the first applicable stable
tag. Only after every applicable result is terminal success may the
operator perform the two staged context transitions. First replace only
`Policy-bootstrap-A` with `Policy-bootstrap-B`, read back the full ruleset,
and verify the Main-B check. Then replace only `Policy-bootstrap-B` with
`Policy`, read back again, and verify that Main-B's ordinary Policy result is
B-backed. Only then may the operator open Tree B. Each replacement uses the
same live-hash lease guard; no unconditional saved-JSON restore is allowed.

## 7. Failure, rollback, and removal

| Failure | Action | Forbidden |
| --- | --- | --- |
| Placeholder, App identity, lease, merge/tree proof, or authority amendment unresolved | Abort before mutation; record exact blocker. | Guessing or fictional atomic merge/publish. |
| App semantic check fails | Leave ruleset unchanged; repair Tree A/generator and rerun review. | Calling old `Policy` green or weakening checker. |
| Ruleset `PUT` response/read-back differs | Watchdog conditionally restores only if live hash equals the exact temporary hash; otherwise freezes and escalates. | Unconditional saved-JSON restore, removing all policy checks, or bypass. |
| App dies or lease heartbeat expires after temporary ruleset | Independent watchdog enters `recovery`, reads live ruleset/main/PR, and performs the same conditional restore only if no main mutation and exact temporary hash hold. | Depending on the dead App, assuming lease expiry restored state, or clobbering concurrent changes. |
| Main ref changes during freeze or merge returns 409/not merged | Stop; do not publish. Watchdog conditionally restores if its guard holds; a reviewed forward repair is required. | Retrying against a new base, force push, or declaring the old check reusable. |
| Merge response/resulting tree/parents do not match the locked PR/base | Mark transaction failed and quarantine evidence; no B/source-main claim. | Accepting a post-mutation unexpected SHA or rewriting history. |
| B publisher has no live run/attempt/job/artifact/release/binding IDs | Keep `Policy-bootstrap-B` failed/pending; no normal Policy or Tree B. | Fabricating IDs, trusting names, candidate binary, stale release, mutable tag. |
| B artifact/release/attestation fails | Keep required bootstrap context failed/pending; repair forward on exact main. | Wrapper green, pre-main release, or App as second publisher. |
| Same-run B `needs` edge missing or Policy starts first | Fail generated contract and stop. | `workflow_run`, manual dispatch, or check-name correlation as dependency. |
| Native/workload child skipped/empty/canceled/neutral | Gate failed; rerun/fix exact main revision. | Exclusion reported as pass; old macOS fallback. |
| Cleanup cannot complete | Keep ruleset fail-closed, revoke temporary App/keys when safe, and mark G1/G7 incomplete. | Standing temporary App/check/rule or deleting evidence. |

The forward and rollback protocol is idempotent and state-bound before any
mutation:

1. `prepared` may advance only to `app-green` for the exact transaction,
   then `ruleset-tree-a` after full-object hash confirmation.
2. `merge-requested` is legal only while the lease, main-before SHA, PR
   head/base, and temporary ruleset hash still match. A duplicate request is
   rejected by transaction ID; the operator never retries against a changed
   base.
3. `main-observed` records the one merge response and exact main ref. It can
   advance to `main-b-verified` only after the full live census and attestation,
   then to `ruleset-main-b` and `ruleset-final-policy` only after each full
   ruleset readback.
4. Recovery from any earlier state is allowed only under the watchdog's
   conditional live-hash/main-ref guards. If either guard fails, it stops and
   requests owner intervention; it never overwrites a concurrent ruleset.
5. `closed` is written only after Tree B verification and removal proof. The
   same transaction ID cannot be reopened.

Before main merge, rollback is only conditional restoration of the temporary
ruleset to the exact pre-state, never a force ref operation. After main merge,
no history rewrite or old-macOS revert is allowed; recovery is a reviewed
forward commit that retains xcode-27 and the B publisher, with required
contexts left fail-closed.

After exact B-backed Main-B success:

1. complete the conditional context transitions
   `{DCO, ci-required, Policy-bootstrap-A}` ->
   `{DCO, ci-required, Policy-bootstrap-B}` ->
   `{DCO, ci-required, Policy}` and verify normal Policy is B-backed by the
   just-published Main-B release, while the literal permanent pin remains a
   Tree-B change;
2. merge the separate reviewed Tree-B immutable-pin PR under the normal
   ruleset, collecting its full PR/main run census;
3. verify permanent B trust from the reviewed GitHub OIDC/Sigstore publisher
   identity and live product binding, independent of the temporary App key;
4. remove `Policy-bootstrap-B`, the one-use App installation, temporary
   verifier configuration, lease/watchdog transition records and temporary
   secrets only after the permanent path is proven. Never remove the permanent
   B publisher or its trust policy;
5. remove the obsolete `0dc` checker, candidate product, old setup action,
   old runtime pins, old macOS aliases, and any compatibility sidecar from
   every generated consumer and source path. No legacy semantic fallback,
   alias, or deprecation period remains;
6. regenerate from a clean checkout and prove no bootstrap path, old macOS
   label, candidate fallback, empty matrix, mutable alias, or temporary
   context remains; and
7. retain only the temporary App public-key fingerprint, signed lease,
   operator audit, and immutable B evidence outside source. Revoke the
   temporary key after that evidence is sealed.

## 8. Independent approval

The prior independent review is **not approved**: report
`G1/bootstrap-transition/authority-transition-review-v1.md`, SHA-256
`df936098a97e3d1b609728b69112b7b872ce64d82ba3e70dc8a84efc76fd857b`, found
missing App/ruleset integration identity, no proven first-main barrier, no
exclusive lease/recovery protocol, incomplete check binding and child census,
and an unresolved signer/build-isolation contract. This v2 addresses those
as design requirements but does not claim that they are implemented or
reviewed. The temporary admission contract is deliberately not a permanent
validator product and has no fabricated Actions provenance.

Approval requires `g0_reviewer` and `authority_transition_review` to inspect
this exact pair, current main/ruleset facts, App permissions, check equivalence,
source/main binding, race behavior, and cleanup. The binding reviewer must run
all 34 hostile fixtures against the real verifier after implementation. The
author cannot approve. Owner approval must explicitly name the App integration
ID, verifier revision, temporary context, ruleset full-body replacement,
lease/freeze actors, watchdog recovery actor, permanent OIDC/Sigstore trust
policy, and removal actions. Approval must also attach the disposable
protected-merge proof and the exact live main B run/job/attempt/artifact/
manifest/attestation/native-child census schema.

Until those approvals exist, this document is only a proposed authority
amendment. It does not pass G1 and does not authorize source or GitHub changes.
