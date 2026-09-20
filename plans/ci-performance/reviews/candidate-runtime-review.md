# Candidate runtime bootstrap: independent review

Date: 2026-09-20

Scope: `plans/ci-performance/experiments/V-CANDIDATE-RUNTIME-001.md`, the
published runtime product, the current policy bootstrap path, and the
pre-merge candidate contract. This review is independent of the candidate
design author. It is a design and compatibility review; it is not a timing or
CI acceptance result.

## Verdict

**HOLD for the proposed end state.** The product identity and trust direction
are right, but the current plan still has a bootstrap cycle and an unsafe
admission boundary:

* A changed generator cannot require a new runtime schema before the trusted
  base workflow can acquire and validate that runtime. A `pull_request_target`
  workflow uses the workflow from the base/default branch. It cannot provide a
  new base-owned producer introduced only by the candidate PR.
* A PR build receipt is not a policy-validation receipt. Producer job success
  is enough to make a candidate available for an early same-run policy step
  only when its exact job, attempt, artifact, and digest are proven. It cannot
  satisfy the later required checks or authorize publication.
* Selecting one of the first five runs by head SHA and an artifact name leaves
  run attempts, duplicate producers, canceled runs, artifact identity, and
  workflow provenance unbound. A successful child job in a canceled workflow
  is not successful producer evidence.
* One Linux x64 binary cannot serve macOS ARM64 or Linux ARM64. Runtime
  compatibility is an explicit product identity, not a display label.
* A privileged policy job must never execute an untrusted PR binary with its
  token, secrets, or inherited write authority. Digest and closure checks do
  not make that execution boundary safe.

The immediate compatibility route is viable: retain the already published
base pin `0dc79895ff1c5e88be7c3822c437e1c5b5282e12` while a candidate artifact
is built and consumed through a separate candidate slot. Runtime 325 is also
published and attested; the defect is that the base policy still runs 0dc
while the audited tree requests 325, then places the candidate bytes in the
declared-pin slot. This route removes that role mismatch, but it does not by
itself fix artifact admission, fork policy, platform fanout, or preview
publication.

## Evidence

The cited policy failures waited for a late candidate publisher:

* Run `35484349032`, policy job `106007857121`, waited from
  `2026-09-20T02:35:06Z` through `02:50:09Z` and failed because no candidate
  product was published within 15 minutes.
* Run `35485890253`, policy job `106012099388`, waited from
  `03:11:04Z` through `03:26:17Z` and failed for the same reason.
* Run `35485817832` had successful child jobs but was canceled. Those jobs do
  not establish a successful producer without the terminal run/attempt
  contract.

The published runtime release was fetched from GitHub's release API on
2026-09-20:

* Tag: `velnor-workflow-runtime-v1-8b96d5108550dfa6`.
* Release body binds closure
  `8b96d5108550dfa61a57ff65c6c357b4119493c660417bf3beb4af2b03742269` to
  source commit `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`.
* Linux ARM64 asset ID `575446820`, digest
  `sha256:31573446bc44f5be82ba75ec6413d74a66987cf00e1c31715e74bae0a52a4993`.
* Linux x64 asset ID `575446824`, digest
  `sha256:915e8112c0f5307741cc6c58b6907d5cfaf6488f23c56e6e8dc78d1ff9f545d1`.
* macOS ARM64 asset ID `575446821`, digest
  `sha256:1bba241e5c13a90b307233808b845230117a571a3c645b8b1c9849e6ceafcc82`.
* Release manifest asset ID `575446822`, digest
  `sha256:a13b01e06421faf616bc5a8095bd815a6dc73b4c8c7ef00a9c147c9ffa7691d9`.

The newer candidate-compatible runtime is also published and attested:

* Tag: `velnor-workflow-runtime-v1-f3ff48920e7bd0d1`.
* Release body binds closure
  `f3ff48920e7bd0d1749eedd3cf290b3b4d31346cf3ebb318b6a5461f7a91bb08` to
  source commit `325719f1e05d3d46322c9fd3eeb9ad545e175638`.
* Linux x64 asset ID `575839655`, digest
  `sha256:08900ff69d1886cd4126104797747c802c85e98d49653497d78f2433266d4cbb`.
* Linux ARM64 asset ID `575839657`, digest
  `sha256:50c67557c1637cb7f8525a39c4179f9eae78e9c9c030b03967362ba0b60587f2`.
* macOS ARM64 asset ID `575839654`, digest
  `sha256:7314c717d722a290e3e23c327430201c898751ffa9f57c1b86b4ecf13a652090`.
* Release manifest asset ID `575839656`, digest
  `sha256:81974ec329402ff6b8eb016f71441cb2c40c86a1e6c157014c00bb3239ca6015`.

The macOS ARM64 product was downloaded to
`/tmp/velnor-runtime-0dc-macOS-ARM64`, and independently reported:

```text
--revision  -> 0dc79895ff1c5e88be7c3822c437e1c5b5282e12
--closure   -> 8b96d5108550dfa61a57ff65c6c357b4119493c660417bf3beb4af2b03742269
sha256      -> 1bba241e5c13a90b307233808b845230117a571a3c645b8b1c9849e6ceafcc82
```

The same 0dc product generated the current Velnor source tree into the
isolated directory `/tmp/velnor-old0dc-render-write` with exit code 0. The
current checked-in TOML is schema 3 and contains the current unit fields
(`watch`, `pr_commands`, `full_commands`, `depends_on`, `capabilities`, and
`workspace_check`), so the published base runtime can parse the present
configuration surface. This is a compatibility observation only. The
source-owned candidate at commit `217996550d737fc2d26b99f08be7e938c7b4af43`
was then rendered after changing only the source pin to 0dc; that render kept
the current MBX 1.12.0 template and directory transport. The isolated
candidate render produced no `1.11.1` output, and its generated textual diff
was pin-only apart from derived ownership-state digests. This does not prove
the old validator implements every current policy feature; generated bytes,
required coverage, and execution semantics still need independent CI review.

The old product was also run against the current source with
`--dry-run --plain`; it completed the scan without mutating the checkout.
The output directories above are disposable experiment products, not
published evidence. See the controlled pin propagation record in
`experiments/V-PIN-0DC-001.md`.

## Root cause

Candidate production is placed after the expensive unit graph. The policy
workflow then treats a sibling run and a short artifact name as if they were
an immutable producer contract. This makes availability depend on the very
checks that need the candidate runtime and allows stale, duplicate, canceled,
or cross-attempt products to enter an elevated consumer. The architecture
also conflates three identities:

1. the requested reviewed generator revision or pin;
2. the actual source/head or merge revision checked out by the producer; and
3. the binary closure and platform/profile/features compatibility identity.

The current runtime product path already demonstrates the needed separation:
main-only release products have immutable tags, per-platform assets, a
manifest, digest verification, and a published source closure. The premerge
path must use the same identity discipline while keeping PR execution
untrusted and separate from policy authority.

## Alternatives and decision

### A. Early PR job in the existing `pull_request` run

Add a static generator-emitted `candidate-bootstrap` job before plan/unit
jobs. It checks out the exact PR head, builds only the Linux x64 candidate
needed by the Linux policy runner, uploads the binary and receipt, then the
policy job consumes the exact job result. This is the smallest latency repair
and does not require a base workflow merge. It is unprivileged and can run for
same-repository PRs. Fork uploads and cross-repository consumption must be
explicitly unsupported or separately authorized; a fork token is read-only and
has no secrets.

This is feasible as the next implementation, but the candidate workflow shape
must be static and base-reviewed. It cannot be selected by the candidate's
own generated plan. A changed runtime schema still requires a two-phase
rollout: first acquire a compatible product with the old runtime, then pin
and regenerate the new schema after that product is available.

### B. Static base-owned premerge producer

Merge a small trusted workflow first. It builds the reviewed same-repository PR
head without reading generated candidate configuration, clears token-like
environment variables before build scripts run, and uploads a receipt. Policy
discovers it by exact workflow identity and run/attempt. This gives the safest
bootstrap boundary and supports a later platform matrix, but it needs a
preparatory base change before it can bootstrap a schema-changing PR.

### C. Immutable preview release after full policy success

A base-owned publisher receives only policy-verified bytes and receipts. It
does not checkout or execute PR code. It rechecks source, closure, profile,
features, target, platform, artifact, and binary digests; attests the assets;
and creates a unique preview tag. Jackin and Parallax consume that exact
receipt. This solves cross-repository distribution, but it is downstream of
the early candidate path and must not be used to hide candidate-build cost.

Decision: implement A as the bounded immediate path, with B as the trusted
follow-up and C only after policy validation. Use the 0dc matching-pin route
for the first schema-compatible rollout. Do not introduce a parser alias or
silently fall back to an old runtime after the new schema is admitted.

## Required admission contract

The producer must emit an immutable receipt containing at least:

```text
schema
repository
workflow_id/path
run_id
run_attempt
producer_job_id/name
producer_conclusion = success
source_head_sha
build_revision
generator_closure
project_or_config_digest
platform
target
profile
features
artifact_id
artifact_name
artifact_digest
binary_sha256
self_report.revision
self_report.closure
```

The policy verifier must query the exact run and attempt, enumerate all pages
of the exact producer jobs endpoint, require the named producer job to be
terminal `success`, require the artifact to be present and unexpired, verify
the artifact ID and archive digest, then verify the receipt and binary digest.
Missing pages, duplicate producer jobs, a canceled run, a stale attempt, a
wrong repository/head, or any digest/profile/platform mismatch fail closed.
Selecting by truncated artifact name or “latest” is forbidden.

Candidate availability is separate from validation. A same-run early policy
step may use a successful bootstrap job after those checks, but publication
and consumer release require the complete required workflow and policy receipt
to be successful. The candidate build itself cannot declare its own validation
success.

The policy process must execute the candidate with `GITHUB_TOKEN`, `GH_TOKEN`,
and all secret-bearing environment variables cleared. Fork candidates remain
untrusted and cannot publish or satisfy privileged policy without a separately
verified producer boundary. The producer must record source head and actual
build revision separately; equal closures may reuse bytes, but a changed
closure or incompatible merge must rebuild.

## Base-0dc compatibility experiment

The existing release proves a usable macOS ARM64 product. The Linux x64 asset
was intentionally not executed on macOS and returned `exec format error`; the
platform matrix must therefore remain explicit.

The immediate compatibility sequence is:

1. Keep the **base-owned** policy workflow's validator pin at
   `0dc79895ff1c5e88be7c3822c437e1c5b5282e12` while the candidate path is
   introduced. Runtime `325719f1e05d3d46322c9fd3eeb9ad545e175638` is already
   published and attested, but the existing base workflow still runs 0dc and
   cannot consume 325 as its trusted policy role until its own trusted policy
   workflow and generated contract are updated together. The PR candidate
   must use a separate candidate role; never place it in the pin slot.
2. Capture the trusted base binary before any candidate resolution. Use a
   separate candidate binary/manifest slot. The base executable proves the
   published pin; the candidate artifact is accepted only after receipt and
   digest verification.
3. Build one Linux x64 candidate for the Linux policy job. Add Linux ARM64
   and macOS ARM64 only for consumers that actually execute the runtime; do
   not claim the x64 product covers them.
4. Validate the old 0dc product against the current schema-3 project (done:
   scan/generation exit 0), then separately compare generated workflow bytes,
   MBX 1.12 cache routing, required checks, and actionlint. TOML parsing alone
   is insufficient.
5. Once the candidate runtime is published/verified, migrate the schema and
   regenerate all consumers against that exact product. The old parser is not
   a compatibility shim.

This route removes the current pin mismatch without weakening source or trust
identity. It does not yet prove a 10x speedup or candidate artifact latency;
those require real controlled CI runs after the admission path is implemented.

### Controlled old-base replay

I replayed the proposed sequence in an isolated copy at
`/private/tmp/velnor-route-check-20260920-1` using the current candidate source
and generator binary, after changing only the copied
`.github-gen/velnor-workflow.toml` pin to 0dc. The current source regenerated
the workflow tree successfully. The temporary source snapshot was committed
as `d388d1618a04a5f78c4467e2b96881611a3654d4`; its candidate binary reported
closure
`1bee82ce87b556db57a3b58a2e2ff8f5780915564a31f78efcb2f7fd0205cc54`, and the
manifest digest was checked before execution.

The old 0dc validator
`/tmp/velnor-runtime-0dc-macOS-ARM64` then ran against that regenerated tree
with its own executable as the matching 0dc renderer. With the historical
candidate slot (`VELNOR_WORKFLOW_PINNED_BINARY`) plus the manifest, its
`generated-tree` rule **passed** with the candidate closure. The process still
returned 1 because this local replay cannot satisfy the live GitHub ruleset
contexts (`required-checks` only); that is unrelated to renderer selection.
Raw output: `/tmp/velnor-route-old0dc-oldslot-rerender.log`.

The same isolated source tree also contained an unmerged forward variant with
the separate `VELNOR_WORKFLOW_CANDIDATE_BINARY` slot. Running that newer
variant against the old 0dc validator returned `generated-tree` failure: 0dc
does not know the new slot and correctly rendered only the pin. Raw output:
`/tmp/velnor-route-old0dc-newslot-rerender.log`. That result is a migration
boundary for the future role split, not a failure of the committed
21799655/current-source route: the committed generated policy still uses the
historical `VELNOR_WORKFLOW_PINNED_BINARY` candidate slot, and that is the
contract exercised by the successful old-slot replay above.

The separate-role ABI must be deployed with its base validator before its
generated slot is emitted. The current pin correction deliberately keeps the
committed old-slot template; a later role-fix unit must use a pre-plan,
receipt-bound candidate bootstrap and update the validator and generated
consumer together.

## Executed source-owned handoff

The executed route is: change `.github-gen/velnor-workflow.toml` to the
published 0dc base, then regenerate with the current candidate source at
21799655. The isolated render and repeated-render comparison prove that this
route preserves the current MBX 1.12.0 templates, cache keys, candidate
product path, and required coverage. The generated textual changes are the
325-to-0dc pin replacement plus derived ownership-state digests. The earlier
hypothesis that rendering current source with the 0dc declaration would
discard MBX 1.12.0 is rejected by this observation.

Changing the source pin without regenerating still creates a generated-tree
mismatch. The old 0dc executable remains the base validator selected by the
deployed target-branch policy workflow; it is not used to regenerate the
current source surface. This is a propagation correction, not the later
separate-role candidate bootstrap.

The exact structural seam is in:

* `crates/velnor-workflow/src/lib.rs`:
  `PolicyJobSpec`, `policy_candidate_step`,
  `workflow_runtime_setup_with_install_rev`, and the policy renderer. The
  source candidate path now names a separate candidate binary, but the base
  workflow currently deployed from main still exports the downloaded
  candidate as `VELNOR_WORKFLOW_PINNED_BINARY`.
* `crates/velnor-workflow/src/s2/mod.rs` and
  `crates/velnor-workflow/src/s2/primitives/ir.rs`: the schema-2 equivalent
  policy and candidate emitters. Both must retain separate base/pin and
  candidate roles at the schema cutover.
* `crates/velnor-workflow/src/policy.rs` and `src/s2/policy.rs`: resolver
  slots and candidate binding. The focused
  `declared_pin_and_candidate_use_distinct_renderer_slots` tests pass in both
  schema implementations; this proves the source runtime seam, not the old
  base YAML deployment.
* `.github-gen/velnor-workflow.toml`: source-owned D19 pin. The isolated
  propagation proof sets it to the published 0dc base runtime and regenerates
  from current candidate source; parent integration must review the generated
  diff and hosted policy result together.

The deployed base workflow at `origin/main` still has `rev: 0dc`, uses the
old `ci-policy.yml` acquire step, and exports its candidate artifact into the
pin slot. Therefore the next safe rollout is a two-step base-owned change:
first deploy the separate-role policy contract while keeping 0dc as the
trusted base validator; then allow the current generator tree to request the
published 325 product. Do not solve this by changing the candidate closure
check or by treating 325 as an old 0dc product.

## Primary references

* [GitHub `pull_request_target` security](https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target)
  explains that the base/default-branch workflow has elevated authority and
  must not execute untrusted PR code.
* [Workflow events and fork behavior](https://docs.github.com/en/actions/reference/events-that-trigger-workflows)
  documents base-branch behavior for `pull_request_target` and restricted fork
  permissions.
* [Workflow syntax and permissions](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
  documents read-only fork token behavior.
* [Workflow run REST API](https://docs.github.com/en/rest/actions/workflow-runs)
  and [workflow jobs REST API](https://docs.github.com/en/rest/actions/workflow-jobs)
  define the run/attempt/job evidence that the verifier must bind.
* [Artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)
  documents provenance verification; an artifact name alone is not provenance.

## Acceptance fixtures

Before accepting implementation, test stale and duplicate attempts, wrong
repository/head, wrong build revision, profile/feature/platform crossings,
artifact ID/archive digest mismatch, canceled producer, missing artifact,
missing attestation, and an artifact from a successful child job inside a
canceled workflow. Run a same-repository PR through the early producer and
policy path, then separately verify preview publication and the three platform
consumer receipts. Preserve all existing required checks and record run IDs,
attempts, job IDs, artifact IDs, raw timestamps, conclusions, and digest
evidence. No performance result follows from this design review.
