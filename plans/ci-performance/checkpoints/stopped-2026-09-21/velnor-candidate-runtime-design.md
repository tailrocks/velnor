# Candidate runtime design challenge

Status: HOLD for privileged policy consumption. This review is read-only against
the current Velnor generator and the candidate-runtime options record.

## Security decision

The current same-repository check is an identity filter, not a trust boundary.
It proves that the PR head repository equals the base repository, but it does
not authorize running that PR's arbitrary code in `pull_request_target`. A
same-repository PR can still contain a malicious or compromised change. The
current policy path runs the candidate binary from a privileged
`pull_request_target` job. `policy_candidate_step` clears `GH_TOKEN` and
`GITHUB_TOKEN` for the closure probe, but `policy.rs::render_and_compare`
executes the binary through `Command::new` without an environment or filesystem
sandbox. The candidate's working directory is the writable audited checkout.
It can alter policy inputs, inspect the workspace, create processes, use the
network, or leave files that later checks consume. Clearing two variables is
not isolation.

GitHub documents `pull_request_target` as an elevated-trust event and warns
against checking out or executing pull-request code in it. The warning also
applies to downloaded artifacts and to `workflow_run` consumers; artifacts from
another workflow must be treated as untrusted data. Same-repository provenance
does not change that rule.

Primary references:

- https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target
- https://docs.github.com/en/actions/reference/security/secure-use
- https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows
- https://docs.github.com/en/rest/actions/artifacts
- https://docs.github.com/en/actions/concepts/security/artifact-attestations

## Safe architecture options

### A. Unprivileged candidate lane (minimum safe slice)

Keep the producer on `pull_request`. Build and run the candidate only there,
with the read-only token, no secrets, and an ephemeral hosted runner. Upload
the binary, receipt, and generated-output diagnostics as artifacts. The
`pull_request_target` policy never executes the binary and keeps using the
base-owned renderer or a trusted published runtime.

This is safe and useful for early build/check overlap, but it does not make a
candidate-produced render a privileged policy verdict. A PR workflow file is
PR-controlled, so its own success status cannot be accepted as proof by the
base policy. The policy must continue its independent merge validation.

### B. Dedicated unprivileged sandbox broker (required for candidate policy use)

If pre-merge policy must use a candidate render, run the render in a separate,
unprivileged sandbox job. The privileged policy consumes only output bytes and
an immutable receipt; it never starts the candidate binary.

Required sandbox properties:

- candidate process runs as a non-root user in a disposable VM or equivalent
  kernel-enforced sandbox;
- no GitHub token, secrets, credentials, shared cache, or network route;
- source checkout is read-only; only a dedicated output directory is writable;
- output crosses the boundary as a new archive with a manifest and digest;
- wrapper, sandbox version, limits, timeout, and process-group cleanup are
  base-owned and immutable;
- the privileged consumer reads output as data and runs only base-owned static
  validators against it.

An ordinary hosted runner with `env` variables cleared, a temporary directory,
or a shell wrapper does not prove these properties. The current project has no
demonstrated sandbox contract, so this option remains blocked until the runner
boundary is implemented and tested. A `workflow_run` broker alone is not a
sandbox: GitHub gives it elevated access, and the official guidance says its
input artifacts remain untrusted.

### C. Trusted base build

Build the candidate from a base-controlled workflow or published runtime. This
avoids executing PR code in the privileged policy, but compiling the PR source
still executes Cargo build scripts, proc macros, and dependencies. It therefore
is not a safe substitute for the sandbox unless the build itself runs inside
the same isolated boundary. It also retains the late producer dependency if it
is attached to the full generator job.

### D. Attestation-only promotion

An artifact attestation proves provenance and integrity claims, not that the
generator produced the correct workflow tree. A receipt signed by the producer
cannot make an untrusted renderer's semantic result a policy verdict. Use
attestations to bind bytes and producer identity, never as the sole permission
to execute them in `pull_request_target`.

Recommendation: implement A first for safe overlap. Implement B only if the
campaign requires candidate renders to accelerate the privileged policy. Keep
the existing main-branch release path separate. Do not implement the current
candidate binary execution path by merely adding more manifest checks.

## Identity contract

Use a versioned candidate receipt. Its minimum fields are:

```text
schema
kind = premerge-runtime

producer:
  repository
  workflow_id, workflow_ref, workflow_sha
  run_id, run_attempt
  required_job_ids and job conclusions
  finalizer_job_id and conclusion

source:
  repository
  head_sha
  merge_sha
  validation_tree = head | merge
  build_revision
  base_sha

closure:
  algorithm/version
  digest
  path-set/version
  lockfile digest

build:
  package and binary
  profile
  canonical feature list
  Rust channel/toolchain
  target triple
  runner OS and architecture

artifact:
  artifact_id
  artifact_name
  archive_digest
  binary_sha256
  size

validation:
  build/self-report result
  closure and revision result
  digest result
  sandbox/wrapper result, if used
  output manifest digest, if used

trust:
  event
  head_repository
  fork
  token/network mode
  attestation signer and workflow, if present
```

`head_sha`, `merge_sha`, and `build_revision` are separate identities. GitHub
documents that a `pull_request` run's `GITHUB_SHA` is the temporary merge commit;
the PR head is `github.event.pull_request.head.sha`. The producer must record
which one it built and set `validation_tree` explicitly. It must not infer a
head identity from a callee workflow or replace a head SHA with a merge SHA.

For a head validation, require `build_revision == head_sha`. For merge
validation, require `build_revision == merge_sha` and reject the receipt when
the current PR event has a different merge SHA. The safest first producer
builds the exact requested tree rather than reusing a binary from a different
merge state.

The candidate consumer accepts a receipt only when all of these match the
current request:

1. same repository and non-fork policy allowed by the selected lane;
2. exact workflow file and producer workflow SHA;
3. exact run ID and run attempt, completed successfully;
4. every required platform job and finalizer completed successfully;
5. exact source tree, closure path set, lockfile, package, profile, features,
   toolchain, target, runner platform, and validation tree;
6. artifact ID, archive digest, binary digest, receipt digest, and expiry;
7. binary self-reports the receipt closure and build revision before any
   sandbox execution;
8. sandbox output, if required, is complete and matches the output manifest.

Missing, stale, expired, partial, canceled, mixed-attempt, fork, platform
mismatch, feature mismatch, or malformed receipts fail closed. “Latest artifact
with this name” is not an acceptable selector.

## Generator reuse and merge validation

Closure equality alone is insufficient. Reuse requires the complete tuple:

```text
(validation_tree, source revision, closure algorithm+digest, lockfile,
 package/bin, profile, feature set, Rust toolchain, target triple,
 runner platform, generator config)
```

The current PR candidate builds with the default Cargo feature/profile path,
while the published runtime builds `--locked --no-default-features --release`.
Those products cannot share a receipt merely because their source closure is
equal. Encode the build tuple and use one canonical candidate kind; do not
silently treat debug/default-feature output as a release runtime.

The candidate can remove duplicate generator builds only for another check with
the same tuple and the same validation tree. It cannot replace merge
validation. A head candidate is not evidence for the merge tree. A merge
candidate is stale as soon as the PR merge ref changes. The privileged policy
must either run the trusted base renderer against the current merge tree or
consume output produced by the hardened sandbox for that exact merge identity.

Linux and macOS products require native or explicitly proven target mapping;
the receipt must carry target triple and runner OS/architecture. Do not reuse
the current Linux-X64-only PR artifact for a macOS consumer. Keep the main
release producer's native matrix and `refs/heads/main` attestation contract
unchanged.

## Workflow and permission constraints

The candidate producer can start early in an independent `pull_request`
workflow. A policy job in another workflow cannot express a GitHub Actions
`needs` edge to it; it must resolve the exact producer run by workflow identity,
head/merge SHA, and attempt, then validate artifacts. Polling a sibling run is
acceptable only with a bounded timeout and exact identity checks; it must not
wait on a late full-CI candidate job.

Artifact list/download APIs require Actions read permission for private
repositories (public resources may be readable without authentication). The
current policy contract intentionally grants `contents: read` only. Verify the
actual repository access path before adding `actions: read`; do not widen the
privileged policy permission block or bypass its validator. If the exact
artifact cannot be read under the approved permissions, the candidate is
unavailable and policy fails closed.

Attestations require the documented OIDC/attestation permissions. A producer
attestation may bind workflow, repository, event, commit, and digest, but it
does not prove semantic correctness. A trusted `workflow_run` verifier must
never checkout or execute PR code; if it signs anything, its receipt must state
that it only verified immutable bytes/metadata.

## Affected generator surfaces and acceptance

Future implementation will touch both legacy and S2 runtime/product renderers,
candidate producer rendering, policy receipt parsing/validation, and generated
Velnor/Jackin/Parallax workflows. Keep release publication code distinct from
candidate artifact publication; no release/tag/package write belongs in the PR
producer.

Before accepting implementation, require:

- legacy and S2 fixtures for head/merge identity, run/attempt/job/artifact
  binding, feature/profile/target mismatch, stale PR, fork, missing platform,
  failed finalizer, expired artifact, and malformed receipt;
- generated actionlint and ShellCheck success;
- an executable sandbox test proving token, secret, network, workspace-write,
  and output-boundary restrictions;
- candidate render/output tests that never execute the binary in the privileged
  policy process;
- a real same-repository Linux+macOS candidate run with raw run, job, artifact,
  receipt, and consumer evidence;
- separate main-branch release validation proving the existing attestation and
  source-ref contract remains unchanged.

Open blockers: no current sandbox implementation; current policy executes the
candidate directly; current manifest omits producer attempt/job/artifact and
build tuple; current candidate producer is late and Linux-only; artifact access
under the contents-only policy permission is unproven; and current candidate
validation is head-oriented while merge validation remains a separate required
obligation.
