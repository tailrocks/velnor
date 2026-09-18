# Authenticate Dockerfile mise provisioning (fix main run 35204207659 flake)

## Problem

`Docker · Docker / GitHub` on main (`c04aec98`, run 35204207659) failed in
"Run unit checks": `Dockerfile`'s `mise install` made **unauthenticated**
GitHub API calls, the shared-runner egress IP had exhausted the 0/60 core
quota, and the `mr-boxington` packslip fetch 403'd. No test or compile
failure — tool provisioning never completed. Same flake class breaks the
zero-rerun streak requirement.

## Fix

Pass the automatic token into the provisioning layer as the `github_token`
build secret — the same secret id `docker/job-ubuntu.Dockerfile` already
consumes and the release lane already passes (`github_token=${{ github.token }}`):

1. `Dockerfile` — `RUN --mount=type=secret,id=github_token` on the mise
   provisioning step; conditionally export `MISE_GITHUB_TOKEN` from
   `/run/secrets/github_token` before `mise install`. The mount stays
   **optional** (no `required=true`): local and Velnor-lane builds without
   the secret keep their previous behavior.
2. Generator (`crates/velnor-workflow/src/lib.rs`) — hosted (GitHub-lane)
   docker unit commands append `--secret id=github_token,env=GITHUB_TOKEN`
   (PR, full, and cache-export commands; Velnor-lane commands untouched).
3. Generator (`crates/velnor-workflow/src/primitives/ir.rs`) — checks steps
   of GitHub-lane Docker jobs export `GITHUB_TOKEN: ${{ github.token }}`;
   all three checks-step render sites covered, other lanes/kinds get nothing.
4. Regen — `.github/ci/project.toml`, `ci-unit-docker.yml`,
   `ci-pr.yml`, `ci-main.yml`, generator state.

`velnor-workflow run` spawns unit commands with inherited step env
(`runtime.rs`, no scrubbing), so the step env name flows straight into the
buildx `--secret ...,env=GITHUB_TOKEN` binding. Secret names only — no
values anywhere in tree or logs.

## Proof the token reaches the layer

Live `docker buildx build` of this `Dockerfile` with a dummy token bound to
the `github_token` secret id failed exactly at the incident's fetch with:

```text
mise ERROR Failed to install packslip:github.com/jdx/mr-boxington@1.11.1:
  ... HTTP status client error (401 Unauthorized) for url
  (https://api.github.com/repos/jdx/mr-boxington/contents/.well-known/packslip.json?ref=HEAD)
hint: the token in `MISE_GITHUB_TOKEN` was rejected by GitHub (401 Unauthorized)
```

Same URL as the incident's 403, but 401/in-`MISE_GITHUB_TOKEN` instead of
unauthenticated quota exhaustion: the secret traverses the full chain
(step env → `--secret` → mount → `MISE_GITHUB_TOKEN` → mise's GitHub client).
With the real automatic token this call authenticates at the 5000/hr quota.

No retry added around the packslip fetch: not trivially safe (output parsing
masks real failures), and auth removes the failure mode structurally.

## Gates

- `cargo test -p velnor-workflow`: all suites green (1504 lib + integration, 0 failed)
- `cargo fmt --check`, `cargo clippy`: clean
- Regen: `--plain --dry-run` reports **0 files**, `--plain --check` passes
- `actionlint` on the three touched workflows: clean
- `docker buildx build --check` on `Dockerfile`: no warnings
- D19 pin untouched (`a6fa8d4a...`); this tree renders via the candidate
  exception until the follow-up pin roll, per the established flow

## Notes for reviewers

- `seed_compat` rotated in `ci-pr.yml`/`ci-main.yml`: the digest covers the
  unit recipe (`snapshot_compatibility`), which legitimately changed. One
  cold docker builder, then warm again.
- Schema-2 (`s2/`) carries parallel copies of the docker command builders;
  this repo generates schema 1, so `s2` is unchanged — needs the same
  treatment when a schema-2 consumer exists (follow-up, not this PR).
