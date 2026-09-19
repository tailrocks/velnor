# G1 source integration: PR #952

Worker settings: `gpt-5.6-luna`, reasoning `max` (thread `01a0ba72-3925-7141-b1f7-5529a5cf6c98`).

Input source: `tailrocks/velnor` main `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.

Integrated source commit: `12cc87b629802c294da9840325cb21087c020df6` on branch `codex/github-first-generator` in `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-generator`.

Upstream attribution: PR `#952`, source commit `a5c1c0bd5c92c4c52d58ccb21042b1b2c0b08637`, author Alexey Zhokhov. The commit message records `Original-PR`, `Original-Commit`, `Original-Author`, `Co-authored-by: Codex <codex@openai.com>`, and DCO signoff.

Only these source files changed:

- `crates/velnor-workflow/src/s2/primitives/ir.rs`
- `crates/velnor-workflow/src/s2/primitives/mod.rs`
- `crates/velnor-workflow/src/s2/primitives/release.rs`

The changes restore the Docker mutable-mount seed lifecycle for concrete release legs and centralize/fetch the D19 generator pin before hosted `--plain --check` runs. No generated `.github` file, workflow, generator state, or configuration was copied or hand-edited.

Checks, using `CARGO_TARGET_DIR=/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/seed-pin-target` and `CARGO_BUILD_JOBS=4`:

- Focused `cargo test -p velnor-workflow --lib native_release -- --nocapture`: **11 passed**, 1726 filtered.
- Full `cargo test -p velnor-workflow --lib`: **1736 passed, 1 failed**. The sole failure is the expected checked-in generated workflow byte snapshot (`release.yml drifted from the generator`) because this handoff is source-only and regeneration/pin promotion is the next staged step.
- Full source suite excluding only `checked_in_workflows_match_the_generator_byte_for_byte`: **1736 passed, 1 filtered**.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy -p velnor-workflow --lib --tests --locked -- -D warnings`: passed.
- `git diff --check`: passed; post-commit worktree clean.

The generated snapshot failure must be resolved by the owner of the staged pin/regeneration step; it is evidence that generated outputs are now stale, not a reason to weaken the drift gate.
