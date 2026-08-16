# Velnor fleet unification goal status

Status: **stopped at an incomplete checkpoint**

Updated: 2026-08-16

The goal is not archived as achieved. The central Velnor-first model exists and
important repairs are merged, but the final acceptance condition—verified
conformance and green main branches across the exact 28-repository fleet—has
not been proved.

## Scope

- `jackin-project`: jackin, jackin-the-architect, jackin-role-action,
  jackin-agent-smith, jackin-sentinel, jackin-dev, homebrew-tap
- `tailrocks`: parallax, tablerock, velnor, holla, ruxel,
  parallax-telemetry-playground, termrock, schemalane, pg-bigdecimal,
  tracing-request-level, tailrocks-skills, homebrew-tablerock,
  homebrew-holla, homebrew-parallax, homebrew-ruxel, velnor-apt, holla-apt,
  velnor-actions-fixture
- `ChainArgos`: java-monorepo, jackin-agent-brown, blockchain-nodes

Legacy repositories outside this list are excluded.

## Completed at this checkpoint

- Established the generated fleet workflow model and exact 28-repository
  inventory.
- Made Velnor the default trusted lane; GitHub and both are optional modes.
- Kept hostile public-unmerged code on GitHub-hosted isolation.
- Added Velnor admission-closure, cache, provenance, runner-routing, and
  negative-proof coverage.
- Added detailed failed-step rejection evidence in Velnor runner 0.1.171.
- Repaired preview trigger/event desynchronization in affected product work.
- Added generated stable/preview package channels and verified Jackin preview
  formula mutation, including bare SHA-256 values and source-SHA binding.
- Merged the related green pull requests completed before this checkpoint.
- Published Velnor runner `v0.1.171` after a green release workflow.
- Released canonical and owner-mirror Velnor Actions `2026.8.27` and rolled the
  generated stable/preview caller into `jackin-project/homebrew-tap`.
- Merged Jackin preview admission/build repairs (#883 and #888) and deployed
  docs closure repair (#889), each after all pull-request checks were green.
- Proved both fresh preview archive build jobs and the publish gate succeed on
  Velnor. The remaining release mutation then failed with GitHub API `403` even
  though the job reported `Contents: write`; this is recorded as an unresolved
  Velnor job-token authorization defect, not hidden as success.

## Not completed

- Runner 0.1.171 package deployment and daemon restart proof. The release itself
  is complete; the live detailed rejection evidence was observed in failed
  Jackin jobs.
- Release and mirror rollout of the new preview-channel generator.
- End-to-end Jackin preview publish, rolling release, tap update, and docs-only
  negative proof on main.
- Resolution of the Velnor release-mutation authorization mismatch: the job
  advertises `Contents: write`, but `gh release edit preview` receives
  `Resource not accessible by integration`. Do not weaken repository defaults;
  repair Velnor token delivery or use a narrowly scoped installed App token.
- Exhaustive current-state audit of every workflow in all 28 repositories.
- Proof that generated duplicate workflow files are intentional or removed.
- Repair and post-merge green verification for every remaining fleet defect.
- Final three-way independent certification and plan removal.

## Resume condition

Resume only from `PLAN_STATUS.md`. Re-read live repository state and CI before
acting; do not assume this checkpoint is still current.
