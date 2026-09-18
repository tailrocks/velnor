# Integration batch 11: D1 processor-loop merge (bastion campaign)

- Role: DESIGNATED INTEGRATION subagent. Read `/tmp/v-d1c.md` first; source
  INDEPENDENTLY CERTIFIED, SHA verified after `git fetch origin`
  (`origin/feat/d1-processor-loop` == `3f935eedee26d06a6dca57570c6b6c72106e643a`),
  source branch untouched.
- Start: `docs/bastion-final-plan` @ `137ca1f9` (clean tree, verified).
- Merge base `HEAD`/`3f935eed` = `966f3623` (d1a tip, already in HEAD);
  source diff `966f3623..3f935eed` = 28 files +6161/-24, 0 `worker/*` —
  matches cert exactly.

## Merge: origin/feat/d1-processor-loop @ 3f935eed

- `git merge --no-ff` → ONE conflict, source-only:
  `crates/velnor-runner/src/scaleset/mod.rs` (doc header + re-export
  block: d1b side added allocator re-exports, d1c side added
  backoff IdlePolicy/PollOutcomeClass + capacity re-exports).
- Resolution (source file, by hand): union — header now reads
  "D1 parts A + B + C", allocator + capacity + extended backoff
  re-exports all kept. No generated YAML touched by hand at any step.
- Staged scope: 29 files = 28 source files + generator state; 0
  `worker/*` paths; nothing outside velnor-runner/model/control +
  generator state.
- `git add -A` BEFORE regen per standing rule; rebuilt
  (`cargo build --locked -p velnor-workflow`); pre-regen `--dry-run`
  predicted exactly 1 file.
- Regen `./target/debug/velnor-workflow --plain --force .` → "Generated
  21 files", only state changed (`scan 558bd1bd5edbde43` → `b0e7a11487b0ece8`,
  config hash unchanged); zero YAML output changes.

## Gates at pushed bytes (`3bb1e23c`)

- `--plain --dry-run`: `0 files would change`, exit 0
- `--plain --check`: `Generated files are current`, exit 0
- `cargo test --locked -p velnor-workflow`: **591 lib** + 0 bin +
  2/6/4/5/9/33 integration + 0 doc (650 total), 0 failed, exit 0
- `cargo test --locked -p velnor-runner` (default): **2276 lib** passed,
  5 ignored, all integration binaries ok, 0 failed, exit 0
- `cargo test --locked -p velnor-runner --features test-support`:
  **2345 lib** + all suites incl. `scaleset_loop` **9/9**,
  `scaleset_protocol` **8/8**, `scaleset_allocator` **6/6** and
  `scaleset_worker` **3/3**, 0 failed, exit 0
- `cargo clippy --locked -p velnor-workflow -p velnor-runner --all-targets
  -- -D warnings`: 0 warnings, exit 0; plus `-p velnor-runner
  --features test-support`: 0 warnings, exit 0 (mbx cache lines only)
- `cargo fmt --all -- --check`: clean, exit 0
- `actionlint` (repo-wide): exit 0, no findings
- `git diff --check`: clean

## Push

- Merge commit: `3bb1e23c0ea4498b6cf6acd180445ea16806af6a`
  "Merge feat/d1-processor-loop into docs/bastion-final-plan" (+ signoff).
  Parents: `137ca1f9` + `3f935eed`.
- `git push origin docs/bastion-final-plan`: `137ca1f9..3bb1e23c`, exit 0.
- Tree clean after push.
