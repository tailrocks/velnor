# PR 932 verification: fix(s2) selection CSV @ 99875eee

## VERDICT: HOLD — do not merge (mechanical 1-line state regen missing)

The fix itself is correct and fully proven end to end, all local gates are
green, and the E2E test is live and fix-sensitive. But the PR omits the
generator-state regen for its own new test file, so `dry-run = 1`, the
`velnor-workflow` CI leg fails on its `--check` gate, no candidate artifact
is published, and Policy fails downstream. Same cascade shape as r2m §5, one
layer shallower. Author fix: run `--plain --force`, commit the 1-line
`scan` digest bump (`e89ef785c3af99ce` → `a215cd4110969456`), no source
changes needed. Nothing was pushed, merged, or amended.

- Heads: branch `99875eeeea9b5a48e3a16f43a1e7376ad5e230cc` (1 commit over
  main); `origin/main` = `52739e176c1cba3ddeeff1ca21eda2edadb52b17`.
- Worktree: `/tmp/932-verify-wt` (99875eee, detached). Read-only except /tmp.
- `gh pr view 932`: `mergeable: MERGEABLE`, head = 99875eee, DCO `pass`.

## 1. FIX CORRECTNESS + COHERENCE — PASS

One format per channel end to end, traced live (all lines at 99875eee):

1. **Plan emission** (`crates/velnor-workflow/src/s2/runtime.rs`): `units_json`
   built 1590-1601 (JSON `[{unit_id, providers}]`); `unit_ids` CSV built
   1621-1625 from the same `planned` set; direct selection-file path already
   used the CSV 1626-1635; GITHUB_OUTPUT gains `("unit_ids", unit_ids)` at
   1653 next to `("units", units_json)` 1652; stdout `unit_ids=` at 1665.
2. **Plan job exposure** (`s2/primitives/ir.rs:4959`):
   `unit_ids: ${{ steps.plan.outputs.unit_ids }}` alongside `units:`.
3. **Caller passing** (ir.rs:3627 control caller, :3685 unit callers — the
   only two `selected_units: ${{` emitter sites):
   `selected_unit_ids: ${{ needs.plan.outputs.unit_ids }}` next to the JSON
   `selected_units:` feed.
4. **Reusable contract** (ir.rs:3936): new required `selected_unit_ids` input.
5. **Materialization** (ir.rs:4182 collapsed provider job, :4807 prep job —
   the only two `SelectionFieldSources` constructions): `units:` source is
   now `${{ inputs.selected_unit_ids }}`; renderer (`s2/mod.rs:5031-5043`)
   writes `units=$SELECTION_UNITS` (line 5020).
6. **Run parse** (`s2/runtime.rs:2277-2292`): `parse_selection_ids` untouched
   by the diff — CSV-only split + `is_unit_id`, no dual-format shim, no
   selection-file version bump. The checks step (`run`, ir.rs:4677) renders
   inside the collapsed provider job *after* its materialize step
   (4144→4177→4188→4368), so every `run` reads the fixed file.

Leftover-JSON-into-`units=` hunt: `grep inputs.selected_units` in s2
non-test emitter code hits only ir.rs:3244 (callee gate) and ir.rs:4012
(reusable provider gate) — both `contains()` JSON-needle matches, correct.
Required-check `SELECTED_UNITS` (ir.rs:3799) feeds jq JSON queries
(ir.rs:3820,3837), correct. `excluded` JSON is exposed (ir.rs:4962) but its
`EXCLUDED` env (ir.rs:3804) is never read — dead env, no consumer, no
mismatch. Kind matrices are JSON read via jq/fromJSON. s1 untouched and
coherent: plan `units` is CSV (`runtime.rs:1460-1465,1495`) feeding all four
s1 materialize sites (`primitives/ir.rs:4447,5057,5246,5859`) into the same
CSV parser. Zero JSON-into-scalar flows remain in s1+s2 runtime/emitters.

## 2. E2E REGRESSION TEST — PASS (live, fix-sensitive, defeat-tested)

- `cargo test -p velnor-workflow --test selection_plan_handoff`: **2/2 pass**.
- Defeat run (reverted ONLY the 3 src files to `origin/main`, kept the test):
  `live_plan_outputs_materialize_a_selection_file_run_executes` **FAILS**
  (`live plan must emit the unit_ids output`), negative control
  `plan_json_in_the_units_field_still_fails_closed` still passes. Restored
  via `git checkout HEAD --`, worktree verified clean at 99875eee after.
- Live, not canned: real binary via `CARGO_BIN_EXE_velnor-workflow`, real
  git fixture (base/head commits), real `plan` → parsed `GITHUB_OUTPUT` →
  selection file → real `run` with marker-file side effects asserted
  (`selected.marker` written, `unselected.marker` absent); asserts the CSV
  channel names exactly the JSON channel's set. The generated-step half is
  covered by the emitter wiring unit test
  (`selection_file_consumes_the_csv_unit_ids_channel_not_plan_json`,
  s2/mod.rs:14351: plan exposes `unit_ids`, every JSON-feeding caller also
  feeds CSV, reusable declares the input, `SELECTION_UNITS` comes from
  `inputs.selected_unit_ids`, JSON never feeds `units=`, callee gates stay
  on JSON).

## 3. RENDER-NEUTRAL — FAIL (state file, 1 line)

- `.github/workflows` diff vs `origin/main`: **EMPTY** (0 lines). Tree is
  schema-1 (`revision = "a6fa8d4a…"`), so s2 emitter changes correctly alter
  no renders. No fixtures changed (diff = 3 src files + 1 new test file).
- BUT: `--plain --force` rewrites
  `.github/ci/.github-actions-generator-state` (`scan e89ef785c3af99ce` →
  `a215cd4110969456`); `--plain --dry-run` on the pristine tree = **1 file
  would change**, not 0.
- Cause proven: the new test file is a new walked path, and the state
  `scan` digest fingerprints the walk (`scan/mod.rs:100-110`; `target/`
  excluded, `file_walk.rs:142`, so not build debris). Removing only the
  test file → force is clean; restoring → 1-line diff returns.
- Convention: same-commit state regen is the norm (35897d0f "and regen").
  The author forgot it. CI's expected digest (`a215…`) matches local
  exactly — deterministic, author just needs to commit it.

## 4. PIN+BASE — PASS

- `origin/main` = `52739e176c1cba3ddeeff1ca21eda2edadb52b17`, unchanged;
  `merge-base(branch, main)` = same; PR `baseRefOid` = same. Fresh, no
  HOLD-stale. Generator pin file untouched by the PR (schema-1 tree).
- DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` on the commit;
  PR `DCO` check `pass`.

## 5. GATES — PASS (all rerun in /tmp/932-verify-wt, all green)

- `cargo test -p velnor-workflow`: all suites ok (lib **1583**, integration
  incl. new 2/2 handoff + new emitter test), 0 failed.
- `cargo test -p velnor-runner`: 2166+1 passed, 4 ignored, 0 failed.
- Contract suite: **6/6** (2+4). `cargo clippy --all-targets
  -p velnor-workflow -- -D warnings`: clean. `cargo fmt --check`: clean.
  `actionlint -config-file .github/actionlint.yaml`: clean.

## 6. CI — FAIL (blocking: PR-caused leg + Policy cascade; terminal, no wait)

Runs at head (both `completed`/`failure`): CI/PR `35223345514`, Policy
`35223345242`.
Fail set (6): `Rust · velnor-workflow / GitHub`, `Control / Prepare Cargo /
prepare-cargo`, `Docker · Docker / Velnor`, `ci-required`,
`Control / Required`, `Policy`. Everything else: 6 github legs SUCCESS
(docker, control, model, runner, topology, Planning), rest skipped.

- **PR-caused**: velnor-workflow/GitHub fails in checks with
  `generated files match but generation inputs changed: scan input changed
  from e89ef785c3af99ce to a215cd4110969456 …; regenerate` — the §3 miss,
  biting through the unit's own `--check` gate.
- **Cascade**: candidate packaging runs after checks in that leg, so no
  `velnor-workflow-candidate-5063ab07…` is published → Policy fails with
  `no same-repository PR run published candidate …` (same mechanism as
  r2m §5). (Policy's `--check` line also lists 5 stale-vs-pin files, but
  that pin/base-closure mismatch is pre-existing on main — see baseline.)
- **Environmental (in scope, unchanged vs merged #931)**: prepare-cargo +
  docker/Velnor fail in 3s with `Velnor rejected this job
  (operational_store) … before execution` — identical signature to #931's
  merged run. `ci-required` (`velnor-docker did not pass`) and
  `Control / Required` fail downstream of both causes.
- **Baseline**: latest main run `35220302468` (CI/Main @52739e17) is
  degenerate (all unit legs skipped; Policy/ci-required/Required fail on
  missing candidate) — no per-leg signal. The informative same-shape
  baseline is merged #931's PR run `35219190605`: identical pass/skip map
  INCLUDING the two environmental velnor failures and red ci-required —
  except #931 had velnor-workflow/GitHub SUCCESS and Policy SUCCESS via
  the candidate path. So the PR's delta vs the merge bar is exactly the
  state-regen leg + its Policy cascade. New failures vs baseline exist →
  item FAIL.

## Per-item scorecard

1. FIX CORRECTNESS + COHERENCE — PASS. 2. E2E TEST — PASS (live 2/2,
defeat-tested). 3. RENDER-NEUTRAL — FAIL (dry-run 1, state scan line).
4. PIN+BASE — PASS. 5. GATES — PASS. 6. CI — FAIL (PR-caused leg +
Policy cascade; environmental set matches #931).

**VERDICT: HOLD** — merge after the author commits the `--plain --force`
state regen (1 line, no source change) and Policy goes green via the
candidate path as on #931. Expected post-fix CI: #931-shaped (Policy +
github legs green; prepare-cargo/docker-Velnor + ci-required red on the
pre-existing admission condition).
