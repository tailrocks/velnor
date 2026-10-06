# Detailed job sample, batch 1

Authenticated REST collection on 2026-09-20. Eleven run/attempts, 24 compressed
pages: ten Jackin runs and one older successful Parallax run. URLs and original
response fields are retained in each envelope. Every nonempty run has matching
page totals, unique IDs and attempt 1; no rerun is inferred. The zero-job failure
is retained with unknown execution timing. Parent independently rechecked totals
and uniqueness; the inventory agent audited pagination and collector replay.

Selection and timing outputs live in
[the sampling manifest](../history/sampling-manifest.json). This is a bounded
representative sample, not complete coverage of all events or slow historical
runs. No desktop-scheduled representative was found in the retained window.
Continue historical obligation mapping and job/log/artifact sampling.

Decompress the JSON envelopes before passing them to `workflow-collector --jobs`.
Use the matching run object from the history projection as `--run`; pass every
page for that attempt. Collector revision: f8ac97b1. Required-gate selectors were
not supplied in this batch; gate latency remains unknown. Raw longest job is not
a dependency critical path, and `updated_at` is never a completion timestamp.
