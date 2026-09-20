# G0 run metadata / child-graph raw capture — 2026-09-20T06:54:56Z..07:58:56Z

Status: complete bounded read-only observation. **Not a G0/G3 gate result.** No dispatch, check write, source mutation, build, install, log download, or API-head-as-local-checkout assertion.

## Scope and provenance

- Canonical scope: `G0/fleet/workload-contract-index-20260920T000017Z.json`
- Scope SHA-256: `75000aca431f4e195a39f605eea846bc0ba6aed58e3bbc38e94e42009b12b269`
- 32 repositories; before/after identity reread completed for all 32.
- Collector: `G0/g0-run-metadata-capture-20260920T062830Z.sh`
- Collector SHA-256: `7f31393a7f177b540d82cdab28cc056362f623f839c4eb098a4cbc0e434b51f8`
- Summarizer SHA-256: `7494ad7c215ca4965749ef42f1b81465ddf08247d1b32219d7bc277265375ed7`
- Corrected provenance: `capture-metadata-corrected-v2.json`. The frozen collector metadata has an empty script hash because its embedded path incorrectly named nonexistent `G0/fleet/...sh`; raw capture is unchanged.
- Raw output: `G0/g0-run-metadata-capture-20260920T065449Z/`

## API phases and results

| phase | requests | OK | non-OK |
|---|---:|---:|---:|
| auth `/user` | 1 | 1 | 0 |
| identity-before | 96 | 96 | 0 |
| fresh checks/statuses | 352 | 352 | 0 |
| fresh missing workflow sources | 219 | 209 | 10 |
| run detail | 392 | 392 | 0 |
| run attempts | 392 | 392 | 0 |
| jobs pages | 395 | 395 | 0 |
| artifact pages | 392 | 392 | 0 |
| identity-after | 96 | 96 | 0 |
| **total** | **2335** | **2325** | **10** |

All 10 non-OK requests are retained HTTP 404 workflow-directory observations, not silently treated as success:

- `tailrocks/homebrew-velnor`
- `tailrocks/tailrocks-code-quality-skills`
- `tailrocks/tailrocks-macos-skills`
- `tailrocks/tailrocks-open-source-skills`
- `tailrocks/tailrocks-pull-request-skills`
- `tailrocks/tailrocks-roadmap-skills`
- `tailrocks/tailrocks-rust-skills`
- `tailrocks/tailrocks-skill-authoring-skills`
- `tailrocks/tailrocks-typescript-skills`
- `tailrocks/termrock`

Issue manifest: 1,202 rows = 1,192 pagination summaries + 10 typed 404 issues. No 401/403/429/5xx; every successful response was valid JSON and no malformed-list issue was emitted.

## Fresh checks and workflow binding

- 172 fixed `(repository, role, revision)` targets from the before identity.
- Every check-runs endpoint used `filter=all` and was paginated; status endpoints were separately paginated (they have no `filter=all` parameter).
- 3,882 check-run records; all 3,882 had GitHub Actions run URLs; non-Actions count is explicitly 0, not fabricated.
- 392 unique workflow run IDs across 22 repositories.
- 1,500 combined workflow-source records across 174 immutable repository/revision pairs; fresh source records were fetched for revisions absent from the historical source inventory, and historical source records were retained and combined.
- Producer binding uses repository + run ID + check IDs; source binding uses repository + revision + workflow path/blob metadata. Child links do not rely on display names.

## Run, attempts, jobs, artifacts, logs

- 392/392 run detail bodies.
- 392/392 attempt bodies; all observed `run_attempt=1`.
- 3,882 job records from 395 paginated jobs requests; all 392 run IDs have bound jobs in `summary/child-graph.json`.
- Runner labels/name/group/id are preserved. API supplied no `runner_os`; `runner_os_inferred=false` for every row.
- 162 artifacts; all 162 carry a SHA-256 digest; one is expired. Artifact metadata only; archives were not downloaded.
- 392 run rows contain a `logs_url` link. `log_api_called=false` and `archives_downloaded=false` for every row; log availability remains unknown.
- Two nonempty commit-status observations were retained separately in `summary/status-observations.json`.
- Before this successor began, an exploratory manual `gh` request against one historical run's logs URL streamed a response to terminal stdout; it was not saved or included. The bounded successor itself made zero logs requests and downloaded zero archives.

## Identity and churn

`identity/reconciliation.json` is `same_identity=false`.

Successor before → after:

- default SHA changes: `jackin-project/homebrew-tap`, `tailrocks/velnor`
- PR changes: `jackin-project/jackin#1004`, `tailrocks/velnor#962`, and `tailrocks/velnor#966` disappeared between rereads

Historical capture before → successor before has seven PR changes:

- known prior five: `jackin-project/jackin#1002`, `#1004`, `#1006`; `tailrocks/velnor#962`, `#968`
- additional observed churn: `jackin-project/jackin#1005`, `tailrocks/velnor#966`

All current-before source/check targets were captured against the frozen before snapshot; after-head churn is reported, not retroactively substituted into the captured run set.

## Machine artifacts

- `capture-metadata-corrected-v2.json`
- `integrity.v2.json` (adds the pre-successor exploratory logs note; `integrity.json` is preserved)
- `raw-manifest.json` (2,335 unique body/HTTP/stderr triplets; all hash-indexed)
- `identity/churn.json`
- `seeds/check-targets.json`
- `seeds/check-run-records.json`
- `seeds/run-index.json`
- `summary/run-observations.json`
- `summary/attempt-observations.json`
- `summary/job-observations.json`
- `summary/artifact-observations.json`
- `summary/log-availability.json`
- `summary/status-observations.json`
- `summary/child-graph.json`

Interpretation boundary: statuses/conclusions, artifacts, runner labels, workflow paths, and logs links are API observations only. They do not prove execution, checkout identity, release validity, install functionality, or any gate criterion.
