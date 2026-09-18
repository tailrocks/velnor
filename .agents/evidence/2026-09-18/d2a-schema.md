# D2 part A evidence: provider-schema core + fanout + strict results

Branch: `feat/d2-provider-schema` (from `origin/docs/bastion-final-plan` @ `bde4d4ec`)
Commits (signed, `git commit -s`):
- `d9b9ce45` feat(d2): provider-schema core + 3-provider fanout + strict results (part A)
- `8db9e5ac` chore(ci): bump D19 pin to d9b9ce45 (pin-embedding surfaces only)

## What part A implements

- `ProviderId` (`github-hosted`, `github-self-hosted`, `velnor`) with set
  semantics across config (`providers` / `automatic_providers` /
  `default_dispatch_providers` + per-provider `selectors`), scan, IR, plans,
  validation, runs-on, bootstrap, caching, artifacts, policy, dispatch
  (`--providers` + `providers` multi-select input), UI names, aggregation.
- `RunnerMode`/`RunnerLane` removed everywhere: `lanes.rs` and `runners.rs`
  deleted, no shims/aliases/dual parsers/inference. Mechanical proof:
  `generator_source_and_renders_carry_no_legacy_provider_vocabulary`
  (sources + dogfood + fixture renders; extended with the removed config
  keys) plus a clean grep sweep of `src/` and `tests/`.
- Typed `Platform` / `TrustReq` (`untrusted-ok`, `trusted-only`) /
  `Capabilities` with explicit-failure negatives (unknown ids, sets outside
  the universe, unknown trust tiers, unsupported capabilities).
- Planner fans every selected unit to one caller per provider: identical
  inputs, provider display names (`Rust · crate00 · velnor — rust-crate00`),
  disjoint local selectors, per-provider bootstrap. No matrix, no fail-fast
  surface: `kind_reusable_jobs_are_linear_in_units_not_a_matrix_product`
  pins 25 callers (8x3+control) and one checks step per provider job.
- Strict expected-result set: spec §2 identity (`ResultIdentity`), plan
  digest frozen pre-execution, verdict arms for missing/skipped/cancelled/
  failed-or-timed-out (`*`), wrong-provider admission, unexpected results
  outside the set, and missing plan-digest identity. Fail-fast disabled:
  only literal `success` passes (`success|skipped` asserted absent).
- Dogfood surface regenerated via the generator only
  (`generate . --plain --force`); cache keys re-segmented by
  provider/platform/trust with compat digests re-captured and documented in
  the contract fixture.

## Bugs found and fixed while migrating the suites

- Verdict `plan_expects_local()` named the universe's local providers instead
  of the deployed set (foreign names on provider-less surfaces).
- Policy `trusted-runners` ignored default selectors, failing default-routed
  local jobs as foreign; now overlays declared selectors on
  `scan::default_selectors()`, mirroring generation.
- Policy `strip_combined_selected_units_selector` used `rfind`, grabbing an
  inner gate boundary; `find` is correct (unit ids cannot hold `)&&(`).
- Contract cache-key + backend fixtures migrated to provider ids, the new
  key grammar, and re-captured compat digests (digest facts gained
  provider/platform/trust).
- Clippy-deny (`panic`, `unwrap_used`) and `cargo fmt` brought to green,
  including pre-existing hits.

## Gates (clean clone `/tmp/d2a-clean` @ `8db9e5ac`)

- `cargo build -p velnor-workflow`: ok
- `--plain --dry-run --default-branch main .` → `Dry-run: 0 files would change`, exit 0
- `--plain --check --default-branch main .` → `Generated files are current`, exit 0
  (D19 guard builds pin `d9b9ce45` from source; the pre-D2 pin could not parse
  the new schema, hence the pin-bump commit)
- `cargo test -p velnor-workflow`: 648 passed, 0 failed
  (lib 589 incl. no-legacy mechanical test; velnor_first_ci 33;
  provider_pairing 9; synthetic_surface 6; selection_artifact_handoff 5;
  promote_atomic 4; generic_surface_literals 2)
- `cargo test` in `crates/velnor-workflow-contract`: 6 passed, 0 failed
- `cargo clippy -p velnor-workflow --all-targets`: 0 errors
- `cargo fmt -p velnor-workflow -- --check`: clean
- `cargo check --workspace --all-targets`: 0 errors
- `velnor-workflow policy --workflow-root . --base-revision bde4d4ec…`:
  11 rules, 0 failed

## Return

- Branch: `feat/d2-provider-schema` (pushed to `origin`)
- Tip: `8db9e5ac` (D2 core `d9b9ce45` + D19 pin bump)
- Tests: 654 passed / 0 failed; dry-run=0, check=0 in a clean clone
