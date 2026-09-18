**Verdict: CHANGES-REQUESTED** — 1 moderate defect (dropped push trigger under false grounding), 2 low defects, 4 notes. All else verified.

## Defects (cited)

**D1 (moderate) — merge tier silently drops `push: branches: [main]`; grounding comment false.**
Copy has the trigger (`fix/restore-static-ci-after-992:.github-gen/sources/workflows/desktop-cadence.yml:17-18`); merge job runs on push+dispatch (`:41`). Migration renders dispatch-only (`.github/workflows/desktop-merge.yml:5-6`) while `.github-gen/velnor-workflow.toml:86-90` claims "no push trigger in the copy" — refuted by bytes (gap-report G2 had it right; ledger §10 "flag 2 DISSOLVES" premise is wrong). Schema genuinely can't express it (bare `push:` only, `check_profiles.rs:343-345` at pin) — but that makes it a schema-gap deferral needing explicit product sign-off, not "faithful". Mitigating: copy never merged, so nothing live regresses; still, intended main-push desktop signal (full graph + UI tests) is adopted at 0%. Also `toml:87-88` "every file carries it" is imprecise — unit files are `workflow_call`-only (`ci-unit-swift.yml` `on:`). Fix: rewrite comment with true facts + record dispatch-only acceptance + follow-up for branch-scoped events.

**D2 (low) — "no schema surface" claim false for the mise allowance.**
`toml:33-34` says the copy's unsafe-execution allowance has no surface; `[renovate] allowed_commands` exists at the pin and renders `RENOVATE_ALLOWED_COMMANDS` (`renovate.rs:269-278`). Mitigating: `renovate.json` has no `postUpgradeTasks`, so live effect is unclear, and deprecated-knob→regex translation is non-trivial. Fix: correct the comment; optionally declare or record acceptance.

**D3 (low) — L6 dead filter silently kept.**
`scripts/ci/docs-lychee-contract.sh` present with 0 refs; ledger L6 says delete. Neither deleted nor named in deferrals. Fix: delete or explicitly defer with docs.

**Notes (accept, no change required):** D4 — G12 sub-facts (`mirror.gcr.io`, fork-safe login gating, `MISE_TASK_RUN_AUTO_INSTALL=false`, copy `construct.yml:208,287,328,409`) unbanked for the construct follow-up; D5 — dispatching `desktop-scheduled.yml` runs the scheduled tier, copy gated it to schedule-only (new manual path, no auto change); D6 — copy's "Verify toolchain" step + mise version/cache-key pins not carried (diagnostic/tuning only); D7 — renovate timeout 60→120, action v46.2.5→v46.3.1, validator PR trigger widened-but-path-filtered — benign.

**Flags resolved with evidence:** strict-validator concern CLEARED — `renovate-config-validator --strict` passes on current `renovate.json` (tested with pinned 44.93.6 image); rate-limit flag DISSOLVES — `mise-action@v4.3.0` defaults `github_token` to `github.token`; `lanes="github"` is schema-forced (pin's `config/mod.rs:2723-2745` requires trust facts for velnor/both; repo has none — only `velnor_labels`).

## Per-row verdicts

| Row | Verdict | Evidence |
|---|---|---|
| L1 Swift runs-on | IMPLEMENTED | `ci-unit-swift.yml:120` macos-26, setup-action `:162`, zero e05aee6 |
| L2 pin | IMPLEMENTED | `toml:7` dead5ecb = origin/main HEAD post-#917; all refs dead5ecb; 403-fallback generated `ci-policy.yml:94-96` |
| L3 cleanup | IMPLEMENTED | `maintenance.yml:38-86`: 500-cap, no auth-retry |
| L4 construct | DEFERRED ✓ | tag-only schema; guard `mise.toml:76`, Dockerfile/VERSION kept; note D4 |
| L5 desktop | PARTIAL — D1 | tools/env/cron/timeout byte-exact vs copy; scheduled ✓; merge push dropped |
| L6 docs | DEFERRED ✓ | zero `docs:*` tasks; except D3 |
| L7 hygiene | DEFERRED ✓ | no named tasks, `tasks` required |
| L8 jackin-dev | DEFERRED ✓ | no gate/build tasks; G5 group correctly absent |
| L9 preview | DEFERRED ✓ | contract/producer/unsigned-publish gaps |
| L10 release | DEFERRED ✓ | 1-pkg contract vs 2 pkgs; no sign surface |
| L11 renovate | IMPLEMENTED | schedule/token/author/signoff exact; lanes forced; strict green; note D2 |
| L12 REUSE | DEFERRED ✓ | no task (tool exists `mise.lock:506`) |
| L13 aggregate | OBSOLETE ✓ | generated ci-required; no skip==ok |
| L14 archives | DEFERRED w/ L10 ✓ | zigbuild kept `mise.lock:267` |
| L15 registry-cache | IMPLEMENTED | generated prep + save gates `ci-unit-rust.yml:237-261` |
| L16 deployed-docs | DEFERRED w/ L6 ✓ | — |
| L17/L18 handoff | IMPLEMENTED | no copies; zero CI_XTASK refs; G4 dead ✓ |
| L19 attest | DEFERRED w/ L4/L10 ✓ | identity untouched |
| L20 capsule | RETAINED ✓ | zero non-.github diff |
| L21 fakes | IMPLEMENTED | no fakes; Record-step `:140`; 13/13 headers; pins uniform |
| F2/Q4 | IMPLEMENTED | `toml:129-130,142-143`; both callers `apple_executor:true` (`ci-pr.yml:2583,2599`); `needs:[plan]` = selection-only ✓ |
| C3 mise-arch | DEFERRED ✓ | inputs exist (`versions.env:3`), task missing |
| G1/G2/G3/G4/G6 | OBSOLETE ✓ | no sources-exclude, zero excludes, no dead branches, no env contract |
| G5/G8/G11/G13/G14 | ABSENT/DEFERRED ✓ | correct with L8/L9/L6/L10 |
| G7 split | STRUCTURE ✓ | one-file-one-trigger; D1 (push) + D5 note |
| G9/G10/G12 | IMPLEMENTED/NOTE | save gates ✓; generic cache keys ✓; D4 banks missing |

## Claim checks

(2) Every declared task/tool/path/env verified real: tasks `mise.toml:139,228,236`; all 11 tool ids are `mise.lock` `[[tools]]` keys; `renovate.json`, `Package.swift`, swift globs non-empty (`Package.resolved`/`.swiftpm` watch entries absent-but-harmless); env copy-grounded + `project.yml:29`. Nothing invented. (3) Headers 13/13 intact, pins uniform, all content traces to toml — no hand edits (policy self-check at dead5ecb left for CI; draft proved it at ancestor dc150d2f). (4) Pin real + schema-complete (verified each used key at dead5ecb). (5) All 13 YAML parse (ruby); run-names single scalars; triggers exact. (6) Zero `static_files`. (7) Swift on macos, both units Apple-bound.
