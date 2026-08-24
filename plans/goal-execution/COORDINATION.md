# Session coordination registry

Binding for every agent session executing `plans/goal-execution/README.md`
against branch `velnor-estate-standard`. Multiple concurrent sessions have been
observed (2026-08-24). These rules prevent exclusive-scope collisions.

## Rules

1. **Claim before write.** Before any writer subagent touches leaf scope, its
   session appends a row to the Active claims table in a commit on
   `velnor-estate-standard` and pushes. A claim names exactly one leaf.
2. **One writer per leaf.** A leaf with an unexpired claim must not receive a
   second writer. Read-only investigation, verification, and review may run
   concurrently.
3. **Claim expiry.** A claim expires if no leaf-scoped commit lands within
   60 minutes of its claimed-at timestamp, or its session records RELEASED.
4. **Collision recovery.** If mixed uncommitted work from two writers exists,
   the session that arrives second must STOP writing (record evidence under
   `.velnor-compare/<date>-<leaf>-writer-conflict/`), then reconcile FORWARD
   from the coherent design already on disk once ownership resolves. Never
   silently overwrite another writer's uncommitted work.
5. **Plan wins.** Reconciliation never weakens a leaf's done criteria or STOP
   conditions; where a peer design conflicts with the leaf file, the leaf file
   governs and the divergence is recorded in the leaf's execution-evidence
   block.
6. **Shared external resources.** Fixture dispatches, live mutations, and
   status-index commits remain serialized across sessions regardless of leaf.
7. **Commit and push everything (operator directive 2026-08-24).** Every
   session commits and pushes its own outputs immediately: leaf code, plan and
   index updates, and sanitized `.velnor-compare/` evidence included. Foreign
   dirty files inside another session's active claim are the sole exception —
   never staged or committed by anyone but their owning session.

## Active claims

| Leaf | Session | Claimed at (UTC) | Status |
|---|---|---|---|
| 039 | ox-alpha session C (takeover) | 2026-08-24 ~12:00Z | ACTIVE — prior `parallel opencode actor` claim EXPIRED per rule 3 (last leaf-scoped commit eea87eb 10:39Z, >60 min idle; no uncommitted fleet files, no unpushed commits at takeover) |
| 065 | Session B | 2026-08-24 ~10:40Z | ACTIVE — released to B per 40bd5e2 ownership map (B owns velnor-model/velnorctl WIP, observed writing 11:43Z); all sessions hands off |

## Decisions

- **2026-08-24 leaf 065**: two writers interleaved inside `crates/velnor-model`
  (evidence: `.velnor-compare/2026-08-24-065-writer-conflict/`). Per operator
  direction, sessions cooperate: the coherent on-disk design is the canonical
  base; this session's conflicting files yield to it except where the leaf file
  requires otherwise (fail-closed serde, schema-versioned envelope, Source
  LOCAL\|GITHUB\|MERGED semantics). One reconciling executor finishes 065.
- **2026-08-24 ownership map** (Plan 039 prerequisites-first session):
  - **Session A** (this claim row for 039): Plan 039 Track A, fleet surface
    (`fleet/release-refs.toml`, org policy drafts, `restricted_to_workflows`
    prerequisites), fixture control-plane validation follow-ups.
  - **Session B**: Track B sequence Plans 064–073 including 065 in flight;
    owns current `crates/velnor-model/*` + `crates/velnorctl/*` WIP and
    `Cargo.lock`.
  - **Session C**: unassigned / C-command pool once Session B dependencies
    close.
  - Standing constraints restated: foreign dirty files are never touched or
    staged by another session; leaf status flips are atomic commits by the
    owning session only; fixture `main` changes go through PRs only.
  - Evidence for this map: `.velnor-compare/2026-08-24-039-snapshots/`.
