# v6 executable contract check

Status: structural regression pass; execution blocked. No GitHub API, Actions run, release, or cryptographic attestation operation was performed.

- Plan: `AUTHORITY-CHANGE-PLAN-2026-09-20-v6`
- Current main bound: `325719f1e05d3d46322c9fd3eeb9ad545e175638`
- Checks: 23 pass, 0 fail.
- Script SHA-256: `3d0d5a1d7dc5053c8216f3b2fbc0ee9abf0ce973aca62b632b4ecafa15bed2f3`
- Result SHA-256 is recorded by the paired freeze manifest after this file is written.

Validated: caller `uses:` job has no outputs or shell step; workflow-call-only publisher; direct Policy needs edge; typed DAG lineage; no reserve-time binding artifact; distinct release-asset/Actions-artifact IDs; no digest self-preimage; verifier self-check excluded from upstream census; both binding/release predicates and OIDC/certificate fields; provider-bound ruleset contexts; fenced coordinator and independent recovery; supporting workflows; full Linux-X64/Linux-ARM64/xcode-27 matrix.

This is not a gate pass. Remaining hard blockers are the reviewed target generator and B source/output, real verifier and live IDs, old-parser semantic admission, external App/provider identities, enforceable coordinator/proxy/watchdog exercise, current recursive closure, clean publishable runtime, and independent approval.

