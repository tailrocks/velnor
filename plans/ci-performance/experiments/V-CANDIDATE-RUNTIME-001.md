# V-CANDIDATE-RUNTIME-001: premerge generator and runtime products

Status: **design only; no implementation or timing acceptance**.

## Observed failure

The current candidate publisher is in the generated Rust reusable workflow,
after the unit's full checkout, setup, build, and checks. It runs only when
`candidate_publish` is true, the event is `pull_request`, and the head belongs
to the same repository. It uploads a one-day artifact named from a truncated
closure and platform. Its manifest has `profile`, `platform`, `repository`,
`run_id`, PR-head `revision`, `closure`, optional `build_revision`, and
`binary_sha256`; it has no producer job, run attempt, artifact ID/digest,
attestation, or successful-validation receipt.

Both `ci-policy.yml` and the `ci-main.yml` policy path discover sibling
`ci-pr.yml` runs by `head_sha`, poll the first five results, then poll each
run's artifacts for that name. They download by run ID and check the binary
digest and closure, but do not bind a run attempt or immutable artifact ID.
This makes a late producer a correctness dependency of the privileged policy
job. Raw evidence:

- Run `35484349032`, policy job `106007857121`, waited from
  `2026-09-20T02:35:06Z` to `02:50:09Z` and failed with
  `no candidate product ... b510e2700de75668-Linux-X64 was published within
  15 minutes`.
- Run `35485890253`, policy job `106012099388`, waited from
  `03:11:04Z` to `03:26:17Z` and failed with
  `no candidate product ... 1e7d319be3752d36-Linux-X64 was published within
  15 minutes`.
- Successful child jobs in canceled run `35485817832` do not establish a
  successful producer. The workflow conclusion and exact run attempt remain
  part of admission.

The runtime-product producer has a separate guard:
`ci-runtime-products.yml` accepts only `refs/heads/main`, builds release
profile with empty features for Linux X64, Linux ARM64, and macOS ARM64, then
attests and publishes an immutable release. The setup action accepts only
that main-attested release. Jackin and Parallax currently consume the action
and revision through their generated workflows, so a premerge Velnor product
cannot cross that release boundary. A PR Actions artifact also cannot be
assumed readable by another repository's `GITHUB_TOKEN`.

## Alternatives

1. **Move the existing artifact step to the front of `ci-pr`.** A bootstrap
   job builds the debug/tui generator before plan and uploads its artifact;
   policy downloads it by exact producer run and consumers in the same
   repository reuse it. This fixes the observed wait for Velnor, but the
   artifact's normal one-day scope and repository-scoped token do not provide
   Jackin and Parallax a stable cross-repository distribution. It also cannot
   bootstrap a schema that the current pinned renderer cannot parse unless the
   bootstrap workflow is schema-independent.

2. **Use a static, base-owned premerge producer plus exact artifact fanout.**
   A small workflow from the trusted base, independent of generated config,
   builds the reviewed same-repository PR head immediately. It has no secrets,
   no persisted credentials, isolated Cargo state, and no candidate execution
   with a token. It uploads platform/profile/feature-specific products and a
   receipt. Policy discovers that workflow by exact head and workflow ID,
   verifies the exact run and attempt, then executes the generator tokenless.
   This is the first bounded repair for the policy timeout.

3. **Promote the verified artifact into an immutable preview release.** After
   the base-owned policy check succeeds, a separate narrowly-permissioned
   publisher stages the already verified bytes, rechecks every receipt and
   digest, attests the staged assets, and creates a never-overwrite preview
   tag containing the full receipt and platform assets. It never compiles or
   executes PR code. Jackin, Parallax, and Velnor can download that public
   release by an explicit receipt/tag, while ordinary setup continues to use
   main-only releases. This supplies the missing cross-repository boundary.

4. **Give consumer repositories an Actions-read GitHub App token.** The
   consumers download the producer artifact by exact run/attempt and artifact
   ID, with no public release. This avoids release publication but introduces
   a cross-repository secret, fork limitations, token rotation, and a
   service-availability dependency. It is a valid fallback only where the
   App policy is available; artifact name/latest-run lookup remains invalid.

Compiling the PR inside the privileged policy job, publishing a normal main
release, or letting consumers fall back to the latest product are rejected.
The first mixes untrusted execution with policy credentials, the second
erases source/trust identity, and the third permits stale or mixed candidates.

## Recommended bounded architecture

Use alternatives 2 and 3 together. Build once per required identity before
the expensive plan and unit graph, then fan out the exact receipt:

1. A static base-owned premerge producer starts on a same-repository PR and
   checks out the exact PR head. Its build job is isolated, has read-only
   contents permission, no secrets or persisted credentials, and clears token
   variables before any PR build script runs. It does not read `.github-gen` or
   invoke the candidate generator to decide its own shape. This is required
   because a changed generator/schema cannot be trusted to render the bootstrap
   job. Fork PRs produce no candidate for privileged policy or preview release.
2. The producer builds the debug/tui generator product for the policy runner
   and release/empty-feature runtime products for each required consumer
   platform. Identical `(closure, profile, features, platform)` products are
   built once and reused; differing profile or feature products are separate
   identities. Each build uses `--locked`, an isolated Cargo home, a clean
   checkout, no wrapper or ambient flags, and a fixed timeout.
3. The producer proves the binary's own `--closure` and `--revision`, computes
   the binary digest, records the actual build revision, and uploads the
   binary plus receipt. A candidate build receipt is not a successful policy
   result. The policy job must still run the candidate against the audited
   tree and pass its generated-tree and required-check validation.
4. Policy selects one exact producer run. It requires same repository, exact
   PR head, exact workflow identity, `run_attempt`, successful producer
   conclusion, nonexpired artifact ID, artifact digest, platform, profile,
   features, closure, and binary digest. It verifies the producer attestation
   and then runs the binary with `GH_TOKEN` and `GITHUB_TOKEN` empty. No
   consumer may select by truncated artifact name or latest run.
5. A base-owned publisher receives only the policy-verified receipt and bytes
   through a workflow artifact. It has `contents: write` and attestation
   permission only in that isolated job. It never checks out or runs the PR
   binary. It rechecks the source/closure/profile/features/platform contract,
   signs the preview manifest and assets, and creates a unique immutable
   preview release keyed by the complete candidate identity. A rerun with the
   same closure gets a different release identity when its binary or producer
   attempt differs; no tag or asset is overwritten.
6. The setup action gains an explicit preview receipt input. Preview use is
   opt-in and never falls through to the main release. It downloads the exact
   preview tag and platform asset, verifies the publisher attestation,
   receipt, binary digest, and self-reported closure/revision, then caches by
   preview release ID plus platform/profile/features. Mainline consumers keep
   the existing main-attested path.

The three consumer repositories use the same preview receipt: Velnor's own
policy/PR lanes, Jackin, and Parallax. Their source configuration, generated
task graph, required checks, packaging, and provider matrices remain under
their existing declarations. The candidate runtime changes only the runtime
product input; it does not remove checks or silently source-install a
generator.

## Receipt contract

The machine-readable receipt must bind these fields before any consumer runs:

```text
schema
repository
candidate_kind = premerge
source_head_sha
build_revision             # actual checked-out commit; may be a merge SHA
closure                    # binary-reported canonical closure
profile
features                   # canonical sorted feature string
platform
producer.workflow_id/path
producer.run_id
producer.run_attempt
producer.job_id
producer.conclusion = success
artifact.id
artifact.name
artifact.digest
binary_sha256
self_report.revision
self_report.closure
validation.producer_status
validation.policy_run_id
validation.policy_run_attempt
validation.policy_status = success
validation.validator_revision
attestation.signer_workflow
attestation.source_ref
```

`source_head_sha`, `build_revision`, and `closure` are separate facts. Equal
head/merge closures may reuse bytes, but the receipt still records both
revisions. A closure-changing merge must build from the reviewed head or fail
closed. The producer run attempt is never inferred from a callee or from a
top-level timestamp. A canceled workflow, missing page, stale artifact,
malformed receipt, digest mismatch, or missing attestation is unknown and
blocks publication and consumer use.

## Acceptance gates

Before implementation is accepted, prove a fixture for stale/duplicate run
attempts, wrong repository/head, wrong build revision, profile/feature/platform
crossing, artifact ID/digest mismatch, canceled producer, missing artifact,
and missing or invalid attestation. Exercise one same-repository PR through
the early producer, policy, preview publication, and all three consumer
receipts. Preserve the existing full test/check graph and record raw run IDs,
attempts, job timestamps, artifacts, manifests, and conclusions. Measure only
after the exact candidate is consumed; this design supplies no speed claim.
