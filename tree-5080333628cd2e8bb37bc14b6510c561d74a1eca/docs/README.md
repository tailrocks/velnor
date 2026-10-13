# Velnor Docs

This folder is the **source of truth** for Velnor's direction. Prompts in
[`../prompts/`](../prompts/) and all other documentation defer to it. When the
vision, plan, or roadmap changes, update the relevant file here (and record the
direction change in [`../AGENTS.md`](../AGENTS.md)) so nothing goes stale.

Start here:

- [Master plan](master-plan.md): **the top-level plan** — goal, the 2026-06-10
  incident review, the operator mandates (universal caching +
  rerun-idempotency, performance-first, estate unification, the three-repo
  dual-lane policy, switch-readiness, native-adapter completeness), and the
  phased execution sequence with live status.
- [Mission](mission.md): what Velnor optimizes for — fastest, Rust-first,
  nicer output, cheaper — and the hard rules every change inherits.
- [Vision](vision.md): product direction; drop-in runner replacement is
  achieved and in production, next phases are performance ceiling-chasing,
  adapter completeness, and UX superiority.
- [Roadmap](roadmap.md): runner-internal implementation reference —
  protocol decisions, daemon/slot model, Docker execution model, adapters,
  verification layers.
- [UI comparison](comparison.md): Velnor vs GitHub-hosted runner output and
  the API-driven method behind the UI parity work.
- [Runner usage](runner-usage.md): operator how-to — apt install, systemd
  units (`velnor-daemon`, `velnor-daemon@<name>`, doctor timers), secrets
  layout, and local/dev invocations.
- [Target live runbook](target-live-runbook.md): operator commands for
  fixture and target validation.
- [Debian + apt repository](debian-apt-repo.md): the release/delivery chain
  design (tag → CI → deb → signed apt repo → Pages) — now fully automatic.
- [Native action adapter contract](native-action-adapter-contract.md):
  contract for Rust-native action adapters (the no-JS product path).
- [Rust automation policy](rust-automation-policy.md): repository automation
  rule.
- [Runner V2 reference](reference/latest-runner-v2-refresh-2026-06-01.md):
  current upstream `actions/runner` V2 source pin used by
  `velnor-tools check-runner-reference`.
- [Target action registry](reference/target-action-registry.md): every
  in-scope action with a direct link to its latest upstream source — the
  source-of-truth for implementing/verifying adapters (Velnor tracks latest
  only).
- [Rust build cache hygiene → Velnor](rust-build-cache-hygiene-velnor.md):
  jackin disk-hygiene research mapped to runner gaps (budgets, GC, sccache
  vs kache, doctor inventory).
