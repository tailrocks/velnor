# Exact checkout-proof serde-boundary review: ccd2144e

Status: **PASS for the bounded untrusted-deserialization fix; still REJECT
for authoritative G0/live use.**

This exact commit closes the proof-injection route identified in the 5c43
review. It does not create checkout evidence, install an authenticated live
collector, or authorize CAS. Those remain disabled and out of scope.

## Exact scope

- HEAD: ccd2144e5685f875cefc0241dabf4275b5b0692d
- Parent: 5c43daa48ad88e33e3d78df0fd039643b71f8e64
- Detached worktree: /private/tmp/g3-combined2
- Remote branch tmp/g3-combined2 resolves to the exact HEAD.
- Diff scope: github_live_collector.rs only; g0_contract is untouched.
- No source edits, live capture, credentials, installation, or remote writes.

## Verification

- Default cargo test --locked --all-features --package velnor-tools:
  316 passed.
- Serialized package tests with --test-threads=1: 316 passed.
- cargo fmt --all -- --check: passed.
- cargo clippy --locked --all-features --package velnor-tools --all-targets
  -- -D warnings: passed.
- git diff --check 5c43..ccd2144: passed.
- Focused tests passed:
  untrusted_checkout_observation_rejects_injected_proof,
  checkout_observation_does_not_promote_api_head_to_checkout_proof, and
  workflow_binding_rejects_conflicting_checkout_proofs.

The raw-store fixture directory remains untracked test output in the detached
tree. No tracked source diff was introduced. The prior 5c43 review's
parallel raw-store race remains a separate unchanged concern; this commit
does not modify that code.

## Boundary review

### Untrusted JSON

LiveCheckoutObservation no longer derives Deserialize. Its manual
deserializer accepts only the private UntrustedCheckoutObservation DTO:

- api_head_sha
- api_raw_object_refs

The DTO has deny_unknown_fields, so an injected proof object or proof null is
rejected. The focused test exercises both forms. The same deny-unknown map
visitor rejects unknown fields; serde's duplicate-field visitor rejects
duplicate DTO keys rather than selecting one. The resulting trusted
observation is always constructed through api_head_only and therefore always
has proof None. API head SHA remains an observation, never checkout
attestation.

### Constructor boundary

LiveCheckoutProof no longer derives Deserialize. The only constructor is the
private from_verified_producer validator, which rejects non-40-hex checkout
SHA, blank source kind, empty raw references, and blank raw-reference entries.
Direct proof field construction is unavailable outside the module. The
conflict-proof fixture now uses this validator.

This is safe in the current tree, but the constructor is private to
github_live_collector.rs itself. A future authenticated producer in another
module cannot use it without an intentional narrow producer-only API. Any
such API must keep JSON deserialization and caller-supplied proof paths
closed; do not make the proof type or fields public merely to wire the
producer.

### Serialization implication

LiveCheckoutObservation remains Serialize but not round-trip Deserialize for
a populated proof. That is acceptable for the current untrusted capture
boundary: no caller-authored serialized collection may rehydrate a proof.
If a future authenticated adapter serializes proof-bearing captures, it needs
a separate authenticated envelope/ledger with raw-object and complete
run/job/attempt/check identity validation.

## Verdict

The exact serde fix closes the previously identified private-field bypass:
hostile proof object, null proof, unknown fields, and duplicate DTO keys cannot
promote API head data into checkout proof through deserialization. The commit
is approved only as this bounded source fix. Live authority remains
unavailable; no G0, checkout, model-runtime, workload, or CAS authority
approval is granted.
