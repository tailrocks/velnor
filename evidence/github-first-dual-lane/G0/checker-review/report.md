# Independent G0 checker and records review

Status: **not approved**. This is a read-only design/review checkpoint. No
source worktree was edited.

Reviewer: `/root/g0_reviewer`

Observed at: `2026-09-19T17:19:09Z`

Inputs:

- Base source: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`.
- Checker exact commit: `b3b6b2ef5239ff3354f504b8aeb638129fd0504b` in
  `dual-lane-checker`.
- Records exact tip: `df9591e6fd7ca5f79ef2f9b493103e905653106a` in
  `dual-lane-records`.
- Authoritative goal: `velnor-github-first-dual-lane-goal.md`.
- External live evidence root:
  `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence`.

## Verification performed

Checker worktree was clean. `git show --check` was clean and the commit has
DCO signoff plus `Co-authored-by: Codex <codex@openai.com>`.

Commands and results:

- `rtk cargo test -p velnor-tools --no-fail-fast`: **216 passed**.
- `rtk cargo clippy -p velnor-tools --all-targets --all-features -- -D warnings`:
  **clean**.
- `rtk cargo fmt --all -- --check`: **clean**.
- Running the checker against the live records input produced **475 findings
  and non-zero exit**. This is the correct result for incomplete records; it is
  not a gate pass.

The records worktree is clean at `df9591e`. Its docs correctly state that G0
is in progress and no gate has passed. The external fleet refresh reports 32
main revisions and 77 current PR rows after reconciling a baseline of 78:

- `G0/fleet/requirements.json`, generated `2026-09-19T17:03:32Z`;
- `G0/fleet/main-verification.json`, refreshed `2026-09-19T17:03:32Z`;
- `G0/fleet/context-pages.json`, generated `2026-09-19T17:01:00Z`;
- `G0/fleet/handoff.json`, generated `2026-09-19T17:05:57Z`.

The current reconciliation records `jackin-project/homebrew-tap#494` merged
and `tailrocks/velnor#953` head changed. The source docs do not yet link or
transcribe all of this current evidence.

## Requirements matrix

| Gate | Required by goal | What exact checker currently proves | Acceptance gap |
| --- | --- | --- | --- |
| G0 | Exact fixed 32; current branches/SHAs, PRs, checks, workflows, workload/platform matrix, dependency graph, model settings, access gaps | Manifest count/shape, supplied snapshot row shape, record identity/workload/provider coverage | No canonical exact-name binding, fresh/provenanced snapshot, PR/check/workflow/dependency/model/access evidence, or per-PR coverage. G0 can rely on a self-authored `gate_status`. |
| G1 | Hosted generated policy, real required PR checks, post-merge main CI without Velnor | One GitHub record per eligible repository/provider with source/job/check URL shape checks | No required PR plus post-merge-main pair, run/check API provenance, complete workflow/child graph, or host/trust identity. |
| G2 | New preview/stable releases, APT/Homebrew delivery, clean install/upgrade, hosted checks | Release/install objects and several identity/digest shape/cross-field checks | Applicability can downgrade install independently; publication/feed/signing/channel/upgrade provenance is not authoritative. |
| G3 | All 32 approved generator; migration PRs and current main hosted; open PR reconciliation | GitHub provider rows and current default SHA correspondence | No generator/output/pin proof, migration-PR/current-main pair, open-PR coverage, or category workload behavior. |
| G4 | Actual Mac/OrbStack host, install/routing, representative jobs, hosted comparisons, defects | Velnor execution plus release/install shape | No hosted comparison, actual Mac/OrbStack/Docker identity, trust/routing/container/resource/cancellation/recovery/cache evidence. |
| G5 | Defects fixed/reviewed/released/installed; complete packaged pilot | Same Velnor/release/install contract | No defect-to-release-to-installed provenance or full pilot checklist/no-unpublished-checkout proof. |
| G6 | Both providers on same PR and resulting main for every eligible workload; native-only checks retained | One GitHub and one Velnor record per eligible repository | No same-candidate/resulting-main pairing, cross-lane source/workload equivalence, native-only evidence, or singular publisher binding. |
| G7 | Independent audit of current revisions/PRs/checks/package channels/pins/runtime/run evidence; checker and separate reviewer pass | Owner/reviewer strings must be nonempty and differ | No independent attestation, live API reconciliation, or complete current-evidence graph. |

## Exact checker findings

### F-01 — G0 is a scalar-success bypass (blocker)

`crates/velnor-tools/src/evidence_check.rs:1288-1450` returns early for G0.
At `:1439-1449`, it only requires `gate_status == "pass"`; blocker and
next-action fields are checked only for execution stages. `EvidenceRecord` at
`:288-342` has no fields for the G0 PR/check/workflow inventory, dependency
graph, access gaps, or effective model settings.

Goal requirements are `velnor-github-first-dual-lane-goal.md:91` and
`:273-296`. A fixture with 32 rows, valid supplied pins/snapshot/workload rows,
provider maps, and `gate_status: "pass"`, but no current PR/check/workflow or
dependency/model/access evidence, can pass G0. A passing G0 must be derived
from required observations, not accepted from a scalar record claim.

### F-02 — Count/shape is not the fixed canonical scope (blocker)

`REQUIRED_REPOSITORIES = 32` is declared at `evidence_check.rs:17`, but
`check_manifest` (`:982-1089`) checks only count, duplicate names, and
owner/name syntax. It does not compare against the exact names in the goal
(`velnor-github-first-dual-lane-goal.md:21-58`). The schema explicitly says the
checker does not contain the repository list (`evidence-schema.md:20-29`).

Under the accepted architecture, this is fixed by binding the external
manifest to a compile-included immutable canonical manifest/digest from the
accepted SPEC. The generic workflow crate remains estate-agnostic; the gate
checker, or a separately pinned canonical-scope artifact, must verify the
manifest digest and exact set. Count-only external input is insufficient.

### F-03 — Expected jobs, checks, and children are caller-authored (blocker)

`expected_jobs` is part of each record (`evidence_check.rs:390-400`).
`check_jobs` (`:1928-2076`) compares actual IDs and conclusions only against
that record's expected list. `check_required_checks` (`:2078-2143`) compares
only the record's required-check list. `check_child_runs` (`:2145-2245`)
requires a child only when an optional `ExpectedJob.child_run_id` was supplied.

Adversarial case: remove real job B from `expected_jobs` and
`required_checks`, retain one fake successful job A, and omit B's child link.
The record can satisfy the checker while required work is absent. Expected jobs,
required checks, and child edges must be independently derived from the
immutable workflow/run/check snapshot, not selected by the record producer.

### F-04 — Snapshot freshness and provenance are not authoritative (blocker)

`check_snapshot` (`evidence_check.rs:1091-1152`) validates format, duplicates,
SHA shape, and row fields. `check_record` (`:1288-1358`) rejects a record older
than the supplied snapshot, but no maximum age, fetch provenance, API query
identity, signed artifact, or independent source binding is checked. A stale
snapshot can therefore be supplied with matching records.

PR head equality is checked only when a `pull_request` execution record exists
(`:1830-1886`); G0 requires no per-PR execution record. Goal freshness requires
a fresh default-tip/open-PR read at `:294-296`, `:315`, and `:318`. The checker
must compare live independently fetched revisions and invalidate affected rows
when a branch/PR head moves. No arbitrary timed soak is required or desired.

### F-05 — Run terminal state and check provenance are absent (blocker)

`EvidenceRecord` has no run status or run conclusion (`evidence_check.rs:315-342`).
`check_execution` (`:1640-1793`) validates positive run IDs/attempts, source
SHAs, and nonempty HTTPS URLs, then consumes self-authored job/check maps.
`nonempty_url` (`:3231-3237`) only checks the `https://` prefix. Required
checks have no immutable API URL/run binding. A queued, canceled, timed-out,
or unrelated run can be represented with successful self-authored jobs and
checks. Goal explicitly requires those states to fail (`goal:294-296`).

Run evidence must include an independently fetched terminal status/conclusion,
repository/workflow/run identity, check-suite/app identity, job IDs, and exact
URLs or API object hashes bound to the same source/event.

### F-06 — Workflow and trust provenance are not bound (blocker)

`check_event_source` (`evidence_check.rs:1795-1926`) accepts `workflow_dispatch`
when trigger and checkout equal the snapshot tip. It does not require the
required-check/PR association promised by `evidence-schema.md:190-193`.
`workflow_run` is unsupported. `workflow_revision` is only a 40-hex shape;
`runner_name` and `host_id` are only nonempty checks (`:1640-1793`). Provider
eligibility is a self-authored map.

The external live contract records direct workflow dispatch run
`35430875046` with no PR association at
`G0/check-contract/check-contract.json`. A manual dispatch with current
source, self-authored successful jobs, and no PR association is an executable
false-green case.

Goal requires workflow-run producer identity (`goal:294`) and trust identity
derived from event, repository, policy, and immutable authorized head
(`goal:252-254`). Add typed provider/host/platform identity and immutable trust
authorization; do not accept labels, actor strings, or workflow claims alone.

### F-07 — G4/G5 omit the required hosted comparison (blocker)

`Stage::needs_velnor` and `required_providers` (`evidence_check.rs:62-76,
1259-1285`) require only Velnor for G4/G5. Only G6 requests both providers.
The goal requires hosted comparisons for G4 (`goal:95`, `:226-234`, `:254`).
Therefore a Velnor-only G4/G5 fixture can pass the provider coverage portion
without the required hosted counterpart.

### F-08 — Required release can bypass required installation

`check_release_and_install` (`evidence_check.rs:2247-2334`) guards release
downgrade only when manifest release applicability is exactly `Required`
(`:2269-2279`). Install applicability is evaluated independently
(`:2292-2332`). A manifest-required release plus
`install.applicability: not-applicable` and a justification bypasses clean
install/upgrade evidence. Cross-object applicability must be derived from the
canonical product/release contract; an install cannot be downgraded when its
release is required unless the SPEC explicitly marks installation inapplicable.

### F-09 — Legacy aliases and unknown fields permit silent bypass

Raw manifest/snapshot aliases appear at `evidence_check.rs:152-275`; record
flat aliases and compatibility fallbacks appear at `:344-385` and
`:2961-3008`. The top-level `EvidenceRecord` has no `deny_unknown_fields`.
Unknown fields such as run state or trust claims are silently ignored. This
conflicts with the project no-legacy rule and with typed strict evidence.
`PLAN.md:84-89` explicitly retains both schema spellings and therefore needs
revision or an explicitly completed migration before approval.

### F-10 — G1/G3/G6 phase coverage is collapsed to one row per provider

`check_record_coverage` (`evidence_check.rs:1217-1285`) indexes only
`(repository, provider)`. It cannot require both a PR integration candidate and
post-merge current-main run. Goal requirements are G1 (`goal:92,120-128`), G3
(`:94,208-212`), and G6 (`:265-269`). One push record per repository/provider
can satisfy the checker. G6 also has no cross-lane source/workload equivalence
or singular-publisher binding.

G7's owner/reviewer fields (`:1450-1501`) are self-authored strings, not an
independent reviewer attestation. Phase role, source candidate, resulting-main
run, and reviewer attestation need explicit typed records.

## Existing helper audit: reuse limits and false greens

### `audit_ci`

- `crates/velnor-tools/src/audit_ci.rs:253-279` reads the estate manifest.
- `:572-595` hardcodes `expected exactly 28 repositories`.
- `:357-470` audits static/generated workflow structure and remote default
  identity, not PR/check/run state, package delivery, trust, or G0 evidence.

It is not a fixed-32 G0 source and must not be used as the gate's scope or
runtime evidence.

### `lane_compare`

- Explicit `--github-job/--velnor-job` skips the census
  (`lane_compare.rs:167-187`).
- An empty census has no parity failure and can reach PASS with zero pairs
  (`:221-238`, `:325-360`).
- The job loop checks `status == completed` but not job conclusion success, and
  skips skipped jobs (`:240-253`). Two skipped jobs can pass the pair loop.
- Missing Velnor artifacts and GitHub/HTML logs become warnings/empty stats
  (`:256-307`); content loss is penalized only when Velnor has positive log
  lines (`:1521-1532`).
- Velnor-only steps are informational (`:1493-1502`).
- Watch mode counts matched pairs only and ignores census orphans/duplicates
  (`:1058-1096`).
- Artifact discovery fetches one `per_page=100` page without pagination
  (`:596-636`).
- Fixture latest-run selection chooses the newest workflow run by ID only
  (`crates/velnor-tools/src/main.rs:1576-1603`); it does not bind event,
  provider, source SHA, or required-check association.

These helpers can provide diagnostics after independent inputs are established,
but cannot serve as the deterministic G0/G7 gate.

## Provenance-aware architecture constraints

The checker author should return a design proposal satisfying all constraints
below before making source edits.

1. **Canonical scope binding.** Keep generic workflow generation estate-agnostic.
   The accepted SPEC must define an immutable canonical 32-row manifest and
   digest. The checker must compile/include that canonical digest or verify an
   externally supplied manifest against it. A caller-supplied count-only
   manifest is not authoritative.

2. **Separate authority classes.** Model independently captured inputs as
   distinct types: canonical scope, live repository/PR/check snapshot, workflow
   inventory, run/check/job/child evidence, package/release evidence, and
   reviewer attestation. Do not let an evidence record supply its own expected
   jobs, required checks, current source, or provider policy.

3. **Live G7 reconciliation.** The final checker must perform, or consume a
   cryptographically bound output of a fixed read-only collector that performs,
   live API reconciliation. Independently derive expected source revisions,
   open PR/head/base inventory, active ruleset required contexts/apps, workflow
   revisions, run/job/check state, and provider/trust policy. Ignore caller
   booleans such as `source_matches`, `required_checks_pass`, or `overall_green`.

4. **Freshness by revision comparison.** Compare the live fetch's exact branch
   and PR SHAs to the attested evidence. If any relevant revision moved, fail or
   invalidate affected evidence and require a new snapshot. Record UTC fetch
   times and API query provenance, but impose no arbitrary timed soak.

5. **Required-work derivation.** Derive expected jobs and child edges from an
   immutable workflow revision plus the declared workload/platform contract, or
   from an independently captured run/job graph. Require every expected job,
   terminal success conclusion, required check/app, and child run. Missing,
   skipped, queued, canceled, timed-out, or failed objects fail. A producer may
   report observations, never redefine the expected set.

6. **Run identity binding.** Bind repository, workflow path/revision, event,
   PR head/base/merge or main SHA, run ID/attempt, check-suite ID/app, job IDs,
   child producer workflow/repository/source, and exact API/URL objects. Verify
   terminal status and conclusion independently. Reject arbitrary HTTPS links.

7. **Trust and host identity.** Derive eligibility from event, repository/fork,
   ruleset/policy, and immutable authorized head/integration SHA. Type GitHub
   hosted, Velnor, macOS, OrbStack, Docker server, image, architecture, and
   runner identity. Do not treat a runner name, label, title, actor, or
   workflow-authored boolean as trust proof.

8. **Phase coverage.** Represent PR-candidate and resulting-main executions as
   separate required roles. For G4/G5 require hosted and Velnor comparison
   records; for G6 require both providers on the same candidate and resulting
   main source/workload contract. Represent native-only obligations and one
   publisher explicitly.

9. **Applicability is canonical.** Release and install applicability must be
   derived from the product/repository contract, not freely downgraded by the
   record. `not-applicable` is explicit non-executed evidence, never success.

10. **Strict schema migration.** Remove legacy aliases/fallback spellings under
    the no-legacy project rule, or complete a separately versioned migration
    before this checker becomes the gate. Reject unknown fields so newly required
    provenance cannot be silently discarded.

11. **Independent reviewer proof.** G7 reviewer identity must be an external,
    session-bound or signed attestation over the exact evidence digest. Distinct
    free-form owner/reviewer strings are not enough.

## Records-doc review: exact tip `df9591e`

- `SPEC.md:138` lists dependency graph and access gaps as G0 exit evidence, but
  the commit contains no dependency-graph artifact or link.
- `SPEC.md:384-392` claims the checker rejects queued/canceled/timed-out and
  unexpectedly skipped work; exact checker behavior does not yet prove those
  states independently.
- `SPEC.md:435` says access gaps for 31 repositories, contradicting the fixed
  32-repository scope.
- `STATUS.md:10-16` and `:40-41` retain the old 78-PR `16:34` inventory and
  links while current external reconciliation is 77 rows. Current
  `requirements.json`, `main-verification.json`, `context-pages.json`, and
  `handoff.json` are not linked in the status checkpoint.
- `PLAN.md:84-89` explicitly preserves legacy manifest/schema aliases, contrary
  to the project no-legacy rule and strict provenance design.

Docs are a valid in-progress checkpoint, not a G0 pass. Reconcile or clearly
label the stale baseline and link current external evidence before approval.

## Decision and dependency

Decision: **review finds material acceptance holes; no checker or G0 approval**.

Next dependency: checker author must provide the provenance-aware architecture
proposal above, then implement authoritative scope/live snapshot/run graph
bindings and adversarial fixtures. Records author must reconcile current
external evidence and documentation claims. Final review requires exact new
commits plus fresh live revision comparison; the existing 216-test result and
475-finding failure are necessary hygiene evidence, not semantic gate proof.
