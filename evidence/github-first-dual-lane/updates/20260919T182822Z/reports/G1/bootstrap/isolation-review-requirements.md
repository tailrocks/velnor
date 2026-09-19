# G1 bootstrap isolation review requirements

Observed 2026-09-20 Asia/Ho_Chi_Minh. This is a read-only security design review for bootstrap source `122fd50a60f55c66f34bed1ab035233a0d4b5744`. It closes the two exact findings recorded in [`../reviews/bootstrap.md`](../reviews/bootstrap.md): candidate code can reach the API-step parent environment/GitHub command files, and candidate rendering can mutate the authoritative policy checkout. No source or generated-output files were changed. These requirements are gates for the next exact source commit; they are not source approval.

## Security invariant

PR-built generator bytes are untrusted executable code. The policy result may become green only after a trusted base-owned verifier proves, for the exact same-repository PR head:

1. the producer run/job/workflow, source object, repository object IDs, platform, binary bytes, and candidate closure are bound by trusted API/tree observations;
2. the candidate was executed with no recoverable GitHub credential, runner credential, secret, or workflow command channel;
3. the candidate could write only disposable input/output space, never the authoritative source baseline or trusted verifier state; and
4. the candidate render exactly matches the clean checked-in bytes from that immutable source object.

Candidate `--closure`, `--revision`, JSON, artifact names, job display names, and any output written by PR code are assertions only. They may be compared as diagnostics, never used as provenance. A digest computed by a trusted wrapper over downloaded bytes, a trusted API response, and a clean commit/tree read are the provenance.

## Required hosted sequence

Keep candidate production independent of ordinary unit/Velnor jobs, but split trust boundaries into separate jobs and workspaces. Every action below is base-owned and pinned to a full SHA.

### 1. Unprivileged producer (`pull_request`)

- Keep the owner-only, same-repository gate and exact PR-head checkout. Forks do not produce a privileged candidate.
- Build the binary in the PR job with no secrets and no write-capable GitHub permission. The producer may upload the immutable candidate artifact through the normal artifact service; its manifest is untrusted input.
- The producer job may fail independently. No `continue-on-error`, skipped test, or fallback artifact may turn a failed build into a candidate.

### 2. Trusted acquire job (`pull_request_target`)

Use a base-owned job with a fresh workspace. It may hold the minimum token needed for fixed API/download actions (`contents: read`, `actions: read`; add `actions: write` only if a separate handoff artifact is unavoidable). Do not expose that token through a job-wide environment inherited by candidate code. No secrets, `id-token`, package-write, pull-request-write, or deployment permissions.

The fixed job must:

- resolve base/head from event fields and verify target repository full name **and numeric object ID**;
- query only the expected `ci-pr.yml` workflow and `pull_request` event, then require exact head SHA, repository/head-repository names and IDs, and the successful producer job's API database ID plus stable job identifier/display name;
- select an exact artifact ID from that run, reject expired/duplicate/missing artifacts, verify the service-reported artifact digest where available, download into a fresh scratch directory, and recompute SHA-256 before any binary execution;
- compute the full candidate closure from the clean head tree itself. A short artifact-name prefix is only a locator; full closure equality is mandatory, so same-prefix collisions fail closed;
- reject merge SHA, wrong workflow, wrong run, wrong job, wrong platform/features/profile, wrong head, stale run, and all fork identities;
- create a trusted handoff containing only measured byte digests, exact IDs, and immutable source data. Do not pass a writable `policy-checkout` directory or candidate-supplied manifest to the next job as authoritative state.

The acquire job must not run the candidate binary. Its only executable content is base-owned checkout/API/hash/archive logic. If it creates a handoff artifact, its wrapper computes the handoff digest and records the artifact ID/digest from the upload action; later jobs must re-download and re-verify it rather than trust a name or candidate JSON.

### 3. Candidate execution job (`pull_request_target`, `needs: acquire` only)

Run on a fresh hosted runner; preferably use a digest-pinned container with no Docker socket and no network (`--network none` or the runner-supported equivalent). This job has no GitHub write permissions and no secrets. `actions/download-artifact` may run as one fixed, SHA-pinned setup step with the minimum read access it requires, but its token/runtime environment ends before candidate execution.

Before launching the candidate, the fixed wrapper must:

- use a fresh disposable directory for the candidate binary, extracted source copy, and render output;
- recheck the trusted handoff's measured binary/source digests and exact head metadata;
- launch the candidate from an allow-listed environment (`env -i` or equivalent), with `GH_TOKEN`, `GITHUB_TOKEN`, `ACTIONS_RUNTIME_TOKEN`, `ACTIONS_ID_TOKEN_REQUEST_TOKEN`, `RUNNER_TOKEN`, package/cloud/registry credentials, and all other credential variables absent or empty;
- point `GITHUB_ENV`, `GITHUB_PATH`, `GITHUB_OUTPUT`, `GITHUB_STATE`, `GITHUB_STEP_SUMMARY`, and similar workflow-command paths at inert disposable sinks, or omit them entirely. The candidate job must not consume candidate-written env/path/output state;
- use absolute paths for every fixed tool and handoff file; do not let candidate writes alter `PATH`, later action inputs, the source archive, or verifier configuration;
- pass only nonsecret synthetic inputs such as `SOURCE_HEAD_SHA`, `SOURCE_REPOSITORY`, `SOURCE_CLOSURE`, runner platform, and explicit source/output paths;
- treat candidate self-reports as optional consistency checks after byte verification. A self-reported revision/closure never selects a source, artifact, or policy result.

The candidate may mutate its disposable source copy; that is a denial/failure at most. It must not have the trusted acquire job's workspace, its artifact service files, or the authoritative baseline. A candidate result is untrusted until the next job verifies it.

### 4. Trusted verify job (`pull_request_target`, `needs: execute`)

Use another fresh workspace and the minimum fixed API permissions (`contents: read`, `actions: read`). Re-download the authoritative source archive from the trusted acquire artifact, verify its recorded artifact ID/digest and exact head binding, and extract it independently. Never reuse the candidate job's source directory or a candidate-written baseline.

Collect the candidate render through a fixed wrapper/artifact. Treat every candidate result byte and result manifest as untrusted: identify the exact execute job/run/artifact through trusted API metadata, recompute its digest, reject path traversal/symlinks/unexpected files, and compare bytes against the clean source archive's checked-in generated files. The verifier must also reject generated output missing from or extra to the clean source surface, hand-written workflow files, and any source mutation. A candidate that rewrites its local source copy cannot alter this comparison.

Only this verifier may emit the policy pass/fail. Candidate job status, candidate test output, candidate self-report, artifact name, or a stale cached product cannot bypass a failed comparison. If handoff, execute, download, digest, closure, revision, or source comparison fails, the policy fails closed.

## Authentication and process-boundary rules

- API/auth token exists only in the trusted acquire/verify action or shell process that needs it. Never put `GH_TOKEN: ${{ github.token }}` on a step that launches PR bytes, and never rely on inline `GH_TOKEN=""` prefixes: a child can inspect its token-bearing parent through `/proc` on hosted Linux.
- A separate job is mandatory; a separate step alone is insufficient because steps share workspace, command files, and process-visible state. The candidate job must not inherit the acquire/verify workspace or token-bearing process tree.
- `permissions: {}` on candidate execution is necessary but not sufficient: remove token/runtime variables and command-file paths from the candidate process environment, and verify with an adversarial binary. No candidate process may have a useful `github.token` context, secret, artifact-service token, runner registration token, or package credential.
- Use only SHA-pinned base-owned actions for checkout, artifact handoff, and wrapper steps. Never check out or invoke a PR-controlled action under `pull_request_target`.
- Caches are acceleration only. On every restore, recompute the binary/source digest and exact closure; a cache hit never establishes provenance or replaces the trusted handoff.

## Required adversarial tests

Add deterministic fixture tests plus one hosted canary. They must fail the candidate path or leave the trusted result red:

1. Candidate reads its environment, `/proc` parent/all same-user environments, workspace, and command-file paths; tries to recover a token and writes env/path/output/state/summary files. Assert no credential is present, no trusted job state changes, and no later verifier command is hijacked.
2. Candidate rewrites generated workflows/config, deletes files, changes `.git`, creates symlinks/path traversal, and mutates its source copy during both metadata probes and render. Assert trusted clean-baseline comparison rejects the result.
3. Candidate self-reports a false revision/closure and supplies forged JSON; assert trusted tree closure, artifact SHA-256, run/job metadata, and clean bytes still decide acceptance.
4. Feed wrong workflow/path, run, job database ID/name, event, head repository/name/ID, head SHA, merge SHA, platform, expired artifact, duplicate artifact, malformed manifest, same-prefix/different-full-closure artifact, and fork fixtures. Every case must fail closed.
5. Make the producer compile/test fail, candidate render fail, or ordinary unit jobs fail/queue. Assert no `continue-on-error`, stale artifact, or test result bypass can produce a policy pass.
6. Repeat with an empty cache, shallow base checkout, and fresh runner. Verify exact head/tree acquisition, artifact revalidation, and no source build fallback in the consumer.

Static assertions must inspect generated YAML for separate jobs, `needs` only on the handoff chain, minimal permissions, no candidate execution in token-bearing steps, no candidate-supplied command-file state, SHA-pinned actions, exact workflow/event/head filters, and fail-closed branches. `actionlint`, format, source tests, and generated-output drift checks remain required.

## Admission gate

Do not approve a corrected commit from source inspection alone. Require the exact corrected SHA, generated outputs regenerated from it, the adversarial tests above, a clean/shallow hosted canary, and evidence showing the trusted verifier—not candidate status or self-report—issued the policy result. The schema-1 `candidate_publish` producer must still be removed in the separate migration; this design does not waive that no-legacy gate.
