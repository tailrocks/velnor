# Independent G1 review: G0 run-metadata capture

Date: 2026-09-20
Reviewer: G1 independent source/evidence review
Disposition: **APPROVE bounded raw-observation integrity and child-graph bookkeeping only.** This is not approval of a G0/G3 gate, execution, checkout, release, or merge claim.

## Frozen identity

Review target:

`G0/g0-run-metadata-capture-20260920T065449Z/`

The first freeze was the raw manifest:

```text
raw-manifest.json
SHA-256 6cc98c2886ad23a72bd17cf72f0fb5cea9038da8f01090579360affe62d53637
```

Related immutable evidence hashes:

```text
capture-metadata-corrected-v2.json  67257ad285784a8b976ffdda0862bceff0e68a47b1c84635776f2166fd70e39f
integrity.v2.json                   d729b6a741da0a6de27db6d3e8d5b2b1b779c8f1818a19d856b71798d978c973
request-ledger.json                 a541d2b1bc506c7b0b6f555fb8a91458f63aff1f549e188f908b2fd896e7b307
issues.json                         74bed084242023679fb38d80a33a1709b531de8d150d5f70fc0c3688eed33457
summary/run-observations.json       abb83e32b70ac00e16ab4b48d0f84165113d4811822433327b6ef06f30101359
summary/child-graph.json            0e8ba80815a2906899e468c53272e00f69f136b54a28e4c53e9f0b78f10f91ed
collector                         7f31393a7f177b540d82cdab28cc056362f623f839c4eb098a4cbc0e434b51f8
summarizer                        7494ad7c215ca4965749ef42f1b81465ddf08247d1b32219d7bc277265375ed7
canonical scope                   75000aca431f4e195a39f605eea846bc0ba6aed58e3bbc38e94e42009b12b269
```

The manifest says `status=raw_observation_only_not_gate`, `request_count=2335`, `body_count=2335`, `http_count=2335`, `stderr_count=2335`, and `all_triplets_unique=true`. Recomputed manifest SHA matched the frozen value before the remaining checks.

## Independent checks

1. Raw completeness and hashes

   - `find .../raw -type f` returned 7,005 files.
   - Manifest path expansion returned 7,005 body/HTTP/stderr paths; missing files: `0`.
   - Parallel recomputation of every manifest body digest and byte count: `body_entries=2335 missing=0 hash_or_size_mismatches=0`.
   - Request ledger is an array of 2,335 records; status counts are HTTP 200: 2,325 and HTTP 404: 10. Every record is valid JSON. All ten 404s are the typed workflow-directory observations retained by the capture, not converted to success.

2. Pagination and query contract

   - Check-run requests are generated with `filter=all`; status requests are separate endpoints without that parameter (collector lines 179–182).
   - 172 target rows are unique `(repository, role, revision)` tuples. The ledger has 352 fresh-check/status requests: 180 check-run page requests all containing `filter=all`, and 172 status requests. The request seed records all 172 target revisions.
   - Page-sequence audit: 344 check families and 392 job families; non-contiguous pages `0`, non-final pages missing `has_next=true` `0`, final pages with `has_next!=false` `0`.
   - The ten 404s are the only non-OK requests; no 401/403/429/5xx or malformed JSON was emitted.

3. Run, attempt, job, and child binding

   - Collector code reads each run's `run_attempt`, loops `n=1..attempts`, fetches `/actions/runs/<id>/attempts/<n>`, then fetches jobs for that exact attempt (collector lines 196–211). It does not blindly fetch only a latest attempt.
   - Captured set: 392 run bodies, 392 attempt bodies, every observed `run_attempt=1`; 3,882 jobs; 3,882 check-run records.
   - Independent set comparison: check action-run IDs `392`, child-graph runs `392`, run observations `392`, attempt observations `392`; all three run-ID set comparisons match. All check records are `github_actions_workflow_run`; non-Actions `0`.
   - Check records: unique check IDs `3882`, missing action job/run IDs `0`, check ID versus action job ID mismatches `0`. Job observations: unique job IDs `3882`, missing producer run IDs `0`. Child graph: 3,882 job IDs, 3,882 producer bindings, missing head SHA `0`.
   - Every child source workflow has `source_ok=true` and `listed_sha==observed_sha`. The 1,023 unique referenced source/directory paths resolve to capture roots: 112 fresh and 911 historical, missing or malformed `0`. This correctly accounts for the historical source root instead of assuming every relative path belongs to the successor capture.

4. Artifacts and logs

   - 162 artifact rows; all 162 digests are `sha256:`; one is explicitly expired; every row has a workflow-run object and matching producer run ID; no artifact producer run is outside the captured 392-run set. Archives were not downloaded.
   - 392 run rows have `logs_url`; `log_api_called=false` and `archives_downloaded=false` for all rows. This is `unknown_not_fetched`, not “no logs” and not a failed log check. The report records one earlier exploratory manual logs request streamed to terminal stdout and not retained; the bounded successor ledger made zero logs requests.

5. Auth identity and stale-head protection

   - `/user` identifies principal `donbeave`, numeric ID `139017`, type `User`; no token, bearer, or authorization value is present in the principal JSON, and a raw-capture scan found no `ghp_`, `github_pat_`, `Bearer`, or `Authorization:` credential pattern.
   - Before/after identity rereads cover all 32 repositories. `identity/reconciliation.json` says `same_identity=false`; normalized before SHA `d42f9da3030f4b5a9aa67209ea7f9e4359fad58c6f75eaa257f35d322639a2c9`, after SHA `1416e6a49b57371029ad8e4f83d1c1a5166989924612c7ee3df474e744aa65d2`.
   - Churn is explicit: successor-before to after changes default heads for `jackin-project/homebrew-tap` and `tailrocks/velnor`, plus PR changes for `jackin-project/jackin#1004`, `tailrocks/velnor#962`, and `tailrocks/velnor#966`; historical-before to successor-before records seven PR changes. The target/check set remains frozen against the before snapshot and is not silently replaced with after heads.

## Findings and limits

- No integrity blocker found for this bounded capture. The ten workflow-directory 404s, one expired artifact, absent runner OS, and log unknowns are represented as observations/unknowns rather than hidden success.
- Provenance correction is required when consuming the artifact: the original embedded collector path points at a nonexistent `G0/fleet/...sh` and has an empty script hash. `capture-metadata-corrected-v2.json` records the actual collector path and SHA above. The raw capture itself is unchanged, and the actual collector file was independently hashed.
- Capture code and artifacts establish API observations and identifier bindings only. They do not prove a local checkout, source execution, successful producer job semantics, release validity, install behavior, or any gate criterion. The capture report explicitly states this boundary (report lines 3 and 99; collector metadata lines 239–243).

## Final verdict

**Approve** this exact, frozen artifact for use as bounded G0 raw API/child-graph evidence and source-review bookkeeping. **Do not promote it to a G0/G3 gate or execution claim.** Preserve the raw manifest SHA and the explicit log-unknown, 404, identity-churn, provenance-correction, and no-checkout limitations when integrating it.
