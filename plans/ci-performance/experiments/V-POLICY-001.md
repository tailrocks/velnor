# V-POLICY-001: typed declared-pin and candidate renderers

Status: source implementation complete for independent review; generated
workflows and a real post-regeneration policy run remain pending.

## Root cause

The policy consumer had one `VELNOR_WORKFLOW_PINNED_BINARY` input serving two
roles. The candidate acquisition step wrote the PR-built binary into that
slot, while `resolve_pinned_binary` interpreted the same bytes as the
declared generator pin before the candidate exception ran. Run
35483489289/job106005422506 demonstrated the result: candidate closure
`af47d1e21c7775e9598e32cc8d855dd3d8747142b42647782b0f306bd732a3e8` was
downloaded, then rejected as not the declared pin closure `325719...`.
The old path also allowed the running executable to bypass candidate manifest
binding through self-recognition.

The enabling condition was an ambiguous environment contract, not a missing
closure comparison. A candidate cannot prove the declared pin, and a base
validator cannot be silently substituted for a different declared pin.

## Alternatives

1. Keep one environment slot and add ordering or closure heuristics. Rejected:
   the roles remain ambiguous and a candidate can still enter pin resolution.
2. Keep the candidate slot but let the base validator stand in for the pin
   when its closure differs. Rejected: it recreates the observed `0dc` versus
   declared `325` mismatch as an unaudited substitution.
3. Build the candidate and use it as both pin and candidate renderer when the
   manifest matches. Rejected: candidate provenance does not establish that it
   is the declared pin, and it collapses the two trust decisions.
4. Use two typed slots: `VELNOR_WORKFLOW_PINNED_BINARY` always names the
   declared pin; `VELNOR_WORKFLOW_CANDIDATE_BINARY` must pair with
   `VELNOR_WORKFLOW_CANDIDATE_MANIFEST`. Rejected one-sided pairs fail before
   rendering; candidate bytes are digest-checked before `--closure` or render.
   This is the selected design.

## Implementation boundary

`PinRenderer` and `CandidateRenderer` are separate policy inputs in both
schema implementations. The owner policy emitter acquires the PR candidate
into the candidate slot, then provisions/resolves the audited tree's declared
pin into the pin slot before enforcement. The candidate path is same-repo only
and keeps the existing closure, digest, and manifest checks. Manifest v1 is
used only for its parsed revision/closure/digest fields; repository, workflow,
run/attempt, source, profile, platform, artifact, and successful producer
proof stay workflow-layer obligations because the policy parser cannot
authenticate those claims from v1 alone.

The binary identity remains independent of the consuming project/configuration
receipt. A future bootstrap producer must replace the late producer path,
rather than leave competing producer mechanisms indefinitely.

## Validation

Focused renderer, manifest, emitter, and consumer negative tests pass;
`cargo check -p velnor-workflow --lib`, scoped rustfmt, and
`cargo clippy -p velnor-workflow --lib --no-deps -- -D warnings` pass. The
full generated-file suite requires regeneration after the parent-owned
generator revision bump; no generated workflow was edited here.
