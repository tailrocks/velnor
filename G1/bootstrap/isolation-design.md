# G1 bootstrap isolation design (conditional-review amendment)

Observed 2026-09-20, Asia/Ho_Chi_Minh. This is a design-only review artifact. It does not approve source, generated workflows, a hosted canary, or a merge.

The reviewed source checkpoint is branch codex/g1-bootstrap-isolation at 78a39cf32e66e055c504247530b635640c5c42a7, based on main e713841bdb9c33d853b7a9af88ceac924af1b3b6. Its only source change is the pre-existing, deliberately untouched experiment in crates/velnor-workflow/src/s2/mod.rs; this document does not include it. The branch's previous review report is the untracked evidence file /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap/isolation-design.md, whose current byte SHA-256 before this amendment is d56b187098710e9609d91dd45632b622cc1991ebd063b9ba531a22fc74403963. The historical 03c6a468… label was a reported file-digest prefix, not a Git object; this design makes no Git-object claim for it. The committed copy's Git object and byte SHA-256 are recorded at handoff.

## Decision and gate

The candidate must cross four independently owned roles:

~~~text
same-repository pull_request
  candidate_producer (unprivileged, no secrets, no ordinary CI needs)
      -- immutable producer artifact + service/raw digests -->
pull_request_target, base-owned
  policy_acquire (API and archive verification; never executes candidate bytes)
      -- trusted handoff artifact, exact IDs, exact source object -->
  candidate_execute (new hosted runner, no privileged ancestry, isolated container)
      -- untrusted result artifact + service/raw digests -->
  policy (new hosted runner, independent source/API verification; only status writer)
~~~

There is no parent/child process boundary between policy_acquire and candidate_execute: they are separate hosted jobs with fresh workspaces and fresh process trees. The candidate process itself is a separate container with no host workspace, runner socket, command-file path, credential, or network. The acquire and verify jobs may execute trusted base code and API clients, but never candidate binaries, candidate scripts, or candidate-provided commands.

No implementation may begin until an independent reviewer approves this boundary and every hostile row below has an actual negative binary/canary result. A green unit test, clippy, or actionlint result is not security approval.

## Why 78a39cf3 was rejected

The checkpoint moved API lookup and candidate launch into separate steps, not separate trust domains. The candidate remains a child of a shell whose step environment contains GH_TOKEN (crates/velnor-workflow/src/s2/mod.rs:L4590-L4668 at the checkpoint). GH_TOKEN="" prefixes do not hide the parent token from /proc/$PPID/environ. The same job workspace contains the checkout, GitHub command files, and runner mounts; candidate code can rewrite or redirect them before the trusted step. permissions: {} does not repair either defect.

git archive plus unshare was treated as an immutable container. It was only a copied input and did not prove a read-only mount graph, private process tree, private network, absent Docker socket, or absent command files. Inline empty variables were symptom-layer masking, not isolation.

The acquisition contract was also incomplete. It used a candidate-derived short name, omitted the producer REST job id (the REST field is id, not databaseId), omitted stable workflow/job/run-attempt identity, allowed a blank artifact service digest, and did not bind profile, platform, features, target/head numeric repository IDs, base SHA, or exact commit tree. Candidate JSON remained authoritative. Those are architecture failures, not test-only gaps.

## Exact job graph and token ancestry

All four jobs use the explicit GitHub-hosted label ubuntu-24.04. The label is not itself the sandbox: only candidate_execute may invoke the candidate, and it must establish the container boundary below before doing so.

| Job ID (exact) | Generated workflow | Trigger | Permissions | Allowed code and token ancestry | Candidate bytes |
| --- | --- | --- | --- | --- | --- |
| candidate_producer | .github/workflows/ci-candidate.yml | pull_request; same target/head numeric repository ID required for upload | {} | PR source may build a binary. No secrets, no write token, no ordinary CI/Velnor needs. The workflow asserts GITHUB_REPOSITORY_ID equals github.event.pull_request.head.repo.id before publishing. | Builds only; never receives a privileged token. |
| policy_acquire | .github/workflows/ci-policy.yml | pull_request_target | contents: read, actions: read only | Base-pinned checkout/action and read-only GitHub API/archive operations. It may hold a read token in the fixed API step, but has no call site that executes candidate bytes. | Bytes are data; no execute bit is used. |
| candidate_execute | .github/workflows/ci-policy.yml | pull_request_target, needs: policy_acquire | {} | Fresh hosted job. A fixed download action receives only the trusted handoff ID from needs; the action step ends before the wrapper starts. The wrapper has no GH_TOKEN, GITHUB_TOKEN, secret, socket, command-file, or host-workspace input. | Exactly one fixed candidate entrypoint inside the isolated container. |
| policy | .github/workflows/ci-policy.yml | pull_request_target, needs: candidate_execute | contents: read, actions: read only | Fresh base-owned checkout, fresh target-object fetch, and independent API/artifact verification. Only this job invokes the policy command and emits the status. | Never executes candidate code. |

The producer's same-repository condition is a trust gate, not a secret boundary. Fork producer runs may build without secrets, but their artifact is ineligible and the trusted acquire path must reject the numeric head repository ID mismatch. There is no pull_request_target checkout of PR code and no dynamic PR-owned setup action.

needs carries opaque expected IDs only. It never chooses the first result, passes a candidate name, or makes candidate JSON authoritative. Every consumer repeats the relevant GitHub API query and exact comparison.

## Existing source surfaces and planned ownership

| Current surface | Evidence path | Structural change after approval |
| --- | --- | --- |
| Candidate packaging | crates/velnor-workflow/src/s2/primitives/ir.rs:L1888-L1999, candidate_publish_steps | Remove the unit-job producer. Render one dedicated candidate_producer workflow with a fixed job ID, fixed artifact contract, exact head build, and no ordinary/Velnor dependency. |
| Candidate provider selection | crates/velnor-workflow/src/s2/primitives/ir.rs:L4380-L4403 and L4780-L4805 | Delete the ProviderStepFacts.candidate_publish path after the dedicated producer is rendered; do not add a second legacy alias. |
| PR aggregate | crates/velnor-workflow/src/s2/primitives/ir.rs:L3521-L3580, render_nested | Emit the dedicated workflow/job as a direct aggregate member; no hidden candidate step in reusable Rust CI. |
| Policy jobs/acquire | crates/velnor-workflow/src/s2/mod.rs:L4524-L4590 and L4565-L4703 | Render the four-role graph, exact API contract, handoff/result transport, and fail-closed checks. Separate acquire, execute, and verify jobs. |
| Candidate comparison | crates/velnor-workflow/src/s2/policy.rs:L1435-L1473 and L1487-L1584 | Treat candidate render as untrusted bytes only. Verify source identity and handoff/result tuple before comparison against a separately archived target object. |
| Generated caller coupling | .github/workflows/ci-pr.yml:L292-L340 and .github/workflows/ci-unit-rust.yml:L196-L490, L560-L628 | Regenerate callers after source changes. Remove the current candidate steps from the unit job and its ordinary check dependencies. |
| Generated policy | .github/workflows/ci-policy.yml:L1-L190 | Regenerate the four jobs. Never hand-edit generated YAML. |

The current checked-in producer is appended to the hosted Rust unit job at .github/workflows/ci-unit-rust.yml:L560-L628; that coupling is why a blocked unit/Velnor gate can strand the only candidate artifact. The replacement workflow is .github/workflows/ci-candidate.yml, not a second producer hidden in the unit workflow. Existing PR952 source closure work and generated recovery work are separate dependencies; this design does not modify them or the APT tree.

## Trusted identity contract

Every producer, handoff, and result transport carries one immutable identity record. The record is trusted only after API comparison; a candidate manifest can assert fields but can never select an object.

~~~json
{
  "role": "producer|handoff|result",
  "workflow_path": ".github/workflows/ci-candidate.yml",
  "workflow_id": 0,
  "run_id": 0,
  "run_attempt": 0,
  "run_status": "completed",
  "run_conclusion": "success",
  "job_id": 0,
  "job_name": "candidate_producer",
  "upload_step_id": "candidate_upload",
  "artifact_binding_method": "static-single-uploader-v1",
  "event": "pull_request",
  "target_repository": "owner/name",
  "target_repository_id": 0,
  "head_repository": "owner/name",
  "head_repository_id": 0,
  "head_sha": "40-hex",
  "base_sha": "40-hex",
  "tree_sha": "40-hex",
  "artifact_name": "velnor-workflow-candidate-linux-x64",
  "artifact_id": 0,
  "artifact_service_digest": "sha256:64-hex",
  "artifact_raw_zip_sha256": "64-hex",
  "artifact_expires_at": "RFC3339",
  "profile": "debug",
  "features": "default",
  "platform": "Linux-X64",
  "closure": "trusted closure",
  "binary_sha256": "64-hex",
  "manifest_sha256": "64-hex",
  "action_ref": "owner/action@40-hex",
  "action_archive_sha256": "64-hex"
}
~~~

For a handoff, workflow_path, workflow_id, run_id, run_attempt, job_id, and job_name identify the base policy_acquire run in addition to the nested producer tuple. For a result, they identify the base candidate_execute run and stable candidate_execute job, in addition to the producer and handoff IDs. The result also records the base policy workflow and verify-job tuple when the verifier consumes it. Numeric repository IDs are mandatory even when names match. A missing, blank, non-numeric, or duplicate identity field rejects the transport.

### Artifact-to-job binding: current API limitation and required mechanism

The artifact REST response supplies an artifact ID, name, expiry, service digest, and workflow_run association, but it does not supply an uploader job ID or run attempt. The jobs REST response supplies job IDs but does not supply artifact IDs. The observed correlation between producer job 105961562345 and run 10591573147 is therefore not API proof of artifact ownership. This design does not claim that it is.

The required binding is an explicit trusted unique-uploader workflow contract, or an independently verifiable attestation; otherwise the gate remains BLOCKED:

* Each transport workflow has exactly one fixed upload step and exactly one eligible job: candidate_upload in candidate_producer, handoff_upload in policy_acquire, and result_upload in candidate_execute. The static contract forbids every other upload-artifact invocation, dynamic artifact name, matrix duplicate, reusable-workflow uploader, or post-job uploader.
* The base verifier fetches the exact workflow blob from the target head object and checks it against a base-owned normalized contract digest: exact workflow path, exact job ID/name, exact upload step ID/action commit, exact permissions, exact static artifact name, no extra jobs that can upload, and no candidate-controlled upload action. A missing or changed contract is red. For base-owned policy workflows, the same contract is checked against the base pinned blob.
* Only after that contract check does the verifier derive artifact uploader_job_id/name from the unique uploader. It records artifact_binding_method=static-single-uploader-v1 and still rechecks the selected job REST id/run id/run attempt. The artifact service digest and raw ZIP digest remain mandatory. The derivation is a proof from the trusted workflow contract plus singularity, not an invented API field.
* If the workflow cannot be independently normalized and contract-hashed, or if the artifact service later provides an uploader job/attempt field, implementation must use that stronger field. An unavailable contract/attestation or any non-singular uploader fails closed; it must not infer ownership from timing, names, candidate JSON, or job/artifact numeric proximity.

This is a concrete blocker from G1/bootstrap-artifact-feasibility-2026-09-20.md and must be resolved in source design before code. A future signed uploader attestation is acceptable only if the verifier checks its signer, exact run/attempt/job/artifact IDs, action archive digest, and raw bytes independently; a candidate-written unsigned manifest is not an attestation.

### API selection: exact, singular, and stale-safe

policy_acquire uses the base-owned API client against the target repository; it never accepts a run/job/artifact selected by the candidate:

1. Resolve .github/workflows/ci-candidate.yml through the target repository's workflows API and require exactly one matching path and one numeric workflow_id. A tag, display name, or candidate-provided workflow ID is not a substitute.
2. Enumerate every page of GET /repos/{target}/actions/workflows/{workflow_id}/runs filtered by event=pull_request and the exact head_sha. Require exactly one eligible run with status=completed, conclusion=success, exact path, exact workflow_id, exact target/head names and numeric IDs, exact event, exact head SHA, and a non-null run_id plus run_attempt. Multiple successful runs or attempts are ambiguous and reject; there is no “latest” or first-response rule.
3. Enumerate GET /repos/{target}/actions/runs/{run_id}/jobs and require exactly one completed-success job with stable name candidate_producer, REST field id, exact run_id, exact head SHA, and the expected workflow path. databaseId, a display name alone, and a candidate-reported ID are not accepted.
4. Enumerate GET /repos/{target}/actions/runs/{run_id}/artifacts and require exactly one non-empty, non-expired artifact named the trusted static contract velnor-workflow-candidate-linux-x64. Require exact workflow_run.id == run_id, artifact id, created_at/updated_at not older than the selected run, and a non-blank service digest matching sha256:[0-9a-f]{64}. A short closure prefix is never an artifact name or selector.
5. Download only by the selected numeric artifact ID through the REST zip endpoint. The raw downloaded ZIP SHA-256 must equal the API service digest; a missing service digest, unavailable digest endpoint, mismatch, expiry, stale attempt, duplicate name, duplicate normalized member, or extra file is a hard failure.

The API client must fail on pagination errors, malformed JSON, ambiguous eligible objects, and an action/service digest that cannot be obtained. It must not silently fall back to gh run download, needs output, latest, a candidate name, or a candidate manifest.

The static handoff name is velnor-workflow-candidate-handoff; the static result name is velnor-workflow-candidate-result. The same singularity, expiry, action-digest, service-digest, raw-ZIP, and unique-uploader checks apply to each. The artifact upload action's artifact-id and artifact-digest outputs are mandatory. If either output is unavailable, the producer/handoff/result job fails; it does not continue with a locally computed digest alone.

### Source object and tree proof

The target repository is an API identity, not the current checkout:

1. Acquire the PR event's target/head numeric IDs, base SHA, and head SHA. Query GET /repos/{target}/git/commits/{head_sha} and verify the response repository ID, commit SHA, and tree.sha. Query that exact tree through GET /repos/{target}/git/trees/{tree_sha}?recursive=1; reject a truncated tree response.
2. In a fresh disposable object database, fetch the exact head object from the target repository URL with tags and hooks disabled. Require git cat-file -e {head_sha}^{commit} and git rev-parse {head_sha}^{tree} == API tree.sha. Create the source archive from {head_sha} itself. Do not use HEAD, a base checkout, a merge SHA, or a branch-name fetch as the source identity.
3. Record head_sha, tree_sha, object-format, source-archive SHA-256, and target numeric repository ID in the handoff. The candidate sees only this read-only, safely extracted archive; it cannot choose or rewrite it.
4. policy independently repeats the target API commit/tree queries and the fresh exact-object fetch/archive in a new job. It compares its archive byte-for-byte to the handoff archive and rejects any mismatch. It never trusts a current HEAD, the base checkout's object, a candidate manifest, or a handoff-only source SHA.

The fresh fetch must use --no-tags, --no-replace-objects, disabled hooks, and no target worktree execution. Source archive extraction is subject to the same safe-member rules as transport archives; symlinks, hardlinks, absolute names, .. components, device nodes, duplicate normalized names, and files outside the declared source surface are rejected before the candidate starts.

### Action archive and service digest proof

Every fixed producer, handoff, and result action has both an immutable commit reference and a trusted raw action-archive SHA-256 recorded in the base contract. The base action verifier downloads the pinned action archive from the official repository endpoint, compares its raw bytes with that SHA-256, and fails if the endpoint or digest is unavailable. A version tag, mutable branch, or unverified action checkout is not permitted. The same rule covers checkout, upload, download, and any API helper action.

The action archive digest is distinct from the artifact service digest. For each producer/handoff/result artifact, the trusted record contains:

* the upload action ref and verified action archive SHA-256;
* the action output artifact-id and artifact-digest;
* the REST artifact id, static name, run ID/attempt, and expiry state; and
* the raw ZIP SHA-256 fetched by the trusted consumer.

The service digest and raw ZIP digest must be present and equal. A digest that is merely reported by a candidate, or an action archive SHA inferred from a Dockerfile/tag, is not evidence.

## Candidate execution boundary

The fixed handoff step downloads and validates data, then ends. It stages only these paths under a fresh, bounded workspace:

* /input: exact trusted source archive, safely extracted, mode 0555, bind mounted read-only;
* /candidate: measured candidate binary, mode 0555, bind mounted read-only;
* /output: an empty container tmpfs, never a writable host bind;
* /tmp: a separate fixed-size container tmpfs.

The wrapper must create a container from the final image digest using a numeric non-root UID/GID and these mandatory controls:

~~~text
--network=none
--read-only
--pid=private
--cap-drop=ALL
--security-opt=no-new-privileges:true
--pids-limit=64 --memory=512m --cpus=2
--tmpfs /tmp:rw,nosuid,nodev,size=64m
--tmpfs /output:rw,nosuid,nodev,size=64m
--mount type=bind,src=/bounded/input,dst=/input,readonly
--mount type=bind,src=/bounded/candidate,dst=/candidate,readonly
--user=65532:65532
~~~

There is no host workspace, RUNNER_TEMP, Git directory, Docker socket, artifact-service path, command-file path, runner socket, or baseline checkout mount. The candidate cannot see the acquire/verify process tree because the container has a private PID namespace. It cannot alter the source because /input is read-only. It cannot return files through a writable host bind: after the process exits, the fixed wrapper copies /output into a separate bounded host scratch area, validates it, and only then uploads it.

The entrypoint is explicit /usr/bin/env -i, followed by the candidate. The only environment entries are the static allowlist SOURCE_HEAD_SHA, SOURCE_REPOSITORY, SOURCE_CLOSURE, HOME=/tmp/home, PATH=/usr/bin:/bin, and an explicit output path. No GITHUB_*, GH_*, ACTIONS_*, runner, package, cloud, registry, secret, proxy, or command-file variable is passed. The wrapper must inspect /proc/1/environ and every /proc/[0-9]*/environ from the canary and prove that only this allowlist is present; an image default or parent environment not explicitly scrubbed is a failure. Empty GH_TOKEN variables are not a substitute.

The wrapper must fail closed if any image pull/inspect, read-only mount, private PID/network namespace, cap drop, non-root UID, or command-file absence cannot be established. There is no host-shell, token-blanking, unshare, or privileged fallback.

## Final runtime image gate

The Dockerfile's docker/job-ubuntu.Dockerfile:L21 digest sha256:2260313b31c8c011cd2eebe728008efac1b3982be73eb71348ea2648d2c0e09b is a multi-architecture build input, not a final runtime identity. It must not be used as the candidate image proof.

Local evidence captured on 2026-09-20 for the current platform was:

~~~text
platform manifest: docker.io/library/ubuntu@sha256:889d056d5c6c0bfb55789ff3710681d68e50713cb562d2196dc07110599c7a6f
config digest:    sha256:af52039db3f8df8b54cd80945bdabea797445f414955027fa0bed9cd3908244b
Config.Env:       ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
RepoDigests:      ["ubuntu@sha256:889d056d5c6c0bfb55789ff3710681d68e50713cb562d2196dc07110599c7a6f"]
~~~

This is digest mechanics evidence, not hosted approval. Ubuntu is not yet proven to be the minimal final candidate runtime. Before source code, the implementation must either produce a purpose-built minimal image and record its exact platform manifest and config digests, or explicitly approve this image after a fresh hosted pull/inspect and minimal-surface review. A Dockerfile FROM digest, tag, local image ID, or manifest-list digest alone does not satisfy the gate. Hosted execution is BLOCKED until pull/inspect returns the expected final manifest/config tuple and the image-environment canary passes.

The image gate is:

~~~text
docker pull <image>@sha256:<platform-manifest>
docker buildx imagetools inspect --raw <image>@sha256:<platform-manifest>
docker image inspect <image>@sha256:<platform-manifest>
docker run --rm --pull=never --entrypoint /usr/bin/env \
  --env-file /dev/null <image>@sha256:<platform-manifest> -i ...
~~~

The raw manifest's config digest, RepoDigests, config environment, platform, and canary output must all match the trusted contract. Any mismatch fails closed.

## Archive, quota, and timeout gate

Every downloaded or produced ZIP/TAR is checked before extraction and again after extraction. Reject symlink entries, hardlink entries, absolute names, .. components, special files, duplicate normalized names, duplicate device and inode pairs, unexpected files, missing required files, and output paths outside the declared root. A candidate cannot smuggle an extra result through a symlink, hardlink, duplicate, or archive prefix.

The wrapper must provision a fixed-size host scratch filesystem for staging and copying results, in addition to the container /tmp and /output tmpfs limits. If a fixed quota cannot be established, it fails rather than using an unbounded runner directory. Container creation must fail if either tmpfs quota is unavailable. The wrapper uses a fixed execution timeout and kill grace (for example, 900 seconds plus 5 seconds); if the timeout utility or equivalent cannot be verified, it fails rather than running without a bound. Quota and timeout failures are result failures, never policy success.

## Independent verification and status rule

policy starts on a fresh ubuntu-24.04 runner. It independently verifies:

1. the base policy_acquire workflow/run/attempt/job tuple, the producer workflow/run/attempt/job tuple, and the exact event/head/base/target/head IDs;
2. the producer artifact, handoff artifact, and result artifact through API IDs, static names, mandatory action/service/raw digests, expiry, exact run association, and the unique-uploader contract;
3. the target commit/tree through a new target-repository API query and exact object fetch/archive, never current HEAD or a base checkout;
4. the final candidate image and container canary evidence; and
5. the result archive's safe members, exact output surface, binary/render hashes, and comparison against the independently materialized source.

Only after all five pass does policy invoke the base-owned velnor-workflow policy command. Candidate exit status, candidate JSON, candidate manifest, cache hits, artifact names, needs values, and any producer self-report cannot emit a green status. Acquire, execute, and verify fail closed on every missing or ambiguous transport field.

## Hostile evidence matrix

The following rows are mandatory evidence, not claims of current success. The fixture paths are source-ownership proposals; they must exist and be run in a fresh hosted canary before implementation approval. Current status is BLOCKED—no approved implementation/canary unless explicitly noted.

| ID | Hostile case | Required binary/canary and static evidence | Current evidence |
| --- | --- | --- | --- |
| H1 | Recover token/secret through environment, /proc, parent process, or image ENV | A hostile binary reads /proc/1/environ and every process environment, searches GH_TOKEN, GITHUB_TOKEN, secrets, and image defaults; generated YAML proves no secret ancestry and wrapper proves env-i. | BLOCKED; no hosted hostile binary has run. |
| H2 | Write GitHub command files or alter GITHUB_ENV, PATH, OUTPUT, STATE, summary, or action runtime files | Hostile binary enumerates the known command-file paths and attempts writes; canary proves paths are absent/unreachable and the result cannot affect later jobs. | BLOCKED; no canary. |
| H3 | Reach workspace, checkout, baseline, runner socket, Docker socket, or acquire/verify process | Hostile binary probes /workspace, /__w, /var/run/docker.sock, runner temp, mounts, /proc, and parent PIDs; generated YAML asserts no host/baseline/socket mounts, private PID, and separate jobs. | BLOCKED; no hosted boundary evidence. |
| H4 | Mutate source; symlink/hardlink/absolute/../duplicate/archive traversal; add extra output | Hostile binary rewrites/deletes source and emits each archive trick; safe extractor and post-copy inode/member checks must reject every one. | BLOCKED; no implementation fixture. |
| H5 | Network exfiltration or service reachability | Hostile binary attempts DNS, loopback, runner metadata, IPv4/IPv6, proxy, and Unix socket access; container must report no network and no socket. | BLOCKED; --network=none is design only. |
| H6 | Forge manifest, closure, revision, source SHA, digest, job/run/artifact ID, or status | Static fixtures mutate each field and feed forged JSON, names, and self-reports; trusted API/object/archive comparison must reject all mismatches. | BLOCKED; old string tests are not evidence. |
| H7 | Wrong full identity or ambiguous selection: workflow/path, numeric IDs, run/attempt, job REST ID/name, event, head/base, target/head repo, fork, profile/platform/features, stale/expired/duplicate artifact, prefix collision | API fixtures return every pairwise duplicate/stale/foreign variant; client must enumerate all pages and reject zero or more than one eligible object. | BLOCKED; no API matrix run. |
| H8 | Producer failure, candidate nonzero/timeout/quota failure, missing artifact, failed handoff/result, or verify/status bypass | Hosted jobs intentionally fail each role and assert no downstream green status; only policy may write status and every missing transport tuple is red. | BLOCKED; no hosted failure matrix. |
| H9 | Empty cache, shallow checkout, wrong image manifest/config/env, unavailable quota/timeout, or wrong target object | Cold-cache and shallow exact-object canary runs git fetch/tree checks; image canary pulls and inspects the final digest/config; quota and timeout are deliberately unavailable once and must fail. | Local image evidence above only; hosted row BLOCKED. |

The hostile fixture source should be a standalone, reviewable binary under crates/velnor-workflow/tests/fixtures/bootstrap-hostile-candidate/, with shell/API identity fixtures adjacent to the S2 policy tests. No fixture may use a real secret. A failed canary must preserve logs and exact handoff tuple for review; it cannot be converted into a warning.

## Generated static gates

After the source implementation is separately approved, regeneration must prove all of the following on every generated workflow:

* exactly the four role IDs above, plus no hidden candidate invocation;
* candidate_producer has no ordinary CI/Velnor needs and no privileged permission;
* policy_acquire never invokes candidate bytes and has only read API scope;
* candidate_execute is a fresh hosted job with {} permissions, fixed action/image references, no writable authoritative checkout, no socket, no command-file mount, and the complete sandbox flags;
* policy is the only status writer and repeats API/source verification;
* all action refs are immutable SHAs with trusted action archive digests;
* all producer/handoff/result artifacts use static names, mandatory service/raw digests, and the unique-uploader binding; and
* absent image, quota, timeout, action, API, artifact, source, or uploader proof is a hard failure.

Generated output must be produced by the generator. Do not hand-edit .github/workflows/ci-candidate.yml, ci-policy.yml, ci-pr.yml, or ci-unit-rust.yml.

## Implementation order after design approval

The first bounded source task is generator-only: introduce the typed four-role job/transport contract and render the dedicated producer, while deleting the unit-job candidate_publish path. It must have static tests for job IDs, permissions, needs, artifact names, exact unique-uploader shape, and absence of candidate execution in the acquire job. It must not execute a candidate, alter the sandbox, or emit generated outputs until reviewed.

Then, in separate reviewed increments:

1. implement the exact API/object/service/action-digest acquisition, static unique-uploader contract, and handoff;
2. implement the fresh execute job, final-image digest gate, explicit env scrub, mounts, PID/network/capability controls, quota, timeout, and safe archive transport;
3. implement independent verify re-fetch/object/tree/archive checks and the single status writer;
4. add the hostile binary/API matrix and hosted canary; and
5. regenerate all YAML, run actionlint and focused tests, then request an independent security review.

The old candidate_publish_steps and every legacy unit-path consumer are removed in the same migration; no compatibility shim, alias, or second producer is acceptable.

## Reproduction commands

These commands reproduce the source checkpoint and the observed image evidence without executing candidate code:

~~~text
git -C /private/tmp/velnor-g1-bootstrap status --short --branch
git -C /private/tmp/velnor-g1-bootstrap show --stat --oneline 78a39cf32e66e055c504247530b635640c5c42a7
sha256sum /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/bootstrap/isolation-design.md

gh api repos/OWNER/REPO/actions/workflows --paginate
gh api 'repos/OWNER/REPO/actions/workflows/WORKFLOW_ID/runs?event=pull_request&head_sha=HEAD_SHA&per_page=100' --paginate
gh api 'repos/OWNER/REPO/actions/runs/RUN_ID/jobs?per_page=100' --paginate
gh api 'repos/OWNER/REPO/actions/runs/RUN_ID/artifacts?per_page=100' --paginate
gh api repos/OWNER/REPO/actions/artifacts/ARTIFACT_ID/zip > artifact.zip
sha256sum artifact.zip

git -c core.hooksPath=/dev/null -c fetch.prune=false fetch --no-tags TARGET_URL HEAD_SHA
git cat-file -e 'HEAD_SHA^{commit}'
git rev-parse 'HEAD_SHA^{tree}'
git archive --format=tar HEAD_SHA > source.tar

docker pull docker.io/library/ubuntu@sha256:889d056d5c6c0bfb55789ff3710681d68e50713cb562d2196dc07110599c7a6f
docker buildx imagetools inspect --raw docker.io/library/ubuntu@sha256:889d056d5c6c0bfb55789ff3710681d68e50713cb562d2196dc07110599c7a6f
docker image inspect docker.io/library/ubuntu@sha256:889d056d5c6c0bfb55789ff3710681d68e50713cb562d2196dc07110599c7a6f
~~~

Post-implementation gates are cargo fmt --all -- --check, focused S2 generator/policy tests, cargo clippy --locked -p velnor-workflow --lib --tests -- -D warnings, generator drift checks, the full hostile matrix, and actionlint on every generated workflow. These commands are verification requirements, not a substitute for the trust-boundary review.

## Primary references

* [GitHub secure use of pull_request_target](https://docs.github.com/en/actions/reference/security/securely-using-pull_request_target) — base-token/secrets boundary and prohibition on executing untrusted PR code.
* [GitHub workflow syntax and permissions](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax) — least-privilege job permissions.
* [Workflow-runs REST API](https://docs.github.com/en/rest/actions/workflow-runs), [jobs REST API](https://docs.github.com/en/rest/actions/workflow-jobs), and [artifacts REST API](https://docs.github.com/en/rest/actions/artifacts) — path, run/attempt, job id, artifact ID/digest/expiry, and repository identity fields.
* [upload-artifact action metadata](https://github.com/actions/upload-artifact/blob/main/action.yml) — immutable artifact ID/digest outputs.
* [Docker container runtime controls](https://docs.docker.com/engine/containers/run/) — private network/filesystem, capability, PID, and resource controls.

This amended design remains conditional and blocked on the six mandatory review rows, the artifact-to-job binding mechanism, and the nine actual hostile canaries above.

