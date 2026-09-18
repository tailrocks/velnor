# PR #927 re-verification — after repair @ fa826405

Target: branch `fix/docker-mise-gh-token` @ `fa826405b075eeac3f5e2d7eba6a785f1cea0838`.
Prior: `/tmp/927-verify.md` (HOLD: stale seed_compat fixture, trap bug, runner flake).
Method: read-only on repo (all content reads pinned to explicit SHAs);
local gates run in `/tmp/pr927-reverify` (`git archive fa826405` + snapshot commit).
Nothing pushed, merged, or amended.

## VERDICT: MERGE-OK

All three HOLD reasons are resolved with proof. Repair commits are minimal and
correct, every local gate passes, and CI on head `fa826405` shows all five
required GitHub-lane/Policy checks SUCCESS with the fail set confined to the
known environmental (Velnor-backend) set. `mergeable == MERGEABLE` (BLOCKED =
only the environmental fails, same as main).

---

## 1. REPAIR COMMITS: PASS

`git log --oneline f66356eb..fa826405` (oldest first):

- `dc890899` D2 remainder, `52c424b1` #925, `4d27b19c` regen, `08bfa747` merge,
  `83fb1d0e` regen, `06050c9f` #928, `7aa4b4e0` #926 (= main advance to 7aa4b4e0)
- `9de09111` test(ci): rotate docker seed_compat fixture to 8a4ee3c72d39
- `35897d0f` fix(workflow): disarm EXIT trap after explicit candidate worktree removal
- `fa826405` Merge origin/main into fix/docker-mise-gh-token

(a) Fixture: `9de09111` touches exactly one file,
`crates/velnor-workflow-contract/tests/fixtures/pre_parameterization_cache_keys.rs`,
3 lines changed, all `9c97194a807b` → `8a4ee3c72d39` (primary + 2 restore_keys,
:52,54,55). Branch-only diff `f66356eb..35897d0f` is 5 files: fixture, the two
emitter sources + their new trap assertions (see b), generator-state hash, and
the +1-line rendered yml. No other test-expectation edits anywhere in range.

(b) Trap fix at generator source, yml via regen only:
- Source (at `fa826405`): `crates/velnor-workflow/src/primitives/ir.rs:1863`
  (s1 emitter, `candidate_publish_steps`) and
  `crates/velnor-workflow/src/s2/primitives/ir.rs:1847` (s2 emitter) — each
  adds `trap - EXIT` after the explicit `git worktree remove --force`; new
  assertions at s1 `:960-966` / s2 `:947-953` pin the pairing.
- Rendered `.github/workflows/ci-unit-rust.yml:610` gains exactly that one
  line (`:409 trap - EXIT` is pre-existing, unrelated `rm -f` trap).
- Merge made zero yml edits: `git diff 35897d0f fa826405 -- ci-unit-rust.yml`
  is empty; branch-vs-main delta is exactly 1 added line.
- Regen idempotency proven in fresh `/tmp/pr927-reverify` snapshot @fa826405
  (built `velnor-workflow`, `--default-branch main` needed only as a snapshot
  artifact): `--plain --force` → `git status` clean (empty);
  `--plain --dry-run` → `0 files would change`. The merge's generator-state
  union is therefore byte-accurate, and the yml change is purely generated.

(c) Main update:
- `7aa4b4e0` is an ancestor of HEAD (`merge-base --is-ancestor` YES).
- Merge `fa826405` (parents `35897d0f` + `7aa4b4e0`) is a clean two-side union:
  `35897d0f..fa826405` = main-only files (s2 D2 sources, release publisher,
  tests, plan); `7aa4b4e0..fa826405` = branch files only. Overlapping files
  resolved sanely: `lib.rs` keeps both branch (`DOCKER_BUILD_GITHUB_TOKEN_SECRET`
  :2694) and main (named-task `ReleaseSpec` :955) hunks; generator-state keeps
  branch output hashes + main `scan` hash — validated by the idempotent regen.
- Pin `a6fa8d4a…` unchanged: 0 occurrences in `f66356eb..fa826405` diff;
  `revision` line identical at both ends.
- DCO: all 3 branch-authored commits (`9de09111`, `35897d0f`, `fa826405`) carry
  `Signed-off-by`. The range also contains 3 trailer-less GitHub merge commits
  from main (`52c424b1`, `06050c9f`, `7aa4b4e0`) — pre-existing on origin/main,
  not authored by this PR. GitHub DCO check on the PR: pass.

## 2. GATES (rerun locally in `/tmp/pr927-reverify` @ `fa826405`): ALL PASS

| Gate | Result |
|---|---|
| `cargo nextest run --locked --all-features -p velnor-workflow` (CI-identical flags) | 1692/1692 PASS |
| contract tests from `crates/velnor-workflow-contract` | 6/6 PASS, incl. `parameterized_callees_resolve_to_the_pre_parameterization_cache_keys ... ok` (the previously-failing test) |
| `cargo test -p velnor-runner` | exit 0: lib 2166 passed / 0 failed (+4 ignored), all integration targets ok |
| known flake `protocol::tests::artifact_upload_sends_finalize_hash_and_rejects_unsuccessful_finalize` (`--all-features`, `test-support`-gated so absent from plain run) | PASS first try, no rerun needed |
| `cargo clippy --locked --all-targets --all-features -p velnor-workflow -- -D warnings` | exit 0 |
| `cargo fmt --check` | exit 0 |
| `actionlint` (bare, as CI does) | exit 0 |

## 3. CI: PASS (within environmental scope)

Runs on head `fa826405`, both completed: CI/PR `35209936180`, Policy `35209933852`.

- `Policy`: SUCCESS (4m0s) ✓ (was FAIL — candidate now publishes via fixed trap)
- `Docker · Docker / GitHub`: SUCCESS (3m55s) ✓ (again)
- `Rust · velnor-workflow / GitHub`: SUCCESS (3m26s) ✓ (packaging green)
- `Rust · velnor-workflow-contract / GitHub`: SUCCESS (23s) ✓ (fixture fixed)
- `Rust · velnor-runner / GitHub`: SUCCESS (4m24s) ✓ (clean, no flake)
- `DCO`: pass; all other GitHub-lane jobs pass.
- Fail set (7): Bun/Docker/Documentation/OpenTofu `/ Velnor` (2–3s each) +
  `Control / Prepare Cargo / prepare-cargo` + `Control / Required` +
  `ci-required`. All Velnor-lane fails are backend admission rejections
  (`Velnor rejected this job before workflow execution ... no declared workflow
  command was executed`) — environmental, PR-independent, and a subset of the
  baseline main@c04aec98 fail set from `/tmp/927-verify.md` §4. No GitHub-lane
  failure remains.

```
REPAIR=PASS GATES=ALL-PASS CI=PASS(env-scope) VERDICT=MERGE-OK
```

Not merged per instructions.
