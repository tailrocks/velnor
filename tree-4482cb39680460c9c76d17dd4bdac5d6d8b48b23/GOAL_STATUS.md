# Velnor fleet unification goal status

Status: **active / incomplete**

Updated: 2026-08-17

The goal is not archived as achieved. Central implementation and several live
package paths are proven, but exact conformance and green-main evidence across
all 28 repositories is not complete. The implementation plan must remain until
that fleet-wide proof passes.

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

## Proven complete

- Exact generated inventory: 28 repositories, five classes, five templates.
- Runner selector contract: `velnor` is the default, `github` is optional, and
  `both` means one real Velnor lane plus one real GitHub-hosted lane.
- Selected Velnor lanes are never event-dependently substituted with GitHub.
- Admission closure, cache, provenance, routing, and rejection evidence are
  fail-closed in central source and generated workflows.
- Runner checkout tokens remain runtime expressions when an installed GitHub
  App supplies them; the planner no longer replaces them with the Actions
  service token.
- Velnor runner `v0.1.173` is signed, released, installed through APT, and live
  on all ten Sentry daemon units; all nine doctor units pass.
- Exact live Velnor package-update proof passed after the runner repair.
- Jackin preview package update passed on Velnor and advanced
  `Formula/jackin-preview.rb` with the release source SHA and bare SHA-256
  values.
- Package updater source now uses the GitHub App client ID interface; six
  package repositories have owner-correct client-ID variables configured.
- Every pull request merged during these phases had green exact-head checks,
  followed by a green target-branch run.

## In progress

- Release and mirror the latest central changes, including real Velnor routing
  for public-unmerged events and GitHub App client-ID authentication.
- Regenerate and merge consumers across the exact 28-repository fleet.
- Finish the remaining green Java pull request and verify its merged head.

## Still required

- Exhaustively inventory every workflow in all 28 repositories.
- Prove every repository defaults to Velnor and exposes only the optional
  `github` and `both` modes defined by the contract.
- Verify public-unmerged safe work executes on real Velnor; unsafe work must be
  rejected before execution with a clear synthetic rejection step.
- Resolve duplicate trusted/public workflow files: retain only distinct,
  generator-authorized security domains; remove stale or redundant files.
- Verify preview trigger/condition/SHA agreement and live rolling preview state
  for every product/tap pair in scope.
- Verify package workflows, branch rules, required checks, and green main for
  every repository; private repositories must not require a DCO producer.
- Merge all in-scope Renovate and implementation pull requests only after their
  exact heads are green; leave no opened work unfinished.
- Run final generator, remote-closure, fleet-byte, live-run, and independent
  zero-finding audits.
- Remove the implementation plan only after every preceding condition has
  current authoritative evidence.

## Current evidence

- Central Velnor-lane correction: PR #53, merge `243882b49bbc6229ac8a47e413b8bae7d08b6cd4`.
- Exact central main CI: run `31960795662`, successful.
- Runner release: `v0.1.173`; exact release and package deployment previously
  verified before this checkpoint.
- Jackin tap preview update: PR #445, merge `526387d6`; post-merge CI and reuse
  workflows successful.
