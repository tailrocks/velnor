# Velnor Goal Prompts

Goal prompts drive large, well-scoped pieces of Velnor work with an autonomous
coding agent (Claude Code or Codex). Each prompt is two files:

| File | Role |
|------|------|
| `<name>.md` | **The goal** — what must be achieved, why, scope, definition of done, link to its checklist. |
| `<name>.checklist.md` | **The work** — an exhaustive, file-grounded checkbox list the agent works through to completion. |

## Source of truth: always defer to `docs/`

Every prompt defers to [`../docs/`](../docs/) for direction and correctness:
[mission](../docs/mission.md), [vision](../docs/vision.md),
[roadmap (the plan)](../docs/roadmap.md), and
[UI comparison](../docs/comparison.md). If a prompt and the docs disagree, the
docs win — fix the prompt. **When the vision/plan/roadmap changes, update
`docs/` and `../AGENTS.md` first, then reconcile the prompts** so none go stale.

## Run sequence

Run **one prompt per day** with `/goal`, in this order. Do not start the next
until the previous one's definition of done is met (its checklist fully checked
and gates green).

| # | Prompt | Owns | Depends on | Run with |
|---|--------|------|------------|----------|
| 1 | **Target workflow coverage** | Native Rust adapters + runtime features so target jobs *execute correctly*; Rust-first performance (parallelism, aggressive caches). | — (foundation) | `/goal prompts/target-workflow-coverage.md` |
| 2 | **GitHub UI parity** | Equal-or-better Checks UI experience: expandable steps, grouping, ANSI color, timestamps, synthetic steps, conclusions. | #1 (jobs must run before their output can be made faithful) | `/goal prompts/github-ui-parity.md` |
| 3 | **Fixture proof completion** | End-to-end green fixture proof via JIT/V2 + compare + timing/cache evidence; the Phase 0 gate. | #1 and #2 | `/goal prompts/fixture-proof.md` |

Copy-paste, in order:

```text
/goal prompts/target-workflow-coverage.md
```
```text
/goal prompts/github-ui-parity.md
```
```text
/goal prompts/fixture-proof.md
```

> Why this order: make jobs **run correctly** first (coverage), then make their
> **output faithful** (UI parity), then **prove it green end-to-end** with
> evidence (fixture proof). The fixture proof depends on both predecessors.

## Adding a new prompt

Keep it simple and keep the sequence authoritative. Every new prompt:

1. Create `prompts/<name>.md` (goal) and `prompts/<name>.checklist.md`
   (exhaustive checklist grounded in real files/commands, ending in verification
   gates).
2. In the goal file, add the standard header block: a **Mission** callout
   (→ `../docs/mission.md`), a **Direction** pointer (→ `../docs/`), and a
   **Run order** line stating its position and dependencies.
3. **Update the Run sequence table above**: insert the prompt at its correct
   position, set Owns / Depends on, and add its `/goal` line to the copy-paste
   block in order.
4. Confirm it does not contradict `docs/`; if it implies a direction change,
   update `docs/` and `../AGENTS.md` first.

## How `/goal` runs a prompt

The workflow is the same for Claude Code or Codex — both expose `/goal`, which
takes a goal file and executes it. The agent reads the goal, opens the linked
checklist, works top-to-bottom (backfilling anything the goal implies), runs the
verification gates, records evidence, and stops when everything is checked and
green. Nothing in a prompt assumes a specific agent.

## Inherited hard rules

Bound by the [mission](../docs/mission.md) and the repository hard rules
([`../AGENTS.md`](../AGENTS.md), [`../docs/roadmap.md`](../docs/roadmap.md)):

- **Rust-first, fastest, cheapest, nicest** — optimize for the latest Rust; beat
  GitHub-hosted on speed/cost; output equal-or-better.
- **Fixture is the contract** — fix Velnor, never weaken
  `donbeave/velnor-actions-fixture`.
- **`actions/runner` is the protocol source of truth** —
  <https://github.com/actions/runner>.
- **Latest protocol path only** — Results Service over timeline, V2 over
  classic, JIT over registration tokens.
- **Rust-first automation** — prefer `velnor-tools` subcommands over new
  shell/Python.
- **Agents stop at the public fixture** — never run the real ChainArgos / Jackin
  repos or set `VELNOR_REAL_TARGET_MANUAL_CONFIRM=true`.
