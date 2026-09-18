# D2B final verification — PR tailrocks/velnor#926 after main-update

- Branch HEAD: `83fb1d0e` (= origin/feat/d2-three-provider-remainder, verified via fetch)
- Structure: `dc890899` (D2 content) → `4d27b19c` (regen) → merge `08bfa747`
  (parents `4d27b19c` + `52c424b1`) → `83fb1d0e` (regen after main update)
- Base: origin/main `52c424b1` (PR #925, tasks release publisher)
- Prior art: /tmp/d2b-verify.md (content PASS, HOLD for regen) +
  /tmp/d2b-reverify.md (repair exact, HOLD-stale) — this report verifies the UPDATE only
- Worktrees (fresh, detached, /tmp-only writes): `/tmp/d2b-final-wt` @ 83fb1d0e,
  `/tmp/d2b-final-main-wt` @ 52c424b1
- CI run: 35207983693 (+ Policy run 35207980243), complete — 0 pending at final tally
- Read-only vs repo: no push/merge/amend

## 1. ANCESTRY: PASS

- `git merge-base --is-ancestor 52c424b1 83fb1d0e`: holds (ANCESTOR-OK).
- `gh pr view`: `mergeable = MERGEABLE`, `mergeStateStatus = BLOCKED`
  (BLOCKED = required-check gating, same as baseline PRs; never CONFLICTING.
  One transient `UNKNOWN/UNKNOWN` read right after CI completed, re-queried
  60s later back to MERGEABLE/BLOCKED.)
- Pin: `.github-gen/velnor-workflow.toml` at head `cmp`-identical to origin/main;
  line 9 `revision = "a6fa8d4a5096e6df37250b59a8105abfebdb9013"` unchanged.
- Merge `08bfa747` (parents 4d27b19c + 52c424b1): NO hand edits.
  - `git diff 52c424b1 08bfa747`: only the 7 s2 files (branch contribution);
    all 7 blobs byte-identical to parent1 (`git rev-parse` per-file OK).
  - `git diff 4d27b19c 08bfa747`: only the 6 #925 files + state file
    (main contribution); all 6 blobs byte-identical to parent2.
  - State file in merge byte-identical to parent2 (diff vs 52c424b1 empty):
    conflict resolved by taking main's side (`scan f89616b7fc62f8a0`), zero
    hand-crafted bytes.
- DCO trailers present on both new commits (merge + regen); `%G?` = N,
  consistent with project practice per prior report.

## 2. REGEN: PASS

In /tmp/d2b-final-wt @ 83fb1d0e:
- `cargo build -p velnor-workflow`: exit 0.
- `./target/debug/velnor-workflow --plain --force`: exit 0 ("Generated 21 files"),
  then `git status --porcelain`: CLEAN (empty).
- `./target/debug/velnor-workflow --plain --dry-run`: exit 0, "0 files would change".
- `diff -r .github/workflows` branch vs origin/main: EMPTY (byte-identical).
- Regen commit `83fb1d0e` touches only the state file, one line:
  `scan f89616b7fc62f8a0 → cd441bd8f5041f2d`; worktree scan line matches head.

## 3. CONTENT INTACT: PASS

- Six D2 s2 modules present at head (`git cat-file -e` each OK): routing.rs,
  planner.rs, results.rs, trust.rs, watchdog.rs, capability_tests.rs; the six
  `mod` declarations wired in s2/mod.rs (lines 22/27/32/34/38/41). The other
  s2 files (closure, dispatch, provider, …) pre-exist at base c04aec98.
- `cargo test -p velnor-workflow --lib s2::`: **805 passed, 0 failed**
  (identical count to prior report).
- R2 dispatch: `s2::dispatch::run_if_s2()` call at lib.rs:5348;
  `pub(crate) mod s2;` at lib.rs:31.
- #925 tasks-publisher code present: `ReleaseJobSpec` at lib.rs:957/964/2504,
  `tests/release_tasks.rs` + `tests/fixtures/release-tasks/` fixture present,
  `primitives/release.rs` intact.

## 4. CI: PASS

Final tally on head 83fb1d0e (run 35207983693 completed, 0 pending):
7 fail / 20 pass / 42 skipping.
- Fail set: `Bun/Velnor`, `Prepare Cargo/prepare-cargo`, `Docker/Velnor`,
  `Documentation/Velnor`, `OpenTofu/Velnor`, `Control/Required`, `ci-required`
  — EXACTLY the #922/#924 baseline set (5 operational_store leaves + 2 rollups).
- Leaf logs confirm environmental: `Velnor rejected job (operational_store):
  operational store rejected the sanitized admission row; job failed closed
  before execution` (verified on Bun/Velnor job 105158435964 and
  prepare-cargo job 105158763; all 5 leaves fail in 2–3s at admission).
- Required gates green: `Policy` PASS (run 35207980243, 21s),
  `Rust · velnor-workflow / GitHub` PASS (2m58s), `DCO` PASS.
- Every executed `/GitHub` check: PASS (zero non-pass `/GitHub` rows).

## VERDICT: MERGE-OK

All four update-verification items pass: ancestry current with a hand-edit-free
merge, regen idempotent with byte-identical rendered workflows, D2 + #925
content intact (805/805 s2 tests), and full CI green except exactly the
pre-existing environmental fail set. Prior HOLD causes are gone. Do not merge
per task instruction — awaiting author/maintainer action.
