# Velnor Docs

This folder is the **source of truth** for Velnor's direction. Prompts in
[`../prompts/`](../prompts/) and all other documentation defer to it. When the
vision, plan, or roadmap changes, update the relevant file here (and record the
direction change in [`../AGENTS.md`](../AGENTS.md)) so nothing goes stale.

Start here:

- [Master plan](master-plan.md): the top-level goal + execution sequence across
  Velnor and the target/reference repositories — current state, the 2026-06-10
  incident review and the bulletproofing it mandates, and the phased plan
  (operations hardening → GitHub-lane optimization → runner core performance →
  UX parity → default-on with continuous proof).
- [Mission](mission.md): what Velnor optimizes for — fastest, Rust-first, nicer
  output, cheaper. The four pillars every prompt serves.
- [Vision](vision.md): high-level product goal and Phase 0 scope.
- [Roadmap](roadmap.md): the plan — source of truth for what must be
  implemented, target Rust projects, V2/JIT-only protocol, macOS daemon host
  support, Linux Docker jobs, verification order, and next implementation work.
- [UI comparison](comparison.md): Velnor vs GitHub-hosted runner output, the
  observed divergences, and the GitHub-API extraction method behind the UI
  parity work.
- [Runner usage](runner-usage.md): operator commands to configure, run, and
  prove the runner locally, in Docker, and against the fixture.
- [Target live runbook](target-live-runbook.md): operator commands for fixture
  and target validation.
- [Native action adapter contract](native-action-adapter-contract.md): contract
  for Rust-native action adapters.
- [Rust automation policy](rust-automation-policy.md): repository automation
  rule.
- [Runner V2 reference](reference/latest-runner-v2-refresh-2026-06-01.md):
  current upstream `actions/runner` V2 source pin used by
  `velnor-tools check-runner-reference`.
- [Target action registry](reference/target-action-registry.md): every in-scope
  action with a direct link to its latest upstream source — the source-of-truth
  for implementing/verifying adapters (Velnor tracks latest only).
