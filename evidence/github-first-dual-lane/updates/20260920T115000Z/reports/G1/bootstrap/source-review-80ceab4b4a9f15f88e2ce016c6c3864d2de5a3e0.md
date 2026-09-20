# Exact bootstrap transport delta review — `80ceab4b4a9f15f88e2ce016c6c3864d2de5a3e0`

Observed 2026-09-20 Asia/Ho_Chi_Minh. Independent bounded review of detached worktree `/private/tmp/velnor-g1-bootstrap-review-80ce` at exact commit `80ceab4b4a9f15f88e2ce016c6c3864d2de5a3e0` (tree `35355785c307329a7c4e2567dd26dca2c9177ec9`, parent `975e85448495c2ab1b2b131d4e6d38607c0245a6`). The delta is one source file, `crates/velnor-workflow/src/s2/mod.rs`, 5 insertions and 5 deletions. No product, generated, remote, hosted-runner, Docker, or artifact state was changed.

## Verdict

**CHANGES REQUIRED. No G1, security, rollout, or live-authority approval.**

The delta materially improves the API-value scratch boundary and the requested generated hostile fixtures now cover the literal `bash ../candidate-source/evil.sh` and replaced-final-component cases. `O_CREAT|O_EXCL` rejects a pre-existing final symlink on this macOS host. It does not, by itself, prevent a same-UID attacker from replacing the newly-created parent directory; that conditional path escape is demonstrated below. The base-owned policy job's candidate separation and private parent make that attacker unavailable under the stated current boundary, but this is not a descriptor-relative/no-follow proof for a broader same-job threat model. The cross-job artifact binding failure remains red and is not repaired by this commit.

## Exact delta from `975e85448495c2ab1b2b131d4e6d38607c0245a6`

`candidate_bounded_gh_api_script()` (`src/s2/mod.rs:404-414`) changed `bounded_gh_api_value` from a temporary file followed by `rm` to a fresh `mktemp -d "$RUNNER_TEMP/gh-api.XXXXXX"` parent and `value.json` child. Failure and success cleanup now remove the private directory. The bounded writer still opens `${destination}.partial` with `os.O_WRONLY | os.O_CREAT | os.O_EXCL`, mode `0600`, and publishes with `mv`.

No generated workflow or ownership-state file changed in `80ce`; the three generated drift failures reported for `975` therefore remain until the authoritative generator regeneration is published.

## Hostile-fixture evidence

The hostile fixture file from `f2f1a3b0` has immutable blob `77d7d6d1b04f41ebc643821f818569ec7bd8eb19`. The same file was tested against the exact `80ce` source at `cacd384298cda3780e4926011275ea043672b1ff` (its parent is `80ce`; its only delta from `80ce` is that fixture file).

- `generated_namespace_scanner_rejects_candidate_source_host_command` mutates the generated producer workflow with the literal `bash ../candidate-source/evil.sh`; the exact generated scanner rejects it. **Pass.** This is scanner execution against the generated workflow, not execution of candidate bytes.
- `generated_bounded_download_rejects_replaced_partial_path` replaces the generated writer's partial path with a symlink to an outside sentinel; the helper exits nonzero and the sentinel remains unchanged. **Pass.**
- Full `cargo test --locked --package velnor-workflow --test bootstrap_transport_hostile`: **5 passed, 1 failed**. The sole failure is the intentionally hostile cross-job case, `generated_acquire_rejects_same_name_artifact_recreated_by_other_job`: an ordinary job can recreate the same-name artifact and the current acquire path accepts it. This remains an explicit unresolved artifact-to-job binding blocker.

## O_EXCL and private-parent assessment

An independent macOS check opened an existing symlink with `O_CREAT|O_EXCL`; it returned `FileExistsError` (`errno 17`) and left the target sentinel unchanged. The generated partial-symlink fixture independently confirms this behavior.

`O_EXCL` protects only the final component. A same-UID process that can remove the empty `mktemp -d` directory and replace that parent with a symlink can redirect `value.json` outside the scratch root before the bounded writer opens its partial path. Running the exact `80ce` helper shape with that parent-replacement race created `outside/value.json` containing the API bytes. This is a real conditional path-escape proof, not a mechanical request for `O_NOFOLLOW`.

The current policy contract runs this helper in a base-owned `pull_request_target` job, creates a mode-`0700` private parent, and does not execute candidate code in that job; the hostile candidate runs in a separate producer job. Under that trust boundary the same-job parent replacer is not demonstrated reachable. If the parent directory or runner scratch is attacker-mutable, this remains a blocker and requires descriptor-relative/private-parent ownership enforcement. Do not generalize the current result into a security approval.

## Remaining blockers carried forward

1. Exact generated output remains stale in `.github/ci/.github-actions-generator-state`, `.github/workflows/ci-pr.yml`, and `.github/workflows/ci-policy.yml` (the `80ce` source delta did not regenerate them).
2. Cross-job artifact ownership is not service-bound; the hostile replacement fixture fails as described above. Static uploader-role checks do not authenticate the returned artifact ID to the producer job/step/attempt.
3. Empty builder and sandbox image digests still fail closed. This is an adoption boundary, not proof that the unadopted cross-job provenance design is safe.

## Verification

- Exact `80ce` detached tree: clean; tree `35355785c307329a7c4e2567dd26dca2c9177ec9`.
- `git diff --check 975e8544 80ceab4b`: **pass**.
- `cargo fmt --all -- --check`: **pass**.
- Exact hostile fixture suite on `cacd3842`/`80ce` source: **5 pass, 1 expected security failure**; the two transport/scanner regressions above each pass in isolation.
- `crates/velnor-workflow/src/s2/mod.rs` SHA-256: `cd9b0975c8c0c8de3e382a54dd920149697b3414d56b9f05fc2a6dd616ef622b`.

No candidate bytes, Docker daemon, hosted payload, GitHub API mutation, live artifact acceptance, image pull, full-fleet reconciliation, or G1 authority claim was performed.
