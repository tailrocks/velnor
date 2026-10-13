# Baseline observations

These are starting observations, not matched-cohort estimates. Full inventory,
run-history pagination, historical source inspection and independent review remain
pending. One observation cannot establish noise, median improvement or a tail.

## Velnor PR run 35477566106, attempt 1

[Run](https://github.com/tailrocks/velnor/actions/runs/35477566106).
Raw [run metadata](observations/velnor-35477566106-run.json) and
[all 49 jobs](observations/velnor-35477566106-jobs-page1.json), including skipped
jobs, are retained. `total_count = 49`, so this 100-entry page is complete.

- Source head: `eaae46c48dd45479f1eb7a1314708a38c16d6cb5`.
- Reusable workflow revision / PR merge ref:
  `45f18aa8c078bebbe9b39d64a1487a0ff59fd4a7` (`refs/pull/966/merge`).
- Event: `pull_request`; result: success; executed cohort: `ubuntu-24.04`,
  GitHub hosted. Hardware/image version, actual compiler identity, cache state
  and executed generator binary identity require logs.
- Trigger: `2026-09-19T23:59:32Z`.
- `ci-required` completion: `2026-09-20T00:04:54Z`: **322 seconds**.
- Last `Control / Required` completion: `2026-09-20T00:05:35Z`:
  **363 seconds**. Tenfold target for this envelope: **36.3 seconds**;
  for `ci-required` specifically: **32.2 seconds**.
- Aggregate execution of nine non-skipped jobs: **705 seconds**. This is
  neither user-visible wall time nor billed time.

Durations below are `completed_at - started_at`, using API timestamps, not
`updated_at`. Step durations have one-second resolution. Pre-start intervals
cannot be attributed exclusively to queueing without the dependency graph.

| Rank | Job | Execution seconds | 10× target seconds | Largest visible components |
| --- | --- | ---: | ---: | --- |
| 1 | Rust · velnor-workflow | 274 | 27.4 | MBX setup 65; checks 108; candidate preparation 59; publication 6 |
| 2 | Rust · velnor-runner | 208 | 20.8 | checks 173 |
| 3 | Docker | 78 | 7.8 | checks 61; Buildx setup 7 |
| 4 | Rust production topology | 68 | 6.8 | checks 27; MBX setup 11 |
| 5 | Rust · velnor-control | 33 | 3.3 | Rust restore 7; checks 5; MBX setup 5 |
| 6 | Rust · velnor-model | 29 | 2.9 | Rust restore 7; MBX setup 4; checks 2 |

The runner job starts at `00:01:22Z`, 67 seconds after the generator job.
The generator is the longest individual execution, while the runner finishes
one second later. Optimizing only the longest job need not reduce the final
required-result latency. Required-job scheduling adds a further 45 seconds
after the last Rust completion; cause remains unclassified.

## Collection access

The first CLI 30-day collection attempt was:

```sh
gh api --paginate --slurp 'repos/tailrocks/velnor/actions/runs?per_page=100&created=%3E%3D2026-08-21'
```

It failed HTTP 403, public-IP API rate limit exceeded. `gh api user` separately
failed HTTP 401 (`Requires authentication`); existing CLI credentials are invalid.
SSH push succeeded. The authenticated connector's generic `github_fetch` GET
supports explicit `page` and `per_page` and successfully fetched the same API
family. This resolves read pagination access; dedicated list wrappers alone do not.

The first authenticated 30-day Velnor response reports **5,538 runs**. GitHub
[limits filtered searches to 1,000 results](https://docs.github.com/en/rest/actions/workflow-runs#list-workflow-runs-for-a-repository),
so collection must partition time ranges and verify completeness within each
partition. No claim of complete 30-day history is made yet.
