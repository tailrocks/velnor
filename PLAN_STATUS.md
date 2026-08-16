# Velnor fleet unification plan status

Status: **paused / incomplete**

This is the authoritative handoff for the stopped goal. It is not a completion
certificate.

## Finished phases

1. Defined the exact 28-repository scope and excluded legacy repositories.
2. Implemented the Velnor-default / GitHub-optional / both-optional runner
   contract in the central generator.
3. Split trusted and public-unmerged security domains.
4. Added admission-closure and provenance validation, cache isolation, and
   generated workflow audit coverage.
5. Merged Velnor detailed rejection-step support and prepared runner 0.1.171.
6. Repaired the central preview package-channel generator and Jackin tap
   preview updater.
7. Published runner `v0.1.171` and Velnor Actions `2026.8.27`, including both
   owner mirrors and the generated Jackin tap caller.
8. Removed unsupported cross-run preview artifact restoration. Fresh Velnor
   builds and the publish gate now pass.
9. Bound the nested deployed-docs mise action in both caller jobs.

## Active checkpoint work

None. All pull requests opened for this checkpoint are merged. The goal is
stopped with the runtime authorization defect and broader fleet proof recorded
below.

## Remaining ordered work

1. Complete runner 0.1.171 package rollout and daemon restart. Release creation
   and detailed rejection evidence are already complete.
2. Repair the Velnor job-token authorization mismatch. A real publish job
   advertised `Contents: write` but GitHub rejected release editing with `403`.
   Preserve least privilege: repair token delivery or use the installed,
   narrowly scoped Jackin package-updater App; never broaden default workflow
   permissions as a workaround.
3. Prove the full preview loop: current main SHA published, rolling release
   `targetCommitish` matches, preview formula source SHA advances, hashes are
   bare 64-hex, embedded binary version matches, docs-only changes classify as
   `source=false`.
4. Inventory every workflow in all 28 repositories. For each trusted workflow,
   prove Velnor default plus optional GitHub/both selection. For each hostile
   public-unmerged workflow, prove GitHub-hosted isolation.
5. Resolve every duplicate generated workflow pair. Keep both only when they
   represent distinct trusted and hostile execution domains; remove stale or
   redundant files through the generator.
6. Repair all red CI/CD, merge only after all required checks are green, then
   verify each target branch is green.
7. Run the final generator audit, remote action-closure audit, exact fleet
   comparison, and independent zero-finding reviews.
8. Remove the implementation plan only after all acceptance conditions pass.

## Stop rules

- Never claim completion from green central CI alone.
- Never merge a red or pending pull request.
- Never use Homebrew for tooling; use mise.
- Never overwrite generated consumers directly when the generator owns them.
- Never include excluded legacy repositories.
