# Independent G1 review: real API check-suite supplement

Date: 2026-09-20
Reviewer: G1 independent evidence review

## Verdict

**Reject the supplement metadata as exact authoritative metadata until one duplicated request-record defect is corrected.** The underlying 72 raw files and the bounded relationship claims are independently consistent and may be approved as fixture evidence after metadata repair. This remains read-only provider evidence, not gate, execution, rollout, or merge authority.

## Frozen identity

Target: `G0/real-api-fixture-supplement-20260920T085311Z/`

```text
manifest.json             1146764830a0e6e31518dd05abd6c00e6627f554d6dbbc97a55d721facabe70a
relationship-report.json  c1ab3d2eb6802be4e9c67ba07b3ef76e4deeb88757ac31b20a6689224bc69e12
README.md                 8fd79fe81ebd56ccf809a3546558ca7c66d1c868573ccc199b266f07773bcaae
```

Manifest status is `read_only_source_supplement_not_gate`. It names authenticated read-only GETs only, `donbeave` as principal, and explicitly excludes source edits, dispatch, rollout, and credential values. Base-corpus identity is preserved:

```text
base manifest             d2c66c7bb61dfa6a0ddf4ea336feb8704b1c137aaba6031c2930ca58f5fae2bb
base relationship report  28b88ef07aa00166e3914f5abe8ff852c98a7fc2dfde2ca618da6d3e50983845
original Velnor checks    2f30395551e7bfad2f0271dfeaa09bd4768da3905dfa1a42785aa12d6388bf57
original Homebrew checks  c241e54220d3a3c04d5761e100be507b9c2dfec15772414b898a90957b7551b5
```

The preliminary `real-api-fixture-supplement-20260920T085207Z-incomplete-422` directory is not referenced by this supplement and contributes no request or relationship record.

## Independent integrity checks

1. Raw bytes and envelopes

   - Filesystem contains exactly 72 raw files, matching the manifest's 72 entries. All manifest paths exist; all recomputed SHA-256 and byte counts match (`72/72`, missing `0`, mismatches `0`).
   - There are 18 unique request IDs and URLs. Every request is `GET`, HTTP 200, and valid JSON.
   - Every `*.response` envelope is internally exact: response size equals `*.http` + four separator bytes + `*.body`; the HTTP prefix and body suffix compare byte-for-byte for all 18 (`size mismatches=0`, header-prefix mismatches `0`, body mismatches `0`). No synthetic body or fabricated response was found.
   - Raw scan found no `Authorization:`, bearer token, `ghp_`, `github_pat_`, or comparable token value. OAuth scopes and client ID in ordinary GitHub response metadata are not credentials.

2. Pagination and Link stream

   - Velnor commit `df9fb272c025f76cc8711560209afcdfd6cc4e00`: pages 1–3, `total_count=3`, one suite per page, IDs `96108219979`, `96108224766`, `96108227551`.
   - Homebrew commit `c501e90d014c207234ed94ea41f7a1c9b6ea0c7c`: pages 1–4, `total_count=4`, one suite per page, IDs `96059348120`, `96059348227`, `96059348318`, `96059350728`.
   - All seven list pages have contiguous page numbers and correct `has_next` behavior. Raw Link headers agree with manifest `link_header` fields for 17/18 records; all `has_next` booleans agree with raw headers.
   - The one Link metadata mismatch is the finding below. It is not a raw-header defect: the affected direct suite has no Link header in its raw `.http`/`.response` files.

3. Direct suites, Apps, and relationships

   - Seven direct suite bodies exist, and each body ID matches its relationship record and raw path. Four provider objects exist: DCO-2 (`974774`), GitHub Actions (`15368`), SonarQubeCloud (`12526`), and Claude (`1236702`). All seven edges resolve to one of those four App objects; edge count, suite IDs, and App IDs are exact.
   - Velnor's three supplemental list IDs exactly equal the three suite IDs already represented by the approved base Velnor check-runs body. Base raw bytes remain at the recorded SHA.
   - Homebrew's original base check-runs body contains only suites `96059348318` (Sonar failure) and `96059350728` (Actions success). The two newer list-only suites `96059348120` (Claude) and `96059348227` (DCO-2) are explicit additions, not backdated positives.
   - Direct bodies for those two newer Homebrew suites say `status=queued`, `conclusion=null`, and `latest_check_runs_count=0`. No jobs, check-runs, execution, or artifact data is inferred for them. The report's `artifact_run_attempt` limit remains untouched because no artifact endpoint was queried.

## Blocking metadata finding

Record: `velnor-check-suite-96108227551` in both `manifest.json` and `relationship-report.json`.

- `response_date_header` is stored as `Sun, 20 Sep 2026 08:53:22 GM`; raw `.http` and `.response` contain the complete `Sun, 20 Sep 2026 08:53:22 GMT`.
- `link_header` is stored as the tab-separated string `raw/check-suites/velnor/suite-96108227551.http\tHTTP/2.0 200 OK\tSun, 20 Sep 2026 08:53:22 GMT`. The raw header stream contains no `Link:` header, so the correct value is `absent`.
- `status_line`, URL, body, and raw hashes are otherwise consistent for this record.

This violates the manifest/README promise that request records preserve exact Link headers and makes machine-level header validation fail. It does not invalidate the untouched raw bytes or the suite/App relationship, but consumers must not treat the current metadata as exact until the two fields are regenerated from the raw header file and both machine documents are rehashed. Do not rewrite raw response/body files.

## Boundaries

The supplement correctly preserves the 08:53–08:54 UTC capture window versus the base Homebrew capture at 05:20 UTC, distinguishes authentic queued list-only suites from the earlier check-runs chain, and makes no current-gate claim. It is suitable for fixture mapper/checker regression only after the metadata defect is corrected; it proves neither live authority nor execution success.
