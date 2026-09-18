# TRIGGER-FIX: INFEASIBLE as specified (render-neutral GH-API fetch)

Date: 2026-09-17. Verdict: **INFEASIBLE-render-neutral**. Zero repo edits made
(branch created and removed, tree clean at origin/main 33688938). No PR opened.

## Specified mechanism and why it dies

Spec: policy binary, on pin-render != tree + PR context + no manifest, fetches
the unit-published candidate artifact for `wanted` via GH API with "token/run
context from env", bounded poll like Acquire. Render-neutrality required (old
vs new generator render diff EMPTY over velnor + jackin/chainargos fixtures).

The Enforce step exposes NO token to the policy binary, and every way to give
it one is a rendered-YAML change that itself can never land green:

1. **No token in Enforce env** (`.github/workflows/ci-policy.yml:154-168`):
   only WORKFLOW_ROOT/HEAD_SHA/BASE_SHA/VELNOR_WORKFLOW_POLICY_REVISION.
   GH_TOKEN mappings exist ONLY on Acquire (:67) and Ruleset (:133) steps.
   Line 127 actively empties tokens before exec'ing the candidate; the job
   comment states the token is "confined to the Acquire/Ruleset API steps,
   and both candidate exec points tokenless". Design denies tokens to Enforce.
2. **`gh` refuses unauthenticated**: with empty GH_TOKEN/GITHUB_TOKEN and empty
   GH_CONFIG_DIR, `gh api ...` exits "populate the GH_TOKEN environment
   variable" — even for public reads. A binary shelling to `gh` is dead.
3. **Artifact download requires auth** (curl, zero auth headers, public repo):
   runs-list 200, artifacts-list 200 (candidate visible), but
   `GET .../actions/artifacts/10478850186/zip` -> **401 Requires
   authentication**. Listing is public; the blob is not. Raw-HTTPS fetch dies.
   (Unauthenticated poll would also blow the 60/hr shared-IP rate limit.)
4. **No ambient credential anywhere**: every `gh` call-site in every generated
   workflow carries an explicit per-step `GH_TOKEN:` mapping (nothing relies
   on ambient); setup action writes only GITHUB_PATH/GITHUB_OUTPUT (grep for
   GITHUB_ENV empty); checkout uses persist-credentials:false. GITHUB_TOKEN is
   not ambient on runners (else the Acquire mapping would be redundant).
5. **Token-threading is undeployable-green** (base-ownership circularity):
   under pull_request_target the workflow is BASE-owned, so a token-adding
   YAML change (Enforce env or Acquire export) takes effect only post-merge.
   The enabling PR is itself a render-changing generator PR (lib.rs render
   code + regen): phase-1 shape (pin==base) goes red under old base (no
   trigger) AND under new base (trigger present, token still absent from base
   YAML); self-bump shape starves Planning (no pin product). The documented
   jointly-unsatisfiable set; bypass is campaign-forbidden. The token can
   never arrive green — not now, not after any trigger deploy.

## Artifact-content investigation (parent's STOP dimension: SUFFICIENT)

Downloaded the live candidate
`velnor-workflow-candidate-7dcabc83ea2c2a6b-Linux-X64` (run 35175925732).
Contains `velnor-workflow` + `candidate-manifest.json` with
profile=debug, platform=Linux-X64, repository, run_id, revision=1bee4f23 (PR
head), closure=7dcabc83... (== wanted closure from the red diagnosis),
build_revision (merge SHA, unit-binary fast path), binary_sha256.
Digest recomputed locally: 55f7be88... == manifest claim. All fields the
Acquire-equivalent binding needs (profile/platform/repo/run/shape/closure/
digest/reported) are present. **No artifact-content change needed** — the
blocker is purely transport auth, not content. Do NOT change rendered YAML.

## Why no code was shipped

A "fetch when GH_TOKEN present, else skip" trigger WOULD be render-neutral
and WOULD land green — but can never fire in CI (token never present), so it
is dead code that would falsely unblock the chain. Shipping known-dead
load-bearing code violates project rules; refused.

## Sole remaining design (out of scope, needs orchestrator decision)

ACTIONS_RUNTIME_TOKEN (+RESULTS_URL) is ambient in every step and CAN fetch
cross-run same-repo artifacts (what actions/download-artifact uses). A trigger
could reimplement that internal twirp protocol (ListArtifacts ->
GetSignedArtifactURL -> blob GET -> unzip -> bind -> render) via curl shell-outs
with zero YAML change. NOT attempted because: (a) not "GH API" as specified;
(b) undocumented internal protocol, GitHub-breakable, architecturally backward
for a codebase on stable surfaces only; (c) UNVERIFIABLE here — no runtime
token exists outside a live run, so the core would ship blind; first live
exercise is a post-merge phase-1 PR, where failure costs the chain days, not
hours. Verifying it needs a live-run protocol probe (scratch repo + workflow
calling the twirp endpoint with curl and printing results), which is beyond
this task's authorization. Also noted: trusting sibling-run green status
instead of re-rendering would be trust-breaking (ci-pr.yml runs PR-branch
code under `pull_request`, trivially fakable) — forbidden alternative.

## Options for the orchestrator

(a) Authorize the runtime-token protocol probe + implementation as its own
task, accepting fragility. (b) Amend the no-bypass rule for a one-time
bootstrap red-merge (the historical #907/#911 precedent). (c) Redesign the
rendezvous out of artifacts entirely (e.g. candidate-as-release-asset is
publicly fetchable — but its publish-side YAML change faces the same
base-ownership circularity; needs its own deployment proof).
