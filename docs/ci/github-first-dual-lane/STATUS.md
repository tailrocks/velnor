# GitHub-first dual-lane status

Current gate: **G0 — inventory and execution setup**

Overall status: **in progress; no gate passed**.

Last source snapshot: `2026-09-19T16:18:44Z`, Velnor revision
`abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.

Fleet baseline checkpoint: `2026-09-19T16:34:19Z` UTC. All 32 default
branches were observed as `main`; the baseline had 78 open PR rows (75 ready,
3 drafts) across 12 repositories, 23 generator configs, 22 repositories with
nonempty generated workflow sets, and 10 with empty/no workflow output. A later
current reconciliation at `2026-09-19T17:05:57Z` has 77 current PR rows after
`jackin-project/homebrew-tap#494` merged and `tailrocks/velnor#953` changed
head. Baseline records remain under external
`G0/fleet/{inventory.md,configs.tsv,open-prs.tsv}`; current records are
`G0/fleet/{main-revisions.tsv,pr-checks.tsv,requirements.json,context-pages.json,main-verification.json,handoff.json}`.
This is inventory evidence only. It does not prove workload completeness,
required-check success, migration, or a gate exit.

## Completed

- Read the full authoritative goal and applicable root rules.
- Confirmed the records worktree is clean at the initial Velnor revision.
- Confirmed the fixed manifest is represented exactly once in `fleet.json`.
- Verified RTK `0.49.0` and Codex CLI `0.155.0` locally.
- Verified root settings: `gpt-6-astra`/low orchestration and
  `gpt-5.6-luna`/max agents. This records agent turn context is Luna/max.
- Captured the stable external session/ruleset location in `SPEC.md`; the
  mutable registry hash is intentionally checkpoint-owned, not source-pinned.
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
- The complete 32-row fleet baseline branch snapshot is populated in
  `fleet.json`; current PR/head revisions remain in the external refresh TSVs
  so the source tree does not duplicate mutable baseline/current ledgers.
- The current read-only workload projection is external at
  `G0/workload-matrix.json`: all 32 rows align to the latest main-revision
  snapshot, observed responsibilities are separated from unsupported/native/
  trust obligations, and missing or unexecuted work remains a blocker. This is
  not G3 generated coverage.
- Native-routing report found Tablerock/playground Apple workloads incorrectly
  routed to Ubuntu; Jackin macOS routing is available. Central shape-based
  scanner work is isolated and cannot roll out before G2.
- Early read-only category audits are now durable externally: skills at
  `G0/skills-adapter/report.md`, action/roles at `G0/action-roles/findings.md`,
  Rust consumers plus inventory at `G0/rust-consumers/{report.md,inventory.tsv}`,
  distribution consumers plus inventory at
  `G0/distribution-consumers/{report.md,consumer-inventory.json}`, and the
  independent distribution review at `G0/distribution-review/report.md`.
  They record blockers and missing proof; none is a gate pass.
- G1 seed/pin chronology is external at `G1/reviews/seed-pin.md`: 1858 tests
  belong to exact PR head `a5c1c0bd` before regeneration, while 1736 tests,
  fmt, and clippy belong to integrated source `12cc87b`; these counts are not
  combined. `G1/reviews/bootstrap-hosted.md` is a checkpoint only.
- G1 runtime-product audit is external at
  `G1/runtime-product-audit/{runtime-product-audit.json,PROMOTION.md}`:
  published old pin `fdeed261` resolves to closure `81ba31f`/release
  `391347842`; current main `abe9ad82` publishes distinct closure `63cea86`;
  local `12cc87b` needs unpublished closure `1cbf31a`. Promotion order is
  source admission, merged-main publication, immutable verification, then pin
  adoption/regeneration. This is not a G1 pass.

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
| Session registry | Stable external `session.json` path; current hash recorded per checkpoint | Actual Luna/max worker threads, evidence paths, graph amendments, and no gate pass |
| Final ledger | Immutable artifact/evidence ref outside source | Pending |
| PR952 source/integration chronology | Exact PR head `a5c1c0bd5c92c4c52d58ccb21042b1b2c0b08637` had 1858 tests before regeneration; integrated source `12cc87b629802c294da9840325cb21087c020df` has 1736 tests, fmt, and clippy pass; generated snapshot failure remains until regeneration | Separate observations; not a gate pass |
| PR954 current head | `f16592ea165ced141bf0bb1c43466a95d7df8b2e` | Observed; current run still pending/partial |
| PR953 cache result | Run `35453601367` failed cache contract | Observed; cache diagnosis reopened |
| G0-runtime | Read-only report persisted at external `G0/runtime/report.md`; actual Mac not operated before G3 | Completed investigation; G4/G5 pending |
| Runner protocol source | `actions/runner` revision `80bb1fb827fa44d489263061e71ef4adba7ad8cd` pinned for later work | Observed; no implementation here |
| G2 native package compile | Three required ARM64 macOS binaries compile/smoke at `abe9ad82`; nothing installed or published | Preliminary only; G2 remains pending |
| G2 product identity | Application/native asset/component identity contract is missing | Blocker for package acceptance; owned by G2-native-product |
| G1 scan integrity | Candidate `6409a086` from parent `12cc87b` reports 1740 source tests excluding expected stale snapshot plus fmt/clippy | External report; exact G1-review952 approval pending |
| Early category audits | Skills, action/roles, Rust consumers, distribution consumers, and independent distribution review reports | Read-only evidence written; scanner/publication/native proof gaps remain |
| G1 runtime-product audit | Old pin/release, current-main distinction, and candidate closure/promotion sequence | External evidence written; no candidate publication or pin adoption |
| G0 workload matrix | 32 unique rows aligned to current main revisions; observed duties, native/unsupported/trust obligations, and missing execution retained | External `G0/workload-matrix.json`; inventory projection only |
| G0 checker review | Initial b3b6 unit hygiene passed, but semantic and G2 hostile reviews rejected false-green paths | v2 architecture required; no checker completion or gate proof |
| G0 acceptance matrix | Exact 32/no-extras scope, live default/PR/workflow/run/provider/workload/dependency/source/digest/child evidence, and fail-closed stale/missing/manual-only rules | Canonical [`SPEC.md` matrix](./SPEC.md#exact-g0-acceptance-matrix), external `session.json`, checker and independent-review reports; unknown/incomplete |
| G0 baseline-versus-current | Independent findings baseline is `abe9ad82`; integration `12cc87b6` and later candidate SHAs remain separate | External `session.json`; no baseline result promoted to current or gate evidence |
| G0 helper boundary | `audit_ci` is auxiliary; `lane_compare` pair/census/step/artifact-log behavior is diagnostic and has recorded false-green paths | External checker review and lane assignment; complete paginated artifact/log proof pending |
| G0 review checkpoint | Scan `3c46b9e`, checker `017c92e`, preview `5f2b0d3`, and APT `8c19fab` reviews remain rejected/blocked; Homebrew `c772971` is conditional source-contract only | External exact reports; no approval, publication, or gate claim |
| Baseline test chronology | Clean `abe9ad82` `velnor-tools` test reported 207 passed in 11.42s; later 206-pass/one-failure output was contaminated by concurrent parent edits | External `session.json`; preserve both observations, do not call the full baseline green or assume flakiness |

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
| G0-records | Docs commit complete; external registry amended | Preserve unknowns; await independent review |
| G0-checker | Initial commit rejected semantically; v2 architecture/candidate scan pending | Bind canonical scope/live evidence and rerun hostile fixtures; no false completion |
| G0-reviewer | Independent review rejected initial checker/docs state | Review exact v2 commit and refreshed external evidence |
| G1-cache-semantics | Jobs/provider adversarial review rejected initial checker paths | Retain external findings; review v2 candidate without rewriting old evidence |
| G1-hosted-config | In progress; candidate remains unverified | Thread `01a0ba77-5222-7e63-97fa-553849b96d7b`; recheck clean exact commit/output |
| G1-review952 | Scan-integrity candidate `6409a086` approved for review; final decision pending | Thread `01a0ba77-dd88-7ac2-9fb7-118f3c09d1af`; review #952–954 without combining test counts |
| G1-run-operations | In progress; stale-runs evidence updated | Thread `01a0ba7a-9d5d-7291-a2f5-357ff78dba5e`; trace child outcomes |
| G1-runtime-product-audit | Evidence written; next publish verification pending | Same operations thread; old pin verified, candidate remains unpublished |
| G1-seed-pin | Source review written; pin adoption pending | Thread `01a0ba72-3925-7141-b1f7-5529a5cf6c98`; clean regeneration remains required |
| G1-scan-integrity | Assigned; result pending | Repair scan/output integrity in `dual-lane-scan-integrity`; review by `g1_review952` |
| G2-native-packages | Compile/smoke observed; now owns product/manifest contract | Thread `01a0ba7a-328e-7282-943e-5b54c2ac209d`; no install/publication claim |
| G2-homebrew-contract | Assigned; producer contract coordination pending | Thread `01a0ba80-8408-7380-8ac2-b743eb4494a5`; coordinate with native packages |
| G2-native-product | Source implementation assigned to native-packages worker | `/root/g2_native_packages`, thread `01a0ba7a-328e-7282-943e-5b54c2ac209d`, worktree `dual-lane-native-product`; define application/runtime component identity and authoritative package manifest |
| G2-preview-publication | Planned; blocked until G1 | `/root/g1_run_operations`, thread `01a0ba7a-9d5d-7291-a2f5-357ff78dba5e`, worktree `dual-lane-preview-publication`; reviewer `/root/g2_distribution_review` |
| G2-distribution-review | Typed review pending; initial checker hostile G2 suite failed all nine mutations on old b3b6 | Thread `01a0ba81-1af6-7f11-9f24-3ff115b8f314`; no publication approval |
| g3-skills-adapter | Read-only evidence written; central scanner fix required | Thread `01a0ba7b-0152-7a70-8d18-c2387f1c9469`; external report only, no rollout |
| g3-native-routing | Read-only evidence written; rollout blocked until G2 | Thread `01a0ba7b-32f1-7af1-a25f-4cde73f1f075`; native routing report only |
| g3-action-roles | Read-only evidence written; G3 contract incomplete | Thread `01a0ba7b-57a5-7623-bd92-d0666a02b96e`; reusable publisher/runtime gaps remain |
| g3-rust-consumers | Read-only evidence written; scanner/release gaps recorded | Thread `01a0ba7d-bb3f-78c3-84c8-eb0b2d75e5d0`; termrock central fix pending |
| g3-distribution-consumers | Read-only evidence written; G3 blocked/incomplete | Thread `01a0ba7d-e457-7723-81ba-1f7ed038212c`; native install/feed proof missing |
| g3-native-review | Independent review active | Thread `01a0ba8a-5bef-7d71-9a17-1d082a4f122a`; result not observed |

Actual thread IDs are recorded for assigned follow-up/category workers above;
remaining unknown worktrees or result states are deliberate. No task result is
inferred from assignment.

## Blockers and access gaps

1. Live workload/check/migration evidence for the fleet is not yet attached;
   inventory rows are not acceptance proof.
2. Generator/bootstrap/distribution investigations remain incomplete; the
   external category audits expose missing typed scanner/publication/runtime
   contracts.
3. The initial checker implementation is semantically rejected; v2 architecture,
   authoritative scope/live bindings, and hostile-fixture rerun are pending.
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

1. Keep the 32-row workload matrix and current fleet refresh linked from the
   external ledger; do not turn observed duties into generated coverage.
2. Require the checker v2 architecture and authoritative evidence bindings;
   rerun the nine hostile G2 mutations plus G0/G1 adversarial fixtures.
3. Refresh live GitHub inventory and attach source/run/check evidence.
4. Reconcile bootstrap, distribution, fleet, runtime, and failed-run findings.
5. Start G1 only after G0 exit evidence is complete and independently reviewed.

## Checkpoint rule

At each gate, before/after publication or merge, and after any central fix,
record an external checkpoint with UTC time, source revisions, task states,
running jobs, and invalidated evidence. A changed default tip, PR head, release
commit, or generator/runtime pin invalidates affected evidence.
