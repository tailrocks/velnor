# Consumer baseline re-resolve — 2026-09-18 (READ-ONLY)

Method: `gh api` + `git ls-remote` only. Zero clones, zero writes, zero PRs/branches.
Retained baselines: Jackin `92f347ac39fbf0d6f9853168e2896a6c60522924` (40 units),
ChainArgos `235e479b150aeb949bc8a5190fba5b84f6303c80` (71 units).
A0 intermediate lives (2026-09-17): Jackin `0be3fcf9`, ChainArgos `38a8fb57`.

## Live SHAs (api ref + ls-remote agree)

| Repo | Audited baseline | Live main 2026-09-18 | Gap |
| --- | --- | --- | --- |
| jackin-project/jackin | `92f347ac` | `278cdbe517164564fc7206f37becff0e2b92b0b2` | 6 commits ahead, 0 behind |
| ChainArgos/java-monorepo | `235e479b` | `218a44b28984acf4ceee24cd1d9d6ccb8c38ae37` | 15 commits ahead, 0 behind |

Jackin gap (6): `0be3fcf9` heal cache mounts (#995) → `e4300b43` sync b6f53c09 (#996)
→ `1b90eb16` generic scheduled-checks schema (#997) → `5bf20aaf` pin 52c424b1 (#998)
→ `6023f430` pin 06050c9f (#999) → `278cdbe5` desktop tasks release (#1000).
ChainArgos gap (15): `38a8fb57` atlas fixtures + 14 docs(platform) commits (recorded-case
redesign handoffs, single-branch delivery policy) — all docs-shaped by message.

## JACKIN old → new → consequence

| # | Old (audited/A0) | New (live `278cdbe5`) | Consequence for F |
| --- | --- | --- | --- |
| J-1 | 9 workflows | 15 workflows: +`desktop-merge.yml`, +`desktop-scheduled.yml`, +`release.yml`, +`renovate.yml`, +`renovate-validate.yml`, +`renovate-upstream-sources.yml`; `nightly.yml` byte-identical (blob `5c7ef5b4` both) | F1 "9 workflows" ledger STALE. Migration surface +67%; regen inputs must be re-read at F-gate, not copied from A0 |
| J-2 | 40 units (36R/1B/1D/2S), same IDs | 40 units, SAME IDs (diff empty), same kinds; only manifest delta is `[release] kind="tasks"` | Unit coverage math HOLDS (38-Linux + 2-Swift §9.1 base). `ci-required` aggregate = plan+policy+40 unit jobs (verified in live `ci-main.yml`) |
| J-3 | Generator pin `b9c3156c…` | Pin `06050c9fa58fec8b5fd2c11c46756971e5270ec3` (via `b6f53c09` #996, `52c424b1` #998). Old pin STILL EXISTS in tailrocks/velnor (2026-09-16 rev-27 doc commit); all three new pins exist (`06050c9f` = #928 tasks-release execution shape) | Old pin valid but unconsumed. F repins to F-gate product regardless. Note: workflows now download `velnor-workflow-runtime-<pin>` artifact — pin couples to runtime availability |
| J-4 | `release.yml` 404/deleted, `[release] enabled=false`; spec §9.1: reconcile assertion WITHOUT restoring YAML or enabling releases | `release.yml` RESTORED (generated): tag push `v[0-9]*` + dispatch drill; `[release] enabled=true kind="tasks"`; build job (dispatch-only drill) + sign job (tag publish: build/verify/sign/notarize/attest, macos-26, env `release-macos`) | §9.1 ASSUMPTION BROKEN. F1 must re-scope: releases are now enabled via the new tasks-release primitive — validate generated release correctness instead of asserting absence; do NOT blindly re-disable |
| J-5 | Required: `ci-required` + DCO; declared contexts DCO,Policy,ci-required | Ruleset lists unchanged BUT 3 new `status="required"` check profiles OUTSIDE `ci-required`: `desktop-merge` (push→main, macos, 90min), `desktop-scheduled` (cron `41 4 * * 1`, macos, 90min), `renovate-upstream-sources` (push+PR, github runner, 10min) | New required contexts F must migrate/cover. macOS load: 2 existing Swift units + 3 new macOS jobs (2 desktop + release build/sign). §9.1 "114 executions + 2 macOS" math STALE |
| J-6 | `docker-e2e` profile defined, never invoked; 5th binary `per_mount_isolation_e2e` in default profile | UNCHANGED: 0 mentions of `docker-e2e`/`--profile` in all 6 surveyed live workflows; `per_mount_isolation_e2e.rs` still exists (blob `3c225248`, modified in gap); nextest filters unchanged | §9.1 E2E obligation still fully outstanding — carries into F unchanged |
| J-7 | `ci-unit-rust.yml` refs remote `tailrocks/velnor/...report-velnor-ci-outcomes@b9c3156c` | Remote outcome-action ref GONE; unit workflow downloads versioned runtime (`velnor-workflow-runtime-06050c9f…`) + verifies sha256. `.github/actions/` still 404 (correctly unused) | Outcome-reporting shape changed producer-side. F must not assume the A0 remote-action shape |
| J-8 | `velnor-workflow.toml`: 3 unit overrides, external checks `[DCO]` (A0 live) | +`[renovate]` (self-hosted writer, `RENOVATE_TOKEN`, validate), +3 `check_profile`s, +2 release jobs, swift `swift-package-native` +`capabilities=[xcframework]` +`depends_on=[rust-jackin-usage-ffi]`, second Swift unit +`capabilities=[xcode]` | Typed-config surface much larger; F regen must carry renovate/release/desktop inputs. New secrets/vars surface: `DEVELOPER_ID_*`, `APP_STORE_CONNECT_*`, `JACKIN_DEVELOPER_ID_*`, `RENOVATE_TOKEN`, env `release-macos` |

## CHAINARGOS old → new → consequence

| # | Old (audited/A0) | New (live `218a44b2`) | Consequence for G |
| --- | --- | --- | --- |
| C-1 | 11 workflows | 11 workflows, ALL BLOB SHAs IDENTICAL (e.g. `b48b3c6e ci-main.yml`, `f1179ca1 ci-unit-gradle.yml` both SHAs) | G1 "11 workflows" ledger HOLDS exactly |
| C-2 | 71 units (37G/17R/11D/4B/1N/1Docs) | `project.toml` blob IDENTICAL (`cc5d3104`, 110149 bytes) → same 71/same kinds. (Compare API hit its 300-file cap; CI-absence proven by direct blob reads, not the capped list) | 71-unit / 213-execution baseline HOLDS exactly. No coverage correction |
| C-3 | No `revision` key in consumer config; pin `1279c4f9…` unanchored | Config byte-identical (still no revision key). Pin `1279c4f9…` STILL EXISTS in tailrocks/velnor (2026-09-16 #879 clean-room regen) | Provenance gap persists, unchanged. G repins to G-gate product regardless |
| C-4 | `.github/actions/` 404; 6 unit workflows use broken local `uses:` | UNCHANGED: actions dir still 404; `ci-unit-gradle.yml:275` still `uses: ./.github/actions/report-velnor-ci-outcomes` | G1 "repair local actions FIRST" obligation stands, load-bearing for all 71 units |
| C-5 | 9 ansible-configs §6.1 paths pinned, zero drift | ENTIRE `ansible-configs/` dir byte-identical (all entry SHAs match, incl. `6c1e2ecf install-base.yml`, `85b9a2c1 install-docker.yml`, `cc0eda31 hosts.ini`, …) | C1 pins live content = audited content. Reference root: `.../tree/218a44b28984acf4ceee24cd1d9d6ccb8c38ae37/ansible-configs` |
| C-6 | 1-commit docs-only gap at A0 | 15-commit gap, zero CI files touched (docs/product + scripts only by path survey) | No CI drift. Docs churn raises merge-conflict surface at G time, not CI scope |

## Drift summary

- **Jackin: HEAVY CI DRIFT.** 5 CI commits in ~12h (#996–#1000): 3 generator pin bumps,
  new scheduled-checks/renovate/tasks-release primitives adopted, 9→15 workflows,
  releases enabled, 3 new required check profiles. Unit inventory (40 IDs) untouched.
- **ChainArgos: ZERO CI DRIFT.** 15 docs-only commits; every CI blob (11 workflows,
  project.toml, gen-config, ansible-configs) byte-identical to audit.

## Top risks to F/G

1. **(F) Spec §9.1 release clause invalidated** — releases went from disabled/absent to
   enabled/tag-publishing with signing secrets. F1 scope must be renegotiated (validate
   the generated release; new secrets/vars/env inputs), not executed as written.
2. **(F) Required-check surface grew outside `ci-required`** — desktop-merge/scheduled
   (macos, push-main + weekly cron) and renovate-upstream-sources are `status=required`
   standalone workflows. Any F plan that migrates only the 40-unit aggregate undercovers.
3. **(F) Fast-moving target** — 3 pins + new primitives in one day; F-gate pin/product
   must be resolved at execution, and the runtime-artifact coupling (`runtime-<pin>`
   downloads) adds an availability dependency per pin.
4. **(F) macOS execution math stale** — §9.1's "2 genuine macOS units" is now 2 units +
   2 desktop jobs + release build/sign jobs; three-provider/macOS capacity planning
   must be redone.
5. **(G) LOW RISK — but timing matters** — ChainArgos CI is frozen-clean today; the risk
   is a Jackin-style CI-adoption burst landing between now and G. Re-run this read-only
   check at G-gate. Docs churn (300 files) is merge noise, not CI scope.
6. **(G, persistent) Missing local actions still load-bearing** — all 71 units reference
   nonexistent `./.github/actions/report-velnor-ci-outcomes`; G1 repair-first order holds.

Evidence bytes retained: /tmp/jackin-live-project.toml, /tmp/jackin-audited-project.toml,
/tmp/jackin-live-main.yml, /tmp/jackin-live-rust.yml, /tmp/j-*-ids.txt (read-only fetches).
