# Velnor fleet unification plan status

Status: **executing / incomplete**

This is the authoritative progress checkpoint. It is not a completion
certificate and does not replace the detailed implementation plan.

## Completed phases

1. Defined the exact 28-repository scope and excluded legacy repositories.
2. Implemented the generated Velnor-default / GitHub-optional /
   both-real-lanes selector contract.
3. Added admission-closure, provenance, cache-isolation, request-validation,
   and fail-closed aggregation evidence.
4. Added clear synthetic Velnor rejection-step evidence for pre-execution
   rejection.
5. Repaired preview trigger/event/SHA desynchronization and generated stable /
   rolling-preview package semantics.
6. Repaired dynamic GitHub App checkout-token planning in runner `v0.1.173`,
   released it, deployed it through APT, restarted all daemons, and proved a
   live Velnor package-update checkout.
7. Advanced the Jackin preview formula through the Velnor package updater and
   verified green post-merge workflows.
8. Replaced deprecated package-updater App IDs with owner-bound client IDs in
   central source and configured the six public package repositories.
9. Removed event-dependent GitHub-hosted substitution from selected Velnor
   lanes; PR #53 and exact merged-head CI are green.

## Active phase

Release the current canonical generator, create byte-identical owner mirrors,
regenerate all 28 consumers, merge every green rollout pull request, and prove
the resulting main branches.

## Remaining ordered work

1. Release the current canonical source under one new CalVer and create exact
   signed tags for all three owner-local mirrors.
2. Render every consumer from the canonical generator with those three
   immutable owner-local SHAs; never hand-edit generated workflows.
3. Merge every rollout and Renovate pull request after all exact-head checks are
   green; verify each merged main head green.
4. Audit all workflow files in every fleet repository. Confirm generated
   trusted/public domains are intentional; delete stale duplicates through the
   generator.
5. Prove selector behavior live: omitted input runs Velnor; `github` runs only
   GitHub; `both` runs one real lane of each kind. Prove safe public-unmerged
   work on Velnor and clear fail-closed rejection for unsafe work.
6. Audit all preview workflows for trigger/condition/SHA agreement; verify each
   rolling release targets current eligible main and each tap source SHA
   advances. Preserve docs-only `source=false` behavior.
7. Audit package channels, App-token closure, branch protection, required-check
   names, review requirements, and DCO policy across all 28 repositories.
8. Run final local generator/audit gates, remote action-closure verification,
   exact generated-byte comparison, live green-main inventory, and three fresh
   independent zero-finding reviews.
9. Update this checkpoint with final evidence, then remove the detailed plan
   only when every requirement above is proved.

## Hard rules

- Never claim completion from central CI or a subset of repositories.
- Never merge red or pending work; always verify the merged head afterward.
- Never use Homebrew; tooling installation and execution use mise.
- Never overwrite generator-owned consumers directly.
- Never route a selected Velnor lane to GitHub-hosted execution.
- Never include excluded legacy repositories.
- Never remove the implementation plan before exhaustive proof.
