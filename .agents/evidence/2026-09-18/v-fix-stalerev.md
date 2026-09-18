# Verdict: CERTIFIED — fix-stalerev (branch fix/pin-fetch-in-tool @ 48b49d92)

Independent verification only. No repo edits, no merges, no pushes.

## Identity
- Fetched origin; `fix/pin-fetch-in-tool` == `origin/fix/pin-fetch-in-tool` == `48b49d928877585522aaec4fc3495f2e3ff27bd0`.
- Base `docs/bastion-final-plan` == `38ffbfd7` as claimed. Single-commit diff, signed off (`Signed-off-by: Alexey Zhokhov`).
- Diff scope: 9 files, all within the claimed set (policy.rs, lib.rs, policy/tests.rs, ir.rs, release.rs, runtime_products.rs tests, 2 workflow YAMLs, generator state).

## Diff inspection (vs /tmp/a1-stalerev.md P1+P2+P3)
- P1 (policy.rs): `regenerate_and_compare` Checkout arm calls `ensure_pin_present(checkout, pin)` before `expected_closures`; short-circuits on `commit_exists`; else `git fetch --no-tags --depth 1 origin <pin>`; still absent → `GeneratorError::usage(pin_fetch_failure(...))` naming pin + shallow (`fetch-depth: 0`) / full-history (`re-pin`) state + remediation. Remote arm untouched (`expected_closures(...).ok()` fallback preserved). No `CARGO_NET_OFFLINE` gate on the self-fetch (the only offline refs gate the pre-existing pin-build lookup). Strict closure verification kept.
- P2 (lib.rs + callers): `workflow_pinned_policy_runtime_velnor(checkout)` — revision param removed, no baked `PINNED_REVISION` env, runtime `sed` parse of `$CHECKOUT_PATH/.github-gen/velnor-workflow.toml` fail-closed, parse-before-fetch. All three production callers updated: `policy_job` (lib.rs:3998), unit lanes (ir.rs:4251), release lanes (release.rs:1815, Velnor+check-running gate preserved). No 2-arg callers remain.
- P3 (ir.rs + regen): `Fetch D19 pin history` emission deleted; the string survives only in two negative test assertions. YAML diff is regen-only: fetch step gone from `ci-unit-rust.yml`, live-pin provision in `ci-unit-rust.yml` + `release.yml`, generator state hashes only.
- Deferred R2 (PR-side pins lack a Velnor release product) correctly not bundled; no runtime-product changes in diff.

## Proof (scratch worktree /tmp/vs-worktree @ 48b49d92, since removed)
- `cargo run -p velnor-workflow -- . --plain --dry-run` → `0 files would change`, exit 0 (regen-only YAML confirmed by the tool itself).
- Focused: `checkout_arm_self_fetches_the_pin_in_a_shallow_clone` + `pin_self_fetch_fails_loud_when_the_remote_lacks_the_pin` → 2 passed; `velnor_provisioner_reads_the_live_declared_pin` + `no_lane_fetches_pin_history_the_tool_self_fetches` → 2 passed; `checked_in_workflows_match_the_generator_byte_for_byte` → 1 passed.
- Full: `cargo test -p velnor-workflow` → 544 passed (8 suites), 0 failed — matches claimed 489 lib + 55 integration.
- `cargo clippy -p velnor-workflow --all-targets` → no issues; `cargo fmt -p velnor-workflow --check` → clean.
- Live-pin `sed` independently re-run against the real `.github-gen/velnor-workflow.toml` → parses `7341ef4b…`. Worktree clean after runs.
