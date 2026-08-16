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

## Active checkpoint work

Before stopping, finish only already-open green work and record its exact merge
SHA/run evidence. Do not broaden scope.

## Remaining ordered work

1. Complete runner 0.1.171 release, package rollout, daemon restart, and a live
   rejected-job proof containing phase, reason, effect, and remediation.
2. Release the preview-channel generator, mirror it into the three owner action
   repositories, update signed tags, and regenerate affected consumers.
3. Merge the Jackin preview admission and docs toolchain repairs after every
   required check is green; verify main remains green.
4. Prove the full preview loop: current main SHA published, rolling release
   `targetCommitish` matches, preview formula source SHA advances, hashes are
   bare 64-hex, embedded binary version matches, docs-only changes classify as
   `source=false`.
5. Inventory every workflow in all 28 repositories. For each trusted workflow,
   prove Velnor default plus optional GitHub/both selection. For each hostile
   public-unmerged workflow, prove GitHub-hosted isolation.
6. Resolve every duplicate generated workflow pair. Keep both only when they
   represent distinct trusted and hostile execution domains; remove stale or
   redundant files through the generator.
7. Repair all red CI/CD, merge only after all required checks are green, then
   verify each target branch is green.
8. Run the final generator audit, remote action-closure audit, exact fleet
   comparison, and independent zero-finding reviews.
9. Remove the implementation plan only after all acceptance conditions pass.

## Stop rules

- Never claim completion from green central CI alone.
- Never merge a red or pending pull request.
- Never use Homebrew for tooling; use mise.
- Never overwrite generated consumers directly when the generator owns them.
- Never include excluded legacy repositories.
