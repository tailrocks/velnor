# D2B verification — PR tailrocks/velnor#926 (feat/d2-three-provider-remainder)

- Branch HEAD: `dc890899` (= origin/feat/d2-three-provider-remainder, verified via fetch)
- Base: origin/main `c04aec98` (merge #924)
- Worktrees (fresh, detached, /tmp-only writes): `/tmp/d2b-verify-wt` @ dc890899, `/tmp/d2b-main-wt` @ c04aec98
- CI run: 35206077424 (+ Policy run 35206077273), complete — no pending at final tally
- Read-only vs repo: no push/merge/amend; worktree dirtied once by generator run, restored via `git checkout --` (pristine HEAD confirmed)

## 1. BASELINE: FAIL

Final tally #926 (6 fail / 7 pass / 29 skipping, 0 pending):
- fail: `Control / Prepare Cargo / prepare-cargo`, `Control / Required`, `Docker · Docker / Velnor`,
  `Policy`, `Rust · velnor-workflow / GitHub`, `ci-required`
- pass: DCO, Control/Planning, Docker/GitHub, Rust prod-topology/GitHub, velnor-control/GitHub,
  velnor-model/GitHub, velnor-runner/GitHub

Baseline #922 and #924 (identical, 7 fails): `Bun/Velnor`, `Prepare Cargo/prepare-cargo`,
`Control/Required`, `Docker/Velnor`, `Documentation/Velnor`, `OpenTofu/Velnor`, `ci-required`;
Policy PASS and every /GitHub PASS on both.

Deviations from "exactly the pre-existing set":
- ADDED `Policy` FAIL (required check) and `Rust · velnor-workflow / GitHub` FAIL — both PR-caused,
  same root cause, log-proven:
  - workflow job: `error: generated files match but generation inputs changed: scan input changed
    from 33f5782652175e4f to 3924b522e3afa28c in .github/ci/.github-actions-generator-state;
    regenerate to record the current generation inputs`
  - Policy job: same error, then `pin a6fa8d4a… shares the base closure but the tree differs from
    its render; falling through to the candidate path`, then after ~4.5 min
    `##[error]no same-repository PR run published candidate velnor-workflow-candidate-e3b38507…`
- Operational-store leaves proven (log shows `##[group]Velnor rejected job (operational_store)`):
  only 2 — Prepare Cargo/prepare-cargo and Docker/Velnor. Baseline's other 3 Velnor leaves
  (Bun/Documentation/OpenTofu) did not run here (`scope: affected` → `skipping`), so the "5 leaves"
  set cannot literally repeat on this PR.
- Rollups `Control/Required` + `ci-required` fail with bare exit 1 (`Validate generated stack
  results`), same names as baseline rollups; they reflect the leaf failures above.

## 2. PIN+BASE: PASS

- `.github-gen/velnor-workflow.toml`: `cmp` vs origin/main → identical; pin line 9
  `revision = "a6fa8d4a5096e6df37250b59a8105abfebdb9013"` present.
- `merge-base(branch, origin/main)` = `c04aec98` = origin/main HEAD; exactly one commit on top
  (`dc890899 feat(workflow): three-provider planner, strict results, watchdog, trust (D2 remainder)`).
- DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` trailer present; DCO check PASS.
- Note: commit has no cryptographic signature (`%G?` = N) — identical to every direct commit on
  main (73356c2c, eb0303a3, ca832734 all N; only GitHub merge commits carry E). "Signed" holds in
  the DCO sense, consistent with project practice.

## 3. EMISSION: FAIL (rendered output identical; tree-clean criterion fails)

- `diff -r .github/workflows` branch vs main: EMPTY — schema-1 rendered output byte-identical. ✓
- `git diff --name-only c04aec98..HEAD`: only `crates/velnor-workflow/src/s2/` — 6 new files +
  mod.rs wiring; +3270/−0. No config-schema/emission file touched. ✓
- `cargo build -p velnor-workflow` + `./target/debug/velnor-workflow --plain --force` (exit 0,
  "Generated 21 files") then `git status`: NOT clean — `M .github/ci/.github-actions-generator-state`.
  The diff is exactly one line, `scan 33f5782652175e4f → 3924b522e3afa28c`, byte-matching the CI
  error; all output fingerprints unchanged. The PR is missing the regen commit (cf. #924's 73356c2c
  "chore(ci): regen generator state for R2 bridge sources"). This is the same root cause as item 1.

## 4. D2 ACTIONS: PASS (disproof attempts failed)

- `cargo test -p velnor-workflow --lib s2::`: 805 passed, 0 failed.
- New-module suites, all green: routing 6, planner 6, results 15, trust 12, watchdog 18,
  capability_tests 8 = 65/65, 0 failed.
- Unknown selectors fail: `routing::unknown_selector_key_fails_at_parse_never_routes`,
  `routing::missing_selector_fails_explicitly_and_never_falls_back_to_hosted`,
  `trust::unknown_requested_label_fails_explicitly`,
  `planner::overlapping_local_selectors_fail_before_expansion` — all pass; bodies use
  must-fail helpers that panic on Ok (fail-closed, not vacuous).
- Strict-result rejections per class: results:: 15 tests — `failure_classes_cover_all_nine`
  enumerates Missing/Skipped/Cancelled/TimedOut/Failed/(+4 more VerdictFailure variants);
  `cancelled/failed/skipped/timed_out/missing/stale_attempt/identity_mismatch/
  duplicate_conflicting/wrong_provider_report` each fail; `post_hoc_exclusions_never_excuse`;
  positives `all_green_passes`, `planner_declared_exclusions_are_accepted`.
- Trust denials: trust:: 12 tests — `bot_pr_is_untrusted_even_same_repo`,
  `fork_pr_defaults_to_hosted_even_when_verified`, `label_spoofing_a_privileged_lane_is_rejected`,
  `labels_never_widen_the_verdict`, `pull_request_target_is_never_privileged_untrusted_and_checks_out_base`,
  `same_repo_origin_alone_is_never_sufficient`, `untrusted_input_substitution_is_rejected`,
  `dispatch_needs_source_verification`, `every_event_variant_has_explicit_coverage`.
- Watchdog deadline proofs with measured numbers (all pass): `reserve_breach_…_180s…`
  (boundary `check(1000+180)` empty, `detection_latency_secs() <= 181`), `cleanup_…_120s…`
  (ditto ≤121), `stall_diagnoses_against_5m…` (≤301), `full_outage_reflects_within_10m…` (≤601);
  targets 180/120/300/600 also in module docs + `WatchdogThresholds::default`.
- Real routing: capability_tests.rs:18 imports the real `crate::s2::routing::route_unit`
  (+ real `planner::fanout`); `verify_probe_routing` calls it per provider and returns Err on
  outside-lane routing or empty labels; `nested_privileged_docker_routes_to_local_lanes_only`,
  `native_macos_routes_to_hosted_only`, `every_probe_routes_to_at_least_one_lane_with_labels`,
  `unroutable_capability_fails_explicitly` all pass.
- No silent-hosted-default path: `route_unit` propagates `check_capabilities(...)?` and
  `runs_on_for(...)?` (the latter is `.ok_or_else(usage("no [workflow.selectors.{provider}]…"))`,
  provider.rs:207-219); eligibility mismatch yields explicit `Exclude{reason}`; `route_all` aborts
  on first error so no partial table renders. `TrustReq::default = UntrustedOk` is typed, documented
  "hosted only" for untrusted events, and local lanes are still explicitly trust-excluded
  (`untrusted_event_excludes_local_lanes_with_trust_reason`). No `unwrap_or*`/fallback arm on the
  routing path (the `unwrap_or_default` at provider.rs:763 is pre-existing strict-results digest code,
  outside this diff; empty digest → mismatch → reject).

## 5. GATES: PASS (all four rerun locally in /tmp/d2b-verify-wt)

- `cargo test -p velnor-workflow`: PASS — lib 1569 + 15 integration targets, 0 failed.
- `cargo test -p velnor-runner`: PASS — 2166 lib (+4 ignored) + integration targets, 0 failed.
- `cargo clippy --all-targets -p velnor-workflow -- -D warnings`: PASS, exit 0.
- `cargo fmt --check`: PASS, exit 0.
- (Local gates green ⇒ CI's `velnor-workflow/GitHub` fail is the generator-state gate, not tests/lints.)

## 6. MIGRATION: PASS

- Expand-only: 7 files changed, +3270/−0; mod.rs diff adds six `mod` declarations only; nothing
  removed, renamed, or altered outside `src/s2/**`.
- Rollback: revert the merge.

## VERDICT: HOLD

Reason: the PR omits the one-line generator-state regen (`scan 33f5…→3924…` in
`.github/ci/.github-actions-generator-state`), which fails the required `Policy` check and
`Rust · velnor-workflow / GitHub`. Code content (items 2/4/5/6) verifies clean; rendered workflows
are byte-identical so the fix is exactly the fingerprint commit, same as #924's 73356c2c.
Re-verify items 1 and 3 after the author amends/adds the regen. Do not merge as-is.
