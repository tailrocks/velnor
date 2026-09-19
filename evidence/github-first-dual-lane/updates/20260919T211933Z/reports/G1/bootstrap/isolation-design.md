# G1 bootstrap isolation design (review gate)

Observed 2026-09-20 Asia/Ho_Chi_Minh. Design only; no source or generated workflow is approved by this note. The reviewed source checkpoint is `codex/g1-bootstrap-isolation` at `78a39cf32e66e055c504247530b635640c5c42a7`, based on main `e713841bdb9c33d853b7a9af88ceac924af1b3b6`. The working tree has an uncommitted experiment; this note describes the replacement boundary, not that experiment.

## Why `78a39cf3` was rejected

The checkpoint moved the API lookup and candidate launch into separate *steps*, but not into a credential/process/filesystem boundary. The candidate remains a child of a shell whose step environment contains `GH_TOKEN` (`crates/velnor-workflow/src/s2/mod.rs:L4590-L4668` at the checkpoint), and `GH_TOKEN=""` prefixes do not hide the token from `/proc/$PPID/environ`. The same job workspace still contains the checkout and GitHub command files; a candidate can rewrite them or inspect/redirect them before the trusted policy step. `permissions: {}` would not repair either defect.

The checkpoint also treated `git archive` plus `unshare` as if it were an immutable container. The archive is only a copied input; the candidate process still runs with the runner's host mounts and same-user permissions. `unshare --user --mount --pid --net` was not a complete read-only mount graph and had no proof that the host process tree, workspace, Docker socket, or command files were unreachable. Inline empty token variables were a symptom patch, not isolation.

Finally, the acquisition contract was incomplete: it did not require the producer job's API `id` (the REST job field is `id`, not `databaseId`), stable exact job identity, producer workflow path, manifest feature/profile/platform identity, or artifact/service digest before handing bytes onward. Candidate self-report and a short artifact name remained too influential. These are design failures, not test-only gaps.

## Existing trust surfaces

| Surface | Current source | Boundary fact | Required change |
| --- | --- | --- | --- |
| Producer packaging | `crates/velnor-workflow/src/s2/primitives/ir.rs:L1888-L1999` (`candidate_publish_steps`) | Candidate build/publish is appended after ordinary hosted Rust checks in the reusable unit job. Its caller is selected by `ProviderStepFacts.candidate_publish` (`.../ir.rs:L4380-L4403`, `L4780-L4805`), so dependency/Velnor gates can strand the only producer. | Move the generator product to one dedicated same-repository `pull_request` producer job in `ci-pr.yml`, with no `needs` on ordinary unit/Velnor jobs. Build exact PR head, default debug features, and upload only through a fixed SHA-pinned artifact action. Remove the unit-job `candidate_publish` input/step rather than adding a second legacy producer. |
| PR aggregate | `crates/velnor-workflow/src/s2/primitives/ir.rs:L3521-L3580` (`render_nested`) | The PR aggregate currently emits plan and unit callers; the candidate producer is hidden inside a called Rust workflow. | Render the dedicated producer in the PR aggregate only for the owner repository. Its job name and workflow path become the API contract. No candidate code runs in `pull_request_target`. |
| Trusted policy entrypoint | `crates/velnor-workflow/src/s2/mod.rs:L4524-L4590` (`PolicyJobSpec`, `policy_candidate_step`) | The owner policy is a `pull_request_target` job and currently combines base setup, API lookup, candidate probing, rendering, ruleset lookup, and policy. | Replace one job with three base-owned jobs: `policy_acquire`, `candidate_execute`, `policy` (verify). Only fixed base code/actions may run in all three. |
| Candidate renderer | `crates/velnor-workflow/src/s2/policy.rs:L1487-L1584` (`render_with_candidate`) | Candidate rendering must not receive the authoritative writable checkout. The trusted comparison reads the same `tree` that the candidate can mutate in the rejected design. | Candidate gets only a read-only clean source snapshot. Trusted verification compares candidate output against a separately re-materialized source archive from the exact head tree. |
| Policy comparison | `crates/velnor-workflow/src/s2/policy.rs:L1435-L1473` (`regenerate_and_compare`) | `--candidate-render` is an untrusted output input and must not be treated as provenance. | Keep it as a byte-comparison input only; verify source/archive/render identities before invoking policy. |
| Existing generated snapshot | `.github/workflows/ci-policy.yml:L1-L190` | Checked-in YAML is stale and still contains the rejected single-job candidate path. | Regenerate from the admitted source; never hand-edit this file. |

The generated snapshot makes the coupling concrete: `.github/workflows/ci-pr.yml:L292-L340` calls the hosted Rust reusable workflow, `.github/workflows/ci-unit-rust.yml:L196-L490` performs checkout/tool/runtime/checks, and `.github/workflows/ci-unit-rust.yml:L560-L628` is where candidate publish currently runs after the checks. The caller's ordinary dependency gates are visible in `.github/workflows/ci-pr.yml:L292-L340`; the dedicated producer must not inherit those `needs`.

## Replacement job graph

```text
pull_request (same repository only)
  candidate-producer  -- no ordinary CI/Velnor needs; no secrets/write permissions
       | immutable artifact: binary + untrusted manifest
       v
pull_request_target (base-owned)
  policy_acquire -- API lookup/download/hash only; never executes candidate
       | trusted handoff artifact: measured bytes + exact IDs + clean head archive
       v
  candidate_execute -- fresh runner, fixed download step, Docker sandbox only
       | untrusted render artifact + trusted wrapper digest
       v
  policy (trusted verify) -- fresh checkout; re-download/re-hash; clean compare
       | only this job emits policy status
```

`policy_acquire` and `candidate_execute` must have separate workspaces and no shared process tree. `candidate_execute` has `actions: read` only for the fixed artifact-download action; the action step ends before the fixed wrapper starts. The wrapper must fail closed if the sandbox is unavailable. `policy` is another fresh job and never consumes a candidate workspace.

## Concrete candidate boundary

The candidate execution job must not rely on `env -i`, empty token variables, or `unshare` alone. After the fixed handoff download/validation step, the wrapper stages three paths under a fresh `RUNNER_TEMP` directory:

* `/input` — source tree extracted from the trusted handoff, mode `0555`, mounted read-only;
* `/candidate` — measured generator binary, mode `0555`, mounted read-only;
* `/output` — empty disposable render directory, the only writable bind mount.

The fixed wrapper invokes a full-SHA-pinned existing Ubuntu base image (`docker/job-ubuntu.Dockerfile:L21` records the repository's pinned Ubuntu digest) through the host Docker CLI with:

* `--network=none`, `--read-only`, `--cap-drop=ALL`, `--security-opt=no-new-privileges`, default seccomp, a bounded PID/memory/CPU limit, and numeric runner UID/GID (not container root);
* no Docker socket, host workspace, `$RUNNER_TEMP`, Git directory, command files, or artifact-service paths mounted;
* only `/input:ro`, `/candidate:ro`, `/output:rw`, and an ephemeral `/tmp` tmpfs;
* an allow-list environment containing only `SOURCE_HEAD_SHA`, `SOURCE_REPOSITORY`, `SOURCE_CLOSURE`, `HOME=/tmp/home`, `PATH=/usr/bin:/bin`, and explicit `--output /output`. Do not define any `GITHUB_*`, `GH_*`, `ACTIONS_*`, runner, package, cloud, registry, or secret variable. Do not pass command-file paths.

The fixed wrapper computes output hashes after the container exits and writes wrapper metadata outside `/output`. It must reject nonzero candidate exit, symlinks, absolute/`..` paths, unexpected files, and output traversal. If Docker/image pull/inspect, `--network=none`, read-only bind mounts, or the numeric user cannot be established, the job fails; there is no host-shell or token-blanking fallback.

The producer artifact and handoff are not mounted read-write into the candidate. The candidate cannot reach the acquire/verify process tree (`/proc` is the container PID namespace), host workspace, Docker socket, or GitHub command files. A source mutation is either impossible (`/input:ro`) or remains disposable and is rejected by the next job.

## Trusted acquisition contract

`policy_acquire` uses a fresh full-history base-owned checkout and the base setup action only. It may hold `GH_TOKEN` in the one fixed API/download shell, but it never invokes candidate bytes. Its API filters require all of:

1. exact workflow path `.github/workflows/ci-pr.yml`, event `pull_request`, successful completed run, target repository full name and numeric ID, head repository full name and numeric ID, and exact PR head SHA (never merge SHA);
2. exactly one completed-success producer job with exact stable display name and REST database `id` (record both `id` and `name`, not a candidate-supplied value);
3. exactly one non-expired, non-empty artifact with exact full name, run ID, artifact ID, and service digest when present; the 16-character name prefix is only a locator;
4. downloaded archive SHA-256, safe archive member list, exact binary and manifest files, profile `debug`, default feature set, runner platform, repository/name and run/head/closure binding, and binary SHA-256; wrong workflow/job/run/platform/features/profile, stale/duplicate/expired/same-prefix artifact, fork, malformed manifest, or service digest mismatch fails closed;
5. full candidate closure computed by the trusted base runtime from the clean head Git tree. Candidate `--closure`, JSON, artifact name, and job output are assertions only.

The wrapper creates `handoff.json` with schema version, workflow path/event, head SHA, full closure, target/head repository names and numeric IDs, producer run ID, producer job ID/name, producer artifact ID/service digest/archive SHA, and measured binary/manifest/source-archive SHA-256 values. It uploads this handoff with a fixed SHA-pinned action and records the upload action's artifact ID/digest. No `policy-checkout` directory or candidate-supplied manifest is passed as authoritative state.

## Trusted verification contract

`policy` starts from a fresh full-history base-owned checkout, re-downloads both handoff and render artifacts by exact action IDs/digests, and verifies the handoff fields against the event again. It re-downloads the original producer artifact by the handoff's producer artifact ID and recomputes its archive SHA-256. It archives the clean checked-out head independently, rejects source archive traversal/symlinks, and compares the extracted handoff source byte-for-byte with that archive. It rejects render symlinks/traversal/extra or missing files and compares the candidate render against the clean source surface through `--candidate-render`.

Only the verify job invokes the ruleset lookup and `velnor-workflow policy`; candidate exit status, self-reports, artifact names, cache hits, and candidate JSON cannot emit a green policy result. The policy command receives an empty candidate manifest path and only the already-verified render directory.

The three target jobs are still ordinary GitHub jobs, so the design does not claim that a new job is a VM guarantee by itself. The execute job's candidate process is isolated by the fixed Docker boundary above; acquire and verify never share its runner, workspace, process tree, or mounted paths. Artifact actions are the only cross-job transport, and their raw zip bytes are fetched/re-hashed again by the trusted verifier. A hosted canary must inspect `/proc/1/environ`, `/proc/*/environ`, `/workspace`, `/__w`, command-file paths, network sockets, and `/var/run/docker.sock` from the candidate container and assert absence/unreachability before admission.

## Required hostile fixtures and static gates

Add deterministic source tests for:

* a candidate trying `/proc`/environment/runner workspace/Docker socket/command files, token recovery, and `GITHUB_ENV`/`PATH`/`OUTPUT`/`STATE`/summary writes;
* source rewrite/delete/`.git` mutation, symlink and traversal creation, forged manifest/revision/closure, output extras/missing files;
* wrong workflow/path, event, run, job ID/name, target/head names or numeric IDs, head/merge SHA, fork, profile/features/platform, expired/duplicate artifact, service digest, and same-prefix/full-closure collisions;
* producer build/test failure, candidate nonzero render, missing sandbox/image, empty cache, and shallow checkout.

Static generated-YAML tests must prove four distinct jobs/roles, `needs` only along the handoff chain, no candidate invocation in any token-bearing step, no producer dependency on ordinary/Velnor jobs, minimal permissions, SHA-pinned actions, exact API filters, no writable authoritative checkout, and fail-closed sandbox failure. Run `cargo fmt --all -- --check`, the focused `s2::policy` and generator tests, `cargo clippy --locked -p velnor-workflow --lib --tests -- -D warnings`, generator regeneration/drift checks, and `actionlint` on every generated workflow.

This design is a prerequisite for the next source patch. It does not approve `78a39cf3`, generated output, the schema-1 migration, or merge.
