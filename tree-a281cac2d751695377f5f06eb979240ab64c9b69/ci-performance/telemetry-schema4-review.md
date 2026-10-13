# Telemetry schema 4 review

Status: independently reviewed candidate; exact-head CI pending. No speedup claim.

## Structural cause and treatment

Schema 2 grouped candidate preparation and publication into cleanup and treated
partial cache observations as complete totals. Schema 4 models candidate phases
separately, validates their partial order and job time bounds, and preserves
unknown values rather than inventing zero-duration observations. Cache saves
require actual save outcomes. Unobserved post-job costs remain explicitly unknown.

Alternatives considered: relabel the existing aggregate (cannot recover phases),
infer phases from log wording (unstable and incomplete), or emit explicit markers
and outcomes at phase boundaries (selected). Runner queue attribution remains
separate from dependency waits and unclassified pre-start latency.

## Independent verification

The independent reviewer initially rejected future and eight-day-old markers:
durations were null but completion was reported. The repaired marker predicate
now validates epochs, job bounds, maximum age and known job start. Follow-up
review passed future, stale, post-job, malformed, reversed and incomplete graphs;
valid partial preparation and cache observations survive missing later phases.
Focused marker regression tests passed in both rendering paths.

Reviewed marker-bounds patch SHA256:
`5aa4cc8f9b9054ddb75a676dc76ccfca823b8b5105061c15bb0141154565a7bc`.
Parent regenerated source-owned actions and all affected reusable workflows.
An initial full-suite attempt encountered generated parity failures before that
regeneration completed; retained as a failed attempt. The next full-suite run
passed 1,820 tests but failed both marker tests under load; 133 tests did not run.
The fixture used `now + 1..6` before several subprocess probes, allowing its
future markers to become past timestamps. Repair and independent verification
were required before publication; focused-test success did not establish stability.
The test-only clock repair passed all 1,955 tests in the full all-feature suite.
Independent review of platform behavior remains pending; production clock
handling is unchanged.

This repairs measurement correctness. Performance acceptance, controlled samples,
and substantive optimization iteration credit remain pending.
