# Scan-integrity security review and correction

## Revision identity

The prior scanner change was recorded under two commit objects with the same
tree and parent:

| Object | Parent | Tree | Difference |
| --- | --- | --- | --- |
| `7e9a2b5f0f6980f7be7a4ab9ee91d5a8f3d2da9f` | `12cc87b629802c294da9840325cb21087c020df6` | `285a73cebb0a123fcdba56f65c677f1195ffa80b` | Original message contained literal `\\n` escapes. |
| `6409a08678683506c12c7d50820875c1fa703b9d` | `12cc87b629802c294da9840325cb21087c020df6` | `285a73cebb0a123fcdba56f65c677f1195ffa80b` | Amended commit message; same source tree. |

Neither object is an acceptable security-corrected candidate.

## Rejected mechanisms

The prior tree trusted a file's generated header as ownership authority and
allowed a configured static output path to be removed from scan provenance
before static config validation. `direct_legacy_workflows` also ignored any
unknown workflow carrying the generated header. An attacker could therefore
hide an unmanaged workflow or self-referential static workflow from both scan
provenance and legacy-workflow rejection.

## Required correction

- Only exact output paths recorded by the parsed ownership sidecar, plus the
  exact fleet env artifact, are scanner-owned outputs.
- Generated headers are claims and never classify a path.
- Unknown workflow files remain scan inputs and fail legacy-workflow checks.
- Stale paths are allowed only when the ownership sidecar records them; the
  write plan still verifies their recorded digest before removal.
- Static sources must stay outside `.github`; self-source and source-side
  workflow hiding are rejected during config validation.

## Corrected candidate

`3c46b9e83c9a0ca57be88743e49ecae27f731685` is the full source candidate,
directly based on `12cc87b629802c294da9840325cb21087c020df6`. Its tree is
`921391689cf0d64adf47bd9cc9e215704b5b7ab7`; it does not carry either rejected
`6409` or `7e9` commit object.

The candidate derives scanner exclusions from the independently recorded
ownership sidecar (plus the exact host-env and sidecar paths). It never reads a
generated header as authority. `direct_legacy_workflows` accepts only current
renderer outputs or sidecar-recorded stale outputs, with digest verification
still enforced by the write plan. Static sources normalize path components and
reject `.github` sources before a static output can hide workflow/action
behavior or self-reference.

Adversarial coverage now includes forged generated/fleet headers, unmanaged
workflow add/remove, generated-output removal and drift, static self-source,
headerless static output across first generation, handwritten `.github` inputs,
and shallow clones.

## Verification

External target:
`/Users/donbeave/Projects/tailrocks/velnor-project/dual-lane-evidence/G1/scan-integrity-target`

- `cargo test -p velnor-workflow --lib s2::scan::file_walk`: **5 passed**.
- Focused churn, static self-source, and repeated-generation tests: **4 passed**.
- Full library regression with the known checked-in snapshot excluded:
  **1741 passed, 1 filtered**.
- `cargo clippy -p velnor-workflow --lib --tests -- -D warnings`: **passed**.
- `cargo fmt --all` and `git diff --check`: **passed**.

Verdict: corrected source candidate is ready for independent review at the
exact commit above. Do not integrate `6409` or `7e9`.
