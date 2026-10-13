# Plan 001: Fix unary `!` operator precedence in `if:` condition evaluation

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 1645ccc..HEAD -- crates/velnor-runner/src/executor.rs`
> If `executor.rs` changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `1645ccc`, 2026-07-03

## Why this matters

Velnor mirrors GitHub Actions `if:` semantics. GitHub operator precedence is
`!` (highest) → comparisons → `&&` → `||` (lowest). Velnor's **condition**
evaluator (`evaluate_condition_expr`) applies a leading `!` to the *entire*
remaining expression **before** splitting `&&`/`||`, so it computes
`!cancelled() && X` as `!(cancelled() && X)` instead of `(!cancelled()) && X`.
Because `cancelled()` is `false`, `!(false && X)` is **always true**, so the
step runs regardless of `X`. The idiom `if: ${{ !cancelled() && steps.x.outcome
== 'success' }}` is extremely common, and Velnor runs steps GitHub would skip —
a silent correctness divergence on the product's core parity guarantee. The
**value** evaluator (`resolve_expression_value`) already handles this correctly;
the two evaluators disagree. This fix aligns the condition evaluator with the
already-correct value evaluator.

## Current state

- `crates/velnor-runner/src/executor.rs` — the job execution engine.
  `JobExecutionState` has two expression evaluators:
  - `resolve_expression_value` (value context) — **correct** ordering. Excerpt
    at `executor.rs:4723-4745`:
    ```rust
    fn resolve_expression_value(&self, expression: &str) -> Option<String> {
        let expression = expression.trim();
        if let Some(inner) = strip_wrapping_parentheses(expression) { ... }
        if let Some((left, right)) = split_top_level(expression, "||") { ... }   // ||  first
        if let Some((left, right)) = split_top_level(expression, "&&") { ... }   // &&  second
        if let Some(inner) = expression.strip_prefix('!') {                      // !   third (binds tightest)
            let value = self.resolve_expression_value(inner).unwrap_or_default();
            return Some((!expression_truthy(&value)).to_string());
        }
        ...
    }
    ```
  - `evaluate_condition_expr` (condition context) — **buggy** ordering. Excerpt
    at `executor.rs:4950-4986`:
    ```rust
    fn evaluate_condition_expr(&self, expression: &str) -> bool {
        let expression = expression.trim();
        if expression == "always()" { return true; }
        if expression == "success()" { return !self.conclusions.values().any(|o| *o == StepOutcome::Failure); }
        if expression == "failure()" { return self.conclusions.values().any(|o| *o == StepOutcome::Failure); }
        if expression == "cancelled()" { return false; }
        if let Some(inner) = strip_wrapping_parentheses(expression) {
            return self.evaluate_condition_expr(inner);
        }
        if let Some(inner) = expression.strip_prefix('!') {                 // <-- BUG: `!` applied
            return !self.evaluate_condition_expr(inner);                    //     before && / || split
        }
        if let Some((left, right)) = split_top_level(expression, "||") {
            return self.evaluate_condition_expr(left) || self.evaluate_condition_expr(right);
        }
        if let Some((left, right)) = split_top_level(expression, "&&") {
            return self.evaluate_condition_expr(left) && self.evaluate_condition_expr(right);
        }
        if let Some((value, needle)) = parse_contains(expression) { ... }
        if let Some((left, right)) = split_top_level(expression, "!=") { ... }
        // ... "==" etc. below
    }
    ```

- Convention: tests are inline `#[cfg(test)]` in the same file. An existing
  condition test to model after is `evaluates_step_outcome_conditions` at
  `executor.rs:9577`, which builds `JobExecutionState::default()`, calls
  `state.apply("<id>", &StepExecutionResult { exit_code, skipped, failure_ignored,
  state: StepCommandState::default(), stdout: String::new(), stderr: String::new() })`,
  then asserts on `state.evaluate_condition(Some("<expr>"))`. Note line
  `executor.rs:9635` already asserts `failure() && !cancelled()` passes — that
  case works because `!` is **not** the leading token there; the bug only
  triggers when `!` leads an expression that *also* contains a top-level
  `&&`/`||`.

## Commands you will need

| Purpose   | Command                                                                 | Expected            |
|-----------|-------------------------------------------------------------------------|---------------------|
| Format    | `cargo fmt --all --check`                                               | exit 0              |
| Lint      | `cargo clippy --workspace --all-targets --locked -- -D warnings`        | exit 0              |
| One test  | `cargo nextest run -p velnor-runner not_precedence --locked`            | new test passes     |
| All tests | `cargo nextest run --workspace --locked`                                | all pass            |

## Scope

**In scope**:
- `crates/velnor-runner/src/executor.rs` — reorder the branches in
  `evaluate_condition_expr`, and add a regression test in its `#[cfg(test)]`
  module.

**Out of scope** (do NOT touch):
- `resolve_expression_value` — it is already correct; do not "unify" the two
  evaluators in this plan (that is a larger refactor with its own risk).
- Any other expression helper (`parse_contains`, `split_top_level`,
  `strip_wrapping_parentheses`).

## Git workflow

- Branch: `advisor/001-condition-unary-not-precedence`
- Conventional commit, e.g. `fix(executor): bind unary ! tighter than && / || in if: conditions`
- Do NOT push or open a PR unless the operator instructed it.

## Steps

### Step 1: Move the `!`-prefix branch below the `&&` split

In `evaluate_condition_expr` (`executor.rs:4950`), move the block

```rust
if let Some(inner) = expression.strip_prefix('!') {
    return !self.evaluate_condition_expr(inner);
}
```

so it appears **after** the `||` split and the `&&` split, i.e. the new order
is: keyword checks → `strip_wrapping_parentheses` → `||` → `&&` → `!` →
`parse_contains` → `!=` → `==` (mirrors the proven order in
`resolve_expression_value`). Do not change any branch body, only their order.

**Verify**: `cargo fmt --all --check` → exit 0; `cargo clippy --workspace
--all-targets --locked -- -D warnings` → exit 0.

### Step 2: Add a regression test

In the `#[cfg(test)]` module of `executor.rs`, add a test named
`not_precedence_binds_tighter_than_and`. Model its construction on
`evaluates_step_outcome_conditions` (`executor.rs:9577`). It must:

- Build a `JobExecutionState::default()`.
- `state.apply("failed", &StepExecutionResult { exit_code: 1, skipped: false,
  failure_ignored: false, state: StepCommandState::default(), stdout:
  String::new(), stderr: String::new() })` so a step failed (makes
  `success()` false, `failure()` true).
- Assert the bug is fixed:
  - `assert!(!state.evaluate_condition(Some("!cancelled() && steps.failed.outcome == 'success'")));`
    (GitHub: `(!false) && (failure=='success')` = `true && false` = **false** → skip.)
  - `assert!(state.evaluate_condition(Some("!cancelled() && steps.failed.outcome == 'failure'")));`
    (`true && true` = true.)
- Assert existing-correct cases still hold (guard against a regression in the
  reorder):
  - `assert!(state.evaluate_condition(Some("!cancelled()")));`
  - `assert!(state.evaluate_condition(Some("failure() && !cancelled()")));`
  - `assert!(!state.evaluate_condition(Some("!failure()")));`

**Verify**: `cargo nextest run -p velnor-runner not_precedence --locked` → the
new test passes. Then `cargo nextest run --workspace --locked` → all pass (no
existing condition test regressed).

## Test plan

- New test `not_precedence_binds_tighter_than_and` in `executor.rs`
  `#[cfg(test)]`, covering: `!` + top-level `&&` with a false right operand
  (the bug), the same with a true right operand, bare `!cancelled()`, `&&` with
  a trailing `!` (already-passing case), and `!failure()`.
- Model after `evaluates_step_outcome_conditions` (`executor.rs:9577`).
- Verification: `cargo nextest run --workspace --locked` → all pass including
  the new test.

## Done criteria

- [ ] `cargo fmt --all --check` exits 0
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings` exits 0
- [ ] `cargo nextest run --workspace --locked` exits 0; `not_precedence_binds_tighter_than_and` exists and passes
- [ ] In `evaluate_condition_expr`, the `strip_prefix('!')` branch is positioned after both the `||` and `&&` `split_top_level` branches
- [ ] No files outside `crates/velnor-runner/src/executor.rs` modified (`git status`)
- [ ] `plans/README.md` status row updated

## STOP conditions

Stop and report if:
- The `evaluate_condition_expr` body no longer matches the excerpt (drift).
- Reordering the branches breaks any existing test in `executor.rs` that you
  cannot explain by the precedence change alone (it may indicate another branch
  depends on the old order — report which test and its expression).
- You find a third evaluator or that `evaluate_condition` delegates differently
  than assumed.

## Maintenance notes

- The two evaluators (`evaluate_condition_expr`, `resolve_expression_value`)
  duplicate operator-precedence logic. A future cleanup could unify them behind
  one parser; until then, **any** precedence change must be applied to both.
  Flag that in review.
- Reviewer should confirm the branch order matches `resolve_expression_value`
  exactly for `||`/`&&`/`!`.
