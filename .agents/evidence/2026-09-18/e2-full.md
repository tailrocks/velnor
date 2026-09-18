Done. Main task + both follow-ups (one truncated item excluded) complete in one run, uncommitted.

## Main: `versioned-tool` kind (release family)

**Files changed:**
- `crates/velnor-workflow/src/primitives/release.rs`
  - Schema +9 (`Release::schema`, ~117–146): 7 contract args + `name` + `build_tasks`, alphabetical; `Release::render` intercepts `kind == "versioned-tool"` (~155–172)
  - Spec/parse/validate (~379–600): `VERSIONED_TOOL_KIND`, `is_main_branch_driven_release_kind`, `release_workflow_name` (name arg or file stem; tag kinds → `Release`), `VersionedToolSpec`, `valid_version_prefix` (`^[A-Za-z0-9_.-]+-v$`), task/path validators, `declared_versioned_tool_spec`, `versioned_tool_contract_complete`, `incomplete_versioned_tool_contract` (names file + every missing field)
  - `targets_are_real` extracted (~727), shared by both completeness gates (behavior-neutral)
  - Renderers (~2624–2900): `render_versioned_tool_release` (per-row name, push-main+PR triggers over declared paths, lanes dispatch, per-ref/PR-only-cancel concurrency) + `render_version_gate_job` (PR-only, fetch-depth 0, mise, gate tasks; carries the selection-vs-enforcement comment, `runtime.rs` untouched), version (sed X.Y.Z + runner-configs JSON per repo lanes), assert (published-reuse + main-only assert tasks, optional), build (target × fromJSON-config matrix, build tasks, package/attest/upload), publish (main + unpublished gate, `publish_group` mutex, immutable `gh release create <prefix>$VERSION`, no `--verify-tag`, no clobber)
  - 12 unit tests (~7535–7990)
- `crates/velnor-workflow/src/primitives/mod.rs` (~1332–1380): kind-conditional pin exemption (only `versioned-tool` rows leave `release.yml`; tag kinds stay pinned) + duplicate `name:`/`version_prefix` usage errors across resolved release rows
- `crates/velnor-workflow/src/config/mod.rs` (:2609): `valid_check_profile_task` → `pub(crate)`, reused verbatim by all three task lists (no fork)
- `crates/velnor-workflow/tests/versioned_tool.rs` (new, 6 tests) + `tests/fixtures/versioned-tool/velnor-workflow.toml` (new, neutral `example/*`)

**Build-cell decision:** option (i) — matrix cells run declared `build_tasks` as `mise run` steps (new required non-empty arg); generic code keeps package/attest/upload over the conventional `target/<triple>/release/<binary>` output. No zigbuild in generic code (named-task only, per L14; cited once in a comment as the excluded example). `assert_tasks` stays optional per the spec's validation rules.

**Deviation (ownership-forced, spec assumed wrong file):** `ReleaseSpec` lives in `lib.rs` (untouchable), so the kind bypasses it via local `VersionedToolSpec`: dispatch in `Release::render` (not `render_release`), dedicated completeness fn (not a `release_contract_complete` arm — documented at its `_` arm), no `[release]`-section fields (unwireable without `apply_release`; `[release] kind="versioned-tool"` fails closed on the existing unknown-kind error). `[release]`-section support needs a lib.rs-owning follow-up if ever wanted.

## Follow-up 1 (G-config): part 1 done, part 2 truncated
- `config/mod.rs` `validate_check_profile_row`: missing `schedule` now allowed (file-level validity lives in `select_profiles`); rejection-table case removed, new `check_profile_without_schedule_passes_config_validation` test added (refusal covered by existing `schedule_less_profile_in_cron_file_is_refused`).
- Part 2 arrived truncated (`2. primitives/mod.rs c...`) — need the full text; did not guess.

## Follow-up 2 (M-1..M-4, from `/tmp/mx-full.md`): done, all in `release.rs`
- **M-1:** build `if:` gains rehearse arm when modes declared — stable via `needs.verify.outputs.mode` (native inherits via shared base), tarball preview via `inputs.mode` (no resolver upstream; arm can only drill-build, publish stays gated). Updated the vacuous `!contains("outputs.mode")` assertion to the corrected contract + new `dispatch_rehearsal_on_a_feature_branch_builds_without_publishing` test.
- **M-2:** modeless image-platform/index jobs exclude `workflow_dispatch` (modes lanes already safe via `mode==publish`); docker's separate renderer untouched, deliberate recovery intact. New test.
- **M-3:** unbound native preview publish requires `push` (mirrors tarball); bound path keeps producer admission without push requirement (anchor updated, replacement unchanged). New test covering both.
- **M-4:** `TAG_IMMUTABILITY_STEP` const extracted — proven byte-identical vs pre-change render (`STEP_BYTE_IDENTICAL` diff); binary + native-non-debian publish gain checkout + step. New 3-lane step-equality test. Carried 3 pin digests deliberately (binary `release.yml` = M-4 only; native `release.yml` = M-2 only; native `preview.yml` = M-3 only — verified each fixture's delta set; other pins held).
- M-5/M-6 excluded (lib.rs/runtime.rs, out of ownership, low severity).

## Verification (tails)
- `cargo test -p velnor-workflow --lib`: **712 passed, 1 failed** — only `checked_in_workflows_match_the_generator_byte_for_byte`, pre-existing: drift = other-slice `generic_surface_literals` cache keys + other-slice ci-main/ci-pr drift + now the intended M-2/M-3 deltas. Regen is outside my ownership (checked-in workflows not editable by me).
- Touched suites: `primitives::release::` 77/77, `config::` 65/65, `--test versioned_tool` 6/6, `--test synthetic_surface` 10/10, `primitives::check_profiles` 20/20.
- `cargo clippy -p velnor-workflow --all-targets`: **0 warnings/errors**. `cargo fmt -p velnor-workflow -- --check`: **clean**.
- Not touched: `lib.rs`, `runtime.rs`, `check_profiles.rs`, `docs_site.rs`, `renovate.rs`, checked-in workflows. Nothing committed.