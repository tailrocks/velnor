# G2 preview-publication result

- Branch: `codex/github-first-preview-publication`
- Local HEAD: `5f2b0d384f8833ac6407087c12f6b00d59e2f9cc`
- Remote `origin/codex/github-first-preview-publication`: `5f2b0d384f8833ac6407087c12f6b00d59e2f9cc`
- Worktree: clean and tracking remote.
- Commit: `fix(workflow): make preview publication immutable`
- Commit trailers: `Co-authored-by: Codex <codex@openai.com>` and DCO `Signed-off-by`.

Checks recorded before checkpoint:

- `cargo fmt --all`: pass.
- Release-focused suites: `269 passed`.
- `s2::primitives::preview_publication` fixtures: `6 passed`.
- Full crate suite: `1740 passed, 1 failed`; only
  `s2::tests::checked_in_workflows_match_the_generator_byte_for_byte` failed,
  as expected for the source-only checkpoint that leaves generated `.github`
  workflows unchanged.
- `git diff --check`: pass.
- No real release/API publication, workflow dispatch, merge, or force push.

The source branch is frozen pending coordinator review. Evidence is outside
the repository and was not staged or committed.
