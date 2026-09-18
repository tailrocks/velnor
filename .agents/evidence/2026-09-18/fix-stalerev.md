# A1 stalerev fix evidence

- Branch: `fix/pin-fetch-in-tool` (from `docs/bastion-final-plan` @ `38ffbfd7`)
- Commit: `48b49d92` (signed off, pushed to origin)
- Diagnosis: `/tmp/a1-stalerev.md` (P1+P2+P3 implemented as specified)

## P1 — structural: tool self-fetches the pin (`policy.rs`)

- `regenerate_and_compare` Checkout arm calls new `ensure_pin_present(checkout, pin)`
  before `expected_closures`: short-circuits when the pin exists (full clones /
  offline unchanged), else `git fetch --no-tags --depth 1 origin <pin>`; still
  absent → loud `GeneratorError::usage` via `pin_fetch_failure` naming pin +
  shallow/full state + remediation (`fetch-depth: 0` / re-pin), in the
  `missing_commit_reason` style. Strict closure verification kept; Remote arm
  untouched (revision fallback preserved).
- Not gated on `CARGO_NET_OFFLINE` per diagnosis.

## P2 — Velnor provision reads the live pin (`lib.rs`)

- `workflow_pinned_policy_runtime_velnor(checkout)` (revision param removed):
  no baked `PINNED_REVISION` env; parses live pin at runtime from
  `$CHECKOUT_PATH/.github-gen/velnor-workflow.toml` with the same `sed` as the
  old GitHub step, fail-closed on missing pin, then fetch-if-missing + closure.
- Call sites updated: `policy_job` (`lib.rs`), unit lanes (`ir.rs`), release
  lanes (`release.rs` — third production caller the diagnosis didn't name).

## P3 — cleanup + regen

- Deleted `Fetch D19 pin history` emission (`ir.rs`); regen via generator only:
  `cargo run -p velnor-workflow -- . --plain --force`. No hand-edited YAML.
- Regen touched only `ci-unit-rust.yml` (fetch step gone, live-pin provision),
  `release.yml` (live-pin provision), generator state hashes.

## Tests

- New: `policy::tests::checkout_arm_self_fetches_the_pin_in_a_shallow_clone`
  (2-commit origin, depth-1 tip clone, pin=parent → `regenerate_and_compare`
  returns `Pin`, pin materialized), `pin_self_fetch_fails_loud_when_the_remote_lacks_the_pin`
  (shallow: names pin + `shallow` + `fetch-depth: 0`; full: `full-history` + `re-pin`),
  `velnor_provisioner_reads_the_live_declared_pin` (no baked env, sed over
  `$CHECKOUT_PATH/...toml`, parse-before-fetch order).
- Updated: `github_lane_fetches_pin_history_for_check_running_units` →
  `no_lane_fetches_pin_history_the_tool_self_fetches`; provisioner call sites.
- `cargo test -p velnor-workflow`: 489 lib + 2 + 6 + 5 + 9 + 33 integration,
  0 failed — incl. byte-for-byte after regen.
- `cargo clippy -p velnor-workflow --all-targets`: clean.
- `cargo fmt -p velnor-workflow --check`: clean.
- `actionlint` (all workflows): clean; `shellcheck -S warning` on extracted
  provision script: clean; live-pin `sed` verified against real toml
  (parses `7341ef4b…`).

## Deferred (named, not bundled)

- R2 from diagnosis: PR-side pins still lack a release product for Velnor
  provision (`ci-runtime-products.yml` runs on main + dispatch only) — needs a
  PR-scoped product build or source-build fallback, separate change.
