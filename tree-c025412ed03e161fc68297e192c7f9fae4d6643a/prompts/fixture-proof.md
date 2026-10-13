# Goal: Fixture Proof Completion

> **Mission:** see [`../docs/mission.md`](../docs/mission.md). The fixture proves Velnor is a
> drop-in, Rust-first runner. Beyond "green", capture **timing and cache
> evidence** that shows Velnor is faster than the GitHub-hosted lane on warm
> caches — that speed/cost advantage is the point of self-hosting.

> **Direction (source of truth):** [`../docs/`](../docs/) —
> [vision](../docs/vision.md), [roadmap/plan](../docs/roadmap.md),
> [runner usage](../docs/runner-usage.md). If this prompt and the docs disagree,
> the docs win.
>
> **Run order: 3 of 3** (see [`README.md`](README.md)). Run last — after *Target
> workflow coverage* (#1) and *GitHub UI parity* (#2). This is the Phase 0 gate
> that proves the previous two green end-to-end with evidence.

## Objective

Drive `tailrocks/velnor-actions-fixture` to a **fully green Velnor proof**: GitHub
schedules the fixture jobs, Velnor consumes them through the V2 JIT / broker /
run-service path, the Velnor matrix lanes pass, and the `compare` jobs confirm
the Velnor lane output matches the GitHub-hosted lane — with evidence captured.

## Why

The fixture is the contract (see [`docs/roadmap.md`](../docs/roadmap.md) and
[`AGENTS.md`](../AGENTS.md)). It proves Velnor can execute the exact GitHub
Actions patterns the target repositories use, *before* any operator-run target
validation. The remaining Phase 0 gate is a green fixture proof with comparison
evidence on a Linux host. Agent-owned work stops at the public fixture and a
clear "ready for manual target testing" report.

## Ground truth

- Fixture repo: <https://github.com/tailrocks/velnor-actions-fixture>
- Lane model: GitHub-hosted lane (`ubuntu-latest`) vs Velnor lane
  (`[self-hosted, velnor-target-mvp]`), matrixed, with `compare` jobs.
- Protocol behavior: <https://github.com/actions/runner>.

## In scope

- Fixture readiness, audit, smoke, and compare pipeline running clean.
- Every fixture workflow lane (`compat`, `docker`, `pages`, `renovate`,
  `schedule`, `multi-arch`) passing on the Velnor lane.
- `compare` / `aggregate-needs` jobs confirming Velnor ≡ GitHub-hosted output.
- Evidence written to `.velnor-live-evidence/`.
- Fixing **Velnor** whenever a fixture step fails — never the fixture.

## Out of scope

- The visual UI representation details (that is the *GitHub UI parity* prompt —
  though a green compare depends on correct outputs/artifacts).
- Real ChainArgos / Jackin repositories. **Do not** set
  `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true` or run them.
- Adding new fixture content to dodge a gap (forbidden).

## Definition of done

- `scripts/fixture_readiness.sh` clean; `scripts/fixture_smoke.sh` runs Velnor
  daemon (JIT/V2) and all Velnor lanes pass.
- All `compare` jobs succeed (`gh run watch … --exit-status` exits 0).
- `cargo fmt --check`, `cargo test -q`, `velnor-tools fixture-audit` all green.
- Evidence captured; a "ready for manual target testing" handoff produced.

## Work through

➡ **[fixture-proof.checklist.md](fixture-proof.checklist.md)**
