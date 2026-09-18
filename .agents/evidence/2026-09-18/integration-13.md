# integration-13 — merge d1d wiring into docs/bastion-final-plan (bastion batch 13)

Merged: `feat/d1-daemon-wiring` @ `9d7b6dfd` (base `3bb1e23c`, 28 files)
Cert: `/tmp/v-d1d.md` = CERTIFIED (read first; DCO signed, origin hash matches).
Into: `docs/bastion-final-plan` @ `0477e14f` → merge commit
`04607e6baadc4d19f7f8c36a21c062a646fb1aa5` (parents `0477e14f` + `9d7b6dfd`),
pushed to `origin/docs/bastion-final-plan` (`0477e14f..04607e6b`, in sync).

## Merge
- `git fetch origin`; `git merge feat/d1-daemon-wiring --no-commit`:
  4 conflicted files, 7 hunks, all source-only (no generated YAML touched).
- Hunk analysis (all in `crates/velnor-runner`):
  - 6 doc-only hunks (`client.rs` AdminToken; `credentials.rs`
    GitHubAppAuth/PemJwtProvider/InstallationAccessToken;
    `worker/mod.rs` ProvisionPlan; `worker/runner.rs` RunnerSpec): both
    sides made the IDENTICAL functional change (derive `Debug` removal +
    manual redacting `Debug` impl, merged cleanly once each, verified no
    duplicates); only the doc sentences differed. Resolved by merging
    both wordings (HEAD's precision + d1d's nouns); where HEAD was a
    strict superset, HEAD's sentence stands.
  - 1 const hunk (`worker/runner.rs`): HEAD (secfix) added `JIT_ENV_FILE`
    + doc; d1d side empty (base had no const; secfix replaced `--env`
    argv JIT with the 0600 `--env-file` flow). Kept HEAD's const; use at
    `write_env_file` merged cleanly.
- Verified no duplicate `impl Debug` (one per type), no duplicate test
  names (HEAD `..._redacts_the_...` vs d1d `..._redacts_...` coexist),
  no duplicate imports. `cargo check -p velnor-runner --all-targets`
  clean before staging.
- `git add -A` BEFORE regen per standing rule; `.agents/memory/` (user
  property) unstaged before the final regen+commit — verified NOT in
  the merge (29 files: 28 d1d sources + generator state).
- Regen via rebuilt generator (`cargo build -p velnor-workflow`,
  `./target/debug/velnor-workflow generate . --plain --force`): 21 files,
  all rendered bytes identical; only the `scan` digest in
  `.github-actions-generator-state` moved (new sources). No hand-edits
  to generated YAML.
- Procedure note: the scan digest covers the INDEX, so the final regen
  must run AFTER unstaging user-property paths (caught live: unstage
  invalidated the digest; re-regenned over the final index, then 0/0).

## Merge-interaction fix (test-only, no production change)
`cargo test -p velnor-runner --features test-support` exposed 3
deterministic failures in `tests/scaleset_daemon.rs` — d1d's tests were
certified pre-secfix and hardcoded two contracts secfix changed:
- Slug format: secfix hash-suffixed `OwnershipId::slug()`; d1d helpers
  built the old `s7-velnor-7-4242` shape, so `stop/drop_container` and
  `fail_rm` silently missed (crash test `failed=0`, cleanup test veto
  never triggered → 30s timeout).
- JIT mechanism: secfix moved the blob from `--env` argv to a one-shot
  0600 `--env-file`; e2e asserted the blob in argv.
Fix (in the merge commit, `tests/scaleset_daemon.rs` only): helpers now
derive every name from production (`runner_name()` +
`OwnershipId::bind` → `slug()`/`runner_container()`/`dind_container()`/
`as_str()`); e2e asserts `--env-file` in the runner create argv, exact
env-file bytes via new FakeDocker capture-while-present (`env_files`,
read during `create` before production deletes the file), and blob
absent from ALL argv. Structural: tests can no longer desync from the
name format. Production `lane.rs`/`daemon.rs` already derive names from
the same constructors (verified zero hardcoded formats).

## Gates (all green @ merge)
- `--plain --dry-run`: exit 0, "0 files would change" (this checkout AND
  clean worktree @ `04607e6b`).
- `--plain --check`: exit 0, "Generated files are current" (both).
- `cargo test -p velnor-workflow`: 653 passed (594 lib + 2/6/4/5/9/33),
  0 failed.
- `cargo test -p velnor-runner`: lib 2323 passed + 5 ignored, all
  integration targets ok, 0 failed.
- `cargo test -p velnor-runner --features test-support`: lib 2392 + 5
  ignored; `scaleset_daemon` 11/11; all 19 suites ok, 0 failed
  (3 consecutive full green runs; one transient single-lib-test failure
  seen once in 5 runs, name not captured, never reproduced — flake).
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo clippy -p velnor-runner --all-targets --features test-support
  -- -D warnings`: exit 0 (covers the edited test file).
- `cargo fmt --all --check`: exit 0.
- `actionlint` (1.7.12): exit 0, no output.

## Commit
- `git commit -s` (DCO signoff Alexey Zhokhov), pushed; scratch worktree
  removed. Pre-existing v-d1d minor finding (stale `lane.rs` doc
  paragraph) untouched — flagged for follow-up, not this merge.
