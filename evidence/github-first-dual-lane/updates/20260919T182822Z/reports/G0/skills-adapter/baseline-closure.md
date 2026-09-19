# Closure feature-stamp baseline proof

Parent revision: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`\
Detached worktree: `/tmp/velnor-skills-parent-abe9ad82`\
Worktree status before tests: clean, detached at the parent revision.

Both exact tests fail on the parent without any skills-adapter source:

```text
rtk cargo test -p velnor-workflow --no-default-features --lib \
  closure::tests::stamped_features_match_dev_features -- --exact
exit 101
closure::tests::stamped_features_match_dev_features
left: ""
right: "tui"
test result: FAILED; 0 passed; 1 failed; 1676 filtered out
```

```text
rtk cargo test -p velnor-workflow --no-default-features --lib \
  s2::closure::tests::stamped_features_match_dev_features -- --exact
exit 101
s2::closure::tests::stamped_features_match_dev_features
left: ""
right: "tui"
test result: FAILED; 0 passed; 1 failed; 1676 filtered out
```

The assertion is the existing `build.rs` versus canonical closure default
feature mismatch. Candidate full-library runs fail the same two tests and add
no new failure.
