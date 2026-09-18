# Integration batch 6: PR #916 merge shepherd (bastion campaign)

- Role: DESIGNATED INTEGRATION subagent. NO campaign-branch edits made.
- Start: 2026-09-17T01:21:57Z. Deadline: 2026-09-17T02:51:57Z (90 min max).
- PR: #916 `feat(a2): producer writes source revision; default-branch guard, verify-before-create`
  - head: `feat/a2-producer-revision` @ f4d46b3bc9c286c43b9554d53ef5c1db7c2e65b, base: `main`
  - DCO: pass. mergeable: MERGEABLE, mergeStateStatus: BLOCKED (checks failing at start).

## Gate 1: /tmp/v-a2-split.md reads CERTIFIED
- 01:21Z: file MISSING.
- 01:22Z: PASS — file reads `CERTIFIED (safe to merge)`; producer-only scope, old-consumer
  proof, 546 tests + clippy/fmt/actionlint green in reviewer worktree. Full text kept at /tmp/v-a2-split.md.

## Gate 2: PR #916 CI full green (no bypass, no --admin)
- 01:21Z: FAILING — Control/Planning fail, Policy fail, ci-required fail, Control/Required fail;
  runs 35169868299 + 35169868123 both failure.
- 01:26Z: independently confirmed failure reason from run logs:
  `no runtime product for revision 51e635aff74fdffab138d5751249ca7105b678c4
  (closure 1bfbdc50fa00fdfd); the mainline runtime-product publisher builds it after merge`.
  Red by construction pre-merge (product publishes on the post-merge main push).
  Cert notes landing needs review + override; my orders forbid bypass, so I keep
  waiting for full green until the 02:51:57Z deadline, then report BLOCKED if still red.

## Orchestrator update 02:44Z — v1 gates void, v2 gates armed
- Diagnosis /tmp/pr916-red.md: self-bump chicken-and-egg; #916 must be restructured to
  generator-only (pin revert, keep runtime_products.rs). Old review /tmp/v-a2-split.md STALE —
  do NOT merge on it.
- V2 gate A: /tmp/v-a2-split2.md reads CERTIFIED for the NEW head.
- V2 gate B: full CI green on the new head (no bypass, no --admin).
- Deadline extended (open). If green but no v2 cert within 60 min of green -> BLOCKED-ON-REVIEW.
- 02:45Z: head still f4d46b3b (fix not pushed), CI still 4 fails, v2 MISSING. polling.
- 02:50Z: FIX PUSHED head 1bee4f23 `fix(pr-916): pure phase-1 generator PR (revert self-bump...)`.
  New runs: CI/PR 35175925732 (in progress), Policy 35175919250 (failure, 15s).
- 02:55Z: new head NOT green. Policy FAIL generated-tree: tree differs from render of
  velnor-workflow@7341ef4b in 2 files (generator-state, ci-runtime-products.yml) — fix left
  new-render content under the old pin; same-closure path needs byte-identical tree.
  CI/PR: 4 fast fails `Velnor rejected job (operational_store)` (Prepare Cargo, Docker,
  Documentation, OpenTofu) + ci-required aggregator; 1 job still pending. v2 MISSING.
  Awaiting further author push / re-run + v2 cert. polling.
- 02:59Z: run 35175925732 completed: 7 fails. Progress: Control/Planning now GREEN (pin revert
  worked; units ran). Remain: Policy generated-tree (2 files), 4x Velnor operational_store
  rejections, 2x aggregators. v2 MISSING. polling.
- 03:01Z: /tmp/v-a2-split2.md appeared: MISMATCH for 1bee4f23 (not safe to merge as-is).
  Reviewer: restructure faithful (net 3 files, pin reverted, Planning green, 546 tests green)
  but Policy red BY CONSTRUCTION for old-pin+new-render (same-closure path renders with old
  base product; render not neutral, 93-line producer diff). Only Policy-passing phase-1 shape
  = source-only (revert the 2 regen files too) — parent decision, not pushed. Do NOT merge.
  Awaiting next author iteration + fresh CERTIFIED v2 for the then-current head. polling.

## Merge
- pending (v2 gates).

## Main-push Runtime-products watch
- pending.

## Product manifest proof (revision field)
- pending.
