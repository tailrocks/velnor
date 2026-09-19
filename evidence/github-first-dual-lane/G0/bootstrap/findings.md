# G0 bootstrap findings

## Scope and runtime

- Repository under review: `/Users/donbeave/Projects/tailrocks/velnor-project/velnor3`.
- Input tree: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- Effective local runtime was verified from `/Users/donbeave/.codex/config.toml:1-2,89-90`: Luna/max for this thread and default subagents. Root's Astra/low rollout is an explicit session override and is not inferred from this file.
- No generator output was edited. Main worktree remains `main` at the input tree, with only the user-provided goal document untracked.

## Active schema and provider routing

Schema-2 dispatch is active through `crates/velnor-workflow/src/lib.rs:5691-5699` and `crates/velnor-workflow/src/s2/dispatch.rs:36-58,84-133`.

The repository config currently declares both lanes for every automatic event:

- `.github-gen/velnor-workflow.toml:14-20`: `providers = ["github-hosted", "velnor"]`, and both values in `automatic_providers`/`default_dispatch_providers`.
- `crates/velnor-workflow/src/s2/config/mod.rs:174-188`: provider universe and automatic/dispatch subsets are separate typed fields.
- `crates/velnor-workflow/src/s2/mod.rs:1821-1853`: explicit subsets are applied and validated.
- `crates/velnor-workflow/src/s2/primitives/ir.rs:5142-5200`: a provider outside the automatic set renders `false` for PR/push/schedule admission.
- Existing proof: `crates/velnor-workflow/src/s2/primitives/ir.rs:139-162`.

Immediate hosted recovery is config-only:

```toml
providers = ["github-hosted", "velnor"]
automatic_providers = ["github-hosted"]
default_dispatch_providers = ["github-hosted"]
```

This retains explicit/manual Velnor dispatch while eliminating automatic self-hosted admission.

## Candidate bootstrap architecture

Policy is hosted and product-only:

- `.github/workflows/ci-policy.yml:17-30,51-63` runs the base validator on `ubuntu-24.04` and acquires the base runtime from the base setup action.
- `.github-gen/sources/actions/setup-velnor-workflow/action.yml:1-26,102-185` explicitly refuses to compile and requires an immutable release product with manifest, digest, closure, revision, and attestation checks.
- `crates/velnor-workflow/src/s2/mod.rs:4509-4615` computes the head candidate closure, polls the same-repository `ci-pr.yml` run, downloads the candidate artifact, and validates the manifest/digest/closure before executing it.

The candidate producer is currently coupled to normal Rust verification:

- `.github/workflows/ci-pr.yml:38-107`: Plan must acquire/publish the runtime artifact first.
- `.github/workflows/ci-pr.yml:1011-1017`: generator hosted unit caller has `needs: [plan]`.
- `.github/workflows/ci-unit-rust.yml:178-252`: hosted unit downloads/verifies the Plan runtime artifact.
- `.github/workflows/ci-unit-rust.yml:524-558`: substantive unit checks run before candidate packaging.
- `.github/workflows/ci-unit-rust.yml:559-634`: candidate build/upload is after checks and has no `always()`.
- `crates/velnor-workflow/src/s2/primitives/ir.rs:4751-4803`: source documents and emits the same ordering.
- `crates/velnor-workflow/src/s2/primitives/ir.rs:1904-1921,1923-2021`: candidate may reuse the unit binary; otherwise it builds a head worktree in the same job.

This creates the enabling condition: Plan/runtime, unit success, dependency admission, and Velnor scheduling can all prevent publication of the artifact that policy is waiting for. A bare `if: always()` change does not remove those dependencies.

## Minimal structural fix

Add an owner-only `candidate-bootstrap` control job to the PullRequest aggregate. The composition boundary is `crates/velnor-workflow/src/s2/primitives/aggregate.rs:24-58`, which calls `WorkflowIr::render_nested` at `crates/velnor-workflow/src/s2/primitives/ir.rs:3525-3555`.

Required contract:

1. Render only for the repository that owns `setup-velnor-workflow`; consumers use released products and do not publish candidates.
2. Gate to same-repository `pull_request`; no fork candidate execution.
3. Run on `ubuntu-24.04` with no `needs:` dependency on Plan, Rust units, dependency jobs, or Velnor.
4. Use a base-pinned immutable runtime/action to compute the exact head candidate closure. Do not dynamically execute an unverified PR-owned setup action before trust/source checks.
5. Checkout the PR head and run a locked Cargo build. Do not reuse a unit binary or depend on normal unit checks.
6. Emit the existing `velnor-workflow-candidate-<closure16>-<os>-<arch>` artifact and manifest fields expected by `policy_candidate_step`.
7. Remove the old `candidate_publish` workflow input and candidate steps from `ci-unit-rust`; exactly one producer remains.
8. Leave the policy consumer contract unchanged; missing artifact remains fail-closed.

The closure identity must remain aligned with policy. `crates/velnor-workflow/src/s2/closure.rs:64-76,171-175` defines debug candidate profile/features and states producer/consumer use `closure --rev ... --candidate`.

## Trust and source-identity tests

Add generator tests for:

- owner-only rendering; consumer trees have no candidate-bootstrap surface;
- PullRequest-only rendering; no candidate job in main/nightly;
- candidate job has no `needs:` and no Plan/unit/Velnor dependency;
- same-repository gate and no fork path;
- base runtime/action source is pinned to base identity, not dynamically selected from PR content;
- cold-cache and shallow-checkout paths fetch both PR head and base pin before closure computation;
- candidate artifact name, manifest `revision`, `closure`, `build_revision`, platform, run id, and binary digest match the policy consumer contract;
- missing artifact and digest/closure mismatch remain failures;
- old reusable `candidate_publish` input/steps are absent.

Existing candidate contract tests are at `crates/velnor-workflow/src/s2/primitives/ir.rs:458-519,752-785,908-975,1159-1187`.

## PR overlap and sequencing

Live API snapshot:

- PR952 head `a5c1c0bd5c92c4c52d58ccb21042b1b2c0b08637`; touches `s2/primitives/{ir,mod,release}.rs` and owns the overlapping `ir.rs` area.
- PR953 head `af31b644aa01eb352ba6240fca957e900174d067`, base `abe9ad82`; touches schema/config, `s2/mod.rs`, release, and generated outputs.
- PR954 was observed at head `f16592ea165ced141bf0bb1c43466a95d7df8b2e`, base `8964876f6b6b6eca624b2fb26f7eff1d5dc5d575`; it touches `s2/primitives/{mod,release}.rs`. Re-query before integration because an earlier snapshot reported `856a222b`.

The dedicated producer source task must wait for the PR952 seed/pin owner to finish and explicitly hand off `dual-lane-generator` (`codex/github-first-generator`, currently clean at `12cc87b6`). PR953/954 should not be merged merely to unblock candidate acquisition.

## Reproduction and verification commands

Read-only evidence:

```sh
rtk git status --short --branch
rtk git -C /Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-generator status --short --branch
rtk gh api repos/tailrocks/velnor/pulls/952 --jq '{head:.head.sha,base:.base.sha}'
rtk gh api repos/tailrocks/velnor/pulls/953 --jq '{head:.head.sha,base:.base.sha}'
rtk gh api repos/tailrocks/velnor/pulls/954 --jq '{head:.head.sha,base:.base.sha}'
```

After source implementation and regeneration:

```sh
cd crates/velnor-workflow
mbx run --locked --manifest-path Cargo.toml -- --plain --check ../..
cd ../..
rtk actionlint .github/workflows/*.yml
rtk rg -n 'candidate-bootstrap|candidate_publish|automatic_providers|default_dispatch_providers' \
  .github-gen/velnor-workflow.toml .github/workflows/ci-pr.yml .github/workflows/ci-unit-rust.yml
```
