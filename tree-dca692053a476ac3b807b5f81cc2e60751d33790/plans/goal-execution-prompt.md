# Goal Prompt: Execute the Deep Plan Library on This PR Only

Use this file as the detailed contract for a Codex `/goal` run. Paste this
short command into Codex:

```text
/goal Execute the plan library in plans/README.md for plans 001-032 on the current PR branch only. Read and follow plans/goal-execution-prompt.md as the binding goal contract. Never merge.
```

## Objective

Work through the plan library added in this PR, starting at
`plans/001-condition-unary-not-precedence.md` and ending at
`plans/032-investigate-cancellation-during-broker-outage.md`, including
`plans/README.md`.

The goal is complete only when every applicable plan is either implemented and
verified, explicitly marked `DONE`, or explicitly marked `BLOCKED` or
`REJECTED` with a concrete one-line reason in `plans/README.md`.

## Hard Boundaries

- Work only on this PR branch. Do not move this work to another PR or another
  branch.
- Before editing, confirm `git branch --show-current` is the PR branch.
- Before every push, confirm the current branch is still the PR branch.
- Never merge this PR.
- Never squash, rebase, force-push, or rewrite history unless the operator gives
  explicit instructions for that exact action.
- Do not weaken fixture coverage or change `tailrocks/velnor-actions-fixture` to
  work around Velnor gaps.
- Follow `AGENTS.md` and each plan file's STOP conditions. If they conflict,
  `AGENTS.md` wins, then the plan file, then this prompt.

## Execution Rules

- Read `plans/README.md` first, then read the specific plan file before starting
  that plan.
- Respect dependencies in `plans/README.md`. If the README and a plan disagree,
  stop and resolve the inconsistency before implementing dependent work.
- Prefer the suggested waves, but pull security-sensitive policy-only work
  forward when it is independent and low risk.
- Keep each implementation scoped to its plan. Do not combine unrelated plans in
  one commit unless the code is inseparable.
- For investigate/spike plans, make the decision explicit in the resulting
  commit, docs, or README status. Do not pretend a spike produced product code
  when it only produced findings.
- Update the relevant row in `plans/README.md` as part of each plan's completion
  or blocker commit.

## Commit And Push Cadence

- Commit and push constantly to this PR branch.
- At minimum, commit and push after each completed plan.
- Also commit and push after any meaningful checkpoint that reduces risk, such
  as a passing focused test, a resolved blocker, or a documentation/status
  update.
- Use small, reviewable commits matching the commit message suggested by each
  plan when one is provided.
- After each push, report the pushed commit hash and the plan numbers covered.

## Verification

Run each plan's focused verification commands. When a plan touches shared runner
behavior, also run the repository gates from `plans/README.md` when feasible:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
```

If a required verification cannot be run locally, record exactly why and what
was run instead. Do not mark a plan `DONE` if its own required verification was
skipped without an explicit accepted blocker or operator-gated reason.

## GitHub Actions And Fixture Verification

When dispatching or monitoring GitHub Actions workflow verification, follow the
repository hard rules:

- Cancel pending or in-progress old runs before dispatching a new one.
- Delete stale runner registrations before dispatching a new run.
- Monitor only the newly dispatched run ID.
- Never wait more than 2 minutes without checking visible state and diagnosing
  lack of progress.

## Completion Criteria

The `/goal` run is complete when:

- Every plan from 001 through 032 has an accurate final status in
  `plans/README.md`.
- All code/doc changes are committed and pushed to this PR branch.
- The final response lists the last pushed commit, plan statuses, verification
  commands run, commands that could not be run, and any remaining operator-gated
  actions.
- The PR remains unmerged.
