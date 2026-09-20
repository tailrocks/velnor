# Velnor bootstrap authority amendment v4

Status: **approval required; no mutation performed**.

This is the immutable successor to v3
(`AUTHORITY-CHANGE-PLAN-2026-09-20-v3.md`, Markdown SHA-256
`eb130052633ba5a2fec067d835a6a2c2d4e7c89c567563d618f7902a4ca54871`, JSON
SHA-256 `d1c4145af0fb9e11d007881436232e19552469e78572db28f80f27a3fb7b7beb`).
The v3 pair remains frozen and is not an approval record. This v4 freezes the
same coherent design with the measured closure and publisher-contract
corrections below:

1. **Tree A:** source/generator transition. It has no `[policy.validator]`,
   permanent B pin, candidate path, old checker, old setup/runtime identity,
   or old macOS path. One external App check (`Policy-bootstrap-A`) admits the
   exact PR head plus synthetic merge tree. It is not a product and carries no
   Actions artifact/run/release provenance.
2. **Main B:** the first actual resulting `refs/heads/main` revision. Its
   main push starts the source-owned B publisher, which publishes typed
   Linux-X64 B from the real main SHA; its downstream live verifier emits
   `Policy-bootstrap-B`. `ci-main`'s `policy-validator-B` job consumes that
   exact result by bounded live API binding, and `ci-main.Policy` has a direct
   `needs` edge to that verifier. The full Linux-X64/Linux-ARM64/xcode-27
   application closure runs.
3. **Tree B:** a separate reviewed PR. The trusted base policy workflow reads
   one exact typed adoption file from this PR and validates the already-live
   Main-B binding before emitting normal `Policy`. The PR then materializes the
   immutable B pin and permanent OIDC/Sigstore trust policy.

This is a narrow proposed amendment to the ordering authority, not a claim
that a future main ref can already publish. Current GitHub APIs provide no
truthful future-main release provenance before that ref exists. If the owner
does not approve this temporary Tree-A authority and its protected lease,
G1 remains externally blocked. No candidate, old macOS, skipped job, mutable
tag, local binary, or compatibility fallback is accepted.

No source edit, workflow dispatch, App installation, ruleset update, release,
package publication, merge, or host operation is authorized by this document.
All placeholders and independent reviews are hard gates.

Observed UTC: `2026-09-20T00:27:10Z`.

## 1. Revision-bound facts and complete affected closure

At this observation, `tailrocks/velnor` main is
`d20d4d1d17590cca85b501d982cbaad70d42c641`, parent
`1048337062ea625fada1b4f7c07f2feed75f60c7`. Its latest workflow routes Apple
jobs to forbidden `macos-26`; the required native label is exact `xcode-27`.
Runtime-products run `35475920678` used `macos-26` and completed; the same
push's Preview run `35475920808` and CI/Main run `35475920826` failed. These
are historical facts, not accepted evidence.

The d20 config pins generator revision
`0dc79895ff1c5e88be7c3822c437e1c5b5282e12`. Tree A must record a different,
reviewed immutable generator revision if S2 changes; it must never claim that
the old 0dc parser or generator admitted the new graph. A missing reviewed
target revision, unpublished local binary, or mutable revision is a hard stop.

The canonical read-only Tree-A closure inventory is observed at
`2026-09-20T00:27:10Z`, main `d20d4d1d17590cca85b501d982cbaad70d42c641`, tree
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
plus the separate `ci-runtime-products.yml` application publisher and the
new generated `ci-policy-validator-products.yml` B publisher. Every
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
BASE_SHA=d20d4d1d17590cca85b501d982cbaad70d42c641
SYNTHETIC_MERGE_SHA=<40-hex>
SYNTHETIC_TREE_SHA=<40-hex>
TRANSACTION_ID=<unique>
LEASE_ID=<signed lease record>
WATCHDOG_ID=<independent recovery actor>
TREE_A_RULESET_HASH=<canonical full-PUT hash>
MAIN_B_VERIFIER_APP_ID=<real read/checks-write App>
MAIN_B_INTEGRATION_ID=<real Main-B Checks provider integration>
MAIN_B_VERIFIER_REVISION=<40-hex reviewed revision>
TREE_B_PR_NUMBER=<real positive integer>
```

### 3.2 Main-B publisher and downstream verifier: `Policy-bootstrap-B`

Tree A generates `.github/workflows/ci-policy-validator-products.yml` from
the source-owned workflow contract. Its jobs are isolated:

* `build-linux-x64` and artifact verification: `contents:read`,
  `actions:read`; no OIDC, attestation, release, package, or secret access;
* `reserve-release` and final `publish`: `contents:write` only for immutable
  draft/assets/publication, with no build or signing token;
* `attest`: `contents:read`, `actions:read`, `id-token:write`, and
  `attestations:write`, no `contents:write`; it downloads the immutable
  artifact by numeric ID/digest and runs no PR-supplied build code; and
* downstream `verify-B`: read-only live API/attestation verifier. It has the
  separate Main-B Checks integration/provider and is the only job that emits
  `Policy-bootstrap-B` after release and binding verification. Its owner-
  controlled App has Metadata/Contents/Actions read and Checks read/write
  only; it has no ref, merge, release, package, workflow, secret, deployment,
  or bypass permission.

The publisher workflow is executable only for the real main push:
`on.push.branches=[main]` and every publishing job requires
`github.event_name == 'push' && github.ref == 'refs/heads/main'`. A
`workflow_dispatch` invocation is diagnostic/non-publishing only: it cannot
reserve a release, attest, publish, or emit `Policy-bootstrap-B`. No
pull-request event can enter the publisher.

The exact publisher-workflow DAG and immutable output transport is:

```text
build-linux-x64
  -> artifact-verify
  -> reserve-release
  -> attest
  -> publish
  -> verify-B

ci-main.policy-validator-B --(bounded exact-SHA API handoff)--> ci-main.Policy
```

`build-linux-x64` outputs the numeric artifact ID/name, service-ZIP digest and
size, inner-payload digest and size, binary digest/size/architecture, source
SHA/tree/closure, and producer job key/database ID. `artifact-verify` must
download by numeric ID and re-compute every digest. `reserve-release` outputs
the nonzero release ID, immutable tag, and exact target SHA. `attest` consumes
only verified artifact/release outputs. `publish` consumes the attestation and
binding outputs and has no build/signing token. `verify-B` consumes the live
published records, checks every output against REST/attestation APIs, and
exports the complete binding record. The separate
`ci-main.policy-validator-B` job polls that publisher run by exact SHA, run,
attempt, job, artifact, release, and binding digests, then passes the verified
record to `ci-main.Policy` through declared job outputs/artifact digest.
`ci-main.Policy` has `needs: [policy-validator-B]` and rejects any output whose
`source_sha` is not the current `github.sha`. No artifact name, run name, or
check name is an identity substitute.

The B product is distinct from the application runtime:

```text
product_id = velnor-workflow-policy-validator
schema = velnor.workflow-policy-validator.v1
purpose = policy-validator
platform = Linux-X64
runner = ubuntu-24.04
profile = release
features = empty
tag = velnor-workflow-policy-validator-v1-<closure16>
asset = velnor-workflow-policy-validator-Linux-X64
source_ref = refs/heads/main
source_sha = RESULTING_MAIN_SHA
mutable_latest = false
overwrite = false
```

Draft-first: build and upload immutable artifact; reserve a unique nonzero-ID
draft release at exact main target; fail if the tag or a release with that tag
already exists (never reuse, mutate, or delete a stale draft); fetch raw REST
ZIP and verify service-ZIP, inner payload, binary digest/size/architecture and
self-report; create one canonical binding JSON after the real release ID and
producer records exist; define `binding_digest` as its SHA-256; attest custom
artifact/release predicates with pinned
`actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6` from the isolated
`attest` job; write the final manifest only after the real release ID exists;
upload and publish once; then verify `draft=false`, exact tag/target/assets,
binding digest, and all live APIs. The attestation subject is the immutable
service ZIP (`subject.name` exact asset filename and
`subject.digest.sha256` exact service-ZIP digest), with predicate type
`https://velnor.dev/attestations/policy-validator-binding/v1` and canonical
predicate path `attestations/velnor-policy-validator-binding.v1.json`. The
predicate includes the canonical binding, its digest, release ID/tag/target,
source SHA/tree/closure, publisher workflow path/SHA, run/attempt/database ID,
producer job key/database ID, producer/check IDs, artifact ID/name, service
ZIP/inner-payload/binary digests and sizes, and required-step conclusions.
The final manifest records the binding and attestation bundle digests but is
not included in the binding preimage, avoiding a circular digest. No
application publisher, candidate artifact, or temporary App creates B.

The custom binding must include repository ID, actual main SHA/ref/tree/closure,
publisher workflow path/SHA, run ID/attempt/database ID, producer job key and
database ID, producer job and check-run IDs, artifact ID/name/service digest,
raw ZIP/inner payload/binary digests/sizes, release ID/tag/target, signer,
predicate type/path/subject, binding digest, and every required step
conclusion. Names and run IDs alone are not proof. The verifier must
cross-check these fields against live Actions, release, artifact, raw-asset,
source-tree, and attestation APIs; a signed predicate supplements but never
replaces those live checks.

### 3.3 Tree-A consumers and Tree-B adoption

`ci-main` calls reusable `policy-validator-B`; its `Policy` job has a direct
same-workflow `needs: [policy-validator-B]` dependency and validates B outputs.
The first Main-B `Policy` result is B-backed through this live producer; it
does not require a literal pin yet. `ci-main` `workflow_dispatch` is an
audit-only/non-publishing path; it cannot invoke B reserve, attest, publish,
or release jobs. Only the actual main push enters the publisher guard.

Base-owned `ci-policy.yml` contains one exact adoption path. Tree A has no
adoption file and emits only a non-required audit. The Tree-B PR adds exactly
`.github/ci/validator-pin-adoption.json`:

```json
{
  "schema": "velnor.policy-validator-pin-adoption.v1",
  "mode": "tree-b",
  "transition_id": "<owner-reviewed>",
  "tree_b_pr_number": "<positive-integer>",
  "target": {
    "config_path": ".github-gen/velnor-workflow.toml",
    "config_sha256": "sha256:<exact Tree-B config blob>",
    "generator_revision": "<exact reviewed immutable Tree-A generator revision>",
    "schema": "velnor.workflow-policy-validator.v1",
    "template_sources": [
      {
        "path": ".github-gen/sources/workflows/ci-policy-validator-products.yml",
        "sha256": "sha256:<exact source blob>"
      },
      {
        "path": ".github-gen/sources/actions/setup-velnor-policy-validator/action.yml",
        "sha256": "sha256:<exact source blob>"
      }
    ],
    "static_files": [
      {
        "file": ".github/workflows/ci-policy-validator-products.yml",
        "source": ".github-gen/sources/workflows/ci-policy-validator-products.yml",
        "sha256": "sha256:<exact generated blob>"
      },
      {
        "file": ".github/actions/setup-velnor-policy-validator/action.yml",
        "source": ".github-gen/sources/actions/setup-velnor-policy-validator/action.yml",
        "sha256": "sha256:<exact generated blob>"
      }
    ],
    "generic_s2": "estate-neutral; no product, repository, provider, pin, or consumer hardcode"
  },
  "main_b": {
    "source_sha": "<RESULTING_MAIN_SHA>",
    "release_id": "<nonzero>",
    "tag": "velnor-workflow-policy-validator-v1-<closure16>",
    "artifact_id": "<nonzero>",
    "artifact_digest": "sha256:<64-hex>",
    "binding_digest": "sha256:<64-hex>",
    "signer_workflow": ".github/workflows/ci-policy-validator-products.yml"
  }
}
```

The trusted base workflow reads that PR file and queries live release,
Actions-run/attempt/job/artifact, raw ZIP, attestation, source-ref, and exact
tree APIs. It emits normal `Policy` only if PR identity, transition ID,
source SHA, release ID/tag, artifact/binding digests, signer, target config and
source/template/static-file hashes, and generated pin all match the verified
Main-B record. Missing or incomplete target mapping means non-required audit;
it never means old checker, candidate, local, alias, mutable latest, or skip.

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
RESULTING_MAIN_SHA&branch=main&event=push`, `GET
/repos/tailrocks/velnor/actions/runs/<RUN_ID>/attempts/<ATTEMPT>/jobs?per_page=100`,
`GET /repos/tailrocks/velnor/actions/runs/<RUN_ID>/artifacts?per_page=100`,
`GET /repos/tailrocks/velnor/actions/artifacts/<ARTIFACT_ID>`, `GET
/repos/tailrocks/velnor/releases/<RELEASE_ID>`, and the paginated release-asset
download plus attestation APIs. The workflow path, run ID, attempt, and exact
SHA are checked against the signed binding on every poll.

1. Query workflow runs by exact repository, workflow path, `head_sha`, branch,
   and expected event predicate. Require the recorded run ID and attempt to
   match the Main-B transaction; zero or multiple applicable candidates is a
   failure, not a choice by name or recency.
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

Ruleset contexts are staged and never claim two providers under one name:

```text
before       = {DCO, ci-required, Policy}
Tree-A       = {DCO, ci-required, Policy-bootstrap-A}
Main-B       = {DCO, ci-required, Policy-bootstrap-B}
Tree-B/final = {DCO, ci-required, Policy}
```

Ruleset update is documented full-object `PUT`; no ETag/CAS is assumed. A
signed external lease freezes every main writer except the named protected
merge operator, including operational freeze/audit of bypass actor 5. An
independent watchdog owns a separate recovery key, polls main/ruleset, expires
before operator tokens, and may conditionally restore only when the live
ruleset hash equals the exact temporary hash and main is still the locked
pre-merge SHA. It never unconditionally overwrites a concurrent ruleset.

The chosen main mutation is normal protected PR merge under this proven
exclusive lease. The merge API `sha` guards only PR head, so the lease is the
explicit base-race guard. A disposable repository with the same ruleset must
prove no competing writer, preserved DCO/ci-required/review enforcement, no
bypass, and retained PR merge attribution. Direct pre-created ref update is
rejected because it can lose attribution or bypass required checks. If the
disposable proof or writer freeze is unavailable, stop before any mutation;
that is an external capability blocker.

The operator transcript must bind every request to the lease transaction:
`GET /repos/tailrocks/velnor/git/ref/heads/main`, `GET
/repos/tailrocks/velnor/pulls/<N>`, `GET /repos/tailrocks/velnor/rulesets/19573071`,
`GET /repos/tailrocks/velnor/commits/<SHA>/check-runs`, and the paginated
workflow-run/job APIs before each ruleset write. Ruleset changes use the
documented full `PUT /repos/tailrocks/velnor/rulesets/19573071` body whose
canonical hash is recorded; no undocumented `If-Match`, ETag, or optimistic
CAS claim is allowed. Merge uses only
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
5. Main-B starts all generated jobs. The publisher push/main guard admits only
   the real `refs/heads/main` push; workflow dispatch is diagnostic and
   non-publishing. Within the publisher run, the exact DAG is
   `build-linux-x64 -> artifact-verify -> reserve-release -> attest -> publish
   -> verify-B`. `verify-B` emits `Policy-bootstrap-B` only after live binding
   checks; reusable `policy-validator-B` exports that immutable record and
   `ci-main.Policy` runs only through direct `needs` and exact SHA validation.
   No cross-workflow `workflow_run`, dispatch wrapper, or check-name shortcut.
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
7. After Main-B live B binding and census pass, full-`PUT` ruleset transition
   Tree-A → Main-B; read/hash. Verify normal Main-B `Policy` is B-backed via
   same-run B, then full-`PUT` Main-B → final `Policy`; read/hash **before
   opening Tree B**. Thus Tree B is evaluated by the exact base-owned adoption
   path under final `Policy`, never by a deadlocked `Policy-bootstrap-B`
   requirement.
8. Open Tree B. Its base-owned adoption verifier consumes the exact PR file
   and live Main-B IDs/digests. Review, pass all eleven ordinary workflows,
   merge normally, and verify resulting main. Only now materialize/retain the
   literal B pin as permanent trust.
9. Remove temporary A context, App, verifier config, lease/watchdog
   transition permissions and any bootstrap files. Never remove permanent B
   publisher/trust. Delete old 0dc/candidate/setup/runtime/macOS aliases from
   every generated/source path before Tree A; clean regeneration proves none
   remain after Tree B.

## 5. Evidence and independent approval gates

Evidence outside source must include the full premerge and Main-B machine
census, exact source/tree/closure, all run/attempt/job/check/artifact/release
IDs, raw ZIP/inner/binary/manifest digests, signer/predicate, native matrix,
Preview applicability, Release predicate/non-applicability, ruleset before/
each-after hashes, signed lease/watchdog recovery, and Tree-B final proof.

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
inspect this exact v4 pair, current d20 facts, staged contexts/provider IDs,
old-parser fixture and new-render byte hashes, full closure, lease/freeze,
signer isolation, cross-workflow API handoff, native census, and cleanup. The
author cannot approve. All App/operator/watchdog/trust IDs remain unresolved
placeholders. Until owner approval plus both reviews and all preflight proofs,
this plan does not authorize implementation, source changes, GitHub changes,
release, merge, or G1 completion.
