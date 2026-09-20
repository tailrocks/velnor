# WIP checkpoint failure: 970a6dd5

Source head: `970a6dd55c772391503664d4f53eb0ab5bfbdc57`.
[PR run 35528148188](https://github.com/tailrocks/velnor/actions/runs/35528148188)
and [policy run 35528146678](https://github.com/tailrocks/velnor/actions/runs/35528146678)
failed. These attempts remain in the population; neither is a successful
performance baseline or an optimization iteration.

## Observed failures

- Documentation: MD032 in the archived ownership relevance diagnosis.
  A missing blank line before its alternatives list caused failure.
  Commit `fb97c33` repairs it; all seven checkpoint Markdown files passed lint.
- Generator: the explicitly unvalidated parser checkpoint lacked regenerated
  workflow/state output. The generator check correctly rejected the drift.
- Policy: candidate acquisition rejected generated-tree drift. This is a
  failed prerequisite, not a successful policy check or successful dispatch.
- Runner: `artifact_upload_sends_finalize_hash_and_rejects_unsuccessful_finalize`
  failed with `send artifact blob PUT: request body transfer failed`; the local
  mock server also failed with BrokenPipe. The runner suite stopped after
  1537/2523 tests: 1536 passed, one failed, five skipped, one leaky. Therefore
  unexecuted coverage is not presumed passing.

## Open diagnosis

The mock server parses only exactly cased `Content-Length:` and otherwise
assumes a zero body. HTTP framing/case handling is a hypothesis requiring
independent reproduction; no production runner fix or flake conclusion is
accepted from this log alone. Preserve the finalize hash and unsuccessful
finalization assertions when repairing the fixture or implementation.

Raw API metadata and failed-job logs accompany this report. No workflow
latency or successful speedup is inferred from these failed attempts.
