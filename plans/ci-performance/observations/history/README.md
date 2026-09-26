# Retained workflow history

Window: 2026-08-21 00:00:00 UTC through 2026-09-20 01:55:00 UTC.
Parallax: all seven pages fetched; API total 623; 623 observed unique run IDs.
The gzip JSON files preserve selected original REST fields and source URLs;
they are explicitly labeled projections, not full raw responses. Decompress
before replay through `workflow-collector`. Jobs, attempts, artifacts and logs
are collected separately; run metadata alone cannot establish elapsed time.

| Workflow path | Event | Runs | Success | Failure | Other / nonterminal |
| --- | --- | ---: | ---: | ---: | ---: |
| `.github/workflows/ci-main.yml` | push | 25 | 0 | 19 | 6 |
| `.github/workflows/ci-main.yml` | workflow_dispatch | 4 | 0 | 4 | 0 |
| `.github/workflows/ci-policy.yml` | pull_request_target | 36 | 9 | 25 | 2 |
| `.github/workflows/ci-policy.yml` | workflow_dispatch | 2 | 1 | 1 | 0 |
| `.github/workflows/ci-pr.yml` | pull_request | 37 | 0 | 25 | 12 |
| `.github/workflows/ci.yml` | pull_request | 87 | 38 | 14 | 35 |
| `.github/workflows/ci.yml` | push | 22 | 19 | 2 | 1 |
| `.github/workflows/ci.yml` | schedule | 4 | 2 | 0 | 2 |
| `.github/workflows/ci.yml` | workflow_dispatch | 9 | 8 | 0 | 1 |
| `.github/workflows/dependency-discovery.yml` | schedule | 4 | 2 | 1 | 1 |
| `.github/workflows/dependency-discovery.yml` | workflow_dispatch | 2 | 2 | 0 | 0 |
| `.github/workflows/footprint.yml` | pull_request | 62 | 46 | 0 | 16 |
| `.github/workflows/footprint.yml` | push | 18 | 15 | 1 | 2 |
| `.github/workflows/footprint.yml` | workflow_dispatch | 4 | 3 | 0 | 1 |
| `.github/workflows/maintenance.yml` | pull_request | 25 | 25 | 0 | 0 |
| `.github/workflows/maintenance.yml` | schedule | 4 | 4 | 0 | 0 |
| `.github/workflows/mcp-evals.yml` | pull_request | 105 | 0 | 0 | 105 |
| `.github/workflows/nightly.yml` | schedule | 4 | 3 | 1 | 0 |
| `.github/workflows/preview.yml` | push | 35 | 26 | 2 | 7 |
| `.github/workflows/preview.yml` | workflow_dispatch | 1 | 1 | 0 | 0 |
| `.github/workflows/scheduled-measurement.yml` | schedule | 27 | 17 | 5 | 5 |
| `.github/workflows/storage-integration.yml` | schedule | 27 | 15 | 7 | 5 |
| `.github/workflows/upgrade-harness.yml` | pull_request | 61 | 51 | 0 | 10 |
| `.github/workflows/upgrade-harness.yml` | push | 17 | 15 | 0 | 2 |
| `.github/workflows/upgrade-harness.yml` | workflow_dispatch | 1 | 1 | 0 | 0 |

No generated PR/main success was observed in this window. Earlier workflows
provide successful evidence, but their validation and product obligations must
be mapped to any replacement graph before comparing performance. Dispatcher
success is not child success. `updated_at` is not used for completion.

Velnor: 5,562 unique runs across 60 pages and 10 contiguous time partitions.
Jackin: 3,233 unique runs across 35 pages and 7 contiguous time partitions.
Queries were split below GitHub's 1,000-result filtered-query ceiling before
pagination. The final snapshot includes three more Jackin runs than the first
count. Independent pagination/identity audit and a representative sampling
manifest accompany these files. Job/step, attempt, artifact and historical
configuration inspection remains in progress; complete run metadata is not a
complete execution inventory.
