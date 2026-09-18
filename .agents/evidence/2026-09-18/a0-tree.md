# A0 local-tree audit — bastion campaign
Date: 2026-09-17 (UTC 2026-09-16). Workspace: /Users/donbeave/Projects/tailrocks/velnor-project/velnor3. Read-only; no edits made.

## 1. Git / PR state
- `git fetch origin`: clean, no new output (already up to date).
- `git status -sb`: `## docs/bastion-final-plan...origin/docs/bastion-final-plan` — clean, no local mods.
- Branch HEAD: `38ffbfd73bc67002ddce7e38fdaf15ff6497317d` ("docs(bastion): fix markdownlint errors in plan files").
- PR #912 (tailrocks/velnor): state OPEN, headRefName `docs/bastion-final-plan`, headRefOid `38ffbfd7…` — **EXACT MATCH, tree is at PR head**.
- Recent log: 38ffbfd7, 60fe0234 (regen), 960587a9, c6ec03df, ef1ea363 — all bastion-plan docs commits.

## 2. crates/velnor-workflow inventory
- 10 crates in workspace: velnor-bench, velnor-client, velnor-control, velnor-model, velnor-render, velnor-runner, velnor-tools, velnor-workflow, velnor-workflow-contract, velnorctl.
- velnor-workflow sources (src, .rs only): 40 files, ~55.6k lines total incl. tests/fixtures (57,563 with fixtures+docs).
- Heavy files: lib.rs 17,701 · ir.rs 5,953 · release.rs 4,834 · runtime.rs 4,237 · policy.rs 3,088 · config/mod.rs 2,773 · snapshot.rs 1,983 · policy/tests.rs 1,457 · mod.rs(primitives) 1,416 · runtime_products.rs 1,351 · tui/mod.rs 1,377 · tui/view.rs 1,076 · scan/rust.rs 1,359.
- Scan backends present: docker, docs, file_walk, gradle, homebrew(stub 22 lines), node, opentofu(stub 36 lines), rust, signals(stub 26 lines), swift. No apt/scale-set scanner.
- Integration tests: 5 files (generic_surface_literals 250, lane_pairing 517, selection_artifact_handoff 185, synthetic_surface 607, velnor_first_ci 1322).
- Test count (`cargo test -p velnor-workflow -- --list`, no run): **541 tests** (`grep -c ": test$"`; tail shows per-binary "33 tests" etc.).
- Crate is static workflow generator + CI client; dev-dep on velnor-runner for contract tests only. Lints: unsafe forbid, unwrap/expect/panic/todo deny.

## 3. .github/workflows inventory
- Count: **14 yml files** (15 dir entries incl. AGENTS.md), **13,191 lines total**.
- Files (lines): ci-main 2124 · ci-policy 178 · ci-pr 1956 · ci-release-package-signer 61 · ci-runtime-products 286 · ci-unit-bun 483 · ci-unit-docker 523 · ci-unit-docs 481 · ci-unit-opentofu 486 · ci-unit-rust 860 · maintenance 279 · nightly 114 · preview 1060 · release 4300.
- Local actions referenced: only **2 exist** (`.github/actions/setup-velnor-workflow`, `.github/actions/report-velnor-ci-outcomes`, one action.yml each).
  - `setup-velnor-workflow`: used by release.yml (×21), ci-pr.yml, ci-main.yml, maintenance.yml, preview.yml (×3).
  - `report-velnor-ci-outcomes`: used by all 5 ci-unit-*.yml (×2 each).
  - Reusable local workflows: ci-pr.yml/ci-main.yml fan out to ci-unit-*.yml; release.yml + preview.yml reuse ci-release-package-signer.yml.
  - Oddity: ci-policy.yml:60 and ci-main.yml:162 reference `./policy-setup-action/.github/actions/setup-velnor-workflow` (non-`./.github` prefix — verify path exists).

## 4. Bastion plan docs
- `plans/bastion-three-provider-ci/`: all 5 required files present (+ README.md 657 bytes).
  - spec.md 347 lines (40.5 KB) · work-plan.md 635 lines (43.4 KB) · checklist.md 26 lines (14.4 KB) · evidence.md 154 lines (13.1 KB) · goal.md 176 lines (15.3 KB).

## 5. Existing APT / release / scale-set code
- "bastion" appears ONLY in plans/bastion-three-provider-ci/*.md — **zero code references**. Campaign builds from scratch on that axis.
- Scale-set protocol (exists, runner-side): velnor-model/src/scheduler.rs (SCALESET_UPSTREAM_COMMIT cb0405b…, endpoint `_apis/runtime/runnerscalesets`, `X-ScaleSetMaxCapacity`, api `6.0-preview`, RunnerScaleSetStatistic, ScaleSetV2 kind) re-exported via velnor-model/src/lib.rs; consumers in velnor-runner/src/node/{scheduler,prove}.rs. 49 matches, 4 files. This is the GitHub Actions-Service scale-set protocol surface, not CI estate/scheduler work the campaign may need.
- APT release pipeline (exists, both sides):
  - Generator: velnor-workflow/src/primitives/release.rs has `render_apt_release` (line 2297), `"apt"` kind dispatch (lines 239/340/1744), apt-get install snippets; estate.rs has `render_apt_package_updater_template` + lane-split updater jobs; lib.rs wires apt updater templates + tests (apt_package_updater_*, apt_adopted_templates_*).
  - Runner: velnor-runner/src/release.rs (2,157 lines) + release/tests.rs (2,398 lines) — signed APT artifact verification, `InRelease` / detached-signature preverified metadata (all 33 InRelease matches live here).
- `acquire` (862 matches) is generic lock/lease vocabulary across crates, not campaign-specific.

## Exists vs must-build (for campaign)
- EXISTS: PR-head sync; 541-test generator; full APT release+verify path (render in workflow crate, InRelease verification in runner); GitHub scale-set protocol constants/types; 14 generated workflows + 2 local actions.
- MUST BUILD (nothing on disk): everything named "bastion" outside plans/ — three-provider CI, whatever spec/work-plan define. No stubs, no TODOs, no partial code.
- REUSE CANDIDATES: release.rs APT render/verify, scheduler.rs scale-set protocol, ci-unit-*/reusable-workflow fan-out pattern, setup/report local actions.
- WATCH: `policy-setup-action` prefixed uses-path in ci-policy.yml/ci-main.yml — confirm it resolves before campaign touches policy lanes.
