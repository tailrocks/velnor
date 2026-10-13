# Velnor Goal Prompts

This folder holds **goal prompts** for driving large, well-scoped pieces of Velnor
work with an autonomous coding agent (Claude Code or Codex). Each prompt is a
self-contained mission statement plus an exhaustive checklist that the agent
works through to completion.

> **Read [`../docs/mission.md`](../docs/mission.md) first.** Every prompt serves one mission:
> the fastest, cheapest, nicest, **Rust-first** self-hosted GitHub Actions
> runner — pre-cached and pre-optimized for the latest Rust, more parallel and
> more informative than GitHub-hosted runners. Keep it in mind for every prompt.

## What is a "goal prompt"?

A goal prompt is two files:

| File | Role |
|------|------|
| `<name>.md` | **The goal.** A short, stable statement of *what* must be achieved, *why*, the scope boundaries, and the definition of done. Hands the agent a link to its checklist. |
| `<name>.checklist.md` | **The work.** A huge, fine-grained, checkbox list of everything that must be done, verified, and proven for the goal to be complete. This is the agent's working memory and progress tracker. |

The goal file rarely changes. The checklist is living: the agent ticks items,
adds discovered sub-tasks, and records evidence as it goes.

## How to run a prompt

The workflow is the same regardless of which agent you use — Claude Code or
Codex. Both expose a `/goal` command that takes a goal file and executes it.

```
/goal prompts/github-ui-parity.md
```

The agent will:

1. Read the goal file to understand the mission and scope.
2. Open the linked checklist.
3. Work top-to-bottom, checking items off, backfilling any task the goal
   implies but the checklist missed.
4. Run the verification gates listed in the checklist and record evidence.
5. Stop when every checklist item is checked and every gate is green.

> The prompt is agent-agnostic. Nothing in a goal or checklist file assumes a
> specific agent. Use whichever you have in front of you.

## Authoring a new prompt

1. Create `prompts/<name>.md` — the goal. Keep it tight: objective, context,
   in-scope / out-of-scope, success criteria, links.
2. Create `prompts/<name>.checklist.md` — the exhaustive task list. Group by
   area, use `- [ ]` checkboxes, ground every item in real files / commands,
   and end with explicit verification gates.
3. Link the checklist from the goal file.
4. Add the prompt to the table below.

### Rules these prompts inherit

Every prompt is bound by the [mission](../docs/mission.md) and by the repository's hard
rules (see [`AGENTS.md`](../AGENTS.md) and [`docs/roadmap.md`](../docs/roadmap.md)):

- **Rust-first, fastest, cheapest, nicest.** Optimize for the latest Rust;
  beat GitHub-hosted on speed/cost; make the output equal-or-better.

- **Fixture is the contract.** Never weaken `donbeave/velnor-actions-fixture`
  to work around a Velnor gap — fix Velnor.
- **`actions/runner` is the source of truth** for protocol behavior. Read it,
  don't guess: <https://github.com/actions/runner>.
- **Always the latest protocol path** (Results Service over timeline, V2 broker
  over classic, JIT over registration tokens).
- **Rust-first** for any new automation; prefer `velnor-tools` subcommands over
  new shell/Python.
- **Agents stop at the public fixture.** Never set
  `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true` or run the real ChainArgos / Jackin
  repositories; the operator owns that.

## Available prompts

Mission (read first): [`../docs/mission.md`](../docs/mission.md).

| Prompt | Goal | Checklist |
|--------|------|-----------|
| GitHub UI parity | [`github-ui-parity.md`](github-ui-parity.md) | [`github-ui-parity.checklist.md`](github-ui-parity.checklist.md) |
| Fixture proof completion | [`fixture-proof.md`](fixture-proof.md) | [`fixture-proof.checklist.md`](fixture-proof.checklist.md) |
| Target workflow coverage | [`target-workflow-coverage.md`](target-workflow-coverage.md) | [`target-workflow-coverage.checklist.md`](target-workflow-coverage.checklist.md) |
