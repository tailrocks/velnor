# Tri-agent campaign coordination ledger

Authority: operator directive 2026-08-24 — three concurrent OpenCode sessions
execute this campaign together and must conclude ownership here. This file is
campaign infrastructure, not plan content; `plans/goal-execution/README.md`
remains the binding execution contract.

## Protocol (binding on every session)

1. CLAIM BEFORE WRITE. Before any non-read-only work on a leaf (implementation,
   gates that mutate state, fixture dispatches, status flips), append a row to
   the live-claims table below and commit this file first. The commit IS the
   lock. Re-check `git status` + latest commits immediately before editing;
   another session's newer claim wins and you switch to read-only validation.
2. ONE WRITER PER LEAF. All other sessions may run read-only investigation,
   gate spot-checks, and adversarial review concurrently.
3. VALIDATION DUTY. Every landing gets independently verified + reviewed by a
   session that did not implement it. Findings go in
   `.velnor-compare/<date>-<leaf>-seam-review/feedback-to-<session>.md`.
4. STATUS FLIPS. Only after verifier AND reviewer pass, by the completing
   session, atomically (item file + TASKS.md + category index).
5. PUSH DISCIPLINE. Conventional Commits, `-s`, Co-authored-by trailer,
   rebase-once retry on rejection. Never force-push the campaign branch.
6. FIXTURE REPO (`tailrocks/velnor-actions-fixture`): PR-gated main; dispatches
   follow cancel-clean-dispatch-monitor hygiene; no HTML evidence ever.

## Session roles (as observed / declared)

| Session | Role | Active lane |
|---|---|---|
| A (orchestrator) | Orchestration, dependency graph, adversarial validation, status flips, fixture-repo writes | this ledger |
| B (fleet/implementer) | velnor-repo implementation bursts; Plan 039 fleet lane | release-ref ledger, snapshots |
| C (unconfirmed) | Unknown — DECLARE YOURSELF by appending a row + claiming a lane below | — |

## Leaf ownership log

| Time (+07) | Session | Leaf | Action |
|---|---|---|---|
| 2026-08-24 ~15:40 | B | 063/039 | docs reconciliation, policy gen, client, fixtures |
| 2026-08-24 ~16:20 | A | 064 closure | fix 7a63d72 + flip 017026e (validated B's 5ab7479) |
| 2026-08-24 evening | B | 064 hardening | b211326 (validated SAFE-HARDENING by A) |
| 2026-08-24 evening | B | 065 model modules | a9f017f, 34a6dad (7 modules + contracts) |
| 2026-08-24 evening | A | 065 convergence | ce98a27 adopted B's surface, repaired compile defects, gates 943/943 |
| 2026-08-24 night | A | 065 verify/review/flip | claimed |

## Next ready queue (dependency-ordered)

065 (in flight, A validating) -> 066 -> 068/074/075/076 -> ... per
`plans/goal-execution/README.md` execution graph. Plan 039 runs independently
(B lane) until live-mutation approval gates.

If you are session C: stop editing until you have appended your row here and
committed it. Uncoordinated writes caused three collisions on 2026-08-24
already; this ledger exists to end them.
