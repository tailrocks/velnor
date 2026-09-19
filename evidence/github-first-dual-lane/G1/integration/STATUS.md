# G1 integration coordinator

Observed 2026-09-19 UTC. Worker thread: `01a0ba72-3925-7141-b1f7-5529a5cf6c98`; effective model `gpt-5.6-luna`, reasoning `max` (verified in `state_5.sqlite`).

## Integration input

- Source repository: `tailrocks/velnor`
- Base revision: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`
- Isolated worktree: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-integration`
- Branch: `codex/github-first-recovery`
- Current candidate: `12cc87b629802c294da9840325cb21087c020df6`
- Candidate provenance: source-only PR #952 integration; reviewed source commit is recorded in `../G1/seed-pin-source.md`.
- Mutation policy: no push, remote merge, release, or generated-file edits until the root authorizes the exact candidate.

## Dependency graph

| ID | Component | State | Required handoff |
| --- | --- | --- | --- |
| G1-SOURCE-952 | PR #952 seed + shallow D19 pin source | approved/ready | `12cc87b629802c294da9840325cb21087c020df6`; `/root/g1_review_952` explicitly approved. Reviewer reports 1858 full tests, clippy, actionlint, seed-key parity, and shallow pin fetch proof. Generated outputs intentionally remain stale until pin promotion. |
| G1-SCAN-INTEGRITY | Handwritten `.github` provenance + generated-output filtering | source commit ready; integration review pending | `6409a08678683506c12c7d50820875c1fa703b9d` on `codex/github-first-scan-integrity`, based on reviewed `12cc87b…`; 1740 tests (snapshot excluded), scanner regressions, shallow clone, clippy pass. Do not cherry-pick until `/root/g1_review_952` records approval of this exact commit. |
| G1-CONFIG-HOSTED | Hosted-only automatic/default providers + typed release verification | source/config in review | Config-only base candidate `a38e459c…`; `/root/g1_hosted_config` subsequently added uncommitted typed `[release] verification_providers` changes in its own worktree. Do not integrate until its independent review approves the complete diff. |
| G1-BOOTSTRAP | Acyclic generator/runtime acquisition and shallow pin proof | implementation active | `/root/g0_bootstrap` received handoff and is editing only `dual-lane-generator`; proposed owner-only `candidate-bootstrap` has no `needs` edge. Current uncommitted diff calls `candidate_bootstrap_job(...)`, but no definition is present yet: do not consume until compile/tests and commit complete. |
| G1-RECORDS | Canonical SPEC/PLAN/STATUS/fleet/evidence/runbook | pending | `/root/g0_records`; must remain outside the attested source revision for live evidence. |
| G1-CHECKER | Deterministic evidence checker and negative fixtures | pending | `/root/g0_checker`; must fail stale/missing/skipped/wrong-provider/failed-child evidence. |
| G1-REVIEW-952 | Independent source review | passed | `/root/g1_review_952`; approved `12cc87b…` with explicit post-merge pin/regeneration requirement. |
| G1-REVIEW-DISTRIBUTION | Independent release/distribution review | pending | `/root/g2_distribution_review`; later G2 dependency, not a reason to mutate G1 now. |

## Required staged sequence

1. Obtain independent approval for `12cc87b…` and the hosted-only config candidate.
2. Integrate only reviewed source/config changes in this worktree; preserve generated files until the selected generator source revision is fixed.
3. Run generator source tests once on the combined candidate with `CARGO_TARGET_DIR` outside the checkout and four build jobs.
4. Use a staged source release/runtime artifact that can be obtained from a clean shallow checkout without depending on the policy check that consumes it.
5. Adopt the resulting immutable generator pin in `.github-gen/velnor-workflow.toml`; regenerate all `.github` outputs and the state sidecar together.
6. Prove clean regeneration and policy/drift checks from a clean shallow clone. Only then advance G1 hosted PR/main runs and any required-check transition.

## Current known risks

- The source-only candidate intentionally makes the checked-in generated snapshot stale; the source suite's one snapshot failure is expected until pin promotion/regeneration and must not be bypassed.
- Current main configuration still selects both providers automatically. G1 cannot pass until the reviewed hosted-only config is integrated and generated outputs match its selected generator revision.
- Existing policy bootstrap is circular on current main: policy can wait 15 minutes for a candidate product whose producing run is blocked by policy/generated-tree failure. The fix must stage producer publication before consumer pin adoption.

## Investigation findings (read-only)

- The D19 mismatch is explainable, not random: current main records scan `bab3e77b3257adc8` with generator revision 52, while declared pin `fdeed261…` is commit `fdeed261` with generator revision 50 and renders scan `d503cfb3b27c320e`. The pin must be promoted only after a source product is published; never “repair” this by editing the sidecar alone.
- The scan input has a structural invalidation risk. `RepositoryShape.files` is the complete tracked path list (`s2/scan/mod.rs:31-76`), and `GenerationInputs::current` hashes its canonical JSON (`s2/mod.rs:6815-6829`). Generated workflow filenames are tracked and therefore enter the scan hash; adding/removing an owned generated file can make a first generation alter its own next scan input. This is an output→input edge, not a safe stable fixed point. Bootstrap/source review must either prove the exact fleet surface is closed before generation or remove generated-owned paths from scan-shape provenance; sidecar edits cannot solve it.
- State provenance is otherwise fail-closed: `generated_check_error` (`s2/mod.rs:6389-6412`) rejects input drift even when output bytes match. The deterministic gate must retain that check and test clean shallow render twice, not suppress it.
