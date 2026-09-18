# D2B re-verification — PR tailrocks/velnor#926 after regen repair

- Branch HEAD: `4d27b19c` (= origin/feat/d2-three-provider-remainder, verified via fetch)
- Prior HEAD: `dc890899`; repair commit parent == `dc890899` (clean append, no amend/rebase)
- origin/main HEAD: `52c424b1` (PR #925, "tasks release publisher") — **main moved**
- Prior full verification: /tmp/d2b-verify.md (HOLD solely for missing regen; all content PASSED)
- Worktree (fresh, detached, /tmp-only writes): `/tmp/d2b-reverify-wt` @ 4d27b19c
- Read-only vs repo: no push/merge/amend

## 1. REPAIR COMMIT: PASS (content exact)

- `git show 4d27b19c --stat`: touches ONLY `.github/ci/.github-actions-generator-state`
  (1 file, 1 insertion, 1 deletion).
- Diff is exactly one line: `scan 33f5782652175e4f → 3924b522e3afa28c` —
  byte-matches the fingerprint CI demanded in the prior report (§1/§3).
- DCO trailer present: `Signed-off-by: Alexey Zhokhov <alexey@zhokhov.com>`.
- `merge-base(branch, origin/main)` = `c04aec98` ≠ origin/main HEAD `52c424b1` →
  **main moved under the branch; STALE.** New main commit `52c424b1` also touched
  `.github/ci/.github-actions-generator-state` (scan now `f89616b7fc62f8a0` on main
  vs `3924b522e3afa28c` on branch) → rebase will conflict on that file and require
  a fresh regen afterward.
- GitHub confirms: PR mergeable = `CONFLICTING`, mergeState = `DIRTY`.

## 2. WORKTREE REGEN: PASS (fix works at new HEAD)

In /tmp/d2b-reverify-wt @ 4d27b19c:
- `cargo build -p velnor-workflow`: exit 0.
- `./target/debug/velnor-workflow --plain --force`: exit 0, then `git status` CLEAN
  (empty) — the prior HOLD cause is gone.
- `./target/debug/velnor-workflow --plain --dry-run`: exit 0, "0 files would change".
- `git diff --name-only c04aec98 HEAD -- .github/workflows`: EMPTY (emission
  identical vs original base, as before).
- `git diff --name-only origin/main HEAD -- .github/workflows`: EMPTY — the task's
  literal check passes even against new main (#925 changed no rendered workflows;
  `c04aec98..origin/main` workflow diff also empty).

## 3. CI: superseded by staleness (snapshot only, no 30-min wait)

Per task instruction, main having moved short-circuits the CI wait — verdict is
already HOLD-stale and a full CI cycle must re-run post-rebase anyway. Snapshot:
- Required `Policy` on head `4d27b19c`: SUCCESS (run 35207049065, completed,
  https://github.com/tailrocks/velnor/actions/runs/35207049065). The regen repair
  flips in CI the exact check that failed before — root cause confirmed fixed.
- `DCO`: SUCCESS. No `CI / PR` run exists yet for `4d27b19c` (only these 2
  check-runs on the head); latest 926/merge CI run is still the old-head 35206077424.
- Post-rebase must re-verify: Policy + `Rust · velnor-workflow / GitHub` SUCCESS,
  every executed /GitHub check SUCCESS, fail set within the affected-scope
  environmental set per prior report §1.

## VERDICT: HOLD-stale

Reasons:
1. origin/main advanced to `52c424b1` (PR #925); branch merge-base `c04aec98` ≠ main
   HEAD. GitHub reports the PR as CONFLICTING — unmergeable as-is.
2. Rebase will conflict on `.github/ci/.github-actions-generator-state` (both sides
   moved the scan line); a fresh regen is required after rebase since the scan
   fingerprint over the rebased tree will differ again.
3. Full CI on the post-rebase head has not run (no CI/PR run even on `4d27b19c`).

The regen repair itself is verified correct: exact one-line commit with DCO, local
regen idempotent (clean status, 0-file dry-run), required Policy green in CI on the
new head. Remaining work is mechanical: rebase onto `52c424b1`, regen, re-verify
items 1+3. Do not merge.
