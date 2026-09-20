# Workflow timing summary

Observed run/attempt groups: 1.
Verified run completion rows: 0.
Censored or unknown completion rows: 1.
Jobs with unknown duration: 27.
Full execution and attempt-wall sums require a complete fresh attempt; observed fresh/stale partial sums and unknown counters remain separate.
Pre-start intervals are unclassified; rerun intervals are not queue time, and no queue attribution or critical path is inferred without a dependency graph.
The raw duration table includes stale records for evidence; rows with freshness other than fresh are not current-attempt bottlenecks.

## Ranked jobs by raw API duration

| Rank | Run | Attempt | Job | Duration ms | Conclusion | Runner | Job freshness | Completion state |
| ---: | ---: | ---: | --- | ---: | --- | --- | --- | --- |
| 1 | 35529376700 | 1 | Prepare renderer (ubuntu-24.04) | 160000 | cancelled | GitHub Actions 1000087941 | fresh | unknown_job_timestamps |
| 2 | 35529376700 | 1 | ci-required | 5000 | failure | GitHub Actions 1000087945 | fresh | unknown_job_timestamps |
| 3 | 35529376700 | 1 | Control / Required | 3000 | failure | GitHub Actions 1000087956 | fresh | unknown_job_timestamps |
| 4 | 35529376700 | 1 | Rust · Rust dependency policy · velnor — rust-policy | 0 | skipped | unknown | not_applicable_skipped | unknown_job_timestamps |
| 5 | 35529376700 | 1 | Rust · Rust production topology · velnor — rust-production-topology | 0 | skipped | unknown | not_applicable_skipped | unknown_job_timestamps |
| 6 | 35529376700 | 1 | Rust · unit-collector · velnor — rust-unit-collector | 0 | skipped | unknown | not_applicable_skipped | unknown_job_timestamps |
| 7 | 35529376700 | 1 | Rust · velnor-client · velnor — rust-velnor-client | 0 | skipped | unknown | not_applicable_skipped | unknown_job_timestamps |
| 8 | 35529376700 | 1 | Rust · velnor-control · velnor — rust-velnor-control | 0 | skipped | unknown | not_applicable_skipped | unknown_job_timestamps |
| 9 | 35529376700 | 1 | Rust · velnor-model · velnor — rust-velnor-model | 0 | skipped | unknown | not_applicable_skipped | unknown_job_timestamps |
| 10 | 35529376700 | 1 | Rust · velnor-render · velnor — rust-velnor-render | 0 | skipped | unknown | not_applicable_skipped | unknown_job_timestamps |
| 11 | 35529376700 | 1 | Rust · velnor-tools · velnor — rust-velnor-tools | 0 | skipped | unknown | not_applicable_skipped | unknown_job_timestamps |
| 12 | 35529376700 | 1 | Rust · velnor-workflow-contract · velnor — rust-velnor-workflow-contract | 0 | skipped | unknown | not_applicable_skipped | unknown_job_timestamps |
