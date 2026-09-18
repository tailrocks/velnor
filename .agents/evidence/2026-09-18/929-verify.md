# PR #929 verification — port d2a remainder into s2 pipeline

VERDICT: **HOLD** (2 reasons: missing generator-state regen; stale base)

- PR: tailrocks/velnor#929, branch `fix/d2a-remainder-port` @ `c3da45a1`, 1 commit, base `main@7aa4b4e0`
- Method: fresh detached worktrees `/tmp/929-verify-wt` (c3da45a1) and `/tmp/929-main-wt` (7aa4b4e0); read-only elsewhere; no push/merge/amend.
- CI run: https://github.com/tailrocks/velnor/actions/runs/35210023348 (CI / PR, completed), Policy: https://github.com/tailrocks/velnor/actions/runs/35210023134

## HOLD reasons

1. **Missing generator-state regen (1 line).** The new file `tests/provider_pairing.rs` changes the repo
   scan inventory, so the `scan` input fingerprint moves `cd441bd8f5041f2d` → `e89ef785c3af99ce`,
   but the PR carries no `.github/ci/.github-actions-generator-state` change. Consequences, all proven:
   - `cargo build -p velnor-workflow && ./target/debug/velnor-workflow --plain --force` → `git status`
     shows `M .github/ci/.github-actions-generator-state` (MUST be clean per gate 3). Second regen is
     stable (same 1-line diff), so the committed state is simply stale, not divergent output.
   - `--plain --dry-run` → "1 file would change", not 0.
   - CI fails exactly here, twice, with the identical fingerprints (independent confirmation):
     - Rust · velnor-workflow / GitHub: `error: generated files match but generation inputs changed:
       scan input changed from cd441bd8f5041f2d to e89ef785c3af99ce ... regenerate ...`
       (job https://github.com/tailrocks/velnor/actions/runs/35210023348/job/105165154178)
     - Policy: same error under the pinned runtime, then falls to the candidate path and fails with
       `no same-repository PR run published candidate ...` (job https://github.com/tailrocks/velnor/actions/runs/35210023134/job/105165058442)
   - Main baseline (CI / Main run 35208939994 on 7aa4b4e0) has Policy=SUCCESS and
     velnor-workflow/GitHub=SUCCESS, so both PR failures are NEW and PR-caused, not environmental.
   - Precedent: #925 (52c424b1) added `tests/release_tasks.rs` + a doc and included the 1-line state
     regen in the same commit. Fix for author: rebase (see 2) then commit the regenerated state file.
2. **Base stale.** `origin/main` advanced mid-verification: `7aa4b4e0` → `9772d424` (merge of #927,
   which itself rewrote the state file — 10 lines — and touched `s2/primitives/ir.rs`, one of this
   PR's files). `git merge-tree 7aa4b4e0 9772d424 c3da45a1` exits 0 with no conflict markers, so the
   rebase should be mechanical, but the state fix from (1) must be recomputed on the new base.

## Gate results

1. BASELINE — **FAIL** (PR-caused, not baseline). Final tally: 6 fail / 7 pass / 29 skipping.
   - Fails: Prepare Cargo/prepare-cargo, Docker/Velnor (both `phase: operational_store`, "rejected
     the sanitized admission row" — pre-existing environmental, also failing on main run 35208939994);
     Control/Required (`Run exit 1`) and ci-required (`selected CI job velnor-docker did not pass`) —
     pure aggregate rollups. These 4 = the expected pre-existing set for the executed scope.
   - EXTRA vs baseline: Policy FAIL (required SUCCESS) + velnor-workflow/GitHub FAIL (executed /GitHub
     must be SUCCESS) — both the stale-state error above. DCO passes.
2. PIN+BASE — **PASS with staleness note**. `.github-gen/velnor-workflow.toml` byte-identical to
   7aa4b4e0 (`cmp` clean, pin `a6fa8d4a5096e6df37250b59a8105abfebdb9013`); single commit; DCO trailer
   `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` present; DCO check SUCCESS. At session start
   merge-base == origin/main == 7aa4b4e0; main has since moved (see HOLD-2).
3. NO-FLIP + REGEN — **NO-FLIP pass, REGEN fail**.
   - `diff -r main/.github/workflows branch/.github/workflows` empty; default emission stays schema-1.
   - `--force` regen dirties only the state file (1 line, scan fp). Ablation: with ONLY the new test
     file removed (all 8 s2 edits kept), regen reproduces the committed `cd441bd8…` exactly → the 8 s2
     edits genuinely do not move the fingerprint; the new file does. Main-worktree regen is clean.
4. REMAINDER JUSTIFICATION — **PASS all 9 files** (tried to disprove; falsification test included).
   - s2/scan/mod.rs (`detection_contract` Swift arm MacosArm64+native → LinuxX64+default): main has the
     MacosArm64 arm (R2 line); hunk verbatim == `375c0787:src/scan/mod.rs:199-202` incl. comment.
     Apple handling NOT reverted: Xcode units (swift.rs:166-168) and XCFramework consumers
     (swift.rs:199-201) overlays intact; bridge unit still asserts MacosArm64 in tests.
   - s2/scan/swift.rs (removes `result.platform = LinuxX64` override): now a no-op given the contract
     default (`swift_package_unit` builds via `unit(UnitKind::Swift, …)`); replacement comment verbatim
     == 375c0787 `src/scan/swift.rs`.
   - s2/config/mod.rs (`validate_check_profile_row` missing `[workflow] providers` → default ALL, not
     empty; error texts → "has no velnor provider" / "names no selector for the job"): main has
     `unwrap_or(&[])` + old texts; hunk == 375c0787 `src/config/mod.rs:2805-2814` modulo the correct
     `crate::provider` → `crate::s2::provider` path shift. ALL-default idiom matches siblings at
     s2/config lines 1584 (`validate_workflow`) and 2470 (renovate) — consistency, not duplication.
     Test fixtures updated to match (velnor-selector fixture fails on universe check; selector-less
     fixture now omits `[workflow]` entirely, exercising the ALL default).
   - s2/mod.rs tests: +2 portable-Swift admission asserts grafted into the main-line xcframework test
     (which already had the `LinuxX64` "stays portable" assert). Semantics descend from 375c0787
     (`plain_swiftpm_packages_stay_portable`, lib.rs:9456-57: `provider_supports_unit(GithubHosted/
     Velnor, &swift)`); placement/message adapted to the main-line test — faithful port, reuses R2's
     `provider_supports_unit` (s2/mod.rs:4646, present in main), no second predicate. 2× "collapsed
     lane jobs" → "collapsed provider jobs" == 375c0787 lib.rs (2 occurrences).
   - Wording (check_profiles "lane"→"runner" ×2, docs_site "static lane"→"static runner",
     ir "lane admission/restores"→"provider" ×2, release "lane"→"provider/flow" ×4): comment/test-message
     only, zero behavioral surface; each string verified present in 375c0787's `src/primitives/*.rs`;
     no "collapsed lane jobs" stragglers remain in branch s2/.
   - tests/provider_pairing.rs (new, 528 lines, 6 tests): byte-identical to
     `375c0787:tests/provider_pairing.rs` (524 lines) except a 4-line
     `#[expect(clippy::too_many_lines)]` needed for the repo clippy gate. Black-box suite: writes
     `schema = 2` fixtures, shells out to `CARGO_BIN_EXE_velnor-workflow`, asserts YAML. Not a duplicate
     of `lane_pairing.rs` (schema-1 lanes suite, still present and passing): sibling suites for different
     schemas; no production routing/fanout/results code added anywhere (all `+fn` lines are test helpers
     in this file; production hunks only edit existing function bodies).
   - FALSIFICATION: copied the suite onto main@7aa4b4e0 → 5 pass / 1 FAIL
     (`swift_explicit_hosted_opt_out_stays_hosted`: "unit swift-package- requires unsupported
     capabilities on provider github-self-hosted: native-macos-arm64"), proving the scan hunk is
     genuinely new and load-bearing. On branch: 6/6 pass. Probe file removed afterwards; main worktree
     verified clean.
5. GATES (rerun in /tmp/929-verify-wt) — **all PASS**.
   - `cargo test -p velnor-workflow`: lib 1581 pass + every integration suite incl. provider_pairing 6/6;
     0 failed anywhere.
   - `cargo test -p velnor-runner`: all pass (lib 2166+1, all integration bins), 0 failed.
   - `velnor-workflow-contract` tests from its dir: 6 pass, 0 failed.
   - `cargo clippy --all-targets -p velnor-workflow -- -D warnings`: clean (exit 0).
   - `cargo fmt --check`: clean. `actionlint` 1.7.12: clean (exit 0).
6. MIGRATION — **PASS**. Expand-only: diff touches only `s2/**` + 1 new test; schema-1 (`src/lib.rs`,
   `src/config`, `src/primitives`, `src/scan`) untouched; pin unchanged; rendered workflows byte-identical
   (no emission flip). Rollback = revert the single merge commit.

## For the author

Rebase onto current `origin/main` (≥9772d424), run `velnor-workflow --plain --force`, commit the
resulting `.github/ci/.github-actions-generator-state` change together with the port. No other changes
needed: content, tests, wording, and descent all verified; CI's two non-environmental failures resolve
with the state file (the `--check` gate and Policy both pass on main today).
