# Independent design review

Review scope: V-RUST-001 typed validation stages and the workflow collector.
This is an independent review of the initial design and partially implemented
draft, retained as historical evidence. It does not accept a CI performance
result. Collector findings were subsequently resolved or clarified in
[the final collector review](collector-review.md); its later PASS supersedes
the collector HOLD below. The inferred-stage implementation was rejected.

## Verdict

V-RUST-001 is **HOLD**. The proposed runtime fields (`pr_stages` and
`full_stages`) cannot be consumed by either current schema-1 consumer until a
candidate runtime is built, pinned, and proven to parse and plan the generated
config before planning starts. Unknown, empty, duplicate, and missing stages
must fail closed. The current implementation still contains the implicit
Rust Clippy-to-check prerequisite rewrite and removes `-D warnings` and
`--no-deps`; this changes the verification contract. Stage kind is still
classified by tokenizing arbitrary shell text, so a known-constructor scanner
boundary is not established. The stage object has no product identity,
toolchain/backend compatibility, fixture, or result/failure policy. Stage
prerequisites are serialized but `run_unit` executes the selected vector
without enforcing prerequisite closure, and selecting one stage can therefore
run it without its declared prerequisites. Exact source command preservation
and all non-Rust coverage remain unproven. `cargo check --lib` passing is not
acceptance: the current runtime test target still fails to compile because its
fixtures construct removed `pr_commands`/`full_commands` fields and assert the
old command API. No consumer regeneration or real candidate-runtime plan has
been accepted.

The workflow collector has **conditional arithmetic approval only**. Its
bounded tests pass (`cargo test -p unit-collector --all-targets`: 30 tests;
clippy: clean), and the raw timing rules correctly avoid `updated_at`, keep
parallel execution sums separate from wall latency, return a null sum for an
incomplete set, retain a partial sum, and leave critical path unknown without
a dependency graph. Campaign evidence remains **HOLD** pending the identity
and completeness fixes below.

## Collector blocking findings

1. **High — duplicate exact job IDs are silently accepted.** `group_jobs`
   drops an identical repeated job without setting a conflict. A repeated page
   can therefore hide an unobserved job while `expected_count == observed`
   appears true. Preserve raw observations and mark the run incomplete for
   every duplicate ID, even when payloads are byte-identical. Add a fixture for
   identical and conflicting duplicates.

2. **High — page identity is not fail-closed.** `page_metadata_missing` is
   recorded but never participates in `jobs_complete`. Page continuity is only
   checked when `per_page` exists; a gap or mixed page set without that field
   can pass count and timestamp checks. Missing page identity, page gaps,
   conflicting totals, and mixed run/attempt pages must produce unknown
   completion. Add fixtures for each case.

3. **High — malformed job entries disappear.** `extract_job_pages` ignores
   unrecognized array members, and `parse_job` accepts missing job IDs into the
   collection before later counting. The design says malformed input remains
   visible, but dropped entries can make the observed set look complete. Return
   an explicit malformed-input conflict or retain a censored record.

4. **High — attempt identity is not part of completeness.**
   `records_for_run` receives `attempt_match` but `jobs_complete` does not
   require it. A run with an absent or mismatched attempt can therefore be
   represented as a complete job set if grouping happens to align. Require the
   run ID and run attempt to be known and equal for every expected job before
   derived completion or execution sums become verified; retain mismatches as
   separate censored rows.

5. **High — a reusable-workflow SHA is still promoted to `merge_sha`.**
   `referenced_merge_proof` accepts a same-repository `refs/pull/N/merge`
   callee and emits that callee SHA as the top-level merge SHA. This remains an
   inference from a referenced workflow, not direct evidence of the top-level
   run revision. Keep every referenced SHA as configuration provenance and set
   top-level `merge_sha`/`merge_ref` only from direct run fields that prove the
   top-level merge ref and SHA. The current fixture expectation of
   `merge_sha = workflow-10` should become `None` unless direct run evidence is
   present.

## Collector clarifications

GitHub's normal workflow-run REST object does not expose a reliable
`completed_at` field. Retaining an optional raw field when supplied is useful,
but its absence is not itself a blocker. The authoritative derived completion
must remain the maximum valid non-skipped job `completed_at`, with the API
field kept separate if present and never synthesized from `updated_at`.

The current skipped-job fixture intentionally accepts inverted timestamps.
Keep that policy only if output distinguishes `job_set_complete` from
`executed_timing_complete`; otherwise “all jobs completed” overclaims what the
raw records prove. Skipped jobs remain counted, excluded from execution sums,
and must never silently turn into executed work.

`workflow_config_sha` is currently always null even though referenced workflow
SHAs are retained. Either populate it only as explicitly labeled configuration
provenance (never source/merge identity) or remove the redundant field. Summary
counts should use unique `(run_id, run_attempt)` groups rather than repeating a
verified count once per job.

## Required acceptance evidence

- Candidate runtime artifact built and pinned before any generated plan runs;
  both consumers parse the same schema and unknown fields fail closed.
- Stage round-trip and generated-workflow fixtures preserve every command,
  profile, target, feature, lock, wrapper, backend, tool prerequisite, and
  product identity; deliberate stage failure proves status propagation.
- Collector fixtures cover duplicate IDs, page gaps/missing metadata,
  conflicting totals, mixed run IDs/attempts, missing job IDs, malformed
  entries, and direct versus reusable-workflow SHA evidence.
- A raw replay reports censored/unknown rows explicitly. No aggregate is
  accepted as wall time, critical path, or merge identity without its direct
  evidence.
