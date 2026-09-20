# Checker v2 regression map and repair design

Status: design and regression map, not a certification result. In this
checkout, `--live` fails because the authenticated collector is unavailable;
offline checks always fail closed. No gate can be certified from these inputs.
`audit_ci` and `lane_compare` remain diagnostic tools owned by other tasks;
neither is a source of G0/G7 authority.

The checker enforces structural consistency for workflow source bytes,
including the pinned Contents request, raw response ID, bytes, and digest. It
also cross-checks PR check source SHAs against current PR heads and, in G0,
rejects duplicate run/attempt identities within each main and per-PR execution
array. These checks do not authenticate caller-supplied API rows or prove
global run identity uniqueness. Workflow Contents bytes are structurally bound
to claimed requests, but that GitHub served those bytes is unverified. The
reviewed manifest projection, repository/PR/ruleset/check response rows,
artifact listing/archive/source digest, check-suite/check-run/job IDs, and
response-derived pagination also remain unverified. TrustedLive G0 has explicit
API-row, pagination, and manifest-projection blockers; there is no separate
Contents-authenticity blocker, and no gate can pass while these blockers remain.

This map combines current checker boundaries with findings from an earlier
collector prototype at commit `017c92e0bf608d42675a0b8e495f0486c7296041`. Its
`evidence_live.rs::collect_workflows` callsite (`evidence_live.rs:136-185`)
listed Actions workflows, then fetched each workflow through the Contents
endpoint using the supplied `source_sha`; that module is absent from
this checkout. The failure modes below describe how untrusted projections
could appear consistent, not a current authorization path: `--live` fails while
the authenticated collector is unavailable, and offline reports always fail
closed.

## Finding-to-code map

| Finding | Relevant code/path | Unverified claim or former failure mode | Required authority repair |
| --- | --- | --- | --- |
| Estate config currently lists 28 unrelated rows | `audit_ci.rs:253-279,572-595` reads `config/estate-repositories.json`, whose current contents have 28 rows | Estate CI auditing is not the 32-repository dual-lane scope | Keep `audit_ci` auxiliary. `evidence_check` owns an immutable exact-32 set and checks both manifest and snapshot set/count/duplicates independently. |
| `lane_compare` explicit jobs/empty census/skipped jobs/warnings/watch omissions | `lane_compare.rs:167-187,215-218,240-307,325-360,1058-1096,1493-1502` | Diagnostic comparison permits caller-selected pairs and incomplete census | No gate dependency on `lane_compare`; add checker regression proving its absence cannot authorize evidence. |
| Workload plan and actual job inventory | `check_expected_job`, `check_workloads`, and `check_authoritative_jobs` compare caller-provided manifest and snapshot projections | Internally matching names/IDs do not authenticate the reviewed plan or complete per-run job inventory | Derive expected jobs from pinned manifest bytes and compare them with parsed per-run job responses using an exact bijection and conclusion set. |
| Main/manual dispatch substitution | `check_record_coverage` and `check_source_semantics` require `push` for main | A dispatch event is rejected structurally, but a caller can still label an unparsed run as `push`; event identity is not API-authenticated | Keep push-only qualification and bind event, run/attempt, trigger SHA, and checkout to parsed provider responses. |
| PR execution uses contributor SHA and base workflow inventory | Removed prototype at commit `017c92e0bf608d42675a0b8e495f0486c7296041`: `evidence_live.rs::collect_workflows` (`:136-185`) fetched Contents at `source_sha`; `collect_executions` selected runs | For pull_request, contributor head and synthetic-merge checkout are distinct; merge_group carries a separate queue SHA. Workflow content resolved at default tip can mismatch the run revision | Collect PR runs by PR identity plus both head and merge SHA candidates; bind event/PR/merge-group metadata; fetch workflow content at each run source SHA. Missing association fails. |
| Checkout SHA is copied from run `head_sha` | Removed prototype at commit `017c92e0bf608d42675a0b8e495f0486c7296041`: `evidence_live.rs:273-345` set `actual_checkout_sha = run.head_sha` | Run head is not observed checkout identity | Add typed checkout attestation to the live graph; accept only an independently fetched producer artifact/output bound to repository, run/attempt, job, event, and commit. Absent/conflicting proof fails. |
| Check context is matched by name; parent run/job IDs are fabricated | Removed prototype at commit `017c92e0bf608d42675a0b8e495f0486c7296041`: `evidence_live.rs:524-582`; current structural check: `check_authoritative_checks` | First same-name check can be unrelated; `external_id` is treated as job ID; parent run is copied into check | Model actual check-run/check-suite IDs, repository, source SHA, workflow run/attempt, app ID, job ID, URL. Fetch/check exact associations and reject missing fields. |
| Provider/host is inferred from runner name or labels | Removed prototype at commit `017c92e0bf608d42675a0b8e495f0486c7296041`: `evidence_live.rs:467-503,585-596`; current structural check: `check_host_binding` | The prototype classified runner kind/provider from runner name and labels, used runner ID or fell back to runner name for `host_id`, and copied workload/platform/architecture from the caller manifest. It did not use runner title. Those fields do not prove the claimed provider or host ownership. | Certification needs a trusted runner-kind/provider/host identity binding; Velnor executions also need independently verified provisioning ownership. Names and labels may support classification, but cannot authenticate provider or host. |
| Child graph is shallow and API-unachievable | Removed prototype at commit `017c92e0bf608d42675a0b8e495f0486c7296041`: `evidence_live.rs:350-434`; current child projection lacks recursively collected job/check/log coverage | Matching SHA/event or a missing `parent_run_id` cannot establish parentage; successful wrapper can hide failed descendants | For a child actually triggered by `workflow_run`, accept only that child's typed trigger payload/context and the direct parent it names; require validated producer artifact/output parent links for other relationships. Then recursively collect child runs/jobs/checks/logs. Missing edge/descendant fails closed. |
| G0 typed inventory claims lack API authority | `check_g0_inventory` validates the typed G0 collector projection | Typed rows and `gate_status=pass` do not prove branch/PR/check/workflow/dependency/model/access facts came from GitHub | Bind each row to parsed subject-specific provider responses; retain explicit unverified blockers until the authenticated collector exists. |
| Effective required-check inventory is incomplete | `check_g0_inventory` and ruleset snapshot rows | Rule targets, enforcement, and conditions are not proven active/applicable; classic branch-protection required checks and app IDs are not captured | Emit `required-check-inventory-unverified` until active rules and classic protection checks are collected, parsed, and joined to the branch. |
| Enriched manifest projection is caller supplied | `check_manifest` pins source repository/revision/digest strings but does not load the reviewed source bytes | A caller can attach the pinned identity strings to a modified 32-row workload projection; the checker does not recompute that projection from pinned bytes | Load and hash the exact reviewed source at its pinned commit/path, derive the projection from those bytes, and bind the reviewed result to the snapshot. |
| Reviewer attestation is a digest-shaped self-claim | `ReviewerAttestation` and `check_headers` | Nonempty reviewer/digest/timestamp can be authored by the result producer | Require a typed external attestation identity/source and validate exact manifest, snapshot, report digest, reviewer authorization, and immutable attestation object; absent independent proof fails. |
| Release/install nested fields are structurally present but authority can be rebound | `check_canonical_release` and `check_install_evidence` | Evidence-declared inventory/digest/source/target can be changed consistently | Keep producer-owned canonical manifest as authority; require exact schema, source repository, product component/target inventory, parent digest, structured projections, and typed install operations. Required applicability cannot become N/A. |
| G2+ release manifest origin is caller supplied | `check_canonical_release` recomputes the canonical digest of the supplied manifest | A matching digest proves consistency of caller-authored bytes, not that a producer or GitHub release supplied them; no dedicated release-origin blocker exists | Load the exact manifest through authenticated producer/release evidence and bind its bytes and release identity; fail closed until that source is verified. |

## Required invariants for eventual certification

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
6. Provider and host require independently authenticated evidence; Velnor
   executions also require trusted provisioning ownership. Runner names,
   labels, result booleans, and self-authored host
   strings never upgrade trust.
7. G7 requires live collection and an independently registered reviewer
   attestation. Offline fixtures prove mutation coverage only and cannot prove
   live completion.

## Target requirement-to-authority matrix

This matrix defines evidence needed after authenticated collector integration.
The authority sources and invariants below are not currently satisfied by the
caller-supplied typed snapshot.

| Requirement | Typed field(s) | Independent collector/authority | Invariant | Negative fixture |
| --- | --- | --- | --- | --- |
| Exact fixed scope | `ManifestDocument.repositories`, `SnapshotDocument.repositories` | Compiled task-specific 32-name authority plus live repository API | Both sets/counts equal exactly once; no estate-manifest substitution | replacement, duplicate, missing, extra repository |
| Current branches and SHAs | snapshot repository `default_branch`, `default_branch_sha`, `observed_at_utc` | live repository/default-branch commit API | current tip is the one used by main evidence; supplied snapshot reconciles to fresh facts | stale main SHA, changed tip |
| All open PRs | `SnapshotPullRequest.number/state/head_sha/base_sha/merge_sha`, execution role | live pulls API plus PR merge/merge-group facts | every current PR, including draft/bot/fork, has a required candidate record where the phase requires PR proof | omitted PR, stale head/base/merge, PR replaced by push |
| Ruleset checks/apps | `RulesetObservation.required_checks`, typed check observations | live ruleset details and check-run/check-suite API | exact context/app set, complete pagination, source URL and producing app bind to selected run | omitted context, wrong app, unrelated same-name check |
| Workflow/reusable/action/scanner inventory | workflow observations, reviewed plan `expected_jobs/child_workflow`, generated plan digest | reviewed source revision plus live contents/workflow/run graph | immutable path/revision/event inventory; child edges recursively complete | base workflow substituted for PR workflow, missing reusable child |
| Nonempty workloads/platform/arch/provider | manifest expected workloads/job plan/host contracts; actual job rows | reviewed source plan plus live job/runner API | exact expected-vs-actual bijection; explicit exclusion is not executed success | empty matrix, skipped job, wrong target/provider |
| Dependency/access/model G0 inventory | typed G0 inventory (added in implementation) and snapshot source scopes | reviewed records owner + live collector permissions/source metadata | every required class present; unknown/access gap remains blocker | scalar `gate_status=pass`, missing dependency/access/model |
| Run/source/event/checkout/provider/host | execution status/conclusion, event, SHAs, provider, runner/host identity | live run/job/check API, validated checkout attestation, and trusted runner/provider/host binding | terminal success only; immutable source/event/provider/host binding | manual-only, stale SHA, copied checkout SHA, runner-name/label spoof |
| Child runs and logs | recursive child observations with jobs/checks/log URLs | payload from that child's `workflow_run` trigger (direct parent only), or validated producer output + API | parent/child IDs/attempts exact; every descendant terminal and logged | missing parent edge, failed descendant, missing child logs |
| Release/install evidence | canonical release manifest, typed projections, install environment/operations/identity | producer asset digest + APT/Homebrew/API/install owners | one parent digest, exact target/component/artifact/upgrade identities; required cannot become N/A | digest rebind, mismatched artifact, absent upgrade, unsupported N/A |
| Independent review | typed reviewer attestation | external reviewer artifact/registration, not result ledger | exact report/manifest/snapshot digest and authorized reviewer | self-authored digest, wrong snapshot, stale attestation |

## Non-goals and ownership boundaries

- This repair does not make `audit_ci`'s 28-row estate manifest authoritative.
- This repair does not make `lane_compare` a gate or silently repair its
  diagnostic semantics.
- When GitHub APIs do not expose a required child-parent edge, checkout
  attestation, provider/host identity, check-suite association, or PR merge
  identity, that claim remains blocked unless an approved independent API
  source, the child's triggering `workflow_run` payload (for its direct parent
  only), or a validated producer output supplies the exact binding. Never infer
  it from SHA, event, or name alone.
- Fixture success is regression coverage only. It is not G0/G7 or G7
  achievability evidence.
