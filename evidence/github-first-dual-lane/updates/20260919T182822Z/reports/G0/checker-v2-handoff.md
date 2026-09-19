# Checker v2 evidence migration handoff

Status: **migration plan only; no gate pass**.

Owner: `/root/g0_checker`  
Checker source: `velnor-tools` commit `017c92e0bf608d42675a0b8e495f0486c7296041`  
Branch: `codex/github-first-checker` (pushed)

Existing v1 fleet ledgers are intentionally rejected by checker v2. No v1 row,
`gate_status`, aggregate-green flag, count, badge, or display name converts to
v2 success. Placeholders below are schema skeletons, not fabricated success
evidence.

## Source inputs and authority

The checker requires separate authority classes:

| Input | Authority | Required provenance | Current emitter |
| --- | --- | --- | --- |
| `manifest-v2.json` | Reviewed G0 workload plan | exact fixed 32 names; source repository/revision/SHA-256 digest/reviewer; generated plan/config/pin digests | G0 records owner; derive workload rows from `G0/workload-matrix.json` and reviewed source |
| `snapshot-v2.json` | Read-only GitHub collector | UTC capture, collector identity/revision, API base, read-only mode, page count/scopes, repository IDs, default tips, rulesets, workflow revisions, open PR tuples, main/PR run graph | G0 fleet collector; refresh for G0/G3/G7 |
| `records-v2.json` | Run/result producers | exact manifest/snapshot IDs; run/attempt/status/conclusion; source/event/checkout; actual job/check IDs/apps/URLs; child graph; typed host identity | execution-health/jobs/providers owners |
| `application-manifest.json` | One release producer | canonical bytes plus external SHA-256; immutable source/tag/version/channel; complete target/component/artifact inventory | distribution producer; APT/Homebrew consume parent digest |
| reviewer attestation | Independent reviewer | report digest, exact manifest/snapshot IDs, reviewer identity, UTC time | `/root/g0_reviewer` or separate reviewer |

The generic workflow model remains estate-agnostic. The fixed scope is
checker/task authority only. Current raw fleet inputs are:

```text
G0/fleet/handoff.json       generated 2026-09-19T17:05:57Z
G0/fleet/main-revisions.tsv
G0/fleet/open-prs.tsv       77 current rows after reconciliation
G0/fleet/pr-checks.tsv
G0/fleet/requirements.json
G0/fleet/context-pages.json
G0/fleet/main-verification.json
G0/workload-matrix.json
```

Those rows and times require refresh before any final gate. The handoff records
`jackin-project/homebrew-tap#494` merged and `tailrocks/velnor#953` changed
head; old tuples cannot be reused.

## v2 skeletons

These examples use `<observed-value>` placeholders and claim no run passed.

### Reviewed manifest

```json
{
  "schema_version": 2,
  "manifest_id": "<immutable-plan-id>",
  "source": {
    "repository": "<reviewed-source-repository>",
    "revision": "<40-hex-source-sha>",
    "digest": "sha256:<64-hex-plan-digest>",
    "reviewed_by": "<external-reviewer>"
  },
  "repositories": [
    {
      "repository": "tailrocks/velnor",
      "repository_role": "<role>",
      "default_branch": "<live-default-branch>",
      "expected_workload_ids": ["<source-derived-workload>"],
      "required_check_contexts_and_apps": [
        {"context": "<reviewed-context>", "app_id": "<reviewed-app-id>"}
      ],
      "workload_platform_architecture": [
        {"workload_id": "<workload>", "platform": "<platform>", "architecture": "<arch>"}
      ],
      "expected_jobs": [
        {
          "job_id": "<generated-job-name>",
          "workload_id": "<workload>",
          "provider": "github",
          "platform": "<platform>",
          "architecture": "<arch>",
          "required": true,
          "child_workflow": null
        }
      ],
      "generated_plan_digest": "sha256:<64-hex>",
      "workflow_path": "<workflow-path>",
      "workflow_revision": "<40-hex-workflow-file-sha>",
      "provider_eligibility": {"github": "eligible", "velnor": "<policy>"},
      "host_contracts": {
        "github": {"runner_kind": "github-hosted", "required_labels": [], "forbidden_labels": ["self-hosted"]}
      },
      "release_applicability": "<required-or-not-applicable>",
      "generator_revision": "<40-hex>",
      "runtime_product_id": "<product>",
      "generator_artifact_digest": "sha256:<64-hex>",
      "configuration_digest": "sha256:<64-hex>",
      "generated_tree_digest": "sha256:<64-hex>",
      "scan_state_digest": "sha256:<64-hex>",
      "runtime_release_version": "<version>",
      "runtime_source_sha": "<40-hex>",
      "job_image_digest": "sha256:<64-hex>"
    }
  ]
}
```

The actual manifest must contain the checker’s exact 32-name scope once each.
Expected jobs and child edges come from the reviewed generated plan. A result
producer may not remove a job, check, or child edge to make its row green.

### Independent snapshot

```json
{
  "schema_version": 2,
  "snapshot_id": "<fresh-snapshot-id>",
  "manifest_id": "<same-manifest-id>",
  "observed_at_utc": "<fresh-rfc3339-utc>",
  "source": {
    "collector": "<read-only-collector>",
    "collector_revision": "<collector-identity>",
    "api_base": "https://api.github.com",
    "captured_at_utc": "<fresh-rfc3339-utc>",
    "read_only": true,
    "page_count": "<observed-page-count>",
    "permission_scopes": ["<observed-safe-scope-name>"]
  },
  "repositories": [
    {
      "repository": "<one-of-fixed-32>",
      "repository_id": "<positive-github-id>",
      "default_branch": "<actual-branch>",
      "default_branch_sha": "<current-40-hex>",
      "ruleset": {
        "required_checks": [{"context": "<context>", "app_id": "<actual-app-id>"}],
        "source_url": "https://github.com/<owner>/<repo>/settings/rules",
        "pages_complete": true
      },
      "workflows": [
        {
          "path": "<workflow-path>",
          "revision": "<workflow-file-40-hex>",
          "source_sha": "<commit-40-hex>",
          "event": "<observed-event>",
          "source_url": "https://github.com/<owner>/<repo>/blob/<sha>/<path>"
        }
      ],
      "main_executions": [],
      "open_prs": []
    }
  ]
}
```

The real snapshot must include all fixed-scope repositories, all current open
PRs (drafts/bots/forks included), active ruleset pages and app IDs, workflow
source revisions, and separate main/PR execution arrays when the selected gate
requires execution. Empty arrays are factual only when the collector observed
and recorded no run; they never imply success.

### Evidence envelope

```json
{
  "schema_version": 2,
  "manifest_id": "<same-manifest-id>",
  "snapshot_id": "<same-snapshot-id>",
  "stage": "G1",
  "records": [
    {
      "repository": "<repo>",
      "repository_role": "<manifest-role>",
      "default_branch": "<snapshot-branch>",
      "default_branch_sha": "<snapshot-sha>",
      "observed_at_utc": "<record-time>",
      "generator_revision": "<manifest-pin>",
      "runtime_product_id": "<manifest-pin>",
      "generator_artifact_digest": "sha256:<manifest-pin>",
      "configuration_digest": "sha256:<manifest-pin>",
      "generated_tree_digest": "sha256:<manifest-pin>",
      "scan_state_digest": "sha256:<manifest-pin>",
      "runtime_release_version": "<manifest-pin>",
      "runtime_source_sha": "<manifest-pin>",
      "job_image_digest": "sha256:<manifest-pin>",
      "expected_workload_ids": ["<must-equal-reviewed-plan>"],
      "required_check_contexts_and_apps": ["<must-equal-live-ruleset>"],
      "workload_platform_architecture": ["<must-equal-reviewed-plan>"],
      "provider_eligibility": {"github": "eligible", "velnor": "<policy>"},
      "justified_exclusions": [],
      "pr_number": null,
      "pr_head_sha": null,
      "pr_base_sha": null,
      "tested_merge_sha": null,
      "merge_group_sha": null,
      "workflow_path": "<snapshot-workflow>",
      "workflow_revision": "<snapshot-workflow-sha>",
      "event": "push",
      "run_id": "<authoritative-run-id>",
      "run_attempt": "<authoritative-attempt>",
      "run_url": "https://github.com/<owner>/<repo>/actions/runs/<id>",
      "trigger_source_sha": "<current-main-sha>",
      "actual_checkout_sha": "<current-main-sha>",
      "provider": "github",
      "runner_name": "<observed-runner-name>",
      "host_id": "<observed-host-id>",
      "runner_kind": "github-hosted",
      "runner_labels": ["<observed-label>"],
      "run_status": "<observed-terminal-status>",
      "run_conclusion": "<observed-conclusion>",
      "expected_jobs": ["<exact-reviewed-plan>"],
      "actual_job_ids": ["<actual-job-api-id>"],
      "actual_job_conclusions": {"<actual-job-api-id>": "<observed-conclusion>"},
      "logs": ["https://github.com/<owner>/<repo>/actions/runs/<id>"],
      "child_run_links": [],
      "required_checks": ["<actual-context/app/run/job/source/event>"],
      "release": null,
      "install": null,
      "owner": "<record-owner>",
      "reviewer": "<independent-reviewer-or-placeholder>",
      "gate_status": "<report-only-value>",
      "blocker": null,
      "next_action": null
    }
  ],
  "reviewer_attestation": null
}
```

For a PR record, number/head/base/tested-merge/event/trigger/checkout must bind
the same current PR tuple. Main records are separate; a PR row never
substitutes for resulting-main evidence. Actual job IDs, run status/conclusion,
check app IDs, and child links must be independently fetched facts.

## Gate migration matrix and emitters

| Gate | Independent facts required | Current owner(s) to emit | Acceptance condition |
| --- | --- | --- | --- |
| G0 | v2 exact32 manifest; live default branch/ID/SHA rows; all current open PR tuples; ruleset/status-check/app inventory with pagination; workflow/source inventory; workload/platform/generated-plan matrix; dependency graph; effective model/runtime metadata; safe access scopes/gaps | G0 records owner; fleet snapshot collector; execution-health workload-matrix owner; root/orchestrator for effective Luna/max metadata | Every required inventory fact is present and source-bound. A row’s `gate_status` cannot close an access or inventory gap. |
| G1 | Fresh GitHub-hosted main and every current open PR execution; run status/conclusion; exact jobs/checks/app IDs/source/event; workflow/reusable/action/child graph; trusted hosted runner identity; merge candidate SHAs | jobs/providers execution owner; GitHub collector; per-repository workflow owners | All reviewed jobs/checks complete successfully on exact current PR and resulting-main sources. Missing/pending/skipped/failed/unknown is a blocker. |
| G2 | Producer-owned canonical release manifest + external digest; complete target/component/artifact inventory; immutable producer run; typed APT/Homebrew projections; clean install, same-channel upgrade, channel switch, service, binary/path/digest identity | distribution producer; `velnor-apt` owner; `homebrew-velnor` owner; package/install test owner | Required release/install cannot be marked N/A by a row. APT/Homebrew are subordinate records bound to one parent digest. |
| G3 | New live snapshot after migration; all 32 generator/config/pin/output identities; every current migration PR and resulting main hosted run; all required checks and workflow graph | fleet migration owners; G0 records owner; hosted execution owner | Exact current PR and main coverage for all 32; no stale baseline tuple, omitted category workload, or unverified generated output. |
| G7 | Fresh live reconciliation of branches, PRs, rulesets, workflows, runs/jobs/checks/children, package channels, pins, runtime/host identities; both lanes/parity where eligible; canonical release/install; independent attestation | root final collector; all above producers; separate G7 reviewer | `--live` is mandatory. External attestation binds report digest plus exact manifest/snapshot IDs. Offline fixture success is never G7. |

G4/G5/G6 are not skipped: the same v2 records need typed Velnor
host/provisioning evidence, hosted counterpart parity, installed pilot identity,
native-only applicability, and one publisher. They remain incomplete unless
their owners emit those facts and the checker verifies them.

## API permissions and fail-closed limits

The live collector uses the existing read-only `FleetHttp` transport. The token
must be able to read repository metadata/default branches/commits, workflow
files, pull requests, Actions workflow runs/jobs, check runs, and repository
rulesets including inherited rulesets. Record only safe scope names; bearer
tokens, auth headers, and secrets never belong in provenance.

Hard failures, not warnings or empty fallbacks:

1. HTTP 401/403/404, rate-limit responses, transport failure, null/malformed
   JSON, inaccessible ruleset details, or missing required app IDs.
2. Any paginated endpoint whose next page cannot be fetched, whose response is
   truncated at the collector cap, or whose array/object shape is wrong. One
   `per_page=100` page is not complete evidence.
3. A run with missing status/conclusion, a check with missing app/run/job/source
   identity, a job without actual ID/labels/status/conclusion, or a workflow
   whose immutable content revision cannot be fetched.
4. A current PR/main SHA mismatch between fresh API facts and supplied
   snapshot/evidence. Capture time is provenance, not freshness proof; revision
   comparison is required.
5. A provider/host mismatch. GitHub-hosted requires observed hosted runner
   identity and rejects self-hosted labels; Velnor requires a trusted
   Velnor-managed host/provisioning binding and rejects `github-hosted`. Runner
   title, actor, or a user-entered host string is not binding.

### Child-run association

`GET /actions/runs` filtered by SHA/event does not, by itself, expose a parent
run ID. Matching SHA, workflow name, timestamp, or “newest run” is not a parent
relation. The collector therefore requires explicit `parent_run_id` from a
supported source and verifies both run IDs/attempts, repository, workflow path,
event, source SHA, provider, status, conclusion, and URL against API objects.

Legitimate association sources are limited to:

- a `workflow_run` child event whose payload/context records triggering
  `workflow_run.id`, followed by read-only API verification of both runs;
- a parent workflow API-visible dispatch/output/artifact carrying a typed child
  run ID, when the collector validates immutable artifact/run identity and
  source; free-form log prose is insufficient;
- reusable `workflow_call` work inside the same run/job graph. It is not a
  separate child run and must not be invented as one.

`workflow_dispatch` list results do not establish parentage. If GitHub does not
expose `parent_run_id` through an allowed API/artifact path, the child edge is
unknown and the required workload fails closed. Do not drop the child, replace
it with a count, or accept a caller-supplied link.

## Migration sequence and stop conditions

1. The G0 records owner publishes the reviewed `manifest-v2.json`. It must be
   derived from the reviewed workload matrix/source revision, contain the exact
   checker scope once each, and bind generated plans, workflows, pins, models,
   and platform/provider responsibilities. A hand-edited “all 32” list is not
   an authority.
2. The fleet collector publishes a new `snapshot-v2.json` after the manifest
   is fixed. It must enumerate every page for all 32 repositories, current
   default tips, all current open PR tuples, rulesets/status checks, workflow
   revisions, and the main/PR run graph. Reconcile the snapshot against fresh
   read-only API responses at gate time; capture time alone never proves
   currentness.
3. Execution owners publish `records-v2.json` keyed by the exact manifest and
   snapshot IDs. They must emit actual run IDs/attempts, status/conclusion,
   source/event/checkout, check app IDs, job IDs/status/conclusion/provider and
   host bindings, plus every planned child edge. Expected facts are read from
   the reviewed plan and live workflow graph, not copied from result rows.
4. The distribution owner publishes one canonical application manifest and
   external digest. APT/Homebrew/install owners publish typed projections that
   reference that parent digest and preserve target/component/version/path,
   service, upgrade, channel, and binary/digest identity. No release or install
   row may be converted to `not_applicable` to avoid a required test.
5. Run deterministic offline fixture checks only for schema and mutation
   coverage. Then run the same checker in live mode; live mode must refresh and
   reconcile GitHub state. Offline success is useful test evidence but cannot
   declare G7, or any “current live” completion.
6. An independent reviewer mutates authoritative source, snapshot, and
   producer records separately (including omission, rebinding, stale-SHA,
   provider, child, required-install, and nine G2 hostile cases), confirms each
   mutation fails, and attests the report digest and exact input IDs. A producer
   self-attestation or a boolean `success` field is not sufficient.

Stop and report a blocker, without substituting a count or weakening a gate,
when any required API page, ruleset, workflow revision, run/job/check identity,
current PR tuple, provider/host binding, child parent edge, canonical release
projection, or reviewer attestation cannot be independently obtained. A race
between PR collection and reconciliation also requires a fresh snapshot. The
only honest result while these inputs are absent is “not proven.”

## Ownership handoff

The G0 records owner owns the reviewed plan, inventory/access model, effective
runtime metadata, and manifest digest. The fleet collector owns the fresh
read-only repository/PR/ruleset/workflow snapshot. The execution-health and
jobs/providers owners own source-backed workload responsibilities and actual
run/check/job/host facts. The distribution producer owns the canonical release
manifest; APT, Homebrew, and install-test owners own subordinate typed records.
The final collector binds those records and performs live reconciliation; the
independent reviewer owns the external attestation. None of these roles may
replace another role’s authoritative facts with a copied ledger field.

This file is a migration handoff, not a result ledger. It deliberately leaves
all IDs, digests, counts, conclusions, and gate values as observed-value
placeholders until the named owners emit and the live collector verifies them.
