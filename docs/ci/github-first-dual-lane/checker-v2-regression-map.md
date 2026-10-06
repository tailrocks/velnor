# Checker v2 regression map and repair design

Status: implementation checkpoint, not approval. This map reconciles the
independent base-scope findings with checker `017c92e0` and the rejected
`G0/checker-v2-review/report.md` review. `audit_ci` and `lane_compare` remain
diagnostic tools owned by other tasks; neither is a source of G0/G7 authority.

## Finding-to-code map

| Finding | Current location | Why the current structure can false-green | Bounded repair |
| --- | --- | --- | --- |
| Estate manifest has 28 unrelated rows | `audit_ci.rs:253-279,572-595` reads `config/estate-repositories.json` and hardcodes 28 | Estate CI auditing is not the 32-repository dual-lane scope | Keep `audit_ci` auxiliary. `evidence_check` owns an immutable exact-32 set and checks both manifest and snapshot set/count/duplicates independently. |
| `lane_compare` explicit jobs/empty census/skipped jobs/warnings/watch omissions | `lane_compare.rs:167-187,215-218,240-307,325-360,1058-1096,1493-1502` | Diagnostic comparison permits caller-selected pairs and incomplete census | No gate dependency on `lane_compare`; add checker regression proving its absence cannot authorize evidence. |
| Expected workload plan can be weakened | `evidence_check.rs:1830-2088` compares result rows to manifest rows but does not require exact actual-name/ID bijection | Duplicate names, extra actual jobs, and extra/missing conclusion keys can survive | Require one authoritative actual job for each reviewed job name; exact expected/actual ID sets and exact conclusion-key set; reject duplicate names and unplanned jobs. |
| Main/manual dispatch substitution | `check_record_coverage` and `check_source_semantics` (`evidence_check.rs:1761-1888`) treat dispatch as main | A current-SHA dispatch is not resulting-main push evidence | Main qualification requires a `push` execution; dispatch is diagnostic and cannot satisfy coverage. |
| PR execution uses contributor SHA and base workflow inventory | `evidence_live.rs:93-105,136-184,255-345` filters runs by PR head and resolves workflow files at default tip | Normal PR runs use merge-queue/synthetic merge SHAs; workflow revision can be borrowed from base | Collect PR runs by PR identity plus both head and merge SHA candidates; bind event/PR/merge-group metadata; fetch workflow content at each run source SHA. Missing association fails. |
| Checkout SHA is copied from run `head_sha` | `evidence_live.rs:273-345` sets `actual_checkout_sha = run.head_sha` | Run head is not observed checkout identity | Add typed checkout attestation to the live graph; accept only an independently fetched producer artifact/output bound to repository, run/attempt, job, event, and commit. Absent/conflicting proof fails. |
| Check context is matched by name; parent run/job IDs are fabricated | `evidence_live.rs:524-582`; `check_authoritative_checks` | First same-name check can be unrelated; `external_id` is treated as job ID; parent run is copied into check | Model actual check-run/check-suite IDs, repository, source SHA, workflow run/attempt, app ID, job ID, URL. Fetch/check exact associations and reject missing fields. |
| Provider/host is inferred from labels/title | `evidence_live.rs:467-503,585-596`; `check_host_binding` | A runner name containing `velnor` or self-hosted labels can rebind provider | Collector must use observed runner registration/group facts and a typed Velnor provisioning attestation; labels/display names are diagnostics only. GitHub-hosted requires hosted registration; Velnor requires trusted host binding. |
| Child graph is shallow and API-unachievable | `evidence_live.rs:350-434`; `ChildRunObservation` has no jobs/checks/logs | Matching SHA/event or a missing `parent_run_id` cannot establish parentage; successful wrapper can hide failed descendants | Support only typed `workflow_run` payload/context or validated producer artifact/output parent links, then recursively collect child runs/jobs/checks/logs. Missing edge/descendant fails closed. |
| G0 scalar record bypass | `check_record_coverage` G0 branch and `EvidenceRecord` fields | One row plus `gate_status=pass` does not prove branch/PR/check/workflow/dependency/model/access inventory | Add typed G0 inventory evidence to the snapshot/record authority and require every mandatory class/nonempty source-backed rows; `gate_status` remains report-only. |
| Reviewed manifest source is caller-selected | `check_manifest` validates source shape only | A caller can submit a fabricated exact-32 plan with its own expected jobs/policy | Bind source repository/revision/plan digest to the accepted reviewed authority and require an independently registered reviewer attestation over exact manifest/snapshot IDs. |
| Reviewer attestation is a digest-shaped self-claim | `ReviewerAttestation` and `check_headers` | Nonempty reviewer/digest/timestamp can be authored by the result producer | Require a typed external attestation identity/source and validate exact manifest, snapshot, report digest, reviewer authorization, and immutable attestation object; absent independent proof fails. |
| Release/install nested fields are structurally present but authority can be rebound | `check_canonical_release` and `check_install_evidence` | Evidence-declared inventory/digest/source/target can be changed consistently | Keep producer-owned canonical manifest as authority; require exact schema, source repository, product component/target inventory, parent digest, structured projections, and typed install operations. Required applicability cannot become N/A. |

## Replacement invariants

1. The generic workflow crate remains estate-agnostic. Only this task-specific
   checker owns the exact 32 set. Both reviewed manifest and independently
   collected snapshot must equal that set exactly once; count-only, arbitrary
   replacement, duplicate, missing, and out-of-scope rows fail.
2. Expected workloads, required contexts/apps, provider eligibility, host
   contracts, and child edges come from reviewed plan plus live provider facts.
   A result record cannot add, remove, rename, or downgrade any expected unit.
3. A qualifying run is a terminal `completed/success` run with a supported
   event, exact repository/workflow/run-attempt/source identity, nonempty job
   inventory, exact required-check inventory, and successful terminal jobs.
   `workflow_dispatch` is never resulting-main evidence.
4. PR collection distinguishes contributor head, synthetic merge candidate,
   merge-group source, and resulting main. Runs are selected by observed PR/
   merge identity and are rejected when only a shared SHA/name/timestamp binds.
5. Actual job IDs, check-run IDs, check-suite IDs, app IDs, workflow run IDs,
   and source URLs are observed API identities. The checker requires a
   bijection between reviewed jobs and observed jobs and a recursively complete
   child graph with terminal jobs/checks/logs.
6. Provider and host are derived from trusted API registration/provisioning
   evidence. Runner names, labels, display titles, result booleans, and
   self-authored host strings never upgrade trust.
7. G7 requires live collection and an independently registered reviewer
   attestation. Offline fixtures prove mutation coverage only and cannot prove
   live completion.

## Requirement-to-authority matrix

| Requirement | Typed field(s) | Independent collector/authority | Invariant | Negative fixture |
| --- | --- | --- | --- | --- |
| Exact fixed scope | `ManifestDocument.repositories`, `SnapshotDocument.repositories` | Compiled task-specific 32-name authority plus live repository API | Both sets/counts equal exactly once; no estate-manifest substitution | replacement, duplicate, missing, extra repository |
| Current branches and SHAs | snapshot repository `default_branch`, `default_branch_sha`, `observed_at_utc` | live repository/default-branch commit API | current tip is the one used by main evidence; supplied snapshot reconciles to fresh facts | stale main SHA, changed tip |
| All open PRs | `SnapshotPullRequest.number/state/head_sha/base_sha/merge_sha`, execution role | live pulls API plus PR merge/merge-group facts | every current PR, including draft/bot/fork, has a required candidate record where the phase requires PR proof | omitted PR, stale head/base/merge, PR replaced by push |
| Ruleset checks/apps | `RulesetObservation.required_checks`, typed check observations | live ruleset details and check-run/check-suite API | exact context/app set, complete pagination, source URL and producing app bind to selected run | omitted context, wrong app, unrelated same-name check |
| Workflow/reusable/action/scanner inventory | workflow observations, reviewed plan `expected_jobs/child_workflow`, generated plan digest | reviewed source revision plus live contents/workflow/run graph | immutable path/revision/event inventory; child edges recursively complete | base workflow substituted for PR workflow, missing reusable child |
| Nonempty workloads/platform/arch/provider | manifest expected workloads/job plan/host contracts; actual job rows | reviewed source plan plus live job/runner API | exact expected-vs-actual bijection; explicit exclusion is not executed success | empty matrix, skipped job, wrong target/provider |
| Dependency/access/model G0 inventory | typed G0 inventory (added in implementation) and snapshot source scopes | reviewed records owner + live collector permissions/source metadata | every required class present; unknown/access gap remains blocker | scalar `gate_status=pass`, missing dependency/access/model |
| Run/source/event/checkout/provider/host | execution status/conclusion, event, SHAs, provider, runner/host identity | live run/job/check API plus validated checkout attestation | terminal success only; immutable source/event/provider/host binding | manual-only, stale SHA, copied checkout SHA, runner-title spoof |
| Child runs and logs | recursive child observations with jobs/checks/log URLs | supported workflow-run context or validated producer output + API | parent/child IDs/attempts exact; every descendant terminal and logged | missing parent edge, failed descendant, missing child logs |
| Release/install evidence | canonical release manifest, typed projections, install environment/operations/identity | producer asset digest + APT/Homebrew/API/install owners | one parent digest, exact target/component/artifact/upgrade identities; required cannot become N/A | digest rebind, mismatched artifact, absent upgrade, unsupported N/A |
| Independent review | typed reviewer attestation | external reviewer artifact/registration, not result ledger | exact report/manifest/snapshot digest and authorized reviewer | self-authored digest, wrong snapshot, stale attestation |

## Non-goals and ownership boundaries

- This repair does not make `audit_ci`'s 28-row estate manifest authoritative.
- This repair does not make `lane_compare` a gate or silently repair its
  diagnostic semantics.
- GitHub APIs that do not expose a child parent edge, checkout attestation,
  runner registration, check-suite association, or PR merge identity are a
  hard evidence blocker; the checker must not infer them.
- Fixture success is regression coverage only. It is not G0/G7 or G7
  achievability evidence.
