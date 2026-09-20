# V-GATE-001: canonical required-gate obligation validation

Status: mapping fix committed and pushed as `7e21a551`; real CI and independent
agent verification pending. No performance improvement is claimed.

The committed baseline had no complete selected-obligation validation. An
initial uncommitted draft checked only array/object/string shapes. An injected unknown unit,
unknown provider, empty provider list, duplicate unit, duplicate provider, or
non-string provider could therefore produce no matching rendered caller and be
ignored by the later per-caller loop. The enabling cause was that plan shape
validation and caller verdicts had separate, incomplete contracts.

The candidate renderer now serializes one `EXPECTED_CALLERS` contract from the
same typed `RequiredCaller` list used to build `needs` and verdicts. The gate
validates the generated contract and requires every selected unit/provider pair
to map to it, with nonempty unique unit/provider values. The shell test covers
success, failed/cancelled/skipped/missing policy, empty and nonempty selection,
malformed JSON, unknown unit/provider, empty providers, duplicate unit/provider,
and non-string provider inputs.

Evidence so far:

- `cargo check -p velnor-workflow --lib`: passed.
- Direct `jq` execution of the rendered validation expression: one valid pair
  passed; every malformed or unmapped case failed closed.
- Focused Rust test is currently unable to compile because the shared checkout
  contains an unrelated in-progress policy lookup migration whose test helpers
  still construct removed fields (`PinnedBinaryLookup.pinned_binary` and
  `candidate_manifest`). The failure is recorded, not treated as acceptance.
- Full generated-workflow byte-for-byte tests must be rerun after the parent
  regenerates consumers; this source-only change intentionally did not edit
  generated YAML.

Acceptance requires the focused and full generator tests after that unrelated
policy migration is made internally consistent, regenerated outputs, and an
independent review of the emitted gate on manual empty/nonempty plans.

## Parent independent verification

Exact baseline generated `ci-main.yml` at f0fb1c01 returned success for empty,
unknown-unit, unknown-provider and empty-provider-list selections when plan
and policy succeeded and all callers skipped. The isolated candidate retained
success for the valid empty case and rejected the other three. Raw methods,
script hashes, inputs and exit codes are preserved in
[baseline replay](../observations/gate-baseline-f0fb1c01.json) and
[candidate replay](../observations/gate-candidate-local.json). Local execution
durations describe diagnostic scripts only, not workflow performance.

Isolated full generator library suite passed 1775 tests. All-target Clippy caught
an `expect()` in the new test helper; using the existing `must_ok` helper fixed
it. Focused test, all-target Clippy, fmt and actionlint 1.7.12 then passed.
Emitter revision 56 and regenerated PR/main gates are in the scoped commit.

A separate pre-existing admission-policy issue remains: selected but unadmitted
callers can be skipped successfully. Lack of runner permission is not proof that
required validation is unnecessary. Its obligation/optional-provider semantics
need independent review and explicit failure or verified substitute evidence.
The committed fix does not claim to resolve that issue. Source-1 parity also
remains required while source-1 consumers exist.

## Integrated CI observation

Run `35485817832`, attempt 1, source
`266dd76e314ef9d6a6eb1514b2e96366173abacd`, executed the integrated gate.
Both `ci-required` (job `106013035837`) and `Control / Required`
(job `106013043543`) succeeded; every executed job reported success.
The run-level conclusion is nevertheless `cancelled` after a subsequent push.
This is retained as an observed gate execution, not a successful full-workflow
performance baseline or final acceptance. Raw run/jobs and collector output
are in `observations/velnor-35485817832-*`.

## Admission review

The renderer intentionally omits a provider from `units[].providers` when
provider policy excludes it; that path may report a skipped caller only when
the plan carries an explicit exclusion record. A selected `(unit, provider)`
pair with its admission expression false is different: the plan and caller
contract disagree, so accepting a skipped result is false green. The generic
repair is to derive one obligation set from `RequiredCaller`, require
`admission == true` and terminal `success` for selected pairs, and require an
identity-matched `EXCLUDED` record with a valid reason for omitted pairs. Do not
hardcode provider names or treat missing admission as proof that validation is
unnecessary. A verified substitute artifact could satisfy an excluded
obligation only through the existing exact product contract; cache presence is
not a substitute.
