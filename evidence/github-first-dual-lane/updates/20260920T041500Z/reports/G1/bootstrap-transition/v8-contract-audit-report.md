# V8 contract audit

Observed `2026-09-20`. This report is external evidence for the v8 proposal. It does not authorize source, ruleset, App, release, merge, or runner changes.

## Result

The structural audit passed 51/51 checks. Hostile mutations reject for:

- binding preimage consuming a future release asset;
- binding preimage consuming record transport IDs;
- release predicate missing its published asset;
- generic/untyped provenance;
- omitted strict record schema;
- omitted provider artifact handoff;
- caller/called OIDC role reversal;
- terminal census including its own verifier job;
- record artifact self-preimage;
- missing product asset/tag identity;
- external verifier impersonation by `GITHUB_TOKEN`;
- incomplete canonical leaf mapping.

Results JSON: `v8-contract-audit-results.json`, SHA-256 `2e2764bdd84d6e5fbbf95046ed9e4391046f2ce26f45424325461dc9aaacb96c`.

The proposal remains externally blocked. The audit does not prove runtime behavior, source admission, GitHub API behavior, provider enforcement, or package publication. Hard blockers are:

1. No provider-enforced freeze/recovery excluding bypass actor 5 is proven.
2. Target generator revision is null; current revision 54 and old runtime pin 0dc are not targets. B source/output are absent from current main closure.
3. External B App/provider IDs, integration, verifier revision, credentials, and called-workflow identity are unresolved.
4. A real verifier and raw record-artifact acceptance path are not implemented.
5. Current native evidence used forbidden `macos-26`; complete `xcode-27` proof is absent.

## Bound revisions and evidence

- Plan Markdown `AUTHORITY-CHANGE-PLAN-2026-09-20-v8.md`: SHA-256 `503a564afcbe157e607215c0549480b2cade36525b6f3aeb41198418c6add639`.
- Plan JSON `AUTHORITY-CHANGE-PLAN-2026-09-20-v8.json`: SHA-256 `87a5896eb8cf75e53b49f8eacb61226315a9f79c574af3078fdc34390252c321`.
- Audit script `v8_contract_audit.py`: SHA-256 `d26fc8ce0d1a8d971c3ce88a90e55531466ec93c9698adb36162574618d4d742`.
- Current main: `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`, tree `22ccc1daf9d55bd12d9a58e6652fe92cf3cb9416`.
- Current checkpoint: `77e1ecc848e0a3ad73cdfd853b788054e8cda02e0aae38d076b7c59b0da130bd`.
- Current closure input: `1b890ef899c8a02146e7d6ab1d664fcb92213924e8c7a722f8f57a2bfb55c072`.

V7 remains historical and unchanged. Independent review is still required from `g0_reviewer` and `authority_transition_review`. V8 is a structurally audited proposal, not approval-ready or execution-ready.
