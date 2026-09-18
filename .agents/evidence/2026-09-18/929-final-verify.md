# PR #929 final verification — after repair (ffdf0c16)

VERDICT: **MERGE-OK**

- PR: tailrocks/velnor#929, branch `fix/d2a-remainder-port` @ `ffdf0c16`
  = port `c3da45a1` + merge `d8b4db94` of main `9772d424` + regen `ffdf0c16`
- Method: read-only except /tmp; fresh detached worktrees `/tmp/929-final-wt`
  (ffdf0c16) and `/tmp/929-final-main` (origin/main); no push/merge/amend.
- Scope: ONLY the update since /tmp/929-verify.md (whose content verdicts stand;
  its 2 HOLD reasons — missing regen, stale base — are what this re-verifies).
- CI runs on ffdf0c16: CI/PR https://github.com/tailrocks/velnor/actions/runs/35211456568
  (completed), Policy https://github.com/tailrocks/velnor/actions/runs/35211454941 (completed).

## 1. ANCESTRY — PASS

- `git merge-base --is-ancestor 9772d424 HEAD` → yes; `origin/main` == `9772d424`
  (base current, staleness HOLD resolved).
- `gh pr view 929` → `mergeable: MERGEABLE` (`mergeStateStatus: BLOCKED` is the
  required-check gate, see §4 note — same gate blocks main itself).
- Pin `a6fa8d4a5096e6df37250b59a8105abfebdb9013` present (toml line 9);
  `git diff origin/main HEAD -- .github-gen/velnor-workflow.toml` empty.
- Merge `d8b4db94` parents `c3da45a1 + 9772d424`, merge-base `7aa4b4e0`:
  `git merge-tree` exit 0 with zero `<<<<<<<` markers (conflict-free), and
  merge-vs-each-parent diffs equal the opposite side's changes in pure content —
  the sole delta is one hunk-header line-number shift (`@@ -2909` vs `@@ -2902`)
  caused by main's 7-line addition above in `s2/primitives/ir.rs`: a mechanical
  rebase offset, not a hand edit. No other content differs on either side.
- Regen `ffdf0c16` touches exactly one file, one line:
  state `scan cd441bd8f5041f2d → e89ef785c3af99ce` — precisely the fingerprint
  the prior report predicted. Regen HOLD resolved.
- DCO: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>` on `c3da45a1` and
  `ffdf0c16`; merge commit carries none (normal, DCO ignores merges); DCO check SUCCESS.

## 2. REGEN — PASS (fresh worktree @ffdf0c16)

- `cargo build -p velnor-workflow` ok; `./target/debug/velnor-workflow --plain --force`
  → `git status --short` EMPTY (clean).
- `--plain --dry-run` → "0 files would change".
- `diff -r origin/main/.github/workflows branch/.github/workflows` EMPTY (no-flip holds).

## 3. CONTENT INTACT — PASS (all in /tmp/929-final-wt)

- `crates/velnor-workflow/tests/provider_pairing.rs` present (528 lines), 6/6 pass.
- `cargo test -p velnor-workflow --lib s2::` → 805 passed, 0 failed.
- `cargo test -p velnor-workflow` (full) → 18/18 result lines ok; lib 1581 passed,
  0 failed; 0 failed in any bin.
- `cargo test -p velnor-runner` → all ok (lib 2166 passed +1, all integration bins),
  0 failed anywhere.
- Contract tests (`cargo test` in `crates/velnor-workflow-contract`) → 2+4 = 6 passed,
  0 failed (matches prior round's 6).
- `cargo clippy --all-targets -p velnor-workflow -- -D warnings` clean (exit 0);
  `cargo fmt --check` clean.

## 4. CI — PASS (fail set within environmental scope, zero new failures)

- Policy run 35211454941 on ffdf0c16: completed **success** (was FAIL in prior round).
- CI/PR run 35211456568 on ffdf0c16: completed; `velnor-workflow / GitHub` SUCCESS
  (was FAIL), `velnor-runner / GitHub` SUCCESS; **every executed /GitHub job SUCCESS**
  (17/17, zero non-success across all /GitHub entries).
- Fail set (7): `Docker|Bun|Documentation|OpenTofu / Velnor`, `Prepare Cargo / prepare-cargo`,
  `ci-required`, `Control / Required` — IDENTICAL to main-baseline run 35208939994's
  fail set (name-for-name). Failure text confirmed from job logs:
  "Velnor rejected this job before workflow execution ... operational store rejected
  the sanitized admission row; job failed closed before execution" — environmental,
  fails before any PR code runs. Both prior-round PR-caused fails are green; nothing
  new fails.
- Note: `mergeStateStatus BLOCKED` reflects the `ci-required` / `Control / Required`
  rollups, which fail identically on main itself (35208939994) due to the Velnor
  admission backend — no PR can satisfy them until the backend accepts; merge needs
  the same override path as any main-advancing PR. Not a PR defect.

## Verdict rationale

Both HOLD reasons from /tmp/929-verify.md are resolved (regen committed with the
exact predicted fingerprint; base == origin/main), the merge is mechanical with no
hand edits, all local gates pass with the same counts as the prior round, and CI shows
Policy + all executed /GitHub green with a fail set identical to main's environmental
baseline. No merge performed (per instructions).
