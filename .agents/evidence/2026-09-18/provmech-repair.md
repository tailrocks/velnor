# REPAIR evidence — a2 provmech (G3+G5+G8+G9)

Verdict repaired: MISMATCH on `a05b0e2a12405e737868a2bbf8872ce72169d674`
(see `/tmp/v-a2-provmech.md`, disproofs 1 + 2).
Repair commit (NEW, no amend): `5838b9fe00c29a85085deafc545134dc7ea66a73`
Branch: `feat/a2-provision-promotion` (pushed `a05b0e2a..5838b9fe`, fast-forward).
Work: `/tmp/provmech-repair-wt` (repair) + `/tmp/provmech-verify-wt` (clean clone @ 5838b9fe).

## Fix 1 — stale ownership state (disproof 1)

Post-staging regen in the repair worktree:

- `git add -A` (stage code+test edits first, so the index holds every path)
- `cargo build --locked -p velnor-workflow`
- `./target/debug/velnor-workflow --plain --force` → changed ONLY
  `.github/ci/.github-actions-generator-state` (generated workflows byte-identical)
- committed scan `5fc04182098c3e1d` → `5774aa62dffb375e` — exactly the
  verdict's predicted clean-checkout value; config `951b0f534335b78c` and
  generator `50` unchanged
- `git add -A` again, then verified

Clean-clone verification (`/tmp/provmech-verify-wt`, fresh worktree @ 5838b9fe,
fresh `cargo build --locked -p velnor-workflow`):

- `./target/debug/velnor-workflow --plain --dry-run` →
  `Result Dry-run: 0 files would change`, exit 0
- `./target/debug/velnor-workflow --plain --check` →
  `Result Generated files are current`, exit 0 (via the candidate exception:
  tree matches the candidate render `17baa98f…`, not the declared pin
  `7341ef4b…` — expected for a generator change in flight; notice names
  `promote --rev HEAD` after merge). Same closure in both worktrees.
- `git status` clean after both runs (checks wrote nothing)

Note: the first `--check` attempt in this environment failed at D19 renderer
resolution ("no velnor-workflow renderer for the declared pin … is
provisioned"), before any comparison — environmental (stale PATH binaries
without `--closure`), identical on any commit. One `--check --pin-build` run
cached the pin renderer under `$TMPDIR/velnor-workflow-policy-7341ef4b…`;
every literal `--plain --check` since exits 0.

## Fix 2 — promote force + Update-path fixture (disproof 2)

`crates/velnor-workflow/src/promote.rs` (`promote_rendered_tree`): the writer
call now passes `force=true` (`dry_run=false, check=false, force=true,
adopt=false`) with a justification comment. Force is the intended semantic:
promotion requires a tracked-clean tree, snapshots every preimage for
restore, and proves determinism + write integrity after the write. Force
bypasses ONLY the conflicts guard — ownership proof still rejects manually
modified files, and adopt stays false so unowned workflows are never deleted.
No new CLI flag (promote has no `--force` surface).

`crates/velnor-workflow/tests/promote_atomic.rs`: new test
`promote_advances_a_committed_prior_render` — renders the synthetic consumer
in place with the old pin, COMMITS the prior render, promotes to the binary's
own revision, and asserts the promotion commit contains `M` (modified)
entries via `git show --name-status`, i.e. the Update path every real
promotion takes.

Negative check (fixture is not blind): with the writer temporarily reverted
to `force=false`, the new test FAILS with the verdict's exact error —
`would overwrite generated files: .github/workflows/ci-main.yml,
ci-policy.yml, …; rerun with --force after review`. Fix restored
(byte-identical) afterwards.

## Gates (repair worktree, final content = committed bytes)

- `cargo test --locked --all-features -p velnor-workflow`:
  495 + 0 + 2 + 6 + 4 + 5 + 9 + 33 = **554 passed, 0 failed** (was 553+0 at the
  bundle; +1 is the new Update-path test)
- `cargo clippy --locked --profile test --all-targets --all-features
  -p velnor-workflow -- -D warnings`: exit 0
- `cargo fmt -p velnor-workflow -- --check`: exit 0
- `actionlint` (1.7.12): exit 0
- `cargo test --locked -p velnor-workflow --test promote_atomic` in the CLEAN
  worktree: 4 passed, 0 failed

## Commit

`5838b9fe00c29a85085deafc545134dc7ea66a73`
`fix(velnor-workflow): repair provision-promotion bundle (stale state, promote force)`
signed off (`-s`), 3 files, +77/−2. Pushed to `origin/feat/a2-provision-promotion`
(fast-forward `a05b0e2a..5838b9fe`); the original commit was NOT amended.
