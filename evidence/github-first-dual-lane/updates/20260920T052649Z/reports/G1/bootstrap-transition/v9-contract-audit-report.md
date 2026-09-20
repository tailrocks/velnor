# V9 executable contract audit

V8 is preserved unchanged. V9 is a new external design successor. No source, GitHub authority, release, merge, dispatch, runner, or credential state changed.

## Result

The owner audit passed 33/33 checks:

- actionlint passed on the minimal caller/reusable-workflow fixtures;
- caller permission upper bound includes every called-workflow write scope;
- caller has no output re-export and S0 has an actual capture step plus Contents API request;
- the canonical machine baseline is consumed with 146 permanent leaves mapped;
- S7 ambiguity is split into pre-record upload, external provider result, called identity capture, and terminal census producers;
- lineage orientation and typed producer/consumer edge sets are checked;
- strict positive pre-record and provider-result fixtures validate with `additionalProperties:false` schemas;
- root-bound normalized fixture hashes and current 89f82dd/22ccc1 tuple validate;
- exact S4/S6/S7 preimage fields/exclusions, immutable-release readback contract, native negative, and provider-freeze blocker validate.
- the 15-case provider/transport hostile fixture set is root-bound and fail-closed.

Results JSON SHA-256: `4ddeb3272b5456f8e790e925c4dba4a71ba4436eee38af0910bbc660fc196ca1`.

Plan Markdown SHA-256: `bfcafb4f706ac5c1a1ecccc7d8ba1d0d777f16e52bf4fa335a15085908e832ed`.

Plan JSON SHA-256: `e74a583e492b41fa043b4a78621e6183875a333e3d533a4244e3956121b66615`.

## Corrected contracts

The reusable-workflow caller now grants the exact upper-bound union `actions:write`, `contents:write`, `attestations:write`, `id-token:write`, `checks:read`; each called job downgrades to least privilege. The graph uses `capture-caller-context -> build -> artifact-verify -> reserve -> attest-binding -> publish -> attest-release -> pre-record-upload -> verify-B`.

The record is explicitly two-phase. `pre-record-upload` owns the Actions artifact ID/name/digest and upload IDs. The external B provider owns provider check/result IDs and emits a strict signed provider-result record. `verify-B` owns called-workflow identity capture and terminal census. No field has an `or` producer, and no provider or census field is produced by pre-record upload.

The current product identity is `velnor-workflow-policy-validator` / `velnor-workflow-policy-validator-Linux-X64`, matching the permanent binding schema. The historical predicate schema is excluded. V9 adds strict pre-record, provider-result, release-manifest, and Tree-B adoption schemas plus a root manifest and positive fixtures.

## Remaining hard blockers

- target generator revision and B source/output fixed point absent at current main;
- real external provider App identities, credential, verifier revision, and live verifier absent;
- actor-5 always-bypass freeze/recovery remains unproven;
- current runtime used forbidden `macos-26`; full `xcode-27` and supporting fleet proof absent;
- no authority operation is authorized by this audit.

V9 is structurally audited but remains external-blocked, not approval-ready or execution-ready.
