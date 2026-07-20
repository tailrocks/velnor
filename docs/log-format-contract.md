# Log format contract (NEVER break this)

Velnor emits job log lines over several channels. Each channel has a
DIFFERENT, exact format expectation in GitHub's UI. This contract has been
broken repeatedly during refactors — every break ships a visibly wrong UI
(timestamps doubled in live logs, or timestamps leaking into log content).
Read this before touching any code that builds, prefixes, or sends log
lines.

## The channels

| Channel | Where in code | Line format | Why |
|---|---|---|---|
| **Live WebSocket feed** (`FeedStreamClient::send_log_lines`, powers the in-progress job view / `live_logs`) | `runner.rs` `live_feed_lines` in the step-log publisher | **RAW content — NO timestamp prefix** | The UI renders live frames verbatim and supplies its own timestamp column. An embedded prefix shows as doubled timestamps on every line. |
| **Uploaded step log blob** (Results Service `upload_step_log`, powers the completed-job view / `data-log-url` and the raw log download) | `runner.rs` `blob_log_lines` + `unix_now_iso8601` | **`YYYY-MM-DDTHH:MM:SS.fffffffZ <content>`** — .NET "o" round-trip prefix with EXACTLY 7 fractional digits, single space, then content | The UI strips this prefix into the "Show timestamps" toggle. Wrong precision (e.g. second-only) is not recognised and leaks into visible content. Missing prefix breaks the toggle. |
| **Uploaded job log blob** (Results Service `upload_job_log`, powers GitHub's native per-job log download / `gh run view --log`) | `runner.rs` `build_combined_job_log` + `iso8601_with_blob_precision` | **`YYYY-MM-DDTHH:MM:SS.fffffffZ <content>`** — same raw-download format as step blobs | GitHub's native log archive expects the official runner job-log blob. The `job-log.txt` artifact is only a fallback, not the primary path. |
| **V1 timeline feed** (`append_timeline_record_feed`) | `runner.rs` timeline publishers | RAW content (masked) | Same rendering rule as the live feed. |
| **Step metadata times** (`started_at`/`completed_at` on Twirp steps, timeline records) | `executor.rs` `unix_now_rfc3339`, `runner.rs` `unix_now_iso8601` call sites assigning fields | RFC3339 field values, never prefixed onto lines | These are struct fields, not line content. |
| **Job summaries and forensic timing records** (`$GITHUB_STEP_SUMMARY`, slot `lifecycle.log`) | Native adapters in `executor.rs`; lifecycle instrumentation in `runner.rs` | Structured Markdown or one-line JSON, respectively; never copied into step log lines | These are additive, out-of-band observability channels and do not alter any live or uploaded log-line shape. |
| **Local console mirror** (`append_job_console`, `docker logs`) | `runner.rs` | Velnor's own format (free) | Not rendered by GitHub. |

## Regression history (why this document exists)

1. **Second-precision blob prefix**: blob lines once used
   `YYYY-MM-DDTHH:MM:SSZ` (no sub-seconds) on the mistaken belief that
   sub-seconds broke GitHub's parser. The opposite is true — the UI only
   strips the 7-digit form, so timestamps appeared as content text on every
   line. A stale comment claiming the wrong rule survived in `executor.rs`
   until 2026-06-11 and nearly caused repeats.
2. **2026-06-11, jm run 27319096003**: the live WebSocket feed sent
   blob-formatted (timestamp-prefixed) lines. Live frames render verbatim →
   every in-progress line showed a doubled timestamp next to the UI's own
   time column, while the GitHub-hosted lane looked clean.

## The rules

- Lines destined for `FeedStreamClient::send_log_lines` go through
  `live_feed_lines` (raw). Lines destined for `upload_step_log` go through
  `blob_log_lines` (7-digit prefix). Lines destined for `upload_job_log` go
  through `build_combined_job_log` (7-digit prefix). Never inline these
  transformations at a call site; never route one helper's output into the
  other channel.
- Guard tests (must stay green, must not be weakened):
  - `runner::tests::live_feed_lines_are_raw_and_blob_lines_are_timestamped`
  - `runner::tests::unix_now_iso8601_is_github_strippable`
- Any change to these helpers, their call sites, or the feed/upload clients
  MUST update this document and the guard tests in the same commit, and MUST
  be verified visually: dispatch a fixture `lanes=velnor-only` run and watch
  a step's logs WHILE RUNNING (live view) and AFTER COMPLETION (blob view)
  side-by-side with a GitHub-hosted job.
- When Velnor output differs from GitHub-hosted output in the UI, the bug is
  in Velnor's channel formatting until proven otherwise — check the table
  above first.
