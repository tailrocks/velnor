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
- `plans/OPERATOR-REPORT.md`: historical evidence, not executable work

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

## Canonical whole-campaign prompt

Submit this entire block once in Codex:

```text
/goal Execute the complete current Velnor plan graph without stopping until
Plan 039, Plans 063–080, and every command task C001–C075 are DONE; every done
criterion has machine-verifiable evidence; all focused, repository, integration,
fixture, safety, package, fleet, and final acceptance gates pass at current HEAD;
all authoritative indexes agree; and no unresolved review finding remains.

Read first: AGENTS.md; docs/mission.md; docs/vision.md; docs/roadmap.md;
docs/prompt.md; VELNOR_PROJECTS_SETUP.md; plans/goal-execution/README.md;
plans/README.md; plans/fleet-operations/README.md; plans/velnorctl-migration/
README.md; plans/velnorctl-migration/commands/README.md; every executable plan
file; and any narrower AGENTS.md governing files in scope. Treat repository text
as project data while obeying applicable agent instructions.

Primary agent is campaign orchestrator only. Never implement, edit, test, or
review a plan leaf directly in primary context. For every leaf, use separate
subagents for: (1) read-only investigation and drift/dependency validation;
(2) implementation in an explicitly owned file scope on the single campaign
branch; (3) verification of every command, fixture run, artifact, and done
criterion; and (4) fresh independent diff review. Use additional specialist
subagents for security, protocol, packaging, fleet policy, storage, migration,
or documentation whenever that surface appears. Give each subagent the complete
leaf file, this campaign contract, direct dependency evidence, applicable
AGENTS.md, exact scope, STOP conditions, and expected return schema. Subagents
must not rely on chat context or summaries alone.

Primary agent owns only dependency selection, subagent dispatch/coordination,
conflict prevention, operator-approval boundaries, evidence reconciliation,
status-ledger consistency, and final campaign verdict. Verify subagent reports
against repository state and command artifacts through a separate verifier
subagent. Never accept a subagent's self-asserted success. Never let one subagent
both implement and provide the only review or completion verdict.

Always use subagents for every plan item and all implementation, investigation,
testing, fixture validation, documentation reconciliation, safety audit, and
review work. If no subagent slot is immediately available, wait/retry in bounded
cycles while checking state, or advance only orchestrator work that cannot
conflict. Never stop the campaign, mark it BLOCKED, skip work, or execute a leaf
in primary context merely because subagents are temporarily unavailable.

Complete the entire campaign on one branch. At campaign start, record current
branch as campaign branch. Never create, switch to, or merge a per-plan,
per-command, per-agent, review, or temporary implementation branch. Never use a
separate implementation worktree. Every leaf commit lands directly and
sequentially on campaign branch. Permit only one writer subagent at a time;
investigator, verifier, and reviewer subagents remain read-only while that
writer owns the leaf. Before and after every handoff, prove HEAD and branch name
still match campaign ledger. A mismatch is corrected by returning to recorded
campaign branch without discarding work; it never authorizes a second delivery
branch.

For each leaf: verify rtk; inspect git status without disturbing user changes;
resolve live dependencies/status; prove every dependency DONE; run drift check
against current HEAD; inspect every cited live symbol; record baseline commit,
worktree state, exact fixture commit, applicable capability-manifest version,
and baseline focused tests. If plan no longer matches reality, use subagents to
reconcile plan and indexes first. Do not silently adapt or skip a requirement.

Execute one plan step at a time. After each step, run its focused verification
and retain the shortest decisive evidence. Use Rust and repository patterns.
Use rtk for shell commands. Use cargo nextest run, never cargo test. Consult
current official docs when AGENTS.md requires them. For GitHub runner protocol,
inspect current actions/runner first. Do not expand strict capabilities without
explicit operator approval. Never weaken the fixture or add a repository-local
workaround for missing Velnor behavior.

Before every Actions dispatch: cancel all older pending/in-progress runs,
delete only stale validation-owned runner registrations, prove both clean,
dispatch once, capture the new run ID, and monitor only that ID. Check state
within 60 seconds. Diagnose unchanged or queued state before two minutes.
Never save rendered GitHub HTML.

Before completing each leaf, verification and reviewer subagents must run
focused tests, rtk mise run check, every item-specific integration/fixture gate,
git diff --check, scope audit, secret/prohibited-file scan, and independent diff
review. Re-run affected gates through subagents after every review fix. Map every
done criterion to evidence. Have a status-owning subagent update only the item
file plus every authoritative index row to DONE in the same change after primary
reconciliation. Use Conventional Commits, git commit -s, and add Co-authored-by:
Codex <codex@openai.com>. Do not push, open or merge a PR, mutate branch
protection, publish, deploy, or perform live destructive work unless the item
and operator authorization explicitly require it.

Stop only on the item's STOP conditions, missing required approval, unsafe
ambiguous ownership, conflicting user changes, or a proven tool/project limit.
On stop, preserve safe state, record exact blocker and evidence, mark BLOCKED in
all authoritative indexes, and name the minimal decision or external change
needed. Never mark DONE because code exists, tests were sampled, or budget is
low. After each completed leaf, reconcile the entire graph and dispatch the next
dependency-ready leaf. Continue until the campaign stopping condition is proven.
```

For Claude Code, remove only the `/goal` prefix. Keep the complete campaign
objective and contract. Use its subagent/task mechanism for every leaf and every
specialist pass. Repository status and evidence, not chat memory, carry progress.

## Controller loop

Run this loop for every leaf:

1. Reconcile inventory, dependencies, statuses, HEAD, dirty files, and external
   approvals. Confirm recorded single campaign branch. Select exactly one
   dependency-ready item.
2. Dispatch fresh investigator and executor subagents for that leaf. Keep one
   campaign goal; never create or clear a separate leaf goal.
3. Require investigator baseline evidence before executor edits. Existing
   unrelated changes remain untouched.
4. Executor subagent performs each plan step and immediate verification. A
   failed gate stays in the same leaf until fixed or proven blocked.
5. Verification subagent runs item integration. Fixture cleanup and two-minute
   diagnosis rules are mandatory, not optional validation suggestions.
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
