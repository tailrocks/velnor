# Velnor Vision

Velnor is a GitHub Actions-compatible workflow runner with a Rust runtime.

The long-term product goal is a dependable runner and workflow engine for CI/CD
and general-purpose automation. It should feel familiar to teams already using
GitHub Actions: workflows, triggers, jobs, steps, matrices, environments,
secrets, artifacts, caches, reusable units, and runner labels should remain
recognizable.

## Phase 0 Vision

Phase 0 is intentionally narrow:

- keep existing GitHub Actions YAML unchanged
- let GitHub parse workflows, expand matrices/reusable workflows, schedule jobs,
  manage secrets, and render the Actions UI
- replace the runner side with Velnor
- use GitHub runner V2 through JIT runner configuration only
- run assigned Linux jobs inside Docker containers
- support Rust CI/CD workflows first, using `jackin-project/jackin` and
  `ChainArgos/java-monorepo` as the target projects

This means Velnor first succeeds when it can run real Rust GitHub Actions jobs
as a drop-in self-hosted runner replacement, with correct logs, outputs,
artifacts, cache behavior, Docker/Buildx behavior, and job conclusions in
GitHub UI.

## Non-Goals For Now

- no Velnor-native workflow language
- no Pkl/PQL/KCL implementation work
- no local YAML scheduler
- no broad GitHub Actions marketplace parity
- no macOS job execution or macOS runner-label support

Typed workflow authoring and new language ideas may be revisited after the
runner compatibility goal is proven. Until then, they are not active product
direction.

## Roadmap

Implementation details, open gaps, target project analysis, and verification
order live in [roadmap.md](roadmap.md).
