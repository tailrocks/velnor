# A1 gate suite — velnor3 @ 38ffbfd7 (clean tree)

Run 2026-09-16, no edits. Runner: `rtk` prefix (compresses stdout to one-line summaries; exit codes are the gate verdicts).

| # | Gate | Command | Exit | Result |
|---|------|---------|------|--------|
| 1 | workflow tests (full) | `cargo test -p velnor-workflow` | 0 | PASS — 541 passed, 8 suites, 12.07s |
| 2 | workflow clippy | `cargo clippy --all-targets -p velnor-workflow -- -D warnings` | 0 | PASS — no issues |
| 3 | workspace fmt | `cargo fmt --check` | 0 | PASS — clean |
| 4 | actionlint | `actionlint-1.7.12 .github/workflows/*.yml` (14 files) | 0 | PASS — no output |
| 5 | contract tests | `cargo test --manifest-path crates/velnor-workflow-contract/Cargo.toml` | 0 | PASS — 6 passed, 2 suites |
| 6 | contract fmt | `cargo fmt --manifest-path crates/velnor-workflow-contract/Cargo.toml -- --check` | 0 | PASS — clean |
| 7 | contract clippy | `cargo clippy --manifest-path crates/velnor-workflow-contract/Cargo.toml --all-targets -- -D warnings` | 0 | PASS — no issues |

Notes:
- No `tests/` dir at root; workflow integration tests live in `crates/velnor-workflow/tests/` (covered by gate 1).
- `velnor-workflow-contract` is a standalone workspace (own Cargo.lock, not a member), so it needs its own commands (gates 5–7); root `cargo fmt --check` / `-p velnor-workflow` do not cover it.
- Failure lists: none — all gates exit 0, zero failures to excerpt.
- Raw logs: /tmp/a1-cargo-test.log, /tmp/a1-clippy.log, /tmp/a1-fmt.log, /tmp/a1-actionlint.log, /tmp/a1-contract.log, /tmp/a1-contract-fmt.log, /tmp/a1-contract-clippy.log (+ .exit files).
