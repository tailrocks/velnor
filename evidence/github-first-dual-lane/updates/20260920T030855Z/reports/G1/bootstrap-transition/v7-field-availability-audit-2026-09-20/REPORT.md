# V7 field-availability audit

Read-only structural audit. No GitHub API call, authority mutation, release,
source edit, or cryptographic verification occurred.

## Result

`check_v7_contract.py` returned exit 0: 35 checks passed, 0 failed.
Verdict: `structural-pass-with-external-blockers`. This is not an approval or
the G1 gate. The audit proves only the machine-readable v7 contract is
internally consistent.

Inputs:

- plan Markdown SHA-256: `7339996f7c3d76fe4e750cb9036b474beb7e0d20ae7b2b7e7a9f0787cd81b72e`
- plan JSON SHA-256: `1a1cc175b36611f6ce175f4c82b6f6ce51f36f74bfb22a5d8e1abb9cc5647826`
- audit script SHA-256: `ccc71e4afaba6dfe96fc74d27fe9a522fab6fafd007c57a671a85614efdc3c31`
- result JSON SHA-256: `49d98e32890baf5bb9b148888c4aa2f3a074a08745c30cd1b4e2ccccc486524f`

## Covered invariants

- workflow-call-only publisher; guarded main caller; no caller outputs;
- acyclic job DAG and typed producer-to-consumer lineage;
- no later-stage field consumption; own attestation IDs/digests excluded;
- canonical release predicate path and real record producer/upload fields;
- OIDC `job_workflow_sha` separated from Contents workflow blob SHA;
- Actions job permission scopes contain no `metadata:read` and no duplicate
  read/write entry; metadata read remains external-App-only;
- exact product fields, pinned actions, consumer read scopes, and isolated
  record upload;
- late Tree-B PR/adoption sidecar and explicit Policy state machine;
- provider/integration-paired contexts, full-ruleset snapshot requirements,
  honest actor-5 freeze blocker, native/supporting workflow census, and
  unresolved target/provider gates.

## Remaining blockers

The current main-bound target generator is null; no live B source/output,
record IDs, external Checks App credential/provider, cryptographic verifier,
native xcode-27 run, clean published runtime, full ruleset snapshots, or
enforceable coordinator/recovery proof exists. Provider feasibility evidence
found no atomic exact base/head plus ruleset CAS excluding the current
repository or organization administrator. Execution therefore remains
blocked pending the external authority decision and all live evidence.
