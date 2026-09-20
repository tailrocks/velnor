# Jackin required starting jobs

Fresh metadata was retrieved with authenticated `gh api` from `jackin-project/jackin` during the initial collection, before the later 403 responses. Both exact run/job links in the specification were confirmed as attempt 1. This file records the normalized fields needed for the failure ledger; it is not a substitute for the raw API response or job log.

## Run 35521080097 / job 106105226160

- Run: [CI / main · push · main](https://github.com/jackin-project/jackin/actions/runs/35521080097), event `push`, branch `main`, attempt 1, conclusion `failure`.
- Source: `fce94cea8a15de0c2db3bb4ff880d741baf5c00a`; tree `e12910141bf9062c34a487f3203543dbafbe4d6b`.
- Workflow path: `.github/workflows/ci-main.yml`.
- Job: [Rust · jackin-diagnostics / GitHub](https://github.com/jackin-project/jackin/actions/runs/35521080097/job/106105226160), runner label `ubuntu-26.04`; started `2026-09-20T15:56:03Z`, completed `2026-09-20T15:56:59Z` (56 seconds).
- Failed step: `Run unit checks` (step 29), `15:56:44Z`–`15:56:56Z` (12 seconds). Runtime download/verification, selection, toolchain setup, and cache preparation steps shown in the fresh job response succeeded; the test command step failed.
- Classification: observed test failure. Do not record as a formatting or Clippy failure.
- The full attempt-1 archive is captured in [the raw HTTP exchange](jackin-run-35521080097-attempt-1-logs.http.txt), [ZIP archive](jackin-run-35521080097-attempt-1-logs.zip), and extracted [diagnostics job log](<jackin-run-35521080097-attempt-1-logs/30_Rust · jackin-diagnostics _ GitHub.txt>). It confirms `wire_failure_partial_success::conformance_partial_success_is_not_retried` failed at `crates/jackin-diagnostics/tests/wire_failure_support/mod.rs:51:5`: assertion `left == right`, left 7, right 1. The suite ran 108/122 tests: 107 passed, 1 failed, 1 skipped; 14 were not run due to fail-fast, and three already-running tests were allowed to finish. The command was `mbx nextest run --locked --all-features --package 'jackin-diagnostics' --no-tests pass`.
- The job had warm exact rustup, mold, Cargo, and mbx caches; mbx reports 24 object hits and zero misses. The CI timing record says compile-cache transfer is not included. This evidence rules out an observed format/Clippy failure and shows cache hits did not prevent the test failure.
- Current disposition: root cause under investigation by the diagnostics workstream. Local passes on macOS do not explain or disprove the Ubuntu Actions failure.

## Run 35515575859 / job 106090835001

- Run: [Desktop merge cadence · push](https://github.com/jackin-project/jackin/actions/runs/35515575859), event `push`, branch `main`, attempt 1, conclusion `cancelled`.
- Source: `0163d1b7c654753f23d9ee7334866569c24615d0`; tree `68217e189d6153a29c45d1b301b5742a35912f5f`.
- Workflow path: `.github/workflows/desktop-merge.yml`.
- Job: [Desktop merge cadence](https://github.com/jackin-project/jackin/actions/runs/35515575859/job/106090835001), runner label `macos-26`; started `2026-09-20T14:08:46Z`, completed `2026-09-20T14:44:14Z` (35 minutes 28 seconds).
- `Run desktop-merge` step: `14:09:12Z`–`14:44:09Z`, then cancelled.
- Classification: cancellation, not a demonstrated source/test failure. The public job page annotation says twice: “Canceling since a higher priority waiting request for desktop-merge-jackin-project/jackin-refs/heads/main exists.” The page copy is preserved in [jackin-job-106090835001-public-page.md](jackin-job-106090835001-public-page.md). The full attempt-1 log archive is now captured in [the raw HTTP exchange](jackin-run-35515575859-attempt-1-logs.http.txt), [ZIP archive](jackin-run-35515575859-attempt-1-logs.zip), and [extracted desktop job log](<jackin-run-35515575859-attempt-1-logs/0_Desktop merge cadence.txt>).
- Current disposition: cancellation cause observed as priority replacement. The desktop task expansion and repeated tool-installation analysis belongs to the bootstrap/performance workstream.

## Evidence limits

- Run/job metadata and raw per-attempt logs came from authenticated `gh api` responses. Initial direct attempt requests returned stale HTTP 403 responses; a bounded `Cache-Control: no-cache` request to the attempt endpoint and log archives succeeded. Original response headers, GitHub request IDs, and the binary archives are preserved.
- The installed `gh run view` command was first invoked outside a Git repository and exited before making a request. This is a local CLI-context failure, not GitHub evidence.
- Do not treat a cached runner hit or a locally passing reproduction as proof that either original GitHub occurrence is resolved.
