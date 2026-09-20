# Velnor bootstrap authority amendment v7

Status: **approval required; no mutation performed**.

This is the immutable successor to v6
(`AUTHORITY-CHANGE-PLAN-2026-09-20-v6.md`, Markdown SHA-256
`4aea2c28eb06f21e094adad1b4b0dfe8a8d6918d1cbe18c3a2befb77c849e593`, JSON
SHA-256 `556eb0485a18fe9abb91c132ff2a0cddd21e6eb3f549f66ebd90dbdf9da89947`).
v6 remains frozen historical evidence; it is not an approval record. v7
incorporates the independent v6 findings and preserves the current-main
census:

1. **Tree A:** source/generator transition. It has no `[policy.validator]`,
   permanent B pin, candidate path, old checker, old setup/runtime identity,
   or old macOS path. One external App check (`Policy-bootstrap-A`) admits the
   exact PR head plus synthetic merge tree. It is not a product and carries no
   Actions artifact/run/release provenance.
2. **Main B:** the first actual resulting `refs/heads/main` revision. Its
   main-push `ci-main` caller invokes the source-owned B publisher through
   `workflow_call`; the called jobs publish typed Linux-X64 B from the real
   main SHA, return declared `on.workflow_call.outputs`; an external
   integration-bound Checks App emits `Policy-bootstrap-B`. The caller's `policy-validator-B` is one `uses:` job
   with no job-level `outputs:` declaration; `Policy` consumes the called
   outputs directly through `needs.policy-validator-B.outputs.*`. There is no
   standalone B push run. The full
   Linux-X64/Linux-ARM64/xcode-27 application closure runs.
3. **Tree B:** a separate reviewed PR. The trusted base policy workflow reads
   one exact typed adoption file from this PR and validates the already-live
   Main-B binding before emitting normal `Policy`. The PR then materializes the
   immutable B pin and permanent OIDC/Sigstore trust policy.

This is a narrow proposed amendment to ordering authority, not a claim that a
future main ref can publish before it exists. The fresh current main is
`325719f1e05d3d46322c9fd3eeb9ad545e175638`; its hosted evidence is negative.
If the owner does not approve the temporary Tree-A authority and its proven
freeze coordinator, G1 remains blocked. No candidate, old macOS, skipped job,
mutable tag, pseudo-workflow output, same-name check, or compatibility fallback
is accepted.

No source edit, workflow dispatch, App installation, ruleset update, release,
package publication, merge, or host operation is authorized by this document.
All placeholders and independent reviews are hard gates.

Observed UTC: `2026-09-20T01:28:35Z`.

## 1. Revision-bound facts and complete affected closure

At the fresh read-only census observed at `2026-09-20T01:28:35Z` UTC,
`tailrocks/velnor` main is
`325719f1e05d3d46322c9fd3eeb9ad545e175638`, parent
`e94b48406c4ed206fce2bbf39b788264e72cf39c`, tree
`e9019f00578d34c3f339c7e8d55b66f7e53f8567`. It is PR965's merge result
(source `6b48f8fff2f4943dbf79c21c3274caecdf77bdd5`). Runtime-products run
`35481089522` technically succeeded but scheduled job `105998930189` on
forbidden `macos-26`; Preview run `35481089629` failed `Resolve preview
identity`; CI/Main run `35481089696` was still in progress at capture. These
are negative or incomplete facts, not accepted evidence.

The current generator emission revision is `54`; the older trusted policy
runtime pin remains `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`. Tree A must
record a different, reviewed immutable target revision if S2 changes; it must
never claim that old 0dc admitted the new graph. A missing reviewed target,
unpublished local binary, or mutable revision is a hard stop.

The canonical read-only Tree-A closure inventory is historical, observed at
`2026-09-20T00:27:10Z` against d20 (not current main), main
`d20d4d1d17590cca85b501d982cbaad70d42c641`, tree
`ec2d82aa83a1b2db692d1d3d46d48568d756c602`:

* `G0/authority-tree-a/old0dc-closure-inventory.md`, SHA-256
  `90314d2c32203e06012feb33d149ce864c29ba856a73c8f9b1bc5146b69b7eae`;
* `G0/authority-tree-a/old0dc-closure-inventory.json`, SHA-256
  `97f71e70ffa8f974e0abd9b382696e713f26e88645ef468fa43ad3dd3b8d4264`.

It found 82 active old-renderer hits in 12 files. Zero literal hits for the
old closure/tag are not clean evidence: setup derives the runtime tag from
the pinned revision closure. Final acceptance therefore recomputes the actual
generator source closure, runtime closure, derived tag, product manifest, and
recursive job/action graph from the final config and reviewed generator
revision. It must prove that the old pin, runtime provenance, and candidate
producer/consumer contract are gone—not merely that forbidden strings are
absent. The inventory records 35 reusable calls from each of main and PR
across the five `ci-unit-*` workflows; those edges and their outputs are part
of the graph proof.

Current-main reconciliation for v7:

* Current commit: `325719f1e05d3d46322c9fd3eeb9ad545e175638`, parent
  `e94b48406c4ed206fce2bbf39b788264e72cf39c`, tree
  `e9019f00578d34c3f339c7e8d55b66f7e53f8567`.
* PR965 source: `6b48f8fff2f4943dbf79c21c3274caecdf77bdd5`; its exact change is
  generator emission `53 -> 54` plus structural verified-consumer updates.
* Changed current blobs: generator state
  `.github/ci/.github-actions-generator-state` =
  `c5c83cbe212beade20a6054a591e8c52038c0145`; S2 renderer
  `crates/velnor-workflow/src/s2/mod.rs` =
  `5f3001a4447527af685a93af967f670b4633da4e`; package-release primitive =
  `0a3387910d2e7bce5553f5452850885e55fd34e9`. Critical runtime workflow
  remains `c35974071e31f1cbef495776fc5167ab8748908b`.
* The same census records Runtime products `35481089522` / macOS job
  `105998930189` on `macos-26` and Preview `35481089629` identity failure.
  The old Mac label is forbidden; no absence of macos-15 repairs it.
* The e94 delta is historical supporting evidence:
  `G1/bootstrap-transition/TYPED-PUBLISHER-INPUT-FEASIBILITY-DELTA-E94-2026-09-20.md`
  SHA-256 `cdb5a5bca2820d88bbf3ff8ab80a9249814be9ed639b9e0d9467322ee40baf72`
  and JSON SHA-256
  `3a05e2bd8c1348ed808ca85053de7f7d93b74b99df201d8b331552c095cafcf6`.
  It proves `verify_tasks` is rejected by old 0dc before render and that e94
  adds no B namespace, producer, provenance, or handoff.

Transition-critical d20 blobs:

| Path | Blob | Required Tree-A disposition |
| --- | --- | --- |
| `.github-gen/velnor-workflow.toml` | `b2bd968a484b9786c112c931dc325071ba1dff08` | Preserve old strict config shape and record current generator pin `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`; add only neutral typed inputs accepted by the generic engine. No `[policy.validator]` or literal B pin. |
| `.github-gen/sources/workflows/ci-policy-validator-products.yml` | `absent-at-d20` | New source-owned Linux-X64 B publisher; its post-implementation blob is required in closure and rendered byte-stability checks. |
| `.github/ci/.github-actions-generator-state` | `cd94b8751826154fb424414a484ad39bc320caac` | Regenerate; exact state is part of fixed-point proof. |
| `.github/actionlint.yaml` | `3dfcb29e220adb41ce218c511bdffb40983e07c2` | Regenerate and validate; no stale unsupported scan. |
| `.github/workflows/ci-main.yml` | `1bfa0ce7358d9acca59cac1eb91719b1dbe56491` | Replace old policy/candidate path with Main-B publisher → verifier → Policy. |
| `.github/workflows/ci-policy.yml` | `c8896e9e9bb766105fe949fa668ca15e3fd73f9c` | Base-owned audit/adoption verifier; no old checker or candidate path. |
| `.github/workflows/ci-pr.yml` | `335bebccf33f3ab1cc27176f0f4154eb52cd9f9e` | Full ordinary PR workload; no old policy/runtime/candidate path. |
| `.github/workflows/ci-unit-rust.yml` | `3f17cdbf571e32b5c19c49f83affa47cb9800970` | Full Rust child; no old policy/runtime/candidate path. |
| `.github/workflows/ci-unit-bun.yml` | `937d68cb35aaebbeafe6581269a97072e728ef35` | Ordinary Bun checks; preserve job identity and remove old runtime references. |
| `.github/workflows/ci-unit-docker.yml` | `1a0a6d6df5ec25d0afe88ea69da6c7e3d7873149` | Ordinary Docker checks; preserve trusted/native requirements and remove old identity. |
| `.github/workflows/ci-unit-docs.yml` | `677a0d639cd3c799d65721259b023f079ea13984` | Ordinary docs checks; no policy authority or stale runtime. |
| `.github/workflows/ci-unit-opentofu.yml` | `7b2a4c4d26c3cb7b11bc1de77eef63eaba1406d7` | Ordinary OpenTofu checks; no policy authority or stale runtime. |
| `.github/workflows/maintenance.yml` | `715351889671e4d775492f87717a5143bc14dba4` | Maintenance remains observable, no old checker/product acquisition. |
| `.github/workflows/preview.yml` | `f0a54de82479960b858bfc446ff79d53688ffeff` | Main-B exact-SHA API handoff; Tree-B literal pin later. |
| `.github/workflows/release.yml` | `99c0584852dbdd4beedd04102dc42bff8e0c7232` | Main-B exact-SHA handoff; `v*` release trigger is evaluated, not invented on push. |
| `.github/workflows/ci-runtime-products.yml` | `c35974071e31f1cbef495776fc5167ab8748908b` | Full app matrix: Linux-X64, Linux-ARM64, exact xcode-27; no macOS-26/15. |
| `.github/workflows/ci-policy-validator-products.yml` | `absent-at-d20` | Generated B publisher output; must be included with its post-implementation blob, source/template mapping, workflow graph, and two-render digest proof. |
| `.github/actions/setup-velnor-workflow/action.yml` | `4e48bc2694af7b3d1a969cb108234a9a92b515d3` | Delete old runtime-product consumer path; no alias. |
| `.github/actions/report-velnor-ci-outcomes/action.yml` | `199083f950f67e8befd2282d8d525b105af761e0` | Regenerate source-owned report action with exact child census, no stale path. |
| `.github-gen/sources/actions/setup-velnor-workflow/action.yml` | `4e48bc2694af7b3d1a969cb108234a9a92b515d3` | Replace old setup source; no compatibility alias. |
| `.github-gen/sources/actions/report-velnor-ci-outcomes/action.yml` | `199083f950f67e8befd2282d8d525b105af761e0` | Replace report source; included in renderer/tests. |
| `.github/actions/setup-velnor-policy-validator/action.yml` | `absent-at-d20` | New typed B consumer setup action, generated from source. |
| `.github-gen/sources/actions/setup-velnor-policy-validator/action.yml` | `absent-at-d20` | New source-owned typed setup action. |

The exact active workflow closure is the eleven ordinary workflows
`ci-main.yml`, `ci-policy.yml`, `ci-pr.yml`, `ci-unit-rust.yml`,
`ci-unit-bun.yml`, `ci-unit-docker.yml`, `ci-unit-docs.yml`,
`ci-unit-opentofu.yml`, `maintenance.yml`, `preview.yml`, and `release.yml`,
plus the separate `ci-runtime-products.yml` application publisher, the new
source/generated `ci-policy-validator-products.yml` B publisher, and the
supporting `nightly.yml` and callable-only `ci-release-package-signer.yml`
workflows. Every
recursive generated hit in these workflows, `.github-gen`, actions, state,
and actionlint is audited for `0dc`, candidate-manifest, `--pin-build`, old
setup/runtime product, mutable latest, macOS-15/26, and empty/disabled cells.
Historical old-checker fixtures are external evidence only and never source
inputs for the new tree.

Ruleset `protect-main`, ID `19573071`, is active for main. Current required
contexts are `{DCO, ci-required, Policy}`. Repository-role bypass actor 5 is
reported in `always` mode; this plan never uses it and requires an audit proof
that it was frozen/not used. `/branches/main/protection` returned 404, so the
ruleset API is authoritative.

## 2. Old-parser boundary and one selected Tree-A input

The old checker is source `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`, binary
SHA-256 `ade826fd3a27de43cfce55acfda870493b4921770199fe0afaa653b9a96f1750`.
The old parser uses strict unknown-field rejection. Measured evidence:

* historical b5 fixture: old `--plain --check` 0 and policy 0 (11/11);
* adding `[policy.validator]` to current or PR config: parser aborts
  `unknown field validator` before rendering;
* current-main raw policy: fails `generated-tree` because old state/output is
  not a fixed point;
* xcode-27 transition tree: old trusted-runner semantics reject it; and
* old `[[static_files]]` mapping can copy bytes and pass its own transport
  check, but this did not prove B, main provenance, or semantic authority.

The one selected old-parser-safe typed input for the unpinned publisher is the
existing generic `[[static_files]]` mechanism, with Tree A adding mappings for
the **actual publisher workflow**, not a compatibility data sidecar:

```toml
[[static_files]]
file = ".github/workflows/ci-policy-validator-products.yml"
source = ".github-gen/sources/workflows/ci-policy-validator-products.yml"

[[static_files]]
file = ".github/actions/setup-velnor-policy-validator/action.yml"
source = ".github-gen/sources/actions/setup-velnor-policy-validator/action.yml"
```

This existing typed field transports source-owned workflow/action bytes. The
field is already supported by 0dc; the two concrete mappings are Tree-A
inputs and must be admitted only after the fixture below records their exact
old-parser behavior. It is
not `.github/ci/policy-validator.toml`, an application alias, candidate
manifest, `--pin-build`, local fallback, or mutable release. The generic S2
crate remains estate-neutral: product identity, publisher workflow, and
consumer contract live in `.github-gen/velnor-workflow.toml` and
`.github-gen/sources`; no `tailrocks/velnor` special case is hardcoded in S2.

Measured typed-publisher feasibility is revision-bound evidence:
`G1/bootstrap-transition/TYPED-PUBLISHER-INPUT-FEASIBILITY-2026-09-20.md`
SHA-256 `8764712693e2883d05846de05a3c2137fb3d31e3ee97ab0ad129186f3ff960f`
and its JSON companion SHA-256
`01ad40d473aa26ae7e5e814ef7c3f93e854d682d2c359fa7588f0bba2b4943e9`.
It proves only that `static_files` transports source bytes under the old
parser. It found no actual B source, and the old package-release path is
incompatible because it has mutable tagging, write-capable consumers, and no
B schema/job binding. Current d20 raw old-checker failure remains failure.
These facts are not source admission; Tree A still requires reviewed B source,
the external A check, and the live Main-B handoff.

Before any authority operation, an isolated fixture must run the real old
checker with empty candidate input and this mapping, recording parser result,
old `--plain --check`, old semantic policy result, source/output bytes, and
whether current-main graph still fails. The new generator must render twice
from the reviewed source/config with identical hashes over workflows, actions,
state, actionlint, manifests, and sidecars. A raw old-checker failure remains
a failure; no fixture may relabel it as admission. The external A check exists
because old semantics cannot honestly admit the new native graph.

The publisher source file itself is a required closure input: hash
`.github-gen/sources/workflows/ci-policy-validator-products.yml`, its static
output, every referenced setup/report source, and every generated workflow in
the two independent renders. A missing source blob, untracked source, or
handwritten generated output is a hard failure.

The final closure check must additionally parse the generated graph and prove
that the inventory's candidate producer/consumer route is deleted: the
`ci-unit-rust.yml` candidate producer, hosted `candidate_publish` callers in
`ci-main.yml` and `ci-pr.yml`, policy consumers, and the unchecked
`build_revision` contract boundary are absent from the final reachable graph.
No grep-only result can satisfy this check.

Tree-A source-owned implementation surface is exactly:

```text
crates/velnor-workflow/src/s2/config/mod.rs
crates/velnor-workflow/src/s2/mod.rs
crates/velnor-workflow/src/s2/policy.rs
crates/velnor-workflow/src/s2/closure.rs
crates/velnor-workflow/src/s2/primitives/mod.rs
crates/velnor-workflow/src/s2/primitives/policy_validator_products.rs
matching src/s2 renderer tests and crate fixed-point/negative tests
```

The crate changes are neutral typed schema/rendering only. No product name,
repository path, fixed consumer, pin, provider, or estate list is hardcoded
inside generic S2. The target config and `.github-gen/sources` own those
values; generation must scan first and render byte-stably.

## 3. Exact staged authority and contracts

### 3.1 Tree-A admission: `Policy-bootstrap-A`

One owner-controlled GitHub App `velnor-bootstrap-authority` creates exactly
one check context for exactly one bootstrap PR. It has Metadata read, Contents
read, Actions read, Checks read/write, Pull requests read, and no
Administration, release, ref-write, workflow, secrets, packages, deployments,
merge, or bypass permission. It never executes PR-supplied workflow code.

Its typed admission record binds repository ID, PR number, PR head SHA, locked
base SHA, synthetic merge SHA/tree, live Checks integration ID, provider App
ID, transaction ID, lease ID, and the complete premerge run/attempt/child
census. It has `product_id=null`, `release_id=null`, `artifact_id=null`,
`attestation_subject=null`, no Actions run/job claim, and
`source_ref=refs/pull/<N>/merge`; it never claims `refs/heads/main`.
It emits `Policy-bootstrap-A` only after all required premerge checks are
terminal `success`, non-neutral, non-skipped, and complete.

Placeholders that block execution:

```text
BOOTSTRAP_APP_ID=<real>
BOOTSTRAP_INSTALLATION_ID=<real>
BOOTSTRAP_INTEGRATION_ID=<real Checks provider integration>
BOOTSTRAP_VERIFIER_REVISION=<40-hex reviewed revision>
BOOTSTRAP_APP_PUBLIC_KEY_SHA256=<64-hex>
BOOTSTRAP_PR_NUMBER=<real>
PR_HEAD_SHA=<40-hex>
BASE_SHA=325719f1e05d3d46322c9fd3eeb9ad545e175638
SYNTHETIC_MERGE_SHA=<40-hex>
SYNTHETIC_TREE_SHA=<40-hex>
TRANSACTION_ID=<unique>
LEASE_ID=<signed lease record>
WATCHDOG_ID=<independent recovery actor>
TREE_A_RULESET_HASH=<canonical full-PUT hash>
MAIN_B_VERIFIER_APP_ID=<real read/checks-write App>
MAIN_B_INTEGRATION_ID=<real Main-B Checks provider integration>
MAIN_B_VERIFIER_REVISION=<40-hex reviewed revision>
TREE_B_PR_NUMBER is intentionally absent here. Tree B does not exist before
Main-B succeeds; its PR number is obtained and bound only when the Tree-B PR
is opened after the Main-B handoff.
```

### 3.2 Main-B publisher and downstream verifier: `Policy-bootstrap-B`

Tree A generates `.github/workflows/ci-policy-validator-products.yml` from
the source-owned workflow contract. The workflow is `workflow_call`-only and
has no standalone push, dispatch, tag, or pull-request trigger. The caller
`ci-main` invokes it only when
`github.event_name == 'push' && github.ref == 'refs/heads/main'`. Every
side-effect job repeats that guard. A diagnostic `workflow_dispatch` caller
cannot reserve, attest, publish, upload a record, or emit any required check.

The jobs and permissions are separate:

* `build-linux-x64`: `contents:read`, `actions:read`; no write, OIDC,
  attestation, release, package, secret, or check authority.
* `artifact-verify`: `contents:read`, `actions:read`,
  `attestations:read`; reads the raw Actions artifact and attestation APIs;
  no write authority.
* `reserve-release`: `contents:write` only; no build, OIDC, attestation,
  package, or secret authority.
* `attest`: `contents:read`, `actions:read`, `id-token:write`,
  `attestations:write`; no contents/release write. The
  `attestations:write` scope is the single Actions permission for this job;
  GitHub does not accept duplicate read/write entries for one scope.
* `publish`: `contents:write` only; no build, OIDC, attestation, package,
  or secret authority.
* `record-upload`: `actions:write` only (write includes the required read
  path); it creates
  the canonical verification record artifact from typed upstream outputs. It
  has no contents, release, OIDC, or attestation-write authority.
* `verify-B`: `contents:read`, `actions:read`, `attestations:read`,
  `checks:read`; `metadata:read` is not an Actions job-permission key. The
  external Checks App has repository-metadata read in its installation
  permissions, separately from the workflow token. `verify-B` downloads and validates the
  record, release, raw asset, artifact, source, and both predicates. It does
  not write a check with `GITHUB_TOKEN`.

The required `Policy-bootstrap-B` check is emitted by one external,
owner-controlled Checks App/service after it independently verifies the exact
Main-B record and live APIs. The App uses its own installation credential
from a durable secret manager; `GITHUB_TOKEN` never impersonates that App or
its Checks integration. The App has only metadata/contents/actions/
attestations read and checks read/write permissions, no ref, merge, release,
package, workflow, secret, deployment, or bypass permission. Its check is
bound to the exact provider App ID, Checks integration ID, check context,
resulting main SHA, source ref, record artifact ID/digest, verifier revision,
and transaction ID. The `verify-B` job polls that exact integration-bound
check and fails closed if it is absent, duplicated, stale, or emitted by any
other provider.

The caller job is exactly one local reusable-workflow `uses` job:

```yaml
jobs:
  policy-validator-B:
    uses: ./.github/workflows/ci-policy-validator-products.yml
    needs: []
    permissions:
      actions: write
      contents: write
      attestations: write
      id-token: write
      checks: read
  Policy:
    needs: [policy-validator-B]
    permissions:
      contents: read
      actions: read
      attestations: read
      checks: read
```

The caller `policy-validator-B` job has no `outputs:` block and no shell
step. The called workflow declares outputs from its final `verify-B` job;
downstream `Policy` consumes `needs.policy-validator-B.outputs.*`
directly. Caller permissions are only an upper bound; called jobs reduce them
to the exact per-job scopes above. The caller token does not create the
required Checks App context.

#### Field-availability DAG

The following is the executable contract. A field may be consumed only after
the named stage produces it. No stage may use a later-stage ID or digest.

```text
S0 source/caller:
  caller_run_id, caller_run_attempt <- Actions caller context
  source_ref, source_sha <- caller context + GET ref/commit
  source_tree_sha <- GET commit/tree
  caller_workflow_path, caller_workflow_ref <- OIDC job_workflow_ref
  caller_workflow_file_blob_sha <- Contents API at caller ref
  caller_oidc_job_workflow_sha <- OIDC job_workflow_sha
  closure_digest <- reviewed generator output

S1 build-linux-x64:
  artifact_name <- immutable upload input
  artifact_id, artifact_digest <- upload-validator artifact-id/digest outputs
  upload_step_id <- fixed step id upload-validator
  service_zip_digest/size, inner_payload_digest/size,
  binary_digest/size/architecture <- build self-report plus local bytes
  no producer job id is guessed in this stage

S2 artifact-verify:
  artifact REST record <- GET /actions/artifacts/<artifact_id>
  producer_job_id, producer_check_run_id, producer_run_id,
  producer_run_attempt <- exact run-attempt jobs/check-runs API; job name
  selects a candidate only, numeric IDs and exact head SHA are authoritative
  producer_workflow_path/ref <- OIDC/API binding from the called job
  producer_oidc_job_workflow_sha <- OIDC job_workflow_sha claim
  producer_workflow_file_blob_sha <- Contents API for that exact ref
  raw service/inner/binary digests and sizes <- downloaded bytes
  verified_artifact_digest <- raw artifact bytes

S3 reserve-release:
  release_id, release_tag, release_target_sha, draft=true
  <- release API response after tag/target precondition
  all S1/S2 fields pass through unchanged

S4 attest:
  binding_core <- canonical S2/S3 fields only; excludes every own or later
  artifact/attestation/manifest ID or digest
  binding_record_artifact_id/digest <- upload-binding-record outputs
  binding_attestation_id/digest <- attestation API after binding subject
  final_manifest_artifact_id/digest <- upload-final-manifest outputs
  release_attestation_id/digest <- attestation API after manifest subject
  no attestation includes its own ID/digest in its signed preimage

S5 publish:
  release_asset_id/digest <- release asset API response
  published release/manifest/binding/attestation IDs and digests
  <- exact immutable S4 inputs plus API responses

S6 record-upload:
  record_artifact_id/digest/name, record_upload_step_id
  <- upload-record outputs; record_upload_job_id/check_run_id/run_id/attempt
  <- exact jobs/check-runs API; record contains all S0-S5 fields and provenance

S7 external verify-B App + workflow poll:
  App independently downloads record_artifact_id by numeric ID, raw release
  asset, Actions artifact, manifest, and both attestations; recomputes digests
  and checks source/tree/ref, producer IDs, OIDC claims, release state, and
  provider identity; App emits Policy-bootstrap-B
  verify-B polls the exact App integration check and returns the complete
  record_artifact_id/digest/name, record producer IDs, B check ID, and all
  verified S0-S6 fields as called-workflow outputs
```

The transport validator must reject any consumer field not produced by an
earlier stage, any field produced only by a name, any guessed REST property,
any workspace path, any job/check/artifact/release namespace collision, and
any binding/release predicate that contains its own future ID or digest. The
canonical record is a real uploaded artifact: its numeric
`record_artifact_id`, digest, name, upload step, producer job ID, producer
check ID, run ID, and attempt are all required and API-derived. It is not a
generic `canonical-json-artifact-id-and-sha256` claim.

GitHub REST fields are named exactly: workflow run `id` and
`run_attempt`; job `id`, `run_id`, `run_attempt`, and
`check_run_url`; check-run `id`; artifact `id`; release `id`; release
asset `id`. The canonical product schema's required `run.database_id`
field, if retained, is populated from the documented workflow-run JSON
pointer `/id` and is labeled a canonical schema field, never a claimed REST
property or second ID. No transport field uses `database_id`.

`job_workflow_sha` is the OIDC claim for the executing reusable workflow
revision. `workflow_file_blob_sha` is the Contents API blob SHA for the
workflow file at its exact ref. They are distinct fields and are checked for
the expected relationship; neither is aliased to `head_sha`.

The B product remains distinct from application runtime:

```text
product_id = velnor-workflow-policy-validator
schema = velnor.workflow-policy-validator.v1
purpose = policy-validator
platform = Linux-X64
runner = ubuntu-24.04
profile = release
features = ""
application_namespace_reuse = false
tag = velnor-workflow-policy-validator-v1-<closure16>
asset = velnor-workflow-policy-validator-Linux-X64
source_ref = refs/heads/main
source_sha = RESULTING_MAIN_SHA
mutable_latest = false
overwrite = false
```

#### Immutable release and predicate order

The release API precondition is explicit. Before POST, query the exact immutable
tag and release list. If any release with that tag exists, fail unless it is
the single current transaction's draft with exact target SHA and canonical
transition ID in its draft body. A POST timeout is not retried blindly:
re-query by tag, exact target, draft state, and transaction ID; adopt one exact
matching draft, otherwise fail for operator review. A published release, stale
draft, conflicting target, duplicate, or ambiguous response is terminal failure.
No delete, tag mutation, release replacement, or mutable latest is allowed.

The order is: build/upload; raw artifact verification; reserve fresh draft;
create/upload binding core record; binding attestation; create/upload final
manifest; release attestation; publish immutable release asset; upload the
canonical verification record; external App verification; workflow poll.
Binding and release predicates carry only fields available before their own
creation. The binding subject is the raw service ZIP and uses
`https://velnor.dev/attestations/policy-validator-binding/v1` at
`attestations/velnor-policy-validator-binding.v1.json`. The release manifest
subject uses
`https://velnor.dev/attestations/policy-validator-release/v1` at
`attestations/velnor-policy-validator-release.v1.json`, exactly matching the
canonical B schema. Both predicates carry signer repository/workflow/source
ref, OIDC issuer/policy revision, certificate identity and verification,
source/ref/tree/closure, exact producer run/attempt/job/check IDs, upload
step, artifacts, release IDs/assets, and required terminal conclusions.
`actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a` and
`actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6` are explicit
machine fields; artifact metadata write, registry push, and storage-record
creation are false.

The binding core excludes its own binding artifact ID/digest and both
attestation IDs/digests. The release manifest excludes its own upload and
release-attestation IDs/digests. No signed object contains its own digest.

`record-upload` writes `policy-validator-b-record.json` as an immutable
Actions artifact using the exact upload action. Its output is the typed
record artifact ID/digest/name plus upload-step and producer API identity.
The external Checks App reads that artifact through its own installation
credential; the workflow token never impersonates the App. The App emits
`Policy-bootstrap-B` only after all live checks and predicate validation.

`ci-main.Policy` consumes the called outputs with this concrete command tree
and read-only permissions:

```text
1. Read typed outputs:
   RECORD_ARTIFACT_ID, RECORD_ARTIFACT_DIGEST, RECORD_ARTIFACT_NAME,
   RECORD_UPLOAD_JOB_ID, RECORD_UPLOAD_CHECK_RUN_ID, RECORD_UPLOAD_RUN_ID,
   RECORD_UPLOAD_RUN_ATTEMPT, POLICY_BOOTSTRAP_B_CHECK_ID, RESULTING_MAIN_SHA.
2. GET /repos/tailrocks/velnor/actions/artifacts/RECORD_ARTIFACT_ID/zip
   with the caller GITHUB_TOKEN (actions:read); save only in Policy workspace.
3. sha256sum the raw download; require RECORD_ARTIFACT_DIGEST; unzip only
   policy-validator-b-record.json; parse canonical JSON and require every
   source/ref/tree/run/job/check/artifact/release/predicate field.
4. GET exact workflow run attempt jobs and check-runs; require the recorded
   numeric IDs, terminal success, exact head SHA, and attempt.
5. GET each Actions artifact, release, release asset, and attestation by
   numeric ID; recompute raw ZIP/inner/binary/manifest digests; require
   attestations:read and exact OIDC/certificate/predicate values.
6. GET the provider-bound Policy-bootstrap-B check through checks:read;
   require exact App/integration ID, context, source SHA, and success.
7. Fail closed on absent, duplicate, stale, skipped, neutral, non-terminal,
   mismatched, or name-selected records. Policy returns success only after
   every comparison passes.
```

### 3.3 Tree-A consumers and Tree-B adoption

`ci-main` calls the local reusable publisher with
`uses: ./.github/workflows/ci-policy-validator-products.yml`. Its
`Policy` job has direct same-workflow `needs: [policy-validator-B]` and
read-only `contents:read`, `actions:read`,
`attestations:read`, and `checks:read` permissions. No caller output
block or shell step is used.

The base policy verifier has an explicit transition state machine; it is not
an old-parser fallback:

```text
TREE_A_AUDIT:
  Policy is not required. ci-policy may report a non-required audit only.
  No adoption file or B pin is read.

MAIN_B_HANDOFF:
  entered only after external Policy-bootstrap-B succeeds and its complete
  live binding is stored by the owner-controlled base verifier;
  full ruleset Policy becomes required against the exact resulting main SHA;
  the verifier accepts exactly that one signed live binding, with no source
  pin, no candidate path, and no legacy parser route.

TREE_B_FILE:
  entered only after the Tree-B PR exists and its exact adoption file plus
  sidecar digest are read and bound to the already-live Main-B record;
  the base verifier requires the file mapping and permanent pin.

PERMANENT:
  entered only after Tree-B merge/resulting-main verification; temporary
  handoff state, A/B bootstrap contexts, and transition credentials are
  removed.
```

This state transition is the explicit resolution of the Policy cycle: Tree A
does not require old `Policy`; Main-B first requires `Policy-bootstrap-B`;
only after B is live does the base verifier enter `MAIN_B_HANDOFF` and make
normal `Policy` required; Tree B is then evaluated under
`TREE_B_FILE`. A missing handoff or adoption file is non-required audit only
while its corresponding state is not active. No state silently falls back to
the old checker.

Tree B adds exactly these two files after Main-B has passed. The PR number is
an input produced at this stage, never a Tree-A placeholder:

* `.github/ci/validator-pin-adoption.json`, canonical UTF-8 JSON with no
  self-digest field; and
* `.github/ci/validator-pin-adoption.sha256`, one canonical line containing
  the SHA-256 of the exact JSON bytes and filename.

The adoption digest preimage is explicit: the bytes of
`validator-pin-adoption.json` after canonical JSON serialization (UTF-8,
LF, no BOM, no trailing spaces), excluding no hidden fields because the JSON
has no self-digest field. The sidecar is the only digest carrier. The base
verifier hashes the downloaded JSON bytes and compares the sidecar before
parsing any target field.

```json
{
  "schema": "velnor.policy-validator-pin-adoption.v1",
  "mode": "tree-b",
  "transition_id": "<owner-reviewed>",
  "tree_b_pr_number": "<positive integer created after Main-B>",
  "base_policy": {
    "workflow_path": ".github/workflows/ci-policy.yml",
    "workflow_file_blob_sha": "<immutable Contents API blob SHA>",
    "verifier_key": "policy-adoption-v1",
    "verifier_revision": "<40-hex immutable reviewed verifier revision>",
    "checks_integration_id": "<exact base-policy Checks integration>"
  },
  "target": {
    "config_path": ".github-gen/velnor-workflow.toml",
    "config_sha256": "sha256:<exact Tree-B config bytes>",
    "generator_revision": "<exact reviewed immutable generator revision>",
    "schema": "velnor.workflow-policy-validator.v1",
    "template_sources": [
      {
        "path": ".github-gen/sources/workflows/ci-policy-validator-products.yml",
        "sha256": "sha256:<exact source bytes>"
      },
      {
        "path": ".github-gen/sources/actions/setup-velnor-policy-validator/action.yml",
        "sha256": "sha256:<exact source bytes>"
      }
    ],
    "static_files": [
      {
        "file": ".github/workflows/ci-policy-validator-products.yml",
        "source": ".github-gen/sources/workflows/ci-policy-validator-products.yml",
        "sha256": "sha256:<exact generated bytes>"
      },
      {
        "file": ".github/actions/setup-velnor-policy-validator/action.yml",
        "source": ".github-gen/sources/actions/setup-velnor-policy-validator/action.yml",
        "sha256": "sha256:<exact generated bytes>"
      }
    ],
    "generic_s2": "estate-neutral; no product, repository, provider, pin, or consumer hardcode"
  },
  "main_b": {
    "source_sha": "<RESULTING_MAIN_SHA>",
    "release_id": "<nonzero>",
    "tag": "velnor-workflow-policy-validator-v1-<closure16>",
    "actions_artifact_id": "<nonzero>",
    "actions_artifact_digest": "sha256:<64-hex>",
    "release_asset_id": "<nonzero distinct from actions_artifact_id>",
    "release_asset_digest": "sha256:<64-hex>",
    "binding_digest": "sha256:<64-hex>",
    "record_artifact_id": "<nonzero>",
    "record_artifact_digest": "sha256:<64-hex>",
    "signer_workflow": "tailrocks/velnor/.github/workflows/ci-policy-validator-products.yml",
    "provider_app_id": "<Policy-bootstrap-B App ID>",
    "provider_integration_id": "<Policy-bootstrap-B Checks integration ID>"
  }
}
```

The sidecar, JSON, source/template mappings, and live Main-B record are all
checked against the base-policy workflow file blob and verifier revision. The
base verifier queries release, caller run/attempt, job/check IDs, Actions
artifact, record artifact, raw release asset, both attestation predicates,
source ref/tree, provider integration, and exact resulting main. It emits
normal `Policy` only when all values match. No run/job/check/artifact/release
name is an identity. Missing or incomplete target mapping is non-required
audit until `TREE_B_FILE` is active; after activation it fails closed.

All eleven ordinary workflows retain their role: Bun, Docker, docs, OpenTofu,
Rust, maintenance, Preview, Release, PR, main, and policy. They contain no
old `0dc`, candidate-manifest, `--pin-build`, old setup/runtime identity, or
empty required child. Main-triggered Preview/Release/children discover the
verified B by exact `RESULTING_MAIN_SHA` and live IDs/digests before policy;
they do not use `workflow_run` or dispatch. `release.yml`'s `v*` tag trigger
does not apply to a Main-B push: record exact predicate/non-applicability and
collect its full run/attempt/child census at the first applicable stable tag.

Independent Preview/Release consumers use a bounded live API handoff, not a
workflow trigger. They receive the immutable `RESULTING_MAIN_SHA` and the
Main-B binding digest from the same transaction, then poll every 15 seconds
for at most 45 minutes:

The concrete query set is `GET /repos/tailrocks/velnor/actions/runs?head_sha=
RESULTING_MAIN_SHA&branch=main&event=push` for the caller `ci-main` workflow,
`GET
/repos/tailrocks/velnor/actions/runs/<RUN_ID>/attempts/<ATTEMPT>/jobs?per_page=100`,
`GET /repos/tailrocks/velnor/actions/runs/<RUN_ID>/artifacts?per_page=100`,
`GET /repos/tailrocks/velnor/actions/artifacts/<ARTIFACT_ID>`, `GET
/repos/tailrocks/velnor/releases/<RELEASE_ID>`, and the paginated release-asset
download plus attestation APIs. Caller/called workflow paths, caller run ID,
attempt, called job/check IDs, and exact SHA are checked against the signed
binding on every poll.

1. Query workflow runs by exact repository, caller workflow path, `head_sha`,
   branch, and expected event predicate. Require the recorded caller run ID
   and attempt plus called workflow path/job IDs to match the Main-B
   transaction; zero or multiple applicable candidates is a failure, not a
   choice by name or recency.
2. Query the exact run attempt's paginated jobs and check-runs. Require the
   expected workflow path, `head_sha`, job IDs, step conclusions, and terminal
   `success`; `queued`/`in_progress` may remain pending only before the
   deadline. `failure`, `cancelled`, `timed_out`, `neutral`, `skipped`, a
   missing child, an attempt mismatch, or a non-terminal deadline is terminal
   failure.
3. Query the exact artifact IDs and release ID from the binding; download the
   raw asset, recompute service-ZIP/inner-payload/binary/manifest digests, and
   compare the signed binding and source/tree SHA. Artifact or release names
   are never identifiers.
4. Record the event predicate and `non_applicable` outcome when a workflow's
   path/tag filter does not match. A Main-B push is non-applicable to
   `release.yml`'s `v*` trigger; the first matching stable tag must execute
   the same census. A timeout or applicable failure can never be wrapped in a
   successful dispatch/check.

The handoff record stores query URLs, timestamps, expected SHA, run ID and
attempt, every job/check/artifact/release ID, terminal conclusions, timeout,
and all digest comparisons. Preview and Release may run concurrently, but
each independently fails closed and neither is a publisher or Policy authority.

## 4. Protected transition, lease, and exact order

Ruleset contexts are staged as objects, not bare strings, and never claim two
providers under one name. Each object is bound to the exact Checks integration
ID and immutable provider revision:

```text
before       = [{context:DCO,integration_id:<DCO_ID>},
                {context:ci-required,integration_id:<CI_REQUIRED_ID>},
                {context:Policy,integration_id:<BASE_POLICY_ID>}]
Tree-A       = [{context:DCO,integration_id:<DCO_ID>},
                {context:ci-required,integration_id:<CI_REQUIRED_ID>},
                {context:Policy-bootstrap-A,integration_id:<A_ID>}] 
Main-B       = [{context:DCO,integration_id:<DCO_ID>},
                {context:ci-required,integration_id:<CI_REQUIRED_ID>},
                {context:Policy-bootstrap-B,integration_id:<B_ID>}] 
Tree-B/final = [{context:DCO,integration_id:<DCO_ID>},
                {context:ci-required,integration_id:<CI_REQUIRED_ID>},
                {context:Policy,integration_id:<BASE_POLICY_ID>}]
```

GitHub's documented full-object `PUT` has no proven ETag/CAS conditional
update. An external owner-controlled coordinator and mutation proxy can fence
the actors and credentials it controls, but it cannot enforce a freeze against
an administrator/bypass actor that can call GitHub directly. Ruleset actor 5
is observed as `always`; this plan does not claim the proxy solves that.
Before any transition, the repository owner must perform and record a real
provider operation that removes actor 5 from bypass or changes its provider-
supported bypass mode to a non-bypassing state, then verify the full returned
ruleset body. If GitHub cannot enforce that state for the exact repository,
the operation is an external approval/capability blocker and stops here.

After that provider precondition, the owner-controlled coordinator plus
mutation proxy fences all actors and credentials it controls. The coordinator
holds a monotonic fencing token and exclusive lease in durable storage; every
request carries transaction ID, expected main SHA, expected PR head/base,
expected full ruleset hash, allowlisted endpoint, and fencing token. The proxy
rejects stale tokens, wrong refs/hashes, duplicate operations, and unallowlisted
actors/endpoints. A signed lease alone is evidence, never enforcement.

An independent recovery actor/watchdog has a separate credential and a
documented forward-completion protocol. Before the merge, it may restore the
exact prior ruleset only if the live ruleset hash equals the temporary hash,
main still equals the locked pre-merge SHA, and the same fencing token is
valid. After a successful merge it never rolls main back: it either completes
the staged forward transition or freezes and reports. Recovery survives
coordinator death, uses conditional state transitions in its own durable
store, and cannot clobber concurrent changes. The coordinator, proxy,
watchdog, App IDs, key fingerprints, and durable trust location are unresolved
placeholders and hard blockers until independently exercised.

The chosen main mutation is normal protected PR merge under this provider
precondition and coordinator fence. The merge API `sha` guards only PR head;
the coordinator fence and immediate main/ruleset re-read guard the base race.
A disposable repository with equivalent ruleset objects must prove writer
exclusion, retained DCO/ci-required/review enforcement, no bypass, exact
integration-bound contexts, recovery after coordinator death, and PR merge
attribution. Direct ref update, admin merge, force push, and check-name-only
authority are rejected. If the provider precondition, disposable proof, or
writer freeze is unavailable, stop before mutation.

The operator transcript must bind every request to the lease transaction:
`GET /repos/tailrocks/velnor/git/ref/heads/main`, `GET
/repos/tailrocks/velnor/pulls/<N>`, `GET /repos/tailrocks/velnor/rulesets/19573071`,
`GET /repos/tailrocks/velnor/commits/<SHA>/check-runs`, and the paginated
workflow-run/job APIs before each ruleset write. Save the complete canonical
JSON body and SHA-256 for `before`, `tree_a`, `main_b`, `final`, and every
re-read. Ruleset changes use the documented full
`PUT /repos/tailrocks/velnor/rulesets/19573071` body whose canonical hash is
recorded; no undocumented `If-Match`, ETag, or optimistic CAS claim is
allowed. Merge uses only
`PUT /repos/tailrocks/velnor/pulls/<N>/merge` with `sha=PR_HEAD_SHA` and
`merge_method=merge`; the returned SHA is accepted only after the ref,
parent order, PR attribution, and lease guards are re-read. Any unresolved
ID, endpoint capability, App integration, or lease proof blocks execution.

Exact idempotent order:

1. Create signed lease with current main, PR head/base, synthetic tree, full
   ruleset hash, allowed actors/endpoints, watchdog acknowledgment, expiry,
   transaction state, and conditional rollback guards.
2. Run Tree-A App verifier and complete exact premerge census: DCO,
   `ci-required`, provider, unit, security, native, every run/attempt/job/
   check child; all terminal success, non-neutral, non-skipped.
   Before this admission is eligible, the generated Tree-A closure must have
   removed every active `0dc`, candidate-manifest, `--pin-build`, old
   setup/runtime, and macOS-15/26 path. The detached historical old checker is
   never run as a Tree-A authority.
3. Freeze writers, verify `Policy-bootstrap-A` provider/integration/head/base/
   synthetic-tree binding, then full-`PUT` ruleset Tree-A state. Read/hash it.
4. Re-read main/PR/ruleset/lease and perform normal protected merge with
   `sha=PR_HEAD_SHA`, no admin, force, direct ref, or App merge capability.
   Record returned `RESULTING_MAIN_SHA` only under the already-proven lease;
   immediately verify main ref, parent order `(BASE_SHA, PR_HEAD_SHA)`, tree,
   PR attribution, and lease state. Any mismatch is failed, never published.
5. Main-B starts the `ci-main` caller on the real `refs/heads/main` push;
   `workflow_dispatch` is diagnostic and non-publishing. Its guarded reusable
   `policy-validator-B` job invokes the workflow_call-only publisher. Within
   that called workflow, the exact DAG is
   `build-linux-x64 -> artifact-verify -> reserve-release -> attest -> publish
   -> record-upload -> verify-B`; the external Checks App independently emits
   `Policy-bootstrap-B`, and `verify-B` polls that exact provider result.
   Typed workflow_call outputs then flow to the caller job and its direct
   `needs: [policy-validator-B]` Policy job. Caller run/attempt, numeric
   producer job/check IDs, record artifact IDs/digests, and both workflow
   identity forms are recorded; no standalone B run, `workflow_run`, dispatch
   wrapper, check-name shortcut, or workspace file crosses a job boundary.
6. Census exact Main-B runs/attempts/jobs/checks/artifacts/manifests for
   `ci-main`, B, Policy, DCO, ci-required, all unit/provider/security/native
   children, and application `ci-runtime-products` jobs exactly
   `build[Linux-X64]`, `build[Linux-ARM64]`, `build[xcode-27]`, `publish`.
   Every required result is terminal success, non-neutral, non-skipped,
   non-empty, exact SHA/attempt. Preview is collected through the bounded
   exact-SHA API protocol when its event matches; Release records `v*`
   non-applicability on push, then is collected on its first applicable stable
   tag using the same protocol. The final closure graph must also prove that
   the 35+35 candidate/reusable edges and candidate `build_revision` boundary
   are no longer reachable.
7. After Main-B live B binding and census pass, store the exact signed handoff
   record in the base verifier, full-`PUT` Tree-A → Main-B, read/hash, then
   full-`PUT` Main-B → final `Policy`; read/hash **before opening Tree B**.
   The base verifier is in `MAIN_B_HANDOFF`, so normal `Policy` is B-backed
   by the exact live record without a source pin. This explicit state change
   resolves the old Policy cycle; it is not a legacy fallback.
8. Open Tree B only now. Bind its newly-created PR number, exact adoption JSON,
   sidecar digest, and live Main-B IDs/digests. Review, pass all eleven
   ordinary workflows, and merge normally under `TREE_B_FILE`. Only after the
   resulting main is verified does the verifier enter `PERMANENT` and the
   literal B pin become permanent trust.
9. Remove temporary A context, App, verifier config, lease/watchdog
   transition permissions and any bootstrap files. Never remove permanent B
   publisher/trust. Delete old 0dc/candidate/setup/runtime/macOS aliases from
   every generated/source path before Tree A; clean regeneration proves none
   remain after Tree B.

## 5. Evidence and independent approval gates

Evidence outside source must include the full premerge and Main-B machine
census, exact source/tree/closure, all run/attempt/job/check/artifact/release
IDs, raw ZIP/inner/binary/manifest digests, signer/predicate, native matrix,
Preview applicability, Release predicate/non-applicability, complete canonical
ruleset bodies and SHA-256 snapshots for `before`, `tree_a`, `main_b`, `final`,
and every re-read, signed lease/watchdog recovery, provider actor-5 freeze
proof, and Tree-B final proof. A proxy transcript without a provider snapshot
does not prove freeze.

Current evidence inputs:

* old-validator admission probe MD SHA-256
  `70039e170b154db26ad595549fa8f69e02b509e62ff9d82d9eaf34f365525591`;
* binding audit current MD SHA-256
  `6adfc96925cfda77d6db1329c7e3592da63f541af21d68f8c53267686270b9d8`;
  it has 34 hostile fixtures but no real verifier, so it is not acceptance;
* prior authority review is **NOT APPROVED**, MD SHA-256
  `df936098a97e3d1b609728b69112b7b872ce64d82ba3e70dc8a84efc76fd857b`;
* canonical Tree-A closure inventory MD/JSON SHA-256 values are
  `90314d2c32203e06012feb33d149ce864c29ba856a73c8f9b1bc5146b69b7eae` and
  `97f71e70ffa8f974e0abd9b382696e713f26e88645ef468fa43ad3dd3b8d4264`;
* typed-publisher feasibility MD/JSON SHA-256 values are
  `8764712693e2883d05846de05a3c2137fb3d31e3ee97ab0ad129186f3ff960f4` and
  `01ad40d473aa26ae7e5e814ef7c3f93e854d682d2c359fa7588f0bba2b4943e9`;
* `G1/bootstrap-transition/authority-contract-separation-2026-09-20/`
  `authority-contract-separation-results.json` SHA-256
  `80d0f24d0dfc72abcd8b7f6c5b2e56cefb715791fb7c7cd320c8314c35baba68`,
  and its `canonical-root-manifest.json` SHA-256
  `cdc77d552ada64c3b222b712f689a0958f4164c1e354c5e2557921bc13cf98b5`,
  44 regular files, 0 symlinks, and 30 structural fixtures (26 schema
  rejects, 4 semantic rejects) are bound as contract-only evidence;
* that bundle reports the real verifier **unimplemented**, live API and
  cryptographic execution **not run**, and no G1 claim;
* validator design MD/JSON and admission JSON remain revision-bound inputs;
  all hashes are recorded in the paired machine-readable plan.

The final generator proof is semantic and graph-based: parse the final config,
build the exact reviewed target generator, recompute source/runtime closure,
derive the runtime tag and product manifest, verify each points to the target
revision, then inspect reachable workflow jobs/actions and rendered outputs.
It must prove candidate publication, `candidate_publish`, unchecked
`build_revision`, old setup/runtime, and old runner routes are absent as
behavior, while two independent renders, state, actionlint, manifests, and
sidecars are byte-identical. A text scan is supporting evidence only.

Approval requires separate `g0_reviewer` and `authority_transition_review` to
inspect this exact v7 pair against current main `325719f1e05d3d46322c9fd3eeb9ad545e175638`,
not historical d20/e94 facts. Review must validate the old-parser fixture,
target generator and two-render hashes, full recursive closure, caller/called
workflow identities, field-availability DAG, actual record-upload producer,
workflow_call outputs without caller-job re-export, no-workspace boundary,
acyclic binding/release predicates, canonical release predicate path, release
retry precondition, release-asset versus Actions-artifact namespaces, OIDC
claim versus workflow-file blob, explicit actions/attestations read scopes,
certificate trust, per-hop permissions/digests, exact `{context,
integration_id}` ruleset objects and full-body snapshots, provider-enforced
admin/bypass freeze, coordinator/proxy recovery, cross-workflow API handoff,
native xcode-27 census, nightly and signer workflows, and cleanup. The author
cannot approve. All App/provider, target-generator, B-source, coordinator/
proxy, watchdog, durable-trust, and live-product IDs remain unresolved
placeholders. The v5 executable contract
audit (`v5-executable-contract-check-2026-09-20`, report SHA-256
`16259a66f4e4cd6df4da8dd80cdfb82e73022b44a4b8137f12c90866bdf01fbe`, results
SHA-256 `187e2c01766741cae0e8caf614c307beaadbbd7d88a7135757b3481d6840f4e1`)
is a negative regression baseline, not approval. The independent v6 audit is
also preserved as a rejection: 58 checks, 33 pass, 23 findings, 2
unimplemented; its report/results hashes are recorded in v7 JSON. Until owner
approval, both reviews, a real verifier, all identities, provider freeze proof,
and all preflight proofs exist, v7
does not authorize implementation, source changes, GitHub changes, release,
merge, or G1 completion.
