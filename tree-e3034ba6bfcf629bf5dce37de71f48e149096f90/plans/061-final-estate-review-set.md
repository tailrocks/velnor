# 061 — Final estate review set

Status: IN PROGRESS

## Goal

Produce the final operator-reviewable delivery for every repository named by
`VELNOR_PROJECTS_SETUP.md`, without merging any PR.

## Contract

- Preserve concern-based canonical workflow shape; do not add irrelevant jobs.
- Use `cargo nextest`, never `cargo test`.
- Keep compiler caching local-only; never add GHA/S3 credentials or backends.
- Schedule the canonical `both` parity workflow weekly where the concern is
  applicable.
- Route each organization through its selected `velnor-trusted` runner group.
- Apply only the exact required-check contexts in
  `docs/required-check-handoff.md`, after each context exists.
- Implement only the approved attestation surface in
  `docs/capability-proposal-attest-build-provenance-v4.md`.
- Run fixture V-A before fresh repository V-B/V-C campaigns.
- End with two independent zero-error estate audits and a PR/evidence ledger.
- Never merge the final PRs; operator review is the terminal blocker.

## Deliverables

1. Velnor native attestation adapter, strict manifest coverage, fixture proof,
   pool-name package configuration, and a fresh signed-apt release deployment.
2. One final PR or binding trunk-only record per estate repository.
3. Exact PR heads, checks, lane parity, timing evidence, required-check state,
   and remaining human decisions in the operator report.
