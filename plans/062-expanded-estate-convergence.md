# 062 — Expanded estate convergence

## Goal

Bring all twenty repositories named in `VELNOR_PROJECTS_SETUP.md` under one
concern-based CI/CD contract: Velnor default, explicit GitHub selection,
combined parity, mise-only tools, nextest-only Rust tests, current stable pins,
bounded aggressive caches, single-writer publication, and stable aggregation.

## Scope

Add `jackin-agent-brown`, `velnor-actions-fixture`, `velnor-apt`, `holla-apt`,
`homebrew-tablerock`, `homebrew-parallax`, and `homebrew-holla` to the existing
thirteen-repository inventory. Re-audit all existing entries because current
remote heads and upstream action/tool versions may have changed.

## Work

1. Research current official mise, GitHub Actions, Cargo, nextest, sccache,
   BuildKit, artifact, Pages, package-repository, and Homebrew guidance.
2. Classify every concern per repository using direct product/workflow
   evidence. Never add irrelevant placeholder jobs.
3. Extend `config/estate-repositories.json` and Rust `audit-ci` enforcement so
   omitted expanded repositories, stale pins, ad-hoc installers, wrong lane
   defaults, unbounded caches, missing timeouts/concurrency, and canonical
   drift fail mechanically.
4. Converge each applicable workflow. Repository-specific commands, payloads,
   credentials, target platforms, and writer data remain parameters only.
5. Cancel stale runs/registrations, prove exact heads on GitHub and apt-upgraded
   Velnor, then measure cold, warm, and no-change classes.
6. Merge only Actions-only PRs whose exact-head canonical and dual-runner gates
   pass. Leave mixed product/CI PRs for operator review.

## Definition of done

- All twenty delivered default branches appear in machine-readable inventory.
- Automatic/default Linux workflows select Velnor; `github` is explicit;
  `both` runs identical jobs on both runners.
- Every tool install uses committed mise configuration and lock data; no live
  ad-hoc installer remains.
- Every Rust test path uses `cargo nextest run`; coverage displaced from
  doctests remains nextest-discoverable.
- Every applicable cache is keyed, bounded, trust-scoped, measured, and proven
  warm without mutable cross-job corruption.
- Every action/tool/toolchain uses current stable version and immutable pin;
  Renovate covers ongoing freshness.
- Exact-head V-A/V-B/V-C evidence is green; V-D soak ownership is recorded.
- Two consecutive full live audits find zero new gap.

