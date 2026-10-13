# Slice H-remainder memo (maintenance, Renovate, policy)

Date: 2026-09-17. Base: `33688938` + this slice, worktree dirty (no commit).
No code from Jackin PR #994 was read or copied; all rendering below was
written from the crate's own primitives, with Renovate env names verified
against public Renovate docs (`RENOVATE_REPOSITORIES`, `RENOVATE_GIT_AUTHOR`,
`RENOVATE_ALLOWED_COMMANDS` as JSON array, `RENOVATE_HOST_RULES` JSON-parsed,
DCO via `commitBody` trailer).

## Abstraction

One declarative contract per area, validated fail-closed at generation time
(and therefore at policy time, through regeneration):

- `[renovate]`: `schedules` (extra crons), `repositories` (targets, disables
  autodiscovery), `host_rules_secret` (credential reference; secret holds
  JSON `hostRules`), `author` (`Name <email>`), `signoff` (DCO trailer for
  `author`; requires `author`), `allowed_commands` (execution allowances,
  rendered as the `allowedCommands` JSON array).
- `[maintenance]` (new table, skipped in canonical form when empty):
  `schedule`, `producers` (retention-gate workflows), `max_deletes`
  (per-run delete bound, 1–5000, default 500).
- `[policy]` (previously parsed but unread): `action_pin_admission` must be
  `reviewed-allowlist` (the only implemented admission; anything else fails
  closed); `dco_required = true` requires `DCO` in
  `ruleset_external_status_checks`, else the requirement is unenforced.
- Nested kind reusables carry actual dependency/admission info through the
  existing `LaneStepFacts` → caller `with:` → callee input pattern (new
  `unit_dependencies`, `unit_admission` inputs; new ungated
  `Record unit dependencies` step). No fake/never-triggered visibility step
  exists in this checkout, so there was nothing to delete; the positive
  requirement is implemented and O(1)-safe (callee reads inputs only).
- Maintenance deletes are bounded and progress-aware: one shared
  `delete_cache_id` helper (3 attempts; HTTP 404 = progress/success, HTTP
  401/403 = abort without retry, else bounded backoff), a per-job delete
  bound with "rerun maintenance" semantics, and fail-closed listings
  (a failed `gh api` list exits 1 instead of reporting "nothing found").

## Files touched

- `crates/velnor-workflow/src/config/mod.rs` — new `[renovate]` keys,
  new `MaintenanceSection`, `dco_required`/`action_pin_admission`
  accessors, validators, `validate_renovate`/`validate_maintenance`/
  `validate_policy`, 10 new tests. Fixture coherence: `full_config` gains
  the `DCO` external check its `dco_required = true` needs.
- `crates/velnor-workflow/src/lib.rs` — `RenovateSpec` +6 fields,
  new `MaintenanceSpec` (defaults: `31 3 * * *`, ci-main+nightly,
  500), `ProjectConfig.maintenance`, `apply_renovate`/`apply_maintenance`
  wiring, `GENERATOR_REVISION` 49 → 50, 2 new e2e tests.
  `configured_repository_config` gains the `DCO` external check so the
  drift test's flip stays valid.
- `crates/velnor-workflow/src/primitives/renovate.rs` — extra crons +
  5 env builders, 4 new tests (incl. writer/validator release agreement
  and a `policy::audit_workflows` pass over both rendered workflows).
- `crates/velnor-workflow/src/primitives/release.rs` — maintenance
  template placeholders + `MAINTENANCE_DELETE_CACHE_FN` + renders,
  maintenance.yml digest re-pinned (verified by rendering, `bash -n`,
  and a stubbed-`gh` functional test of the exact generated helper:
  ok→0/1 call, 404→0/1, 403→2/1, flaky→0/3, down→1/3), 3 new tests
  (incl. policy audit pass).
- `crates/velnor-workflow/src/primitives/ir.rs` — `LaneAdmission::info_id`
  (+`Default`), `unit_dependencies`/`unit_admission` lane inputs,
  `LaneStepFacts` +2 fields, `Record unit dependencies` callee step,
  2 new tests (incl. callee O(1): no dependency literal inside).
- `crates/velnor-workflow/src/policy.rs` — `has_trusted_runner_gate`
  learns the self-hosted writer shape
  (`schedule || (dispatch && ref == branch)`); without it the Renovate
  writer fails `trusted-runners` (reproduced before the fix).
- `scan/mod.rs`, `tui/*`, `runtime_products.rs` — one-line
  `maintenance: MaintenanceSpec::default()` in full `ProjectConfig`
  literals.
- Regenerated (binary, `--force` after dry-run review; no hand edits):
  `.github/workflows/{ci-pr,ci-main,ci-unit-*,maintenance}.yml` +
  ownership state. Callers gain `unit_admission` (+`unit_dependencies`
  where non-empty); callees gain 2 input declarations + the info step.

## Wiring points for the integrator

- Config → spec: `apply_renovate` / `apply_maintenance` (`lib.rs`,
  called from the generation-config apply path next to
  `apply_release`); both re-validate (same fail-closed pattern as the
  existing token/cron checks).
- Spec → workflow: `render_renovate` env builders (`renovate.rs`);
  `render_maintenance` placeholder replaces (`release.rs`:
  `__MAINTENANCE_SCHEDULE__`, `__MAINTENANCE_PRODUCERS__`,
  `__MAINTENANCE_MAX_DELETES__`, `__MAINTENANCE_DELETE_CACHE_FN__`).
- Nested info: `unit_lane_facts` → `LaneStepFacts::input_values` →
  caller `with:` → `render_unit_dependency_info_step` (`ir.rs`).
- Policy: writer gate in `has_trusted_runner_gate` (`policy.rs`);
  `[policy]` compliance enforced in `validate_policy` (`config/mod.rs`)
  and inherited by policy runs through regeneration (no new policy
  rule needed).
- No new action pins, no new `uses:` refs: pin consistency unchanged
  (all refs still flow through `ActionPin`/`Pins`).

## Verification

- `cargo test -p velnor-workflow`: 507 lib + 2/6/5/9/33 integration,
  all pass (incl. `generic_surface_literals`, the byte-for-byte
  checked-in-workflows test after regen, and 21 new tests).
- `cargo clippy -p velnor-workflow --all-targets`: 0 findings.
- `cargo fmt --check` (workspace): clean.

## Known deferred item (pre-existing, out of slice)

Velnor-only `maintenance.yml` still fails the policy audit:
`prune-pr-cache` runs self-hosted on a `pull_request` trigger, which no
trusted gate admits (verified by throwaway probe, since removed; this
slice does not touch gates). Fix direction: velnor lane drops the
`pull_request` trigger and lets the scheduled sweep own closed-PR
cleanup — a trigger/availability decision for its own change.
