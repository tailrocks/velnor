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

## Bounded cross-repository raw observations

The following seven run IDs are a bounded baseline set, not matched cohorts.
Each saved jobs response has `total_count == jobs.length` and fits in page 1;
raw files retain every returned job, including skipped jobs. Durations use raw
job `started_at`/`completed_at`; `updated_at` is not used as execution end.
The execution sum excludes skipped jobs and is separate from the max job and
the jobs-span envelope. Each selected run response says `run_attempt = 1`.
The attempted `/actions/runs/{id}/attempts?...` collection URL is not a GitHub
REST endpoint and returned HTTP 404; that failed probe is not evidence about
older attempts. No older attempt was selected or saved in this bounded set.

| Repository / run | Event / result | Head SHA | Jobs | Job span s | Max job / raw job | Max s | /10 target s | Execution sum s |
| --- | --- | --- | ---: | ---: | --- | ---: | ---: | ---: |
| [Jackin desktop 35475235030](https://github.com/jackin-project/jackin/actions/runs/35475235030) | push / success | `3b1e1fc0a20a7d861454746c9ebb50a335c9b412` | 1 | 2040 | [Desktop merge cadence](https://github.com/jackin-project/jackin/actions/runs/35475235030/job/105983237683) | 2040 | 204 | 2040 |
| [Jackin Swift 35475235267](https://github.com/jackin-project/jackin/actions/runs/35475235267) | push / success | `3b1e1fc0a20a7d861454746c9ebb50a335c9b412` | 44 | 890 | [Swift · Apple](https://github.com/jackin-project/jackin/actions/runs/35475235267/job/105983319926) | 766 | 76.6 | 3823 |
| [Parallax PR #109 35300721965](https://github.com/tailrocks/parallax/actions/runs/35300721965) | pull_request / failure | `93cee3556b3bf08c9ba38a43aace43312c233c0a` | 26 | 675 | [parallax-server](https://github.com/tailrocks/parallax/actions/runs/35300721965/job/105794536030) | 636 | 63.6 | 2674 |
| [Jackin desktop 35478203836](https://github.com/jackin-project/jackin/actions/runs/35478203836) | push / success | `41796158b1e45535ae4e74d5ff048cb5bb4e0488` | 1 | 2635 | [Desktop merge cadence](https://github.com/jackin-project/jackin/actions/runs/35478203836/job/105991074920) | 2635 | 263.5 | 2635 |
| [Jackin Swift 35478203945](https://github.com/jackin-project/jackin/actions/runs/35478203945) | push / success | `41796158b1e45535ae4e74d5ff048cb5bb4e0488` | 44 | 1065 | [Swift · Apple](https://github.com/jackin-project/jackin/actions/runs/35478203945/job/105991186732) | 855 | 85.5 | 4967 |
| [Parallax nightly scheduler 35430906774](https://github.com/tailrocks/parallax/actions/runs/35430906774) | schedule / success | `6a12bf47a816b63e848b563aaa45ef9694159c79` | 3 | 7 | [Dispatch ci-main](https://github.com/tailrocks/parallax/actions/runs/35430906774/job/105865174259) | 4 | 0.4 | 4 |
| [Parallax dispatched child 35430912434](https://github.com/tailrocks/parallax/actions/runs/35430912434) | workflow_dispatch / failure | `6a12bf47a816b63e848b563aaa45ef9694159c79` | 52 | 135 | [Policy](https://github.com/tailrocks/parallax/actions/runs/35430912434/job/105865197187) | 119 | 11.9 | 139 |

The Parallax PR and dispatched child are failed observations and remain in the
table to preserve failure order; they are not successful baselines. The
scheduler's success records dispatch acceptance only. No provider, runner
image, runtime revision, compiler identity, or cache claim is inferred from
these raw API responses; those require job logs and referenced-workflow
evidence. Max job is not a critical path because no dependency graph is
available.
