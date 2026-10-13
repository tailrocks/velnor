# Independent review: typed Rust/Mise boundary

Date: 2026-09-20
Patch under review: `/private/tmp/velnor-mise-typed-rust-boundary-fixed.patch`
Patch SHA-256: `63499336098f9842428b072784ff2f8f3cd56fa9d4e5e1477946d25d0aaa9f65`
Author worktree: `/private/tmp/velnor-rust-toolchain-boundary`, base `27bfb54bdecce09a71f320885bb64f5492083744`.

## Verdict

PASS for the typed boundary itself, with integration caveats below. No shared checkout or Git state was changed.

## Independent probes

- In the author worktree, with `CARGO_TARGET_DIR=/private/tmp/velnor-mise-review-target`:
  - `cargo test --locked -p velnor-workflow --lib config::mise_closure::tests -- --nocapture`: 26 passed.
  - `cargo test --locked -p velnor-workflow --lib primitives::check_profiles::tests -- --nocapture`: 65 passed.
  - `cargo test --locked -p velnor-workflow --lib s2::primitives::check_profiles::tests -- --nocapture`: 29 passed.
  - `cargo test --locked -p velnor-workflow --test scheduled_check_profiles -- --nocapture`: 7 passed.
  - `cargo fmt --all -- --check`: passed.
  - `cargo clippy --locked -p velnor-workflow --all-targets --all-features -- -D warnings`: no issues.
- Real Mise `2026.9.11` fixture `/private/tmp/mise-boundary-probe` confirmed actual `depends_post`, `wait_for`, aliases, structured dependency args/env, and structured `run = [{ tasks = [...] }, { task = ... }]` semantics with `mise tasks --json`, `mise tasks deps`, and `mise run --dry-run`. The parser's opaque string policy matches the real `run` type: shell strings are not statically interpreted.
- Candidate binary dry-run against isolated Jackin `/private/tmp/jackin-mise-closure-check-112` completed 40 units/146 edges. Rendering to `/private/tmp/jackin-mise-render` produced desktop jobs whose `install_args` include every nested task tool (Rust is omitted from Mise), and all four auto-install flags are false.
- Generated Rust block checks: cache key hashes both `rust-toolchain.toml` and `rust-toolchain`; cache saves only default-branch push, schedule, or default-branch dispatch and only on exact restore miss; hosted block provisions through file-driven Rustup and adds declared targets; Velnor block checks selected channel/host/components/targets and performs no install. Local Rustup output confirms target listings are bare target names and host-qualified component names, matching the generated exact checks.
- Official Rustup docs confirm toolchain-file `profile`, `components`, and `targets` are additive install inputs: [Rustup toolchain files](https://rust-lang.github.io/rustup/overrides.html).
- Official Mise docs confirm `depends`, `depends_post`, `wait_for`, and structured `run` task references: [Mise task configuration](https://mise.jdx.dev/tasks/task-configuration.html).

## Structural assessment

The previous bug class was generic Mise Rust installation plus profile/status checks that did not prove the pinned Rust component/target contract. The patch moves Rust ownership to the parsed `rust-toolchain` object, requires a matching exact `core:rust` lock identity when Mise declares Rust, removes Rust from generic Mise install arguments only for that typed boundary, and makes task-local identity drift fail closed. Auto-install is disabled at all four Mise layers, so an opaque shell/nested task omission cannot silently download a tool.

Task-local typed closure traverses root tools, task tools, `depends`, `depends_post`, `wait_for` validation, aliases, and structured `run` references. Unknown inherited/config shapes and unsupported structured keys fail closed; opaque shell commands remain unparsed and execute with auto-install disabled. This preserves visibility without pretending shell analysis is complete.

## Caveats for integration

- The patch is based on `27bfb54b`; current Velnor HEAD `17318e523`/`845d474` already contains later policy, lock-validation, and revision changes. Transplant the typed hunks and regenerate; do not whole-file replace current files.
- Jackin's consumer still needs the separately reviewed removal of any user-owned `MISE_TASK_RUN_AUTO_INSTALL` declaration before adopting the generated policy. The typed patch itself correctly rejects that override.
- The generated hosted Rust command intentionally uses `rustup toolchain install` without a channel argument so the checked-out `rust-toolchain.toml` supplies channel/components/profile; this is supported by Rustup's toolchain-file contract. Target additions are explicit.
- `run_windows`/file-task/template semantics are outside the typed static closure. The implementation rejects inherited top-level overlays and unknown structured shapes, and the four auto-install flags make omitted opaque tools fail visibly; consumers using those shapes still need explicit migration or a generated-closure fixture.
- No performance credit is assigned. These checks establish command/trust behavior only.

## Integration checkpoint

Applied over upstream merge `3fb38643` without replacing current generator files.
The parent reran all 1,988 generator tests: all passed, with one leaked-process
report in `apt::tests::channel_update_rejects_incoherent_heads`; investigation
remains open. This is correctness validation, not performance acceptance.
Real Jackin dry-run rejects its existing redundant auto-install override, as
expected; consumer migration remains required before regeneration.
