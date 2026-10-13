# Plan 2026-09-14: panic-policy enforcement rollout (r0-panic-policy)

Base: origin/main tip ec0d6ef5 at fetch time. Branch: r0-panic-policy.
Worktree: /tmp/velnor-panic.

## Problem

`unwrap`/`expect`/`panic!`/`unreachable!`/`todo!`/`unimplemented!` in
production code were unregulated outside `velnor-workflow`. Compiler
triage (`cargo clippy --lib --bins`, default and `--all-features`) found
79 production fires plus 4 real BUGs hiding among the proven-impossible
sites.

## Fix (this branch, strict scope)

Per-crate `[lints.clippy]` denies for all 6 lints in all 10 workspace
members, following the `velnor-workflow` Cargo.toml precedent (workflow
itself gains the missing `unreachable = "deny"`). No workspace-level
lints: test code stays out of enforcement scope.

4 BUG fixes (each with a focused regression test):

1. `runner/service.rs`: `From<ServiceCommand> for Command` panicked
   (`unreachable!`) on 4 of 7 variants despite being a public total
   conversion. Now `TryFrom` returning `anyhow::Error`; the `execute()`
   call site uses `?`. The sibling `unreachable!` in `other_command`
   dies structurally by inlining the 8-arm dispatch into
   `dispatch_service` (behavior-identical: scaffold's `Capabilities`
   arm calls the same `manifest::run`).
2-4. `runner/node/controller.rs`: 3x `.lock().expect("not poisoned")`
   on the metrics mutex, one held across file IO. Now
   `unwrap_or_else(|poisoned| poisoned.into_inner())`: metrics state is
   plain data, so a poisoned lock recovers instead of crashing the
   controller. One `#[tokio::test]` poisons the lock via `catch_unwind`
   and drives `update` + `stop_and_publish` through all 3 sites.

Structural tightening (explicitly scoped, clean as hoped):

- `runner/executor.rs`: `PostJavaScriptAction` gains a `post_entrypoint:
  String` field plus a fallible `new()` returning `None` without a
  post entrypoint. Both registration sites fold their `is_some()`
  guards into the constructor; the drain uses the `String` directly.
  The JS-post `expect` is gone; the type carries the proof. (The
  Docker-post drain keeps a per-site allow: same invariant, unchanged
  shape per scope.)

Enforcement coverage (78 production allows, each with a `// Proof:`
comment citing the invariant + `reason = "..."`):

- model 2, control 3, velnorctl 2, bench 8, tools 12 allows / 14 fires,
  runner-lib 49 allows / 50 fires, build.rs 1 module allow (fail-closed
  release-identity gates must abort the build), test_support.rs 1
  module allow (feature-gated, test-only consumers, verified by grep).
- Zero-site crates (client, render, unit-collector, workflow): bare
  denies, no production changes.
- `#[allow]` (never `#[expect]`) per site: `expect` cannot attach to
  statements/arms, and unfulfilled `expect` would fail CI. Three
  trailing-macro `unreachable!` sites use fn-level allows (attributes
  do not attach to a trailing macro call; `return unreachable!` trips
  rustc's own `unreachable_code`).

Test scope (keeps existing `--all-targets` CI green with zero CI edits):

- 24 `tests/*.rs` file headers + per-item allows on `#[cfg(test)]`
  items: `#![allow(...6 lints..., reason = "tests may panic")]`.
- `#[allow]` is illegal on `use` items (`clippy::useless_attribute`,
  deny-by-default) and unused on `thread_local!` (rustc
  `unused_attributes`); those positions are skipped.
- The lib+bins-only CI invocation was considered and deliberately NOT
  added: per-package `--all-targets --all-features` (the exact CI
  shape) already passes, so a second invocation would be redundant.

## Verification

- `cargo clippy --locked --all-targets --all-features -p <each>` for
  all 10 members: zero errors, zero warnings (workflow keeps its 2
  pre-existing `doc_markdown` warnings, proven present without this
  change; untouched).
- `cargo fmt --all -- --check`: clean.
- Tests: 2 new regression tests pass; runner lib 1967 passed / 3
  failed where clean origin/main fails 4 (same flaky lease/quarantine
  tests, allow-only diffs in those files — pre-existing flakes, no new
  failures); tools 197, model 129, control 238, bench 192, ctl 24,
  client 9, collector 17 all pass; `telemetry_integration` passes.
- Staleness note: manifest-only `[lints]` edits did not invalidate
  cached clippy units (mbx/cargo); enumeration re-ran after touching
  every crate root, and final verification ran per package fresh.

## Follow-ups (not this branch)

- `velnor-workflow-contract` is not a workspace member; out of scope.
- The 3-4 flaky lease/quarantine lib tests fail on clean main; owned
  elsewhere.
- `docker_lease`, `protocol`, `runner` `pat`/lifecycle proofs each rest
  on single-constructor/single-take arguments; a future `expect`-to-
  `let-else` pass could delete the proofs where the callsites allow.
