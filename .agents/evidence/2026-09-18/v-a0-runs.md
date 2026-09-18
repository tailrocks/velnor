# A0 verifier report (independent re-fetch)

Date: 2026-09-17 (UTC). Source under test: /tmp/a0-runs.md.
Method: fresh `gh api` metadata per run + attempted disproof of repo correction + one `--log-failed` signature grep per run.

## Metadata re-fetch (conclusion / head_sha / run_attempt)

| run | claimed | observed | verdict |
|---|---|---|---|
| tailrocks/velnor 35129353335 | failure / 3353310c…786 / attempt 1 | failure / 3353310c7648fca22698b6c0f4a69ab245127786 / 1, status completed, event workflow_dispatch, branch main | MATCH |
| tailrocks/velnor 35136207272 | failure / 62a74bf5…159 / attempt 2 | failure / 62a74bf58993c8c073d47274897f0db88cbd6159 / 2, status completed, event pull_request, branch feat/ci-immutable-runtime-products | MATCH |
| jackin-project/jackin 35114867283 | failure / 92f347ac…924 / attempt 1 | failure / 92f347ac39fbf0d6f9853168e2896a6c60522924 / 1, status completed, event push, branch main | MATCH |
| ChainArgos/java-monorepo 35094895601 | failure / 235e479b…c80 / attempt 1 | failure / 235e479b150aeb949bc8a5190fba5b84f6303c80 / 1, status completed, event push, branch main | MATCH |

## 404-repo correction

- `gh api repos/tailrocks/velnor/actions/runs/35114867283` → `{"message":"Not Found","status":"404"}` + `gh: Not Found (HTTP 404)`. CONFIRMED.
- `gh api repos/tailrocks/velnor/actions/runs/35094895601` → same 404. CONFIRMED.
- Both runs resolve in their corrected repos (above). Correction stands; disproof failed.

## Log-signature spot checks (one per run)

1. Run 35129353335 (Policy generated-tree): log contains `FAIL generated-tree ... at b9c3156cdb88e63c11b9e595a3e694b02238c09a`, `PASS pin-reachable ... ancestor of head 3353310c...`, `policy: 11 rules, 1 failed`. MATCH.
2. Run 35136207272 attempt 1 (Planning race): log contains `Cache not found for input keys: velnor-workflow-v3-Linux-X64-f1f88c200e5b3b82...`, `release not found`, `##[error]no runtime product for revision 48a66ad7d56636f8bfa6069fbaf810d0089fef39 (closure f1f88c200e5b3b82); the mainline runtime-product publisher builds it after merge`. MATCH.
3. Run 35114867283 (swift xcframework): log contains `error: desktop xcframework requires macOS (Apple Silicon)`; also observed `bash: line 1: swift: command not found` and `release.yml: No such file or directory (os error 2)`. MATCH (all three).
4. Run 35094895601 (Policy 2-rule): log contains `FAIL generated-tree ... at 1279c4f92c97b75dc4cc627f122e119f8a5eae16`, `FAIL trusted-runners 4 findings`, `job prune-pr-cache`, `job dispatch-ci-main`, `policy: 11 rules, 2 failed`. MATCH.

## Verdict

CERTIFIED — all four rows match on metadata, the 404 correction holds, and every spot-checked signature reproduces. No row disproven.
