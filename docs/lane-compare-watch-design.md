# Lane Compare Watch Design

`velnor-tools lane-compare` remains the one-run parity gate. Watch mode turns the
same comparator into a scheduled regression guard for recent dual-lane workflow
runs.

## Regression Definition

Watch mode treats the newest completed run as current and the previous completed
runs from the same workflow as the baseline sample. Failed runs stay in scope so
a parity regression that fails the workflow is not skipped. For each paired job
class it compares the Velnor/GitHub wall-time ratio:

- baseline ratio = average Velnor seconds / average GitHub seconds across the
  older sample,
- current ratio = current Velnor seconds / current GitHub seconds,
- timing regression = current ratio exceeds the baseline ratio by more than
  `--regress-threshold` percent.

Any parity loss reported by the existing step comparator is also a regression,
regardless of timing.

## Baseline Strategy

The first implementation computes the baseline from recent runs selected by
`--since`; no committed JSON baseline is required. This avoids stale checked-in
performance numbers while still making scheduled checks deterministic for the
run window they inspect. The command writes `.velnor-compare/lane-compare-watch/`
with:

- `report.md` for the scheduled workflow artifact and human triage,
- `latest-stats.json` for preserving the current run's parsed timing/parity
  facts.

A future workflow can promote a known-good `latest-stats.json` into a committed
baseline if the team wants reviewable static thresholds instead of rolling
baselines.

## Scheduled Usage

A GitHub-hosted scheduled canary should dispatch or inspect a `lanes=both` run,
then call:

```bash
cargo run -p velnor-tools -- lane-compare \
  --watch \
  --repo owner/repo \
  --workflow compat.yml \
  --since 5 \
  --regress-threshold 25
```

The alert surface is the process exit code: zero means no parity or timing
regression; non-zero means the workflow should fail and publish `report.md`.
