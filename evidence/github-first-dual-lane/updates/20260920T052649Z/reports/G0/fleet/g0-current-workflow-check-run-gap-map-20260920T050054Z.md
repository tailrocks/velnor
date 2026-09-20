# G0 current workflow/check/run gap map

Status: **inventory-only; no G0/G1 gate claim**.

Capture: 2026-09-20T05:00:54Z. Scope is the immutable exact-32 index, not the stale 28-row configuration. No GitHub API call, workflow dispatch, scan, install, publication, source edit, or remote write was made in this checkpoint.

## Inputs and identity

- Exact scope: `G0/fleet/workload-contract-index-20260920T000017Z.json`, SHA-256 `75000aca431f4e195a39f605eea846bc0ba6aed58e3bbc38e94e42009b12b269`.
- Current default refs and PR tuples: `G0/fleet/live-default-branches-open-prs-20260920T035717Z-pr-reread-final.json`, SHA-256 `1c844f5c09a6d9d6a5c24f1b36c410a7190eb63a2bc6d2893268e469d9839dee`. It has 32 final refs, 87 final open PR records, 64 retained initial/final raw PR pages, and no opening/closing churn.
- Raw PR page manifest: `G0/fleet/live-default-branches-open-prs-20260920T035717Z-raw-pages/manifest.json`, SHA-256 `88d150e78af354645eac07d31cede2163cb80e75a2ff86e9ef78efa46d3ed30a`. This is reused, not copied.
- Historical source/workload projection: `G0/fleet/workloads-full.json`, SHA-256 `32aa854684af045b6b03798110990b75fda9369c67b7c742b31a2e318d318c52`, captured `2026-09-19T17:05:57Z`.
- Historical ruleset/check/run summary: `G0/fleet/check-contexts-full.json`, SHA-256 `1a51f8a276c912c6f03f3bcb749d1f5845b52c0551cdff6254c8e90c0c78901e`, captured `2026-09-19T19:15:10.439Z`–`19:28:02.940Z`. It has summary pagination/API errors, not retained response bodies.

## Exact findings

Current-vs-historical reconciliation:

- Current refs: 32/32 available. Three changed after both historical projections: `tailrocks/velnor`, `jackin-project/jackin`, and `jackin-project/homebrew-tap`. Their old workflow/check/run facts are stale by source SHA.
- Current PR census: 87. Historical check census: 76. Tuple intersection is 74; 13 current PRs have no historical check/run row and 2 historical rows are no longer current (`tailrocks/velnor#954`, `jackin-project/jackin#975`).
- Historical required contexts: 54 across 32 repositories; 0 carry an app/integration ID. Current ruleset/app binding is therefore unknown.
- Historical check runs: 1,268 main + 1,742 PR = 3,010. Historical workflow runs: 188 main + 184 PR = 372. These were latest check-run/list-run summaries, not a complete current run graph.
- Jobs, run artifacts, job-log bytes, and parent/child edges: 0 retained in the historical artifact.
- Current workflow source bytes/revisions/trigger/dependency graph: 0 retained in a current collector snapshot.
- Workload projection has 115 expected IDs (86 unique), 50 platform rows, 22 native-exclusion rows, and 0 dependency edges; all 32 dependency statuses are `unknown`. Empty/unknown is preserved, never converted to success.

Thus every repository row still lacks the current workflow/check/run surfaces listed in the machine-readable artifact. The full 32-row details, counts, stale-SHA flags, and field names are in:

`G0/fleet/g0-current-workflow-check-run-gap-map-20260920T050054Z.json`

SHA-256: `57d0bbbbe8159f8e5c843681260a88f250498700589034923634e78eacf2fb8c`.

## Collector decision

The existing collector source was inspected at `/private/tmp/g3-live-collector`, HEAD `b6d813a696e2b6054e2880c3eaa87e0a6ab14fbf`. The worktree is dirty at `crates/velnor-tools/src/g0_contract.rs`; no exact executable was built or run. No reviewed `manifest-v2.json`, model-session config input, or built collector binary is present.

The CLI source (`github_live_cli.rs`) writes `live-collection.json`, `binding-capture.json`, metadata, and raw objects, but does not invoke the canonical G0 mapper or emit checker `snapshot-v2.json`/ `records-v2.json`. The latest exact source review `G0/runtime/live-collector-ccd2144-review.md` (HEAD `ccd2144e5685f875cefc0241dabf4275b5b0692d`) approves only the bounded deserialization fix and explicitly rejects authoritative live-G0 use. Running this dirty/unapproved path would create an unbound or misleading capture, so no live collection was attempted.

## Required dependency order

1. Produce and independently review `manifest-v2.json` from the exact scope/workload contract.
2. Review a clean collector revision and authenticated producer/raw store.
3. Capture opening/closing default refs and PR tuples plus active rulesets/apps, workflow source graph, all run attempts, suites/checks, jobs, artifacts/logs, child links, raw bodies, pagination/error/rate/request ledgers.
4. Bind source-derived workload/platform/dependency expectations to that snapshot; produce `snapshot-v2.json`.
5. Produce execution-owned `records-v2.json` with exact run/check/job/provider/checkout identities.
6. Run checker ingestion against all three immutable artifacts. Existing historical v1 summaries and caller-owned green fields cannot substitute.

Machine-readable status is `inventory_only_not_gate`; no gate is closed.

