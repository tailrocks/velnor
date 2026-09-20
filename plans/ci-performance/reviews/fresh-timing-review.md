# Independent fresh timing review

Verdict: PASS for arithmetic and provenance. These observations support timing records and bottleneck ranking. They do not support a speedup comparison across revisions or cache states.

## Parallax 35522592113

Raw inputs:

- `/private/tmp/velnor-ci-integration/plans/ci-performance/observations/parallax-35522592113-attempt1-run-raw.json`
- `/private/tmp/velnor-ci-integration/plans/ci-performance/observations/parallax-35522592113-attempt1-jobs-raw.json`
- `/private/tmp/velnor-ci-integration/plans/ci-performance/observations/parallax-35522592113-attempt1-jobs.jsonl`
- [run](https://github.com/tailrocks/parallax/actions/runs/35522592113)

The run is attempt 1, pull_request, completed/success. The API run started at 16:24:03Z. The jobs page contains 24 of 24 jobs, all attempt 1 and completed/success, with observed timestamps. Independent sums from raw job timestamps:

- `ci-required` completed at 16:38:04Z: 841 seconds from run start.
- `Control / Required` completed at 16:38:09Z: 846 seconds; it is not the primary required result.
- Execution sum: 2,028 seconds.
- Longest job: `Rust · parallax-server / GitHub`, 787 seconds.
- No skipped, missing, stale, malformed, or unknown executed durations.

The normalized row reports `completion_state=verified`, `jobs_expected=24`, `jobs_observed=24`, `fresh_executed_jobs=24`, `execution_unknown_jobs=0`, and `critical_path_state=not_computed_without_dependency_graph`.

## Velnor de6e 35522199636

Raw inputs:

- `/private/tmp/velnor-ci-integration/plans/ci-performance/observations/velnor-35522199636-attempt1-run-raw.json`
- `/private/tmp/velnor-ci-integration/plans/ci-performance/observations/velnor-35522199636-attempt1-jobs-raw.json`
- `/private/tmp/velnor-ci-integration/plans/ci-performance/observations/velnor-35522199636-attempt1-jobs.jsonl`
- `/private/tmp/velnor-ci-integration/plans/ci-performance/observations/velnor-de6e-generator-reports.json`
- `/private/tmp/velnor-ci-integration/plans/ci-performance/observations/velnor-de6e-runner-reports.json`
- [run](https://github.com/tailrocks/velnor/actions/runs/35522199636)

The run is attempt 1, pull_request, completed/success. It contains 68 of 68 jobs, all attempt 1: 20 completed/success and 48 completed/skipped. Independent raw timestamp sums:

- `ci-required` completed at 16:26:26Z: 593 seconds from run start 16:16:33Z.
- `Control / Required` completed at 16:26:32Z: 599 seconds; it is not the primary required result.
- Execution sum over the 20 non-skipped jobs: 1,367 seconds.
- Longest executed job: `Rust · velnor-runner · github-hosted — rust-velnor-runner / GitHub · hosted`, 542 seconds.
- Three skipped jobs have GitHub's inverted synthetic timestamps (start 16:16:53Z, completion 16:16:52Z). They produce the summary's `Jobs with unknown duration: 3`; they are not executed jobs and are excluded from execution sums. The other 45 skipped jobs have zero-duration timestamps.
- No executed job has missing or invalid duration; normalized row has `fresh_executed_jobs=20`, `execution_unknown_jobs=0`, `execution_unobserved_jobs=0`, and `skipped_jobs=48`.

Schema 4 generator report says job-marker total 173 seconds, checks wall 88, candidate 3+7 seconds; runner report says total 533, checks wall 502. These marker totals are in-job phase observations and are not API job envelopes (180 and 542 seconds respectively). Both reports explicitly leave queue, cleanup, post-job MBX save, and other save phases unobserved; no durable cache-save claim follows.

## Fresh Velnor 4f70 control

Raw inputs:

- `/tmp/velnor-4f70-final-run.json`
- `/tmp/velnor-4f70-final-jobs.json`
- `/tmp/velnor-4f70-jobs.jsonl`
- `/tmp/velnor-4f70-generator-reports.json`
- `/tmp/velnor-4f70-runner-reports.json`
- [run](https://github.com/tailrocks/velnor/actions/runs/35523884300)

The run is attempt 1, pull_request, completed/success, 68/68 jobs, 20 executed +48 skipped. Independent raw sums:

- `ci-required` completed at 16:56:22Z: 451 seconds from run start 16:48:51Z.
- `Control / Required` completed at 16:56:28Z: 457 seconds.
- Execution sum: 1,455 seconds.
- Longest job: Velnor runner, 408 seconds.
- Generator API job: 233 seconds; schema 4 marker report: 227 seconds, checks 169 seconds, candidate 3+6 seconds.
- Runner schema 4 marker report: 401 seconds, checks 376 seconds.
- Its 38 unknown summary durations are skipped rows with unavailable/inverted timestamps; none are executed jobs.

## Immutable identity treatment

For de6e: API `run.head_sha=de6e1811...`; embedded PR head is `3fb38643...`; referenced reusable workflow merge SHA is `134a993f...`. For 4f70: run head is `4f70cf74...`; embedded PR head is `ce75f7c3...`; referenced reusable workflow merge SHA is `8a9041da...`. The collector correctly records `source_sha=null`, basis `unknown.pull_request_source_not_proven`, and retains raw observed PR head and referenced-workflow evidence. It does not infer source or merge identity from mutable PR metadata, run head, or reusable-workflow SHA.

No speedup claim is valid: de6e and 4f70 differ in source/runtime revisions, selected work and cache state. The fresh rows are valid raw timing evidence only.
