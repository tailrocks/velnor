# G1 bootstrap/hosted-contract review

Observed: `2026-09-19`  
Reviewer: `g1-cache-semantics`  
Scope: read-only owner-branch review; no source, generated, or remote mutation.

## Checkpoint: exact owner commits

No candidate-bootstrap implementation commit is available yet.

- Hosted owner branch is committed only through [`a38e459c`](https://github.com/tailrocks/velnor/commit/a38e459c7c88fa1c6e9a646a7513b2eecac292b8) (`ci: select hosted recovery lanes`). The typed `verification_providers` work is still dirty in `../dual-lane-hosted` (`.github-gen/velnor-workflow.toml`, `config/mod.rs`, `s2/mod.rs`, `release.rs`); it is not approved.
- Generator owner branch is committed through [`12cc87b6`](https://github.com/tailrocks/velnor/commit/12cc87b629802c294da9840325cb21087c020df6), with unrelated dirty follow-up edits in `s2/mod.rs` and `s2/primitives/ir.rs`.
- Integration branch is clean at `12cc87b6`; no bootstrap job commit is present in recent branch logs.

The following is a gate checklist, not approval of either dirty worktree. Re-review exact owner commits and their regenerated outputs after publication.

## Hosted release/preview contract

The intended recovery contract is:

```text
provider universe: github-hosted + velnor
automatic/default dispatch: github-hosted
release verification: github-hosted during recovery
release verification when field omitted: entire provider universe
```

Required regression proof:

1. Parse and validate `[release].verification_providers = ["github-hosted"]`. Reject empty, duplicate, unknown, and outside-`[workflow].providers` values.
2. Regenerate from a clean exact-head checkout. `--plain --check` must pass, and generator-state input/output digests must match the committed source/pin.
3. Inspect generated stable release jobs. There must be no `release-velnor-*` job, `needs` edge, Velnor runner label, or local-provider result requirement when the explicit hosted set is selected. A rejection/admission message mentioning Velnor is not a dependency; job IDs, `needs`, and `runs-on` are the authoritative scan targets.
4. Inspect generated preview. Current preview has publisher/build jobs, not release verification jobs; it must have no hidden Velnor job/dependency or self-hosted runner. If the requirement means preview must gain verification prerequisites, that needs explicit preview job/`needs` design; the stable-only helper does not satisfy it.
5. Exercise omission separately with the same two-provider universe. Stable release must emit both hosted and Velnor verification jobs and make publication transitively depend on both. It must not infer the release set from `automatic_providers` or `default_dispatch_providers`.
6. Test native build-gate expressions with the hosted-only set and with omission. Selected job IDs, `needs`, and dispatch scope must remain consistent; a local lane must not be required when it was not rendered, and normal dual omission must not become hosted-only.

Direct renderer tests are insufficient. Add one parsed-config/generator-surface fixture that checks generated `release.yml` and `preview.yml` for these exact job/dependency invariants.

## Candidate-bootstrap trust contract

The candidate producer must be structurally independent from Plan, unit tests, and Velnor capacity while policy remains fail-closed:

- Trigger on `pull_request`, same repository only. Never use `pull_request_target` for PR code. Bind immutable repository identity, exact `github.event.pull_request.head.sha`, and the selected run/job to the artifact.
- Use hosted runners and least privilege (`contents: read`; artifact transport only as required). No secrets, deployment environment, `id-token`, package write, self-hosted labels, Velnor admission, or PR-local action/composite. Pin all third-party actions by reviewed full SHA; checkout must set `persist-credentials: false`.
- Build the exact PR head in an isolated checkout with `cargo build --locked -p velnor-workflow`; do not build the synthetic merge or reuse a merge/unit binary. Scrub token/credential variables, wrappers, ambient `RUSTFLAGS`, and credential persistence. Candidate compilation executes PR code and must stay unprivileged/hosted.
- Compute candidate closure with the trusted pinned runtime, using the declared candidate profile/features/closure algorithm. Manifest must bind full `head_sha` as `revision` and exact-head `build_revision`, repository and immutable repository ID, platform, profile/features, full closure, producer run/job IDs, and binary SHA-256. Artifact-name closure prefixes are locators only.
- Policy must verify run/job conclusion and exact head/repository identity, manifest fields, full closure, and binary digest before executing `--closure`. A producer-added field is not a control unless the policy consumer parses and checks it. Current policy only regex-checks manifest revision and Rust `CandidateManifest` consumes `revision`, `closure`, and `binary_sha256`; exact source/run/job binding therefore requires consumer changes plus negative tests.
- Keep candidate publication independent (`needs: []` / no plan or unit dependency), but keep `ci-required` authoritative over every real unit/provider result. Candidate success must not hide a failing generator test; candidate failure must fail policy when a candidate is required.
- Remove `candidate_publish` and the old post-unit producer path completely, including dead reusable inputs. No compatibility alias or second producer may remain.

## Required negative/regression tests

- Candidate job has no `needs: plan`, unit, or Velnor job; unit failure does not prevent artifact publication structurally, while `ci-required` still fails on that unit result.
- Fork PR, wrong repository ID, synthetic merge SHA, stale artifact run, wrong producer job, wrong `build_revision`, wrong full closure, same-prefix/different-full closure, wrong platform/profile/features, and digest mismatch all fail closed before candidate execution.
- Policy tests prove the trusted base pin/setup action and scan state are used; PR-head workflow/action changes cannot grant privileged execution. Generated workflow scan rejects `pull_request_target`, secrets, environments, self-hosted labels, dynamic runners, and unpinned actions in the bootstrap job.
- End-to-end source/pin/scan-state test runs the generator from the exact owner commit, checks generator state, then validates generated release/preview/bootstrap workflows. Hosted run evidence must come from that exact committed, clean candidate—not a dirty worktree.

## Verdict

Checkpoint: waiting on clean exact commits from hosted/bootstrap owners. `a38e459c` and `12cc87b6` are source baselines only; no bootstrap or typed-release commit is approved from the current dirty worktrees.
