# GitHub-first dual-lane status

Current gate: **G0 — inventory and execution setup**

Overall status: **in progress; no gate passed**.

Last source snapshot: `2026-09-19T16:18:44Z`, Velnor revision
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.

Fleet inventory checkpoint: `2026-09-19T16:34:19Z` UTC. All 32 default
branches observed as `main`; 78 open PRs (75 ready, 3 drafts) across 12
repositories; 23 generator configs; 22 repositories with nonempty generated
workflow sets and 10 with empty/no workflow output. Full branch/config/PR
records are external under `G0/fleet/{inventory.md,configs.tsv,open-prs.tsv}`.
This is inventory evidence only. It does not prove workload completeness,
required-check success, migration, or a gate exit.

## Completed

- Read the full authoritative goal and applicable root rules.
- Confirmed the records worktree is clean at the initial Velnor revision.
- Confirmed the fixed manifest is represented exactly once in `fleet.json`.
- Verified RTK `0.49.0` and Codex CLI `0.155.0` locally.
- Verified root settings: `gpt-6-astra`/low orchestration and
  `gpt-5.6-luna`/max agents. This records agent turn context is Luna/max.
- Captured the external session/ruleset baseline link and SHA in `SPEC.md`.
- Recorded G0 distribution findings externally: publication remains blocked on
  G1; Homebrew contract and native-package work are coordinated.
- Recorded G0 bootstrap finding: candidate bootstrap has no Plan/unit/Velnor
  dependency; source edits wait for inventory seed/pin handoff.
- Recorded hosted/config progress: candidate commit
  `a38e459c7c88fa1c6e9a646a7513b2eecac292b8` selects hosted for automatic and
  default dispatch; generated final outputs and release-provider repair remain
  pending. PR954 is at `f16592ea165ced141bf0bb1c43466a95d7df8b2e`.
- Recorded failed-run operations externally: confirmed stale runs
  `35452270126` and `35445034780` were force-canceled; PR954 run
  `35454970877` has hosted work progressing while Velnor is queued; PR953 run
  `35453601367` has a cache-contract failure. No rules changed.
- The complete 32-row fleet branch snapshot is now populated in `fleet.json`;
  PR head/base revisions remain in the external TSV so the source tree does
  not duplicate a mutable 78-row ledger.
- Native-routing report found Tablerock/playground Apple workloads incorrectly
  routed to Ubuntu; Jackin macOS routing is available. Central shape-based
  scanner work is isolated and cannot roll out before G2.

These are documentation/setup facts only. They do not establish hosted CI,
package delivery, fleet migration, Mac operation, or any gate exit.

## Current evidence

| Item | Observation | Evidence/status |
| --- | --- | --- |
| Source revision | `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` | Observed locally |
| Velnor workflow config | generator pin `fdeed261bd2247a38db6922a7726cd45d3d6f31e`; schema 2 | Observed in source; revalidate live |
| Config SHA-256 | `9911f537d1621a265ec6037475d8d5f8bf16bf7d918440cb4dd9a0120f3acd54` | Observed locally |
| Generated state SHA-256 | `2643fad3e4943262ceeb47b888b451119998c2ea14114dd86b27cc29a2647646` | Observed locally |
| Current ruleset | `19573071`, `DCO`, `ci-required`, `Policy`, active | External baseline; no change claimed |
| Evidence root | `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/` | Policy; live records pending |
| Final ledger | Immutable artifact/evidence ref outside source | Pending |
| PR952 integration finding | Source `12cc87b629802c294da9840325cb21087c020df`; 1736 tests, fmt, and clippy pass; generated snapshot failure remains until regeneration | Observed; not a gate pass |
| PR954 current head | `f16592ea165ced141bf0bb1c43466a95d7df8b2e` | Observed; current run still pending/partial |
| PR953 cache result | Run `35453601367` failed cache contract | Observed; cache diagnosis reopened |
| G0-runtime | Read-only report persisted at external `G0/runtime/report.md`; actual Mac not operated before G3 | Completed investigation; G4/G5 pending |
| Runner protocol source | `actions/runner` revision `80bb1fb827fa44d489263061e71ef4adba7ad8cd` pinned for later work | Observed; no implementation here |
| G2 native package compile | Three required ARM64 macOS binaries compile/smoke at `abe9ad82`; nothing installed or published | Preliminary only; G2 remains pending |
| G2 product identity | Application/native asset/component identity contract is missing | Blocker for package acceptance; owned by G2-native-product |
| G1 scan integrity | Source task assigned to remove generated-output self-invalidation while preserving drift checks | Pending reviewed source change |

## Model and runtime evidence

| Scope | Model/effort | Verification |
| --- | --- | --- |
| Root session `01a0ba6f-f806-7d31-9abb-c828b3dc9e4e` | `gpt-6-astra` / `low` | External `session.json` and local turn logs |
| Records agent `01a0ba74-0f82-7800-9937-fb9d22de7e3d` | `gpt-5.6-luna` / `max` | Local turn logs |
| Configured default agents | `gpt-5.6-luna` / `max` | Local config |

Verified host metadata: macOS `27.0`, build `26A428`, `arm64`.
This is the orchestrator machine metadata, not G4/G5 actual Velnor pilot
evidence. OrbStack, Docker server, Velnor package identity, and job image are
unknown.

## Initial task state

| Task | State | Next action |
| --- | --- | --- |
| G0-inventory | Inventory checkpoint complete; follow-up checks pending | Refresh all 32 repos, branches, SHAs, PRs, checks, workflows, access |
| G0-bootstrap | Assigned; result pending | Verify clean/shallow bootstrap and pin/artifact identity |
| G0-distribution | Assigned; result pending | Revalidate product/runtime discovery and both channels |
| G0-fleet | 32-row inventory complete; workload matrix/review pending | Build workload/platform/category matrix |
| G0-runtime | Read-only report complete; actual Mac deferred until G3 | Reuse report for G4/G5 design |
| G0-records | In progress | Commit this canonical records set; retain unknowns |
| G0-checker | Assigned; result pending | Consume manifest schema; add deterministic fixtures/checks |
| G0-reviewer | Assigned; result pending | Independently review docs and raw evidence |
| G1-cache-semantics | Follow-up queued | Assign/verify Luna/max thread and run bounded cache tests |
| G1-hosted-config | Follow-up queued | Use hosted worktree; repair typed hosted-first config |
| G1-review952 | Follow-up queued | Refresh PRs #952–954 and dependencies |
| G1-run-operations | Follow-up queued | Own `G0/stale-runs.json`; trace failed child runs |
| G1-seed-pin | Follow-up queued | Reuse G0-inventory findings for generator seed/pin |
| G1-scan-integrity | Assigned; result pending | Repair scan/output integrity in `dual-lane-scan-integrity`; review by `g1_review952` |
| G2-native-packages | Follow-up queued | Verify hosted native package prerequisites |
| G2-native-product | Assigned; result pending | Define application/runtime component identity and authoritative package manifest |
| G2-distribution-review | Follow-up queued | Independently review APT/Homebrew product contract |
| g3-skills-adapter | Read-only audit queued | Inspect eight skills repositories; write only external G0 evidence |
| g3-native-routing | Read-only audit queued | Inspect Tablerock, playground, and Jackin native capabilities |
| g3-action-roles | Read-only audit queued | Inspect Jackin action and two role-image repositories |
| g3-rust-consumers | Read-only audit queued | Inspect eight named Rust/product consumers |
| g3-distribution-consumers | Read-only audit queued | Inspect six non-Velnor feeds/taps |

Agent IDs/worktrees for follow-up rows are intentionally unknown until assigned
and checked from actual turn metadata. No task result is inferred from assignment.

## Blockers and access gaps

1. Live inventory for 31 repositories and current PR/check/workflow evidence is
   not yet attached.
2. Generator/bootstrap/distribution/runtime investigations are pending.
3. Checker implementation and fixtures are pending.
4. No hosted recovery, package publication/install, fleet migration, actual
 Mac/OrbStack pilot, dual-provider run, or final audit is proven.
5. Required-check transition and App binding remain unchanged/unknown beyond
   the recorded Velnor ruleset snapshot.

Read-only early audits may continue before G2, but no `g3-*` task can claim a
G3 migration or authorize operational rollout. The current runtime audit also
reports a direct host-socket bypass, per-repository capacity defaults, and
incomplete cancellation targets; these are G4/G5 findings pending the required
G3 barrier.

## Exact next actions

1. Merge/verify the eight G0 task records into the external ledger without
   rewriting unknown rows as success.
2. Let the checker agent validate manifest count/uniqueness and schema shape.
3. Refresh live GitHub inventory and attach source/run/check evidence.
4. Reconcile bootstrap, distribution, fleet, runtime, and failed-run findings.
5. Start G1 only after G0 exit evidence is complete and independently reviewed.

## Checkpoint rule

At each gate, before/after publication or merge, and after any central fix,
record an external checkpoint with UTC time, source revisions, task states,
running jobs, and invalidated evidence. A changed default tip, PR head, release
commit, or generator/runtime pin invalidates affected evidence.
