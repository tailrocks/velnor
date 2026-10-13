# Capability proposal: `actions/attest-build-provenance@v4.1.1`

Status: **approval required; not implemented**.

This proposal is intentionally narrower than the upstream action. Approval of
this document would authorize only the exact Jackin Preview surface below. It
would not authorize generic attestations, SBOMs, registry publication, storage
records, custom predicates, alternate refs, or additional inputs.

## Why Jackin needs it

Jackin PR #810 builds complete preview archive sets in two jobs and attests
`dist/*.tar.gz` before uploading them. A cache hit can skip archive creation;
on a cache miss both attestation steps execute. Velnor currently rejects the
action before job execution, so Preview cannot satisfy V-B parity. Removing or
writer-gating the step would weaken the shared workflow and is forbidden.

Exact call sites revalidated at Jackin PR #810 head
`a27fcdaff7ac4c9562eb01c468f2d45aac0dac6c`:

- `.github/workflows/preview.yml`, `build-preview`: attest the complete Jackin
  archive set.
- `.github/workflows/preview.yml`, `build-jackin-capsule`: attest the complete
  capsule archive set.

Both use only `subject-path: dist/*.tar.gz`.

## Proposed strict capability surface

| Field | Accepted value |
|---|---|
| Action | `actions/attest-build-provenance` |
| Ref | `0f67c3f4856b2e3261c31976d6725780e5e4c373` (`v4.1.1`) |
| `subject-path` | exactly `dist/*.tar.gz` |
| `github-token` | omitted; use the job's default GitHub token |
| `show-summary` | omitted; upstream default `true` |
| `push-to-registry` | omitted; upstream default `false` |
| `create-storage-record` | omitted; inert because registry push is false |
| Every other upstream input | rejected before side effects |
| Required permissions | `contents: read`, `id-token: write`, `attestations: write` |

Allowed combination count is one: the exact ref plus the exact subject glob
and omitted defaults above. The manifest must reject explicit alternate values,
unknown keys, missing/extra subject selectors, custom predicates, registry
push, storage records, subject renaming, checksums, digest mode, and more than
the resulting upstream limit of 1024 files.

## Required native behavior

1. Validate the complete job and permission-dependent action shape before
   checkout or any other side effect, using the existing strict manifest.
2. Expand the subject glob relative to `GITHUB_WORKSPACE`; accept regular files
   only; fail when it matches nothing; cap at 1024; compute SHA-256 by streaming
   each file; use the basename as subject name; deduplicate identical
   name/digest pairs.
3. Generate the same SLSA build-provenance predicate as
   `@actions/attest@3.2.0`, including the GitHub workflow identity and runtime
   environment fields that upstream includes. Approximate predicates are not
   acceptable.
4. Obtain an OIDC token through the job message's current
   `GenerateIdTokenUrl` and access token. For a public repository, use the
   public-good Sigstore instance; otherwise use GitHub's Sigstore instance.
5. Produce and sign a DSSE/in-toto attestation bundle, upload the attestation
   to the current GitHub repository attestation service, and preserve upstream
   certificate/transparency-log behavior. No OCI registry call or artifact
   metadata storage-record call is allowed by this surface.
6. Write the JSON bundle beneath a newly created `RUNNER_TEMP` directory,
   append its path to `RUNNER_TEMP/created_attestation_paths.txt`, write the
   upstream-style job summary entry, and expose:
   `bundle-path`, `attestation-id`, and `attestation-url`.
   `storage-record-ids` must be empty/absent.
7. Mask all bearer/OIDC material. Logs may show subject names/digests,
   certificate information, attestation URL, and public transparency-log id,
   but never tokens or authenticated request headers.

## Failure contract

The action fails the step, with no success outputs, when any of these occurs:

- the OIDC request URL/token is absent or GitHub refuses ID-token issuance;
- the subject glob is invalid, matches no regular file, exceeds 1024 files, or
  a subject cannot be read/digested;
- provenance construction or DSSE signing fails;
- Fulcio/GitHub signing, Rekor transparency logging, or repository attestation
  upload returns a non-success response or malformed result;
- `RUNNER_TEMP` is absent, the bundle cannot be written, or outputs/summary
  cannot be finalized consistently;
- any unapproved ref, input, value, or combination is received.

No retry may hide a terminal 4xx or validation error. Network retry behavior,
if upstream uses it for transient failures, must be bounded and regression
tested before fixture proof.

## Trust, network, and storage implications

- Trust expands from repository-scoped `GITHUB_TOKEN` use to OIDC identity
  issuance and Sigstore signing. The action can create a durable public or
  GitHub-hosted attestation tied to repository/workflow identity.
- New outbound calls include the server-provided OIDC URL, the selected
  Sigstore/Fulcio and transparency-log services, and GitHub's repository
  attestation API. Endpoint derivation must follow upstream; no operator URL
  override is approved.
- Local storage is job-scoped only: bundle JSON and the created-path index in
  `RUNNER_TEMP`, plus the existing GitHub step summary/output files. No new
  daemon-persistent cache, credential, key, OCI, or artifact-metadata store is
  approved.
- Signing keys remain ephemeral inside the Sigstore flow. Velnor must never
  persist private key material or OIDC/GitHub bearer tokens.

## Upstream evidence frozen for implementation review

- `actions/attest-build-provenance` latest release is `v4.1.1`, commit
  `0f67c3f4856b2e3261c31976d6725780e5e4c373`. Its composite delegates to
  `actions/attest@a1948c3f048ba23858d222213b7c278aabede763` and declares the
  inputs/outputs described above.
- That `actions/attest` commit locks `@actions/attest` version `3.2.0` and its
  source proves the subject limit, SHA-256 file hashing, no-match failure,
  public-vs-GitHub Sigstore choice, bundle/temp/output/summary behavior, and
  failure contract.
- Official `actions/runner` `v2.336.0`, commit
  `98aabcd429c4e8402406c56ce2d26387fed3b9ce`, is the runner source of truth:
  `JobRunner.cs` waits for the job to be in progress before OIDC use, and the
  script/Node/container handlers export `ACTIONS_ID_TOKEN_REQUEST_URL` plus
  the system-connection access token. Native Velnor behavior must preserve
  that lifecycle and credential boundary.

Before implementation, re-check all three upstream versions. A newer stable
release requires a refreshed proposal or an operator-confirmed replacement
ref; this document does not silently authorize it.

## Fixture and Jackin proof required after approval

1. Add the exact action/ref/input to the strict manifest and target action
   registry; add positive and negative manifest tests for every rejected input
   class above.
2. Add a fixture job that creates at least two deterministic subjects, runs
   the exact approved call, verifies bundle subjects/digests and all three
   outputs, and verifies the created attestation with GitHub's current
   attestation tooling/API. Add negative jobs/tests for missing OIDC permission,
   no matching subject, and an unapproved input.
3. Run V-A on GitHub and Velnor, compare bundle semantics and visible summary,
   and verify no credential appears in raw or uploaded logs.
4. Re-run Jackin Preview on Velnor, GitHub, and `both`; require both archive
   jobs green, identical subject sets/digests, one attestation per lane/job as
   the unchanged workflow specifies, then run the required warm/no-change
   performance proof.

## Approval question

Approve exactly the capability surface in this document: yes or no. A yes
authorizes native Rust implementation and the stated fixture/Jackin proof; it
does not authorize any adjacent attestation input, registry/storage behavior,
ref, or action.
