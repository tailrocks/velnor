# main watch — bastion campaign (tailrocks/velnor)
Snapshot: 2026-09-17 (UTC). Ledger baseline: main 33688938, 14 runtime products, Latest 9f236b40.

## origin/main
- ls-remote: 33688938297eee3933997dbbf368ee20fb1779d4 (refs/heads/main)
- gh api:     33688938297eee3933997dbbf368ee20fb1779d4 — "Merge pull request #914 from tailrocks/chore/proof-candidate-path-3" (2026-09-16T22:49:32Z)
- Both agree. No drift vs ledger.

## PR #916 — feat(a2): producer writes source revision; default-branch guard, verify-before-create
- State: OPEN, not merged. MergeState: BLOCKED.
- Head: f4d46b3bc9c286c43b9554d53ef5c1db7c2e65b2 on feat/a2-producer-revision → main. Updated 2026-09-17T01:15:48Z.
- Checks: Control/Planning FAIL, Control/Required FAIL, Policy FAIL, ci-required FAIL; all language jobs skipping.

## Releases (limit 8)
Latest still velnor-workflow-runtime-v1-9f236b40635970a4 (2026-09-16T22:37:32Z). No NEW runtime product since 9f236b40.
Top 8: 9f236b40(Latest), 93c8a358, fd45efac, ff27a1c0, 1ff514a2, 78673ee1, c0ce56f0, 6ae4ca31.
Runtime-product total still 14 (8 above + ad493401, d39cbf21, e5ad6f4f, 2bd48976, f1f88c20, 203eb9b7); older v0.1.x tags unchanged.
Revision-carrying candidates: NONE — PR #916 (producer revision) still open, so no product built with source revision exists yet.
Note: main advanced to 33688938 at 22:49:32Z (after Latest product 22:37:32Z) and its Runtime-products run succeeded, but no new product release was created (verify-before-create presumably found content identical).

## Latest main runs (branch=main)
1. CI / Main #35159519365 — success — head 33688938 — 2026-09-16T22:49:36Z
2. Preview #35159519217 — FAILURE — head 33688938 — 2026-09-16T22:49:36Z
3. Velnor workflow runtime products #35159518917 — success — head 33688938 — 2026-09-16T22:49:36Z
(Prior: CI/Main #35158060354 failure on 386ccc16; runtime products #35158058879 success on 386ccc16.)

## Drift vs ledger
| Signal | Ledger | Observed | Drift |
|---|---|---|---|
| origin/main | 33688938 | 33688938 (both sources) | none |
| runtime products | 14 | 14 | none |
| Latest release | 9f236b40 | 9f236b40 | none |
| new product since 9f236b40 | — | none | none |
| revision-carrying product | — | none (PR #916 open) | none |
| PR #916 | — | OPEN, blocked, checks failing | watch |
| main CI/Main @head | — | success | ok |
| main Preview @head | — | FAILURE | watch |
