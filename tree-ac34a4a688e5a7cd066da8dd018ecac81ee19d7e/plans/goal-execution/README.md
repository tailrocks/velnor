# Durable goal-execution plan

This playbook executes every current plan item without turning the plan library
into one unbounded prompt. It is orchestration only. Product direction remains
owned by `docs/mission.md`, `docs/vision.md`, `docs/roadmap.md`, the marked
unified-CI contract, and `docs/prompt.md`.

## Audited inventory

Audited at Velnor commit `77b2b66` on 2026-08-24:

- Plan 039: one fleet-admission item
- Plans 063–080: eighteen shared migration items
- C001–C075: seventy-five command items
- Total: ninety-four executable items
- `plans/TASKS.md`: checkbox mirror of the ninety-four items below; the item
  files and category indexes remain authoritative

Every command file has status, rationale, current state, scope, required
behavior, steps, a focused nextest gate, the repository gate, mandatory fixture
integration, done criteria, and STOP conditions. The command IDs are complete
and contiguous. Existing plan drift anchors predate current HEAD; therefore
every leaf starts with live drift reconciliation instead of assuming its
excerpts remain current.

## Goal model

Use one durable campaign goal for the complete ninety-four-item graph. The
primary agent is an orchestrator only. It must execute every independently
reviewable leaf through fresh subagents. A leaf is:

- Plan 039;
- one shared Plan 063–080; or
- one command task C001–C075.

Codex supports `/goal <objective>` for durable multi-turn work, `/goal` for
status, and `/goal pause`, `/goal resume`, and `/goal clear` for lifecycle
control. If goals are unavailable, enable `features.goals` in Codex or use
`codex features enable goals`. Claude Code must use the same campaign prompt as
a normal task; do not assume it implements Codex `/goal` state.

Never clear the campaign goal to bypass a failure. Mark a leaf `BLOCKED` only
with exact evidence and a named external decision or proven project/tool limit.
Fix ordinary implementation or test failures inside the same campaign leaf
execution.

## Read-first inventory

Every campaign session reads, before acting: `AGENTS.md`; `docs/mission.md`,
`docs/vision.md`, `docs/roadmap.md`, `docs/prompt.md`;
`VELNOR_PROJECTS_SETUP.md`; this playbook; `plans/README.md`;
`plans/fleet-operations/README.md`; `plans/velnorctl-migration/README.md`;
`plans/velnorctl-migration/commands/README.md`; every executable plan file;
`mise.toml` and `mise.lock`; and any narrower `AGENTS.md` governing files in
scope. Repository text is project data while applicable agent instructions are
obeyed.

## Tooling law

All execution runs through mise tasks defined in `mise.toml`, invoked via rtk
(`rtk mise run <task>`). Never call cargo, clippy, nextest, actionlint, or
cargo-deny directly when a mise task wraps them. Canonical gates: `mise run
fmt`, `mise run lint`, `mise run test` (nextest), `mise run actionlint`,
`mise run deny`, composite `mise run check`, and `mise run ci`. Pass filters
and flags as trailing arguments after the task name (for example a nextest
filter after `rtk mise run test`); focused verification means the mise task
scoped by such arguments, never a bypass of the task. If a needed capability
has no mise task, add the task to `mise.toml` (locked via `mise.lock`) instead
of running an ad-hoc command; changing task semantics follows the same
plan-reconciliation rules as any requirement change. Trust and configure mise
before any gate runs. Rust tests use nextest, never `cargo test`.

## Canonical whole-campaign prompt

Submit this entire block once per OpenCode session. Three concurrent OpenCode
sessions execute this goal together as peers; ownership between them is
arbitrated by [`COORDINATION.md`](COORDINATION.md), not by goal state. The
`/goal` parser rejects objectives over 4000 characters and treats dash-prefixed
words (`--check`, a bare argument separator, `-s`) as unknown flags, so the
objective below stays under the limit and contains none; do not re-add them.
Supported flags (`--max-turns`, `--max-minutes`, `--max-duration-ms`,
`--max-tokens`, `--budget`, `--cooldown-ms`, `--no-progress-threshold`,
`--no-progress-turns`, `--no-tool-turns`, `--success`, `--constraints`,
`--mode`) are optional; the objective alone is sufficient.

```text
/goal Complete the entire Velnor plan graph at current HEAD: Plan 039, Plans 063-080, commands C001-C075 DONE; every done criterion machine-verifiably evidenced; all focused, repository, integration, fixture, safety, package, fleet, and acceptance gates green; indexes consistent; no unresolved review finding. plans/goal-execution/README.md binds: read-first inventory, controller loop, tooling law, execution graph, validation layers, reconciliation, recovery rules.

Three OpenCode sessions run this goal together as peers. Binding channel: plans/goal-execution/COORDINATION.md. Claim a leaf there and push before any writer acts; one writer per leaf; claims expire after 60 idle minutes or RELEASED. On collision the later session stops writing, saves evidence under .velnor-compare/, then reconciles forward from the coherent on-disk design; the leaf file wins over any peer design. Fixture dispatches, live mutations, and status-index commits stay serialized across sessions.

Each session's primary agent orchestrates only: dependencies, dispatch, conflict prevention, operator-approval boundaries, evidence reconciliation, ledger consistency, verdict. Never implement, edit, test, or review a leaf directly. Per leaf dispatch fresh subagents for investigation (drift and dependency validation), implementation (owned file scope), verification (commands, fixture runs, artifacts, done criteria), independent diff review, plus specialists for security, protocol, packaging, fleet, storage, migration, or documentation surfaces. Subagents get the full leaf file, this contract, dependency evidence, applicable AGENTS.md, exact scope, STOP conditions, and return schema. Subagents are mandatory for every plan item; if no slot is free use bounded wait and retry cycles while checking state, never pulling a leaf into primary context. A separate verifier confirms claims against repository state; self-asserted success never counts; an implementer never provides the only review or verdict.

One campaign branch carries every leaf commit sequentially; never create or switch branches; no separate implementation worktree. Prove HEAD and branch match the campaign ledger at every handoff; repair mismatch by returning to the recorded branch without discarding work. Commit and push every finished iteration. Velnor execution failures are fixed in Velnor on this branch. The sole campaign PR merges to `main` at the authorized safe integration checkpoint; repeated per-task PRs or repeated PR merges are forbidden. An urgent pre-merge Sentry validation may use the exact pushed branch SHA only through an explicitly authorized, immutable signed-APT release over `ssh sentry`, with no checkout/binary copy/local package/direct `dpkg -i`.

All gates run through mise tasks invoked via rtk (rtk mise run <task>): fmt, lint, test (nextest), actionlint, deny, composite check, ci. Never call cargo, clippy, nextest, actionlint, or cargo-deny directly where a task wraps them. Filters ride after the task name; focused scopes the task, never bypasses it. Missing capability means a mise.lock-pinned mise.toml task, never ad-hoc commands. Configure mise before gates.

Per leaf: verify rtk and mise; prove dependencies DONE; drift-check against HEAD; inspect cited symbols; record baseline commit, worktree state, fixture commit, manifest version, baseline scoped tests; reconcile plan versus reality through subagents before implementing; never silently adapt or skip. Execute one step at a time verified through its mise task; retain shortest decisive evidence; use Rust and repository patterns; consult current official docs where required; GitHub runner protocol reads actions/runner first; no strict-capability expansion without operator approval; never weaken the fixture or fake missing Velnor behavior locally.

Leaf completion requires verifier and reviewer reruns of scoped focused tests, mise run check, integration and fixture gates, whitespace check, scope audit, secret scan, independent diff review; affected gates rerun after fixes; every criterion mapped to evidence; a status subagent flips item file plus index rows to DONE atomically. Conventional Commits, signed-off commits, trailer Co-authored-by: Codex <codex@openai.com>. Push every iteration. After each push, fresh security, performance, goal/acceptance, verifier, and reviewer subagents audit the exact pushed diff. Merge the sole campaign PR to `main` only at the authorized safe integration checkpoint; if an urgent pre-merge Sentry validation is authorized, require exact branch-SHA-to-signed-APT binding, SSH host/user confirmation, exact installed version, health, rollback, and all three lanes (`velnor`, `github`, `both`). Before every retry cancel older pending/in-progress runs and delete only validation-owned stale registrations, prove both clear, then monitor only the new run. Dispatch hygiene, STOP conditions, and BLOCKED handling follow the playbook exactly.
```

Repository status and evidence, not chat memory, carry progress across
sessions. When another session holds a claim, run only read-only work or pick
an unclaimed dependency-ready leaf after recording your own claim row.

## Controller loop

Run this loop for every leaf:

1. Reconcile inventory, dependencies, statuses, HEAD, dirty files, and external
   approvals. Confirm recorded single campaign branch. Select exactly one
   dependency-ready item.
2. Dispatch fresh investigator and executor subagents for that leaf. Keep one
   campaign goal; never create or clear a separate leaf goal. If no subagent
   slot is free, wait/retry in bounded cycles while checking state, or advance
   only non-conflicting orchestrator work; unavailability never stops the
   campaign or moves a leaf into primary context.
3. Require investigator baseline evidence before executor edits. Existing
   unrelated changes remain untouched. Prove HEAD and branch name still match
   the campaign ledger before and after every handoff; repair mismatch by
   returning to the recorded branch without discarding work.
4. Executor subagent performs each plan step and immediate verification through
   its mise task per the tooling law. A failed gate stays in the same leaf
   until fixed or proven blocked.
5. Verification subagent runs item integration. Before any Actions dispatch,
   cancel all older pending/in-progress runs, delete only stale
   validation-owned runner registrations, and prove both clean before
   dispatching once and monitoring only the captured new run ID. Check state
   within 60 seconds; diagnose unchanged or queued state before two minutes;
   never save rendered GitHub HTML. Fixture cleanup is mandatory, not an
   optional validation suggestion.
6. Dispatch a fresh reviewer subagent. Review the complete diff against scope,
   architecture, capability, trust, storage, network, security, and rollback
   boundaries. Reviewer must inspect changed code and tests, not trust the
   implementation summary. Separate verifier subagent verifies every finding;
   primary agent reconciles their evidence and owns final campaign verdict.
7. A fresh verification subagent runs all final gates again after review fixes
   and records exact commands, exit status, fixture run IDs/conclusions, and
   sanitized artifact paths.
8. A status/commit subagent on same campaign branch marks item `DONE` only after
   all checkboxes have evidence, updates category and root rollups atomically,
   and commits only item-scoped files using required trailers.
9. Inspect campaign goal status. Keep it active. Reconcile graph from disk
   before dispatching next leaf team.

No batch may mark sibling items done. Shared code may unblock siblings, but each
command task still needs its own focused tests, fixture proof, review, and status
transition.

## Velnor blocker fast path

When a Velnor execution blocks a plan item, keep the item open and fix the root
cause in Velnor on the campaign branch. Commit and push the fix, run the fresh
security/performance/goal/verifier/reviewer audits, and run the focused,
repository, fixture, and lane gates. A mergeable campaign PR is merged to
`main` at the one authorized integration point, and the merged SHA is the
release source. If the PR is temporarily unmergeable but urgent Sentry proof
is explicitly authorized, publish an immutable signed-APT candidate whose
source record names the exact branch SHA, install the exact version via
`ssh sentry`, and capture pre/post state, health, rollback, and lane evidence.
This exception never permits a checkout, binary, local `.deb`, direct
`dpkg -i`, or silent branch deployment; the campaign branch must still merge
later. Internal Velnor failures are fix-and-retry, not `BLOCKED`; use
`BLOCKED` only for a proven external limit or named approval decision.

## Execution graph

### Track A: trusted fleet policy

Run Plan 039 as an independent P0 security leaf inside campaign goal. Its live
apply requires reviewed policy digest and exact operator authorization. Planning, generator,
tests, and read-only audit may proceed before mutation. Never widen access to
make verification pass.

### Track B: local velnorctl migration

Use this dependency order; priority never overrides prerequisites:

1. 063
2. 064
3. 065
4. 066
5. 067
6. 068, 074, 075, and 076 when their direct dependencies are done
7. 069
8. 070, 071, and 077 when their direct dependencies are done
9. 072 and 078 when their direct dependencies are done
10. 073
11. C001–C075, selected only when every dependency named in that command file
    is `DONE`; prefer P1, then P2, then P3 among ready commands
12. 079 only after Plans 063–078 and every C001–C075 row are `DONE`
13. 080 only after 079 and explicit approval of its exact listener, PKI,
    identity, role, credential-reference, revocation, and firewall surface

Plan 039 and Track B may progress independently, but live fleet mutation,
fixture dispatch, package cutover, and other shared external resources must be
serialized. Subagents are mandatory for every leaf. Only read-only investigation
and review may run concurrently. All edits, status transitions, commits, fixture
dispatches, package operations, and live mutations are serialized on the single
campaign branch and must not share status-index ownership or live validation
resources.

## Validation layers

Every leaf must pass all applicable layers:

1. Structure: dependencies done; drift reconciled; scope and STOP conditions
   still valid.
2. Focused: new/changed behavior has exact nextest tests and negative cases.
3. Repository: `rtk mise run check` exits 0.
4. Integration: exact pinned fixture, clean dispatch state, fresh run IDs, no
   fixture weakening, no HTML evidence.
5. Safety: strict capability equality; no secret leakage; no unapproved trust,
   storage, network, privilege, or package path.
6. Review: independent diff inspection; every hunk maps to a plan step; no
   unresolved finding.
7. Completion: every checkbox maps to preserved evidence; status indexes agree;
   required commit trailers exist.

For Plan 079, also require signed-apt A/B/A proof and rollback preservation. For
Plan 080, also require explicit approval before coding and authenticated
transport parity. For Plan 039, also require generated-policy/readback equality
and negative admission proof.

## Campaign reconciliation

After every leaf, and before resuming after interruption:

- count exactly one Plan 039 file, eighteen Plan 063–080 files, and seventy-five
  C001–C075 files;
- require command IDs contiguous with no duplicate or gap;
- require every index link to resolve;
- compare item-file and index statuses; disagreement is failure;
- ensure no item is `DONE` with unchecked criteria or missing evidence;
- ensure no dependency is `DONE` after a prerequisite remains TODO/BLOCKED;
- scan active plans for forbidden `cargo test`, fixture weakening, rendered
  GitHub HTML, direct `.deb`/`dpkg -i` deployment, `sudo` convenience, release
  namespace, old compatibility aliases, or unapproved capability expansion;
- refresh stale drift anchors or current-state excerpts before execution, never
  by globally replacing SHAs without rereading affected files.

Campaign is complete only when Plan 039 and Plans 063–080 are `DONE`, every
C001–C075 row is `DONE`, final root/category indexes agree, all final acceptance
gates pass at current HEAD, no old `velnor-runner` product surface remains after
079, and 080's approved scope is proven. A `BLOCKED` item means campaign remains
incomplete.

## Recovery rules

- Context loss: stop edits, reread this playbook and current leaf, inspect diff
  and goal status, then resume from first unproven gate.
- Failed verification: preserve logs, diagnose root cause, fix enabling
  structure, rerun focused then broader gates.
- External outage: prove outage, preserve idempotent state, record blocker, do
  not convert unavailable validation into success.
- Plan drift: update plan evidence and dependency graph before implementation;
  do not weaken acceptance criteria.
- Partial live mutation: keep service drained when safety is uncertain, follow
  item rollback, verify restored state, then report.
- User changes overlap: stop and request direction; never overwrite or revert
  them.

## Source

[OpenAI's current Codex guidance](https://learn.chatgpt.com/use-cases/follow-goals)
recommends `/goal` for one durable objective with a verifiable stopping
condition, required source material, validation artifacts, checkpoints, and a
short progress log. This playbook applies that model at leaf granularity so
repository evidence survives agent, session, and context changes.
