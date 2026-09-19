# V2 collector contract gap audit

Status: **not approved; no gate verdict**. This is a bounded, read-only
provenance audit. It does not expand the live collector and does not convert
the existing fleet snapshot into V2 evidence.

Audit runtime: `gpt-5.6-luna`, reasoning `max`.

## Exact inputs

| Input | SHA-256 | Schema/revision | Role |
| --- | --- | --- | --- |
| `G0/fleet/check-contexts-full.json` | `1a51f8a276c912c6f03f3bcb749d1f5845b52c0551cdff6254c8e90c0c78901e` | `g0-fleet-check-contexts-full/v1`, 5,778,314 bytes | Existing live compact snapshot under audit |
| `G0/checker-review/report.md` | `0df74945c639900f752bd9d7dafa88cac368fdaca53b86ddc2c9ae4b6477ad04` | exact user-requested independent review | V2 authority/gap source |
| `G0/checker-v2-handoff.md` | `65b50a6a7344d9f0ee7520e6586e339279dde39d8ed49eed680182ac836e140a` | migration handoff | Snapshot/run/API contract |
| `G0/checker-v2-review/acceptance-matrix-audit.md` | `17ae30b6848fe663132d2a237972ef88c3777964e2e285eb94ca3184fcfa4f36` | exact acceptance audit | Collector limitations and negative cases |

The audited snapshot says `schema: g0-fleet-check-contexts-full/v1`, covers 32
repositories and 76 current open PR records, and reconciles its start/end
default refs and PR lists without observed churn. Its capture window is
`2026-09-19T19:15:10.439Z` through `2026-09-19T19:28:02.940Z`. Those facts are
useful observations only; the file is not `snapshot-v2.json` and is not a gate
input.

## Result

The snapshot has broad REST check-run coverage but fails the V2 provenance
contract in every authority class that matters for a fail-closed checker:

- REST-only collection through `gh api`; no GraphQL request, operation,
  variables, cursor, `pageInfo`, or GraphQL response capture is stored.
- Compact projections only. No raw REST/GraphQL response bytes, canonical raw
  object hashes, media types, byte lengths, or raw-object references exist.
- No per-request query/auth identity, request ID, rate-limit state, retry/error
  state, or endpoint completion ledger exists.
- No `/check-suites`, `/actions/runs/{id}/jobs`, artifact, or job-log collection
  exists. A `jobs_url` string is present on workflow rows, but no jobs are
  fetched. Artifacts and logs are absent.
- No immutable workflow-file/content revision graph, reusable-action/scanner
  inventory, generated-state source, or trigger graph exists.
- PR head checks are collected, but tested-merge observations are empty for
  all 65 PRs with a merge SHA. There is no `merge_group_sha`, exact PR event
  binding, checkout proof, or explicit child-run parent edge.
- Ruleset required checks have `integration_id`/`app_id: null`; observed
  check-run Apps are not reconciled to the independently read policy. The
  ruleset list pagination metadata is hard-coded to one page and per-ruleset
  list timestamps are null.
- The single top-level account string (`gh auth active account
  (read-only API)`) is not an observed viewer identity or safe-scope set.
- End reconciliation checks refs and PR identities only. It is not the
  required post-evidence default/PR reread for G0/G3/G7 records and does not
  revalidate rulesets, workflow revisions, check suites, jobs, artifacts, or
  child graphs.

Therefore: **not sufficient for checker V2; no G0/G3/G7 pass and no newest-
green or label-count claim.**

### Bounded API probes (not snapshot evidence)

Two small read-only GraphQL probes were run only to verify the required API
shape; neither response was merged into the snapshot. They demonstrate why
the future collector must retain query/page/auth/raw metadata:

- PR probe: query SHA-256
  `1a0b056c266cdddfc0b92193b76147917d649faf03c07c398c08e6c56460353d`
  (446 bytes), response SHA-256
  `6b1ccd681729cae7129cd04e97c0a3a7268711103a997c361c7c88e83a624773`
  (1,104 bytes). Viewer was `donbeave` (`MDQ6VXNlcjEzOTAxNw==`); GraphQL
  rate-limit response was limit 5,000 / remaining 4,992 / cost 1. The
  `tailrocks/velnor` first-two-PR query returned `pageInfo.hasNextPage=true`
  with cursors, proving that a small query cannot be treated as complete.
- Ruleset probe: query SHA-256
  `a89059ca657988e5bb7b55d36f6f404274a49384ca249814b8abeb3d6f1b9663`
  (221 bytes), response SHA-256
  `80cebf63a8719a46cdcf90b9ea03fb360c09817bb9397a6936635359f78d5541`
  (403 bytes). The response had `pageInfo.hasNextPage=false` and two ruleset
  nodes. This is probe evidence only; it does not supply missing inherited
  rules, required Apps, or raw captures for the 32-repository snapshot.

## Actual coverage versus V2 requirements

| Required authority | Existing snapshot observation | V2 gap / trust consequence |
| --- | --- | --- |
| Fixed 32 scope and live default tips | 32 repository keys, default branch/SHA, repository IDs; start/end ref reread | No `manifest_id`, immutable manifest digest, snapshot ID, per-request raw binding, or canonical exact-scope authority. A file hash alone does not bind each row to independently captured bytes. |
| Current open PR census | 76 rows; draft/author/head repo/head SHA/base SHA/merge SHA captured; 3 drafts, 0 bots/forks in this snapshot | No `merge_group_sha`; no typed trust/applicability; no PR event/check-suite association; 11 tested-merge SHAs are absent; currentness is only one before/after list comparison. |
| Active rulesets and required checks | 45 rulesets and 54 required contexts from REST ruleset details; 31 branch-protection 404s explicitly recorded as `not_protected` | No GraphQL/inherited/org ruleset source; all policy App IDs null; no ruleset page cursors/raw responses; one hard-coded list page; no equality proof between policy-required App/context and observed check. Unexpected 404 is not typed separately from intentional branch-unprotected 404. |
| Check suites/check-runs/statuses | 1,268 main check-runs and 1,742 PR-head check-runs; check ID, suite ID, App ID, status/conclusion, URLs, source SHA projection; only 2 PR status rows were returned | No check-suite endpoint object, suite PR association, suite App/source/event binding, job ID, workflow-run API object hash, or exact required-check producer relation. `external_id` is retained but is not a job proof. Most status arrays are empty, but no independent endpoint-completion/raw ledger makes empty-vs-unknown fail closed. |
| Workflow runs/events | 188 main and 184 PR-head workflow runs; run ID/attempt, workflow ID/path/name, event, head SHA, status/conclusion, actor and URL | No workflow-file fetch/revision/content hash per run source; no complete trigger/reusable/action/scanner/generated graph; no checkout proof. Head SHA is not actual checkout. Scheduled/manual runs cannot be promoted to required push/PR evidence. |
| Jobs and child graph | Only `jobs_url` on workflow rows; `workflow_run_ids` are heuristic URL/suite matches for check-runs | No `/jobs` pages, job IDs/names/status/conclusions/labels/runner identity, logs, children, parent_run_id, recursive descendants, or producer-bound child edge. SHA/time/path matching is not lineage. |
| Artifacts and logs | No artifact or log records | Missing `/actions/runs/{id}/artifacts`, artifact page completeness, run/job binding, artifact digest, log fetch/body hash, and required-log proof. An arbitrary HTTPS URL must never satisfy this. |
| Pagination | `gh api --paginate --slurp`; pages/items retained for check-runs/statuses/workflow-runs. Main check-runs used 1–3 pages; PR check-runs used 1–2 pages; two PR cases exceeded 100 | No REST Link/page cursor or GraphQL `pageInfo`/cursor records, query identity, raw page hashes, cap/truncation flag, or per-endpoint completion status. Rulesets metadata is hard-coded to one page. Any page error, cap, malformed body, or omitted page would be indistinguishable from a compact partial result. |
| Permission/rate-limit/transport handling | 31 branch-protection HTTP 404 bodies retained; no 401/403/rate-limit errors in this run | No per-request auth/rate-limit observations; no typed `unknown`/`forbidden`/`rate_limited`/`truncated` state for every endpoint; no raw error response hash. Branch-protection 404 is an API state, not proof that every 404 is safe absence. |
| Auth/source identity | API base/version and a non-authoritative account label | Missing observed REST viewer/token owner, GraphQL viewer ID, safe scopes, API request IDs, collector revision, query/variables hashes, and source response identity. Secrets must remain excluded. |
| G0/G3/G7 freshness | One collection plus end ref/PR-list reconciliation | Required post-evidence reread must fetch all relevant defaults and PR tuples again and invalidate changed rows. Rulesets, workflows, suites, jobs, artifacts, and policy must also be reread or explicitly remain unknown. |

The existing pagination is real enough to show that two check-run endpoints
needed multiple pages, but page counts are not provenance. They cannot prove
that all REST and GraphQL pages, subresources, or cap boundaries were handled.

## Missing authoritative connections

The next collector must emit and the checker must verify these edges, not infer
them from names, labels, counts, or URLs:

1. `manifest -> repository -> default_branch/default_tip` with repository ID,
   exact request/page/raw-object references, and pre/post reread equality.
2. `repository -> ruleset -> required_status_check -> required_app` including
   ruleset source/inheritance and policy applicability.
3. `repository -> workflow_file -> immutable content revision -> trigger`
   including all reusable/action/scanner/generated-state dependencies.
4. `PR -> head/base/tested_merge/merge_group` with draft/bot/fork/trust and
   applicability state; merge candidates must not be replaced by contributor
   head runs.
5. `source tuple -> check_suite -> check_run -> workflow_run -> job` with
   repository, source SHA, event, App ID, status, conclusion, run/attempt,
   job ID, and canonical API object references.
6. `workflow_run -> parent_run_id -> child_run` only from an allowed explicit
   producer relation; same SHA, timestamp, path, or newest run is insufficient.
7. `job/run -> artifact/log` with complete pagination, immutable content hash,
   source run/job binding, and required expected-object closure.
8. `workload -> expected_job/check -> source run -> provider/host` from the
   reviewed plan, never from result rows or self-authored expected lists.

## Minimal typed collector handoff

This is the smallest contract the live collector should hand to the checker;
it is a design boundary, not an implementation request in this audit.

```text
CollectorSnapshotV2 {
  schema_version: 2,
  snapshot_id: Digest,
  manifest_id: Digest,
  phase: G0 | G3 | G7,
  observed_at_utc: Timestamp,
  completed_at_utc: Timestamp,
  collector: {name, revision, mode: read_only, api_base, api_versions},
  auth: {provider, viewer_id, viewer_login, safe_scopes, secret_excluded},
  rate_limit: {api, limit, remaining, used, reset_at, observed_at},
  requests: [RequestRecord],
  repositories: [RepositorySnapshot],
  reconciliation: {pre_state, post_state, changed_refs, changed_prs, invalidated},
  raw_objects: [RawObjectRef]
}

RequestRecord {
  request_id, api: REST | GraphQL, method, endpoint_or_operation,
  query_sha256, variables_sha256, redacted_variables,
  auth_identity_ref, started_at_utc, completed_at_utc,
  http_status, api_request_id, rate_limit_ref,
  page: {number, per_page, link_next, cursor_in, cursor_out,
         has_next_page, items_returned},
  response_raw_ref, state: complete | empty_complete | forbidden |
    not_found | rate_limited | transport_error | malformed | truncated | unknown,
  error_raw_ref, complete, truncation_reason
}

RawObjectRef {
  raw_id, request_id, object_kind, canonicalization,
  sha256, byte_length, media_type, storage_ref
}

RepositorySnapshot {
  repository, repository_id, default_branch, default_branch_sha,
  rulesets, workflows, open_prs, execution_graphs, access_state
}

PullRequestTuple {
  number, state, is_draft, author, head, base,
  tested_merge, merge_group, trust, applicability,
  required_check_producers, source_refs
}

ExecutionGraph {
  source_kind: main | pr_head | tested_merge | merge_group,
  source_sha, event, trigger_source_sha, actual_checkout_sha,
  check_suites, check_runs, workflow_runs, jobs, artifacts, logs, children
}
```

Rules:

- Every REST and GraphQL list connection is paged until its server-declared end;
  store each page request, cursor/Link state, raw response hash, and item count.
  A page of exactly 100 is not complete until the next page is fetched.
- Any 401/403/404 (unless endpoint semantics explicitly classify the response),
  429/rate limit, transport failure, malformed/null body, inaccessible detail,
  pagination error, or collector cap yields an explicit non-success state and
  unknown data. Never replace it with `[]`, zero, `false`, or a green label.
- `empty_complete` is legal only with a successful terminal page and raw
  response reference. It is not equivalent to `unknown`.
- Check suites, runs, jobs, artifacts, and logs are independently fetched and
  linked by immutable IDs/source; display URLs are secondary.
- `actual_checkout_sha` requires checkout evidence; Actions `head_sha` alone
  does not satisfy it. Provider/host trust is typed and independently bound.
- Before and after every G0/G3/G7 evidence set, reread all default tips and
  current open PR head/base/tested-merge tuples. Any movement invalidates the
  affected evidence. Reconcile policy/workflow/source rows too; capture time
  alone is not freshness proof.
- The checker derives required jobs/checks/children from reviewed manifest and
  workflow graph. Collector observations cannot redefine expected work.

## Required focused fixtures before approval

The checker/collector handoff needs independent failures for:

- REST page 1 = 100 plus page 2 = 1; GraphQL `hasNextPage=true`; missing page;
  malformed page; cap truncation; page-2 403/429/500.
- Missing/raw-hash mismatch, wrong query/variables hash, wrong viewer/scope,
  API request identity mismatch, and secret leakage.
- Ruleset required App mismatch; check-suite from another repository/source;
  check-run with same name but wrong App, suite, workflow, job, or SHA.
- PR draft/bot/fork omission; changed head/base; absent or unrelated tested
  merge; merge-group run substituted by contributor-head run.
- Run status queued/canceled/timed-out/failed/skipped; job or artifact omitted;
  unrelated URL; missing/empty log; child with wrong/missing parent or hidden
  descendant.
- Post-evidence default/PR SHA movement and ruleset/workflow movement. Each
  must invalidate affected rows independent of aggregate counts or labels.

## Handoff decision

`check-contexts-full.json` can seed discovery and identify API workload, but it
must be treated as a historical compact observation until a V2 collector emits
the typed request/page/raw-object graph above. Do not expand it by adding more
counts. The checker owner needs this contract before implementing live
collector consumption. No source, consumer, remote, install, dispatch, or
new large snapshot was changed by this audit.
