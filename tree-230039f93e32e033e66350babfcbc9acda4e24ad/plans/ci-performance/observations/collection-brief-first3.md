# First three raw baseline observations

Collected through authenticated `github_fetch` on 2026-09-20. Each saved jobs
page reports `total_count == jobs.length`; no pagination gap exists in these
three responses. Durations below use raw job `started_at`/`completed_at`.

| Repository / run | Result | Jobs | Earliest job start | Latest job completion | Run job-span seconds | Max job seconds | Sum job seconds |
| --- | --- | ---: | --- | --- | ---: | ---: | ---: |
| [Jackin desktop 35475235030](https://github.com/jackin-project/jackin/actions/runs/35475235030) | success | 1 | 2026-09-19T23:07:49Z | 2026-09-19T23:41:49Z | 2040 | 2040 | 2040 |
| [Jackin Swift/main 35475235267](https://github.com/jackin-project/jackin/actions/runs/35475235267) | success | 44 | 2026-09-19T23:07:49Z | 2026-09-19T23:22:39Z | 890 | 766 | 3823 |
| [Parallax PR #109 35300721965](https://github.com/tailrocks/parallax/actions/runs/35300721965) | failure | 26 | 2026-09-18T23:24:13Z | 2026-09-18T23:35:28Z | 675 | 636 | 2674 |

The job-span is a wall envelope over jobs, not a critical path. The job sum is
aggregate execution and is not user-visible latency. The Parallax failed run
has 18 successful and 8 failed jobs; it is retained as a failed observation,
not a successful baseline. Raw run and jobs responses are in the adjacent
`jackin-35475235030-*`, `jackin-35475235267-*`, and
`parallax-35300721965-*` files.
