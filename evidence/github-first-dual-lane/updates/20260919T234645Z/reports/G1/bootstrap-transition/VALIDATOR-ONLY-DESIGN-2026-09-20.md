# Typed Linux policy-validator product: bootstrap design review

Status: **design only; not implemented, published, merged, or G1-approved**.

Observation UTC: 2026-09-20. This review uses the live default branch
`tailrocks/velnor/main` at `1048337062ea625fada1b4f7c07f2feed75f60c7`; the
historical transition baseline is `b5a4b4afaa6ca807927cacc03659b570a895dd5c`.
PR #957 is at `92387e88c32f933a9061b819256e535662655cb2` (base `b5a4b4af`). The
current PR config declares generator pin `9e06bef6da90d7084853d78a5e1fdea31a1d3a5c`;
that revision has no published runtime product. Current policy run
`35473052923` therefore cannot establish acceptance. The old published
application/runtime product is source `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`
and tag `velnor-workflow-runtime-v1-8b96d5108550dfa6`; it is not a policy
validator product.

No source checkout was edited. No workflow was dispatched. No release,
artifact, or package was published.

## Current-main reconciliation

The default branch advanced after the historical `b5a4b4af` snapshot. The
current live SHA is `1048337062ea625fada1b4f7c07f2feed75f60c7`, parent
`b5a4b4af`. The new commit only makes generator rendering reproducible; it does
not alter the transition-critical workflow, policy, publisher, or consumer
pins. Exact current blobs are:

| Surface | Current blob | Historical `b5` blob | Consequence |
| --- | --- | --- | --- |
| `.github/workflows/ci-runtime-products.yml` | `d5f9c6ade31307025f9886d39cf1cfdd49971461` | same | still schedules `build[macOS-ARM64]` on `macos-15` at lines 96-107 |
| `.github/workflows/ci-policy.yml` | `c8896e9e9bb766105fe949fa668ca15e3fd73f9c` | same | still uses the old policy/pin and candidate acquisition path |
| `.github/actions/setup-velnor-workflow/action.yml` | `4e48bc2694af7b3d1a969cb108234a9a92b515d3` | same | no typed validator product consumer |
| `crates/velnor-workflow/src/s2/policy.rs` | `7ea7ccf4adba0e41ae55763a04e9de2d5a72ce65` | same | `is_github_owned_label` still accepts only `ubuntu-*`, `macos-*`, and `windows-*`; `xcode-27` remains unadmitted |
| `crates/velnor-workflow/src/s2/config/mod.rs` | `4f1097af012453d3378f67bb465f7c66a26c79da` | same | strict `deny_unknown_fields` means early `[policy.validator]` is rejected |
| `crates/velnor-workflow/src/s2/primitives/runtime_products.rs` | `b5c72fa4c37384ae7f0ce3227d723578c519b688` | same | application product remains three-platform, including old macOS builder |
| `.github-gen/velnor-workflow.toml` | `b2bd968a484b9786c112c931dc325071ba1dff08` | same | declared generator pin remains `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`; no validator pin |

The new main therefore does not close or weaken the ordering proof. PR #957
remains a stale-base candidate whose `xcode-27` transition still requires a
typed validator product and the same protected bootstrap authority. The
historical `b5` evidence remains valid as a revision-bound proof, and this
reconciliation extends it to current main.

The independent binding audit is persisted at
`G1/bootstrap-transition/validator-binding-audit-2026-09-20/REPORT.md`
(SHA-256 `15535e68c27c56019665e6045b9f5948bd87238e5664ab3e917739dee417ad1b`).
It supplies a strict predicate schema and 34 hostile fixtures, including
release-ID and service-ZIP/job-binding cases, but explicitly reports no real
verifier, cryptographic verification, live artifact download, or release
lifecycle execution. It is contract evidence only, not G1 acceptance. Its
absence search was against an older checkout; the current-main tree
`1048337062ea625fada1b4f7c07f2feed75f60c7` was independently searched by API
and has no typed validator/publisher/binding paths. The fixture input hashes
predate the corrections in this report; regenerate the fixture bundle and
rerun a real verifier after implementation. Do not treat the stale fixture
bundle as proof of the corrected design.

The independent old-validator admission probe is
`G1/bootstrap-transition/VALIDATOR-ADMISSION-PROBE-2026-09-20.md` (SHA-256
`70039e170b154db26ad595549fa8f69e02b509e62ff9d82d9eaf34f365525591`; JSON
SHA-256 `2609e9cfc2f50e34c4dc02d9b31b46f9d70a441420c1989538d959ba0a51469b`).
It built and executed the trusted `0dc...` checker in isolation against exact
current-main/PR957 fixtures with empty candidate input:

* historical `b5` baseline: `--check` 0; policy 0 (11/11 rules);
* `[policy.validator]` in either current or PR config: parser aborts with
  `unknown field validator` before rendering;
* old-supported `[[static_files]]` typed sidecar: b5 `--check` 0/policy 0;
* PR957 tree preserving `xcode-27`: generated-tree and trusted-runners fail;
* current main `1048337062...`: raw policy fails generated-tree because the old
  0dc renderer cannot reproduce its current generator state.

The probe confirms the sidecar is transport-only and does not establish B,
repair current-main fixed point, hand off `pull_request_target`, or authorize
the merge. It is measured compatibility evidence, not a G1 pass.

## Decision

The smallest correct bootstrap is a separately typed, Linux-X64 policy
validator product. It must not reuse the application/runtime product namespace,
manifest, setup action, or three-platform publisher. The application publisher
remains the sole application publisher. The validator publisher is a separate
owner-only publisher for a different product purpose, with one Ubuntu 24.04
X64 build.

There is one real bootstrap authority gap. The current generator/publisher
cannot create this product before its own new workflow/action is admitted:

* `b5a4b4af`'s application publisher is fixed to Linux-X64, Linux-ARM64, and
  **macOS-ARM64 `macos-15`** (`.github/workflows/ci-runtime-products.yml`,
  matrix lines 96-107). Running it is forbidden by the current specification;
  a Linux-only application manifest would strand macOS consumers and would be
  a false success.
* The current generated-tree rule rejects a merged tree that the declared old
  generator cannot render. A new generated publisher/action cannot be merged
  and then published by that old product without a staged authority.
* PR candidate binaries, `--pin-build`, `runtime-product` fallback, mutable
  `latest`, and a developer checkout are not trusted bootstrap authorities.

Therefore source support can be admitted first, with generated behavior still
fixed-point under the current published validator. After that source commit is
merged on `main`, one explicitly reviewed, base-owned GitHub-hosted source-build
bootstrap publisher must build and attest the validator product from that exact
main SHA. Only then can a separate pin-adoption PR select it. If this
one-time authority is not authorized, G1 is blocked at product creation; there
is no honest no-old-macOS path using the current publisher.

### Strict source-merge caveat

The source-only step does **not** by itself satisfy the user's no-old-macOS
rule. A push to current `main` still starts the existing generated
`ci-runtime-products.yml` and its `macos-15` cell. The old validator cannot
admit `xcode-27`, and it cannot render an xcode-27 generated tree. Thus the
sequence is executable only if the newly authorized bootstrap authority also
provides a reviewed, fail-closed transition for the already-triggered main
surface before that push executes (or if the repository owner explicitly
provides an equivalent trusted default-branch workflow transition). Removing
the macOS cell, skipping the job, dispatching an old image, or calling a
green wrapper is not that transition. If no such authority exists, the exact
blocker is the current main push itself: it necessarily schedules the
forbidden `macos-15` product build before the separate validator can be
published.

The bootstrap authority is temporary and must be removed before G7. It is not a
second application publisher and cannot publish application releases, packages,
feeds, taps, or runtime products.

## Required product contract

Product identity is independent of the application runtime contract:

| Field | Required value/relationship |
| --- | --- |
| Product ID | `velnor-workflow-policy-validator` |
| Manifest schema | `velnor.workflow-policy-validator.v1` |
| Purpose | `policy-validator`; reject any other purpose |
| Release tag | `velnor-workflow-policy-validator-v1-<closure[0:16]>`; immutable, no overwrite |
| Release asset | `velnor-workflow-policy-validator-Linux-X64`; exactly one |
| Platform | `Linux-X64`; builder `ubuntu-24.04`; no ARM/macOS asset implied |
| Profile/features | `release` / empty features, exact equality |
| Source | repository `tailrocks/velnor`, ref `refs/heads/main`, exact 40-hex source SHA |
| Closure | exact 64-hex source closure computed from the same closure contract as the binary |
| Binary identity | raw asset SHA-256, size, executable mode; binary `--revision` and `--closure` equal manifest |
| Release lifecycle | draft reserved first; final manifest carries nonzero draft `release.id` and exact tag/target; publish only after signed verification |
| Application separation | application tag prefix, assets, manifest schema, setup action, and attestation signer are rejected |
| Publisher | `.github/workflows/ci-policy-validator-products.yml`, exact protected main workflow path/ref |

The manifest must carry at least:

```json
{
  "schema": "velnor.workflow-policy-validator.v1",
  "product_id": "velnor-workflow-policy-validator",
  "purpose": "policy-validator",
  "platform": "Linux-X64",
  "profile": "release",
  "features": "",
  "source": {
    "repository": "tailrocks/velnor",
    "ref": "refs/heads/main",
    "sha": "<main-sha>",
    "closure": "<64-hex>"
  },
  "asset": {
    "name": "velnor-workflow-policy-validator-Linux-X64",
    "sha256": "<64-hex>",
    "size": "<positive-integer>"
  },
  "release": {
    "tag": "velnor-workflow-policy-validator-v1-<closure16>",
    "id": "<nonzero-draft-release-id>",
    "target_sha": "<main-sha>",
    "draft_reserved": true
  },
  "producer": {
    "workflow_path": ".github/workflows/ci-policy-validator-products.yml",
    "workflow_sha": "<workflow-source-sha>",
    "run_id": "<positive-integer>",
    "run_attempt": "<positive-integer>",
    "run_database_id": "<positive-integer>",
    "job_key": "build-linux-x64",
    "job_id": "<positive-integer>",
    "check_run_id": "<positive-integer>",
    "artifact_id": "<positive-integer>",
    "artifact_name": "policy-validator-<closure16>-Linux-X64",
    "artifact_service_zip_sha256": "sha256:<64-hex>",
    "artifact_inner_payload_sha256": "sha256:<64-hex>",
    "upload_step_id": "upload-validator",
    "upload_artifact_id_output": "<positive-integer>",
    "upload_artifact_digest_output": "sha256:<64-hex>"
  },
  "attestation": {
    "binary_subject_sha256": "sha256:<64-hex>",
    "producer_binding_subject_sha256": "sha256:<64-hex>",
    "manifest_subject_sha256": "sha256:<64-hex>",
    "signer_workflow": "tailrocks/velnor/.github/workflows/ci-policy-validator-products.yml",
    "source_ref": "refs/heads/main"
  }
}
```

Angle-bracket values above are documentation placeholders only; zero values
are never accepted. The final manifest is generated only after the immutable
draft release returns its nonzero ID, then attested and uploaded to that draft;
the release is published only after the signed manifest and all release/API
checks pass.

### Artifact-to-job binding

GitHub artifact REST metadata exposes artifact ID, name, service/archive
digest, expiry, and enclosing workflow run, but not producer job ID, attempt,
uploader, or step. The existing artifact feasibility records
(`G1/bootstrap-artifact-feasibility-2026-09-20.md` and
`G1/artifact-producer-binding-research-2026-09-20.md`) establish this. Name or
run correlation is not proof.

The publisher must use this exact binding:

1. The fixed `build-linux-x64` job builds the binary from the checked-out
   trusted main SHA under isolated Cargo state, computes the inner payload
   digest, and writes a binding record containing `github.run_id`,
   `github.run_attempt`, `job.check_run_id`, `github.job`, `job.workflow_ref`,
   `job.workflow_sha`, source SHA/closure, platform, and expected artifact
   name.
2. The pinned `actions/upload-artifact` step has a stable ID, `overwrite: false`,
   and captures `artifact-id` and `artifact-digest`. The job attests the binary
   and a deterministic payload containing the binding record.
3. The protected publisher job fetches the exact artifact by numeric ID. It
   compares the upload output to the REST artifact `id`, `name`, `digest`,
   `workflow_run.id`, `expired`, and retention bounds; downloads the archive by
   that ID; verifies the raw service ZIP SHA-256 and inner payload SHA-256.
4. It fetches the enclosing run and exact-attempt jobs from the API. It requires
   exact repository IDs, `head_sha`, `head_branch=main`, event `push` or an
   explicitly protected main dispatch, terminal `success`, `run_attempt`,
   workflow path/SHA, numeric `job_id`/`check_run_id`, the fixed job key, and
   every required build/upload step terminal-success. A duplicate, stale,
   failed, canceled, queued, expired, or mismatched object aborts publication.
5. The custom signed binding attestation's subject digest is the service
   artifact digest and its predicate carries the numeric job/run/attempt and
   upload outputs. Standard provenance separately attests the binary. The
   publisher verifies both before creating the immutable release. Consumers
   verify the publisher-attested manifest/binding; they do not depend on a
   seven-day artifact's continued REST availability.

This resolves the REST limitation with a protected signed binding plus live
API cross-check. It does not trust a candidate manifest, `needs` output, name,
run ID alone, binary digest alone, or standard binary provenance alone.

## Exact implementation surface

The following is the smallest source surface. No source edits are authorized by
this report.

### Source admission commit (fixed-point; no candidate authority)

The source admission commit must not enable a new generator output. It adds the
typed contract and dormant renderer/product code while leaving existing output
byte-identical under `0dc79895...`/the current main pin. It changes no macOS
runner label and does not execute an old-macOS replacement as a success.

Owned source locations:

* `crates/velnor-workflow/src/s2/config/mod.rs`: typed policy-validator and
  application-runtime product sections; separate `generator.revision`,
  `policy.validator.revision`, and `runtime.product.revision`; deny unknown
  fields. The typed section is **not present in the old-validator admission
  tree**: current `0dc...` has `serde(deny_unknown_fields)` and rejects an
  unknown `[policy.validator]` table before rendering. It can first appear
  only in the post-product B handoff tree.
* `crates/velnor-workflow/src/s2/mod.rs`: `ProjectConfig` fields, validation,
  renderer dispatch, and separate policy/runtime pin emission.
* `crates/velnor-workflow/src/s2/primitives/mod.rs` and a new
  `crates/velnor-workflow/src/s2/primitives/policy_validator_products.rs`:
  typed product/publisher renderer, fixed owner repository, fixed Linux-X64
  platform, schema, asset/tag namespace, and no application publisher reuse.
* `crates/velnor-workflow/src/s2/closure.rs`: a typed validator tag/closure
  helper that cannot collide with `product_tag`/runtime tags.
* `crates/velnor-workflow/src/s2/policy.rs`: parse and enforce the separate
  validator pin/product identity; keep renderer pin, runtime product pin, and
  validator pin distinct; reject cross-namespace or empty enabled pins.
* `crates/velnor-workflow/src/s2/policy/tests.rs` and the new primitive tests:
  negative identity, platform, source, digest, release, artifact-binding,
  stale/duplicate API objects, and fixed-point tests.
* `.github-gen/velnor-workflow.toml`: remains byte-for-byte old-schema
  compatible during source admission; do **not** add `[policy.validator]`, a
  parser alias, an empty revision, or a future unpublished `9e06...` pin before
  product B exists. The typed `[policy.validator]` table is introduced only by
  the separately reviewed B handoff.
* `.github-gen/sources/actions/setup-velnor-policy-validator/action.yml`:
  source for the fail-closed validator-only consumer. It is trusted only from
  the base checkout in `pull_request_target`; it never compiles or falls back.

The old-validator source-admission probe is an executable compatibility gate,
not a merge or bootstrap-product gate:

* checkout the exact candidate tree and run the published legacy base checker
  `0dc...` with `velnor-workflow --plain --check` and `velnor-workflow policy`;
* require zero failures without `--candidate`, `--pin-build`, or any local
  binary, and compare every generated output/sidecar byte to the old pin;
* source-only Rust additions are admissible only if that real probe still
  passes; they do not become policy behavior until B is selected;
* an optional bootstrap sidecar may use only the already-supported
  `static_files` schema. Its source bytes, ownership, hosted runner, and
  action pins must pass the same old `0dc...` fixed-point/semantic probe. It
  must contain no `xcode-27`, B pin, candidate path, or fallback authority;
* if the sidecar is not reproduced by the old generator, it is not admitted;
  no parser relaxation, alias, or unknown-field suppression is allowed.

This probe proves old-schema compatibility only. It does **not** authorize a
source-only merge: current `main` would still trigger the forbidden
`macos-15` publisher, and the base `pull_request_target` workflow would still
run the legacy checker after B is published until an explicit old-checker-to-B
handoff occurs.

### One-time trusted bootstrap authority

The one-time authority must operate as a protected **pre-main transition**, not
as a post-merge promise. A source-only merge is unsafe because the first push
would schedule `macos-15` before any post-merge publisher can act. For the
exact final tree/merge SHA, a protected GitHub-hosted `ubuntu-24.04` transition
publisher (or an equivalent owner-controlled merge service) must:

* validate the exact final tree and establish the resulting main SHA before
  allowing the default-branch transaction; a staging ref/name is not source
  provenance and cannot substitute for `refs/heads/main`;
* checkout the exact immutable tree with full history and no persisted
  credentials;
* build only `velnor-workflow` release Linux-X64 with locked dependencies;
* emit the typed manifest/binding above, attest binary, binding, and manifest;
* query run/jobs/artifacts live and reject any failed/ambiguous binding;
* create only `velnor-workflow-policy-validator-v1-<closure16>` using the
  draft-first release protocol below;
* refuse overwrite, mutable tags, candidate/fork source, old-macOS jobs, app
  runtime assets, packages, feeds, or taps.

This is a **new, explicit authority** because the current generated
`ci-runtime-products.yml` cannot perform the contract and the old base
`pull_request_target` checker cannot hand itself to B. It must be reviewed by
an independent agent before dispatch. If the project disallows this protected
pre-main transaction and its temporary required-check handoff, the exact
blocker is ordering authority, not absence of a local build; do not substitute
a local binary, candidate, or application runtime release.

### Exact protected-transition request

The owner decision needed is concrete:

* **Actor.** Install one owner-controlled GitHub App/merge service or
  protected base-owned workflow. It may read contents, checks, actions runs and
  artifacts; mint OIDC only from its immutable workflow; create attestations;
  and create one immutable release. It may not use a developer token, merge
  bypass, repository secret from a PR, mutable ref, or self-hosted runner.
* **Scope.** One reviewed bootstrap PR and one precomputed final main SHA only.
  The service must reject a changed PR head, changed merge tree, changed base,
  changed closure, or a second attempt with a different SHA. It must not become
  the application/package publisher.
* **Pre-main build.** Build B from the exact final tree that will become
  `refs/heads/main`, and bind the custom predicate to repository, exact SHA,
  intended `refs/heads/main`, merge transaction ID, closure, workflow path/SHA,
  run/attempt, numeric job/check-run, artifact ID/name, service-ZIP digest, and
  inner payload digest. A staging branch or matching commit message is
  insufficient. If GitHub cannot produce standard source provenance for the
  future main ref before the ref exists, the service must use an owner-reviewed
  atomic merge/publish primitive that does; otherwise this gate remains
  blocked.
* **Base-checker handoff.** Before the handoff PR can merge, replace the
  legacy base `Policy` execution path (`0dc...`, old setup action, candidate
  acquisition) with the B-backed typed validator path. The handoff must run B
  against the exact PR tree, publish a real terminal check with run/job/artifact
  evidence, and retain DCO plus every non-policy required check. No successful
  wrapper may mask a failed legacy child. The temporary ruleset/check-context
  change must be explicitly reviewed, scoped to this one bootstrap transaction,
  and removed immediately after the generated B-backed `Policy` is live on
  main; no admin merge bypass is acceptable.
* **Main transition.** Merge only after B release/attestation and the B-backed
  handoff check are verified. The first resulting-main workflows must contain
  the complete Linux-X64, Linux-ARM64, and xcode-27 macOS-ARM64 application
  matrix and consume B; no `macos-15` cell may be scheduled, and no native job
  may be disabled or made empty.
* **Removal.** After the first B-backed PR and main runs are terminal success,
  restore the ordinary ruleset contexts with generated `Policy`, delete the
  sidecar/transition workflow and external check integration, remove all
  temporary B-bootstrap branches/permissions, and prove the generated fixed
  point has no bootstrap references. Failure to remove any temporary authority
  is a G7 failure.

If the owner cannot provide this exact actor, source-ref-capable pre-main
transaction, and temporary old-checker-to-B required-check handoff, the task is
externally blocked at G1. Ordinary PR review, release authorization, or a
post-merge `workflow_dispatch` does not supply these capabilities.

The implementation may carry the one-time workflow/action as a source-owned
bootstrap side file only if the root reviewer explicitly authorizes that
temporary escape hatch and the old validator can reproduce its bytes from the
typed source/config. It must be deleted when the normal typed renderer is
active. A copied handwritten workflow, an admin bypass, or a policy exclusion
is not an acceptable implementation.

### Product publication verification

Record outside the source revision: source SHA, closure, workflow run/attempt,
numeric build/publish job IDs, artifact ID/name/service digest/inner digest,
raw binary digest/size, manifest digest, release ID/tag/target SHA, attestation
subjects/predicate digest, and terminal conclusions. Verify via API and
`gh attestation verify` against the exact publisher workflow path and
`refs/heads/main`. This proves a live product; an uploaded archive or green
wrapper does not.

The release ID must not be claimed or attested before it exists. Use this
draft-first sequence:

1. Query the immutable tag. If a published release exists, require exact
   target SHA, closure, manifest, assets, and attestations; otherwise fail. If a
   draft exists, require the exact expected target/tag and no conflicting
   assets; otherwise fail without deleting or reusing it.
2. Create a draft release with the final immutable tag and exact target SHA.
   Require a nonzero numeric `release.id`, matching `tag_name`, matching
   `target_commitish`, `draft=true`, and no published timestamp. The release ID
   is now a signed input; it was not attested earlier.
3. Download the Actions artifact by numeric ID through the REST ZIP endpoint
   into an isolated temporary directory. Hash the raw service ZIP and require
   equality with the REST `digest` and the upload step's `artifact-digest`
   output. Enumerate and safely extract it; reject absolute paths, traversal,
   links escaping the directory, unexpected files, missing files, or inner
   payload digest/size/architecture mismatch.
4. Write `binding.json` containing the exact run/attempt/database ID, workflow
   path/SHA, numeric producer job/check-run ID and successful step IDs,
   artifact ID/name, raw service-ZIP digest/size, extracted inner-payload
   digest/size, source SHA/ref, closure, and draft `release.id`.
5. Use the current pinned generic attestation action
   `actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6` (v4) with
   `id-token: write`, `attestations: write`, `contents: read`,
   `push-to-registry: false`, and `create-storage-record: false` (no
   `artifact-metadata: write`: this is not an OCI subject). Create the binding
   attestation with:

   ```yaml
   subject-name: velnor-workflow-policy-validator-artifact-<artifact-id>.zip
   subject-digest: sha256:<raw-service-zip-sha256>
   predicate-type: https://tailrocks.dev/velnor/attestation/validator-artifact-binding/v1
   predicate-path: <isolated>/binding.json
   ```

   Verify it with `gh attestation verify <service.zip> --repo tailrocks/velnor
   --signer-workflow tailrocks/velnor/.github/workflows/ci-policy-validator-products.yml
   --source-ref refs/heads/main --predicate-type
   https://tailrocks.dev/velnor/attestation/validator-artifact-binding/v1
   --format json`, then enforce subject name/digest and every predicate field.
6. Write the canonical manifest including the now-existing `release.id`, tag,
   target SHA, release asset names, local SHA-256/size, closure, and the full
   producer binding. Create a release predicate with
   `predicate-type: https://tailrocks.dev/velnor/attestation/validator-release/v1`
   and `predicate-path: <isolated>/release-predicate.json`; attest the manifest
   with the same pinned `actions/attest` action and the same non-OCI permission
   set. Verify using `--predicate-type` and enforce the manifest digest and
   signed release ID/target/source fields.
7. Upload binary, manifest, and sidecars to the existing draft release. Query
   the release REST API and require each asset's name, nonzero ID, size, and
   SHA-256 digest to match the locally signed values. Only then publish the
   draft; re-query `draft=false`, immutable tag, target SHA, and exact asset
   census. No overwrite, mutable `latest`, or release recreation.

`actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8`
may additionally attest the binary's standard SLSA provenance, but that
attestation alone does not establish artifact-job binding or release identity;
the custom predicates and live REST checks remain mandatory.

### Separate PR957 pin adoption

Only after the live validator product is verified:

1. Complete the protected old-checker-to-B handoff first: the base
   `pull_request_target` setup/action must consume B, its candidate acquisition
   path must be removed, and its real `Policy` check must verify the exact PR
   tree. Publication of B alone does not change the base workflow.
2. Set the typed validator pin to the published source SHA/product closure;
   this is the first tree allowed to contain `[policy.validator]`.
3. Keep the application/runtime pin on its last complete published product
   until the full xcode-27 application product exists; never point it at the
   validator product or `9e06...`.
4. Generate the validator action/workflow from the authoritative typed
   renderer. The policy job must consume only the Linux-X64 validator product;
   no candidate binary, `--pin-build`, local checkout, or app runtime product.
5. Update provider admission to the exact official `xcode-27` label and
   regenerate the native application publisher/product matrix in the same
   later epoch. Its full Linux-X64, Linux-ARM64, and xcode-27 macOS-ARM64
   application product must be published and verified before its consumer pin
   changes. This is a separate product publication, not a validator shortcut.
6. Run the new validator on the PR itself and on the merged main SHA. Only
   after both product contracts pass may the temporary bootstrap side files and
   fallback path be removed.

No “skip macOS,” old `macos-15` exception, `macos-latest`, partial matrix,
empty required job, or fallback application asset is part of this sequence.

## Trust/job graph

```text
old-schema source-admission probe (0dc; no [policy.validator], no xcode output)
  └─ protected pre-main transition authority
       ├─ build B from exact final main tree
       ├─ draft-first release + API-bound attestations
       ├─ old-checker-to-B base pull_request_target handoff
       └─ real required check for the exact bootstrap PR
            └─ reviewed merge to main S
                 ├─ first main policy/ci-required run consumes B
                 ├─ complete runtime publisher runs Linux-X64, Linux-ARM64,
                 │  and xcode-27 macOS-ARM64 (no skipped native cell)
                 └─ PR957 typed pin adoption and later consumer verification
```

The bootstrap authority must not wait on a check that itself requires the
unpublished validator B. The legacy `0dc` checker is only a compatibility
probe; it is never B and never a typed validator-product authority. The
base-checker handoff must be real before the pin-adoption PR: publishing B
alone leaves `pull_request_target` on 0dc. The separate pin-adoption PR must
not claim a product until the release and attestation evidence exists.

## Exact transition feasibility

The following transition matrix is the independent ordering review. “Pass”
means all constraints hold simultaneously: no `macos-15`, no skipped required
native job, no candidate semantic authority, exact main source provenance, and
no unpublished/local product.

| Candidate transition | First failing invariant | Result |
| --- | --- | --- |
| Add `[policy.validator]` to the source-admission config before B exists | Legacy `0dc` parser uses `deny_unknown_fields` and rejects the table before rendering/checking | **Reject** |
| Add only dormant source code and an old-compatible `static_files` sidecar | Can pass the old fixed-point probe only if actual `0dc --check` and semantic policy both pass; it does not publish B or change the main macOS/policy ordering | **Compatibility proof only; not a transition** |
| Merge source-only typed support; publish validator B afterward | Current push on `main` still runs existing `ci-runtime-products.yml`'s `macos-15` cell before B exists | **Reject** |
| Publish B from PR/head before merge | Product source/ref is not `refs/heads/main`; candidate/unmerged bytes become the trust anchor | **Reject** |
| Put xcode-27 + typed publisher in the source-admission PR | Base validator `0dc` cannot admit `xcode-27`; its `trusted-runners` rule fails before merge | **Reject** |
| Publish B, then submit a normal pin-adoption PR | Its `pull_request_target` still executes the base branch's old `0dc` setup/checker; publication alone does not hand off the base policy | **Reject until explicit handoff** |
| Let PR policy build/execute its own candidate to admit xcode | Candidate becomes semantic policy authority; explicitly forbidden | **Reject** |
| Replace runtime publisher with Linux-only output | Application runtime loses its required macOS asset and native publication contract; validator/application identities also collide unless a new namespace is added | **Reject** |
| Disable old publisher/native jobs for the merge | Missing/skipped required work is not success; native obligation is dropped | **Reject** |
| Use old runtime product or `--pin-build` as validator B | Wrong product purpose/provenance; local or mutable bootstrap authority | **Reject** |
| Add a protected external/base-owned authority before merge | Can validate/publish the exact main-bound typed product without PR candidate or old-Mac execution | **Conditionally viable; new authority required** |

Therefore a generated PR alone cannot satisfy the requested first transition on
the current branch. The exact missing capability is a protected authority that
can establish the typed validator **before** the first resulting-main workflow
would execute the old matrix, while binding source/ref to the resulting main
SHA. The ordinary in-repository publisher cannot establish its own prerequisite.

The authority must do all of the following, explicitly and reviewably:

1. Provide a protected pre-main builder/publisher whose attested source
   binding is the exact resulting `refs/heads/main` SHA, not a PR/staging ref,
   and whose output is available before that SHA's first policy/main execution.
2. Publish B with the draft-first release and custom artifact-binding
   attestations above.
3. Perform the old-checker-to-B base `pull_request_target` handoff: the actual
   required policy check must execute B against the exact bootstrap PR tree,
   while DCO, `ci-required`, and all native obligations remain required. A
   temporary, exact-PR ruleset/check-context transition is acceptable only as
   an independently reviewed protected mechanism; a merge bypass or green
   wrapper is not.
4. Merge the final tree only after that real handoff check and B release are
   terminal success; the first main push must already render xcode-27 and the
   B-backed policy path.

Neither authority exists in the refreshed current repository state. A temporary
workflow added by the PR is not enough: `pull_request_target` runs the base
workflow before merge, so it still runs `0dc`; publication after that fact does
not change the base workflow; after merge the current push workflow has already
scheduled `macos-15`. A workflow_dispatch race or a commit-message skip does
not establish required checks. If root/user cannot provide this exact
pre-main source-bound publisher plus old-checker-to-B handoff, the precise
blocker is external ordering authority, not an implementation defect that can
be repaired inside the existing product publisher.

## Acceptance matrix

| Requirement | Deterministic check/evidence | Failure disposition |
| --- | --- | --- |
| No obsolete macOS path | publisher job matrix contains only `ubuntu-24.04`; no `macos-15`, old image, alias, skip, or reroute in bootstrap | hard fail |
| Old-schema source admission | actual `0dc --plain --check` and `0dc policy` on exact tree; no unknown `[policy.validator]`; old-owned outputs/sidecars byte-identical | no admission; no parser relaxation |
| Exact source admission | source PR head/base, current legacy checker run, generated-tree bytes, pre-main final SHA, and handoff check | no candidate/pin adoption |
| Typed identity | manifest schema/product/purpose/tag/asset and consumer exact equality | reject namespace collision |
| Exact Linux platform | `platform=Linux-X64`, runner `ubuntu-24.04`, binary architecture probe | reject mismatched asset |
| Source binding | main ref, repository ID, exact SHA/closure, clean checkout, binary self-reports | reject moving/different source |
| Release immutability | draft-first nonzero release ID, exact tag/target, asset census, publish transition, overwrite refusal, no `latest` | reject existing mismatch |
| Build provenance | binary attestation signer path/source-ref plus standard provenance | reject missing/wrong signer |
| Artifact transport | upload outputs equal REST ID/name/service digest; raw ZIP and inner payload digests; safe extraction | reject mismatch/expiry |
| Custom attestation | pinned `actions/attest@1e69f48...`, exact subject name/digest, predicate type/path, `create-storage-record=false`, no OCI permission | reject wrong signer/subject/permission |
| Artifact job binding | signed custom predicate + live run/jobs API exact job/check-run/attempt/steps | reject name/run-only correlation |
| Base checker handoff | B-backed real `pull_request_target` check replaces legacy 0dc path before pin adoption; no failed child hidden by wrapper | reject publication-only handoff |
| Manifest provenance | manifest SHA and attestation subject; embedded producer binding matches all live facts | reject forged manifest |
| No candidate authority | bootstrap has no PR artifact, `--pin-build`, candidate manifest, or local fallback | hard fail |
| Acyclic order | old-schema probe → exact final-tree B publication/verification → base checker handoff → reviewed main merge → live main verify → pin adoption | reject reverse edge |
| Consumer behavior | clean action downloads validator namespace, verifies identity/digest/self-reports, no build fallback | reject runtime namespace/fallback |
| Generated fixed point | typed renderer reproduces workflows/action/state from declared generator pin | reject stale sidecars/output |
| Native obligations | xcode-27/full application matrix is a later explicit product gate, not omitted | keep G1 incomplete |
| Final cleanup | bootstrap side files/fallback removed after typed path succeeds; generated tree reverified | no G7 claim while present |

## Independent adversarial review

Review verdict: **DESIGN CONDITIONALLY VIABLE; NOT APPROVED FOR IMPLEMENTATION**.

The current revision explicitly incorporates the independent review defects:
the old `0dc...` `deny_unknown_fields` boundary, the fact that B publication
does not alter the base `pull_request_target` checker, the draft-first release
ID ordering, and the exact generic-attestation action, permissions, subject,
predicate, ZIP/extraction, and storage-record contract. These are design
corrections, not implementation approval.

The design survives these attacks only if the exact conditions above are
implemented:

* **Namespace substitution:** a runtime tag or manifest cannot satisfy the
  validator action because product ID, schema, purpose, tag prefix, signer
  workflow, asset, and platform are all exact.
* **Stale sibling artifact:** name/run matching is insufficient; artifact ID,
  service digest, signed binding predicate, numeric job/check-run ID, attempt,
  and live API status are all required.
* **Rerun replay:** run attempt and workflow/source SHA are signed and checked;
  a prior attempt or same digest from another run is rejected.
* **Candidate injection:** old-schema admission has no candidate semantic path;
  B is built only from the exact final main-bound tree under the protected
  transition authority. PR957 uses a published validator, not a PR artifact.
* **Publisher privilege drift:** only the fixed main workflow gets contents
  write; build has no release write; no PR/fork event can reach publish.
* **Manifest forgery:** publisher verifies binary, binding, artifact API, and
  attestations before signing the manifest; consumers verify the publisher
  attestation and exact schema.
* **Old-macOS greenwash:** no bootstrap job advertises or runs an old macOS
  label; the full xcode-27 application product remains an explicit later gate.
* **Self-reference:** validator product publication has no dependency on its
  own consumer pin; pin adoption is a later PR only after live publication.

Unresolved authority: current repository state has no existing trusted workflow
that can publish this new product with OIDC provenance, perform the old-
checker-to-B base `pull_request_target` handoff, and leave the app publisher
untouched. Root/user must provide the exact protected pre-main transaction and
temporary required-check handoff above, then independently review its source
binding and removal. Until then this report is a blocker, not a G1 pass.
