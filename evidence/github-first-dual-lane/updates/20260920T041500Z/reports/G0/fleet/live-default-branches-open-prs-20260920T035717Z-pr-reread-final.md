# Live fleet default-branch and PR reread inventory

Status: complete read-only inventory. This is not a G0/G3 gate result and contains no workload/build/install/dispatch/publication claim.

## Scope and accepted successor

- Canonical source: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/fleet/workload-contract-index-20260920T000017Z.json`
- Canonical SHA-256: `75000aca431f4e195a39f605eea846bc0ba6aed58e3bbc38e94e42009b12b269`
- Exact scope: 32 repositories. Stale 28-repository configuration was not used.
- Accepted JSON: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/fleet/live-default-branches-open-prs-20260920T035717Z-pr-reread-final.json`
- JSON SHA-256: `1c844f5c09a6d9d6a5c24f1b36c410a7190eb63a2bc6d2893268e469d9839dee`
- Capture interval: `2026-09-20T03:57:17Z`–`2026-09-20T04:00:39Z`
- Prior corrected snapshot preserved and superseded by this successor: `live-default-branches-open-prs-20260920T034032Z-corrected.json`, SHA-256 `a642f2e968931d92c6e16f283f6ce46b65985b744fa3e448b75c6993ab4de33e`.

All paths above are under `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/`; the shorter `/Users/donbeave/Projects/tailrocks/dual-lane-evidence/` path is incorrect.

## Collection and retained raw evidence

- Metadata: live `GET repos/{owner}/{repo}` before and after each repository.
- Default ref: live `git ls-remote --refs https://github.com/{owner}/{repo}.git refs/heads/{default_branch}` before and after each repository. Local cached `origin` was not authoritative.
- PR pages: live `GET repos/{owner}/{repo}/pulls?state=open&per_page=100&page={n}` twice per repository. No draft, author, bot, head-repository, fork, or other PR filter was applied. Pagination continued until a page had fewer than 100 records.
- Raw page root: `/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G0/fleet/live-default-branches-open-prs-20260920T035717Z-raw-pages/`
- Raw page manifest: `manifest.json`, SHA-256 `88d150e78af354645eac07d31cede2163cb80e75a2ff86e9ef78efa46d3ed30a`
- Retention: 64 full response-body files (32 initial + 32 final), 4,014,031 bytes total. Each manifest entry records phase, repository, endpoint, page, timestamp, raw body path/SHA-256, normalized SHA-256, and count.
- Independent hash check: all 64 manifest body hashes matched the retained files.

## Results

- 32/32 repository rows complete; 32/32 initial and final metadata/ref reads complete.
- 32/32 initial and 32/32 final PR page passes complete; 87 initial and 87 final records.
- All default branches were `main`; no default-branch or default-ref SHA churn.
- Repository-keyed PR reconciliation: same open set; added `[]`; removed `[]`; head changes `[]`; base changes `[]`; merge-commit-SHA changes `[]`; no other normalized PR record changes.
- Direct raw-body checks, independent of normalized records, found both passes: 87 `open` states; 6 `draft:true`; 81 `draft:false`; 0 missing draft flags; 87 `user.type:User`; 87 `head.repo.fork:false`; 0 fork/null or bot identities observed.
- “No bot observed” is a live snapshot result, not proof that the endpoint cannot return automation identities. The endpoint had no identity filter.
- API/access/rate/pagination errors: none. Rate snapshots are recorded in the JSON (core remaining 5000 initially/finally; search remaining 30 initially/finally).

## Synthetic normalizer check

Separate positive fixture, not live evidence:

- Fixture: `raw-pages/synthetic-normalizer-fixture.json`, SHA-256 `1d906420e63a636d9db55aff653d6f44e2ee55aac368110eb7498e33a1bfc378`
- Output: `raw-pages/synthetic-normalizer-output.json`, SHA-256 `f1e8e56ac3071c7083c7c0ccc243573be0af6c9598ca62d37820c54427f258d6`
- Passes that `draft:false`, `locked:false`, `maintainer_can_modify:false`, `user.type:Bot`, `head.repo.fork:true`, and `merge_commit_sha:null` survive normalization.

## Preservation and boundaries

- Original same-run candidate and prior snapshots remain untouched.
- No source checkout, build, test, install, dispatch, release, publication, PR mutation, or rules mutation occurred.
- Evidence is inventory/reconciliation only; it does not establish workload correctness or any G0/G3 gate.

