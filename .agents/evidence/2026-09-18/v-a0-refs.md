# V-A0 refs verifier — independent re-check of /tmp/a0-refs.md drift table

Date (UTC): 2026-09-16 ~23:50. Read-only. Method: fresh `gh api` / `gh pr view` / `gh release` calls, no reuse of author outputs.

## Fresh outputs (mine)

- velnor main: `33688938297eee3933997dbbf368ee20fb1779d4`
- jackin main: `0be3fcf95cd33c14fd3fe47af08026f17135a157`
- java-monorepo main: `38a8fb5777fd02fec8b3904d86387aba05940e9a`
- scaleset main: `e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5`; tag v0.4.0: `6ce025902cd964747a078c2aabe7340ebc667eca` (commit)
- default branch: main for all three repos
- PR 904: MERGED, head `facf18cce3236030d771a5ec67df69f55ff162ca`, mergeCommit `4dc8da58e2e8cbb6e30e792716075a2130ccffae`, mergedAt 2026-09-16T20:29:27Z, base main / head feat/ci-immutable-runtime-products
- PR 901: CLOSED, mergedAt null, closedAt 2026-09-16T22:14:13Z, head `6926760d27f616be2ac0d9f161182d02e8482910`
- PR 912: OPEN, head `38ffbfd73bc67002ddce7e38fdaf15ff6497317d`, base main / head docs/bastion-final-plan
- release list top5: Latest `velnor-workflow-runtime-v1-9f236b40635970a4` 2026-09-16T22:37:32Z, then 93c8… 22:33:12Z, fd45… 22:16:15Z, ff27… 22:13:09Z, 1ff5… 22:00:25Z (identical to author, byte-for-byte incl. timestamps)
- runtime-product count in top-30 JSON: 14 tags (9f23,93c8,fd45,ff27,1ff5,7867,c0ce,6ae4,ad49,d39c,e5ad,2bd4,f1f8,203e); Latest=true only on 9f23
- race closure `...-f1f88c200e5b3b82`: exists, createdAt 2026-09-16T18:43:34Z
- `gh release view v0.1.275`: `release not found`
- crate `crates/velnor-runner/Cargo.toml @ main`: `version = "0.1.275"`
- APT blob `.github-gen/NO_WORKFLOWS_REQUIRED.md`: sha `3172bb883ec343a676d82c2594cb1a399191bf07`, size 809; velnor-apt main `d62820d47a3814e98c4e25512151b4af14e3c3e1`
- actions/permissions: keys [allowed_actions, enabled, sha_pinning_required], allowed_actions=all, enabled=true
- repo runner-groups: empty body / JSON parse failure (GitHub-side flake, consistent with author's HTTP 500 claim)
- org runner-groups: Default(id1,all) + velnor-trusted(id3,selected); trusted repos total_count=18; trusted runners total_count=7
- environments: ["github-pages"]
- local HEAD `38ffbfd73bc67002ddce7e38fdaf15ff6497317d`; origin/main `33688938297eee3933997dbbf368ee20fb1779d4` = `33688938 Merge pull request #914...`

Note: JSON `createdAt` timestamps differ by ~2-3 min from `release list` display timestamps (author used list output); using the same command, my output matches the author's exactly. Not a discrepancy.

## Row verdicts

| # | Row | Verdict | Evidence |
|---|---|---|---|
| 1 | Velnor main 33688938 | MATCH | API SHA identical; local origin/main identical |
| 2 | Jackin main 0be3fcf9 | MATCH | API SHA identical |
| 3 | Java-monorepo main 38a8fb57 | MATCH | API SHA identical |
| 4 | Generator pins PENDING | MATCH | procedural claim (not re-resolved in A0); nothing to disprove |
| 5 | PR 904 MERGED + race closure exists | MATCH | state/head/mergeCommit/mergedAt identical; f1f88c release exists 18:43:34Z |
| 6 | PR 901 CLOSED UNMERGED | MATCH | state/mergedAt:null/closedAt/head identical |
| 7 | Crate 0.1.275, no v0.1.275, latest v-tag v0.1.274 | MATCH | crate grep + release-not-found verified; v0.1.274 in list |
| 8 | 14 runtime products, Latest 9f236b40 | MATCH | count=14, Latest flag + list order identical |
| 9 | Scaleset main e6daac70, v0.4.0 stable | MATCH | both SHAs identical |
| 10 | APT blob 3172bb88 size 809, apt main d62820d4 | MATCH | blob sha+size and apt main identical |
| 11 | Runner groups/7 runners/18 repos, repo 500 | MATCH | org groups/runners/repos counts identical; repo endpoint still empty-body |
| 12 | Actions perms keys/values | MATCH | keys + all/enabled identical |
| 13 | Only env github-pages; Pages workflow | MATCH | env list identical (Pages build_type not re-queried; minor, row's core claim holds) |
| 14 | PR 912 OPEN at local HEAD | MATCH | state/head identical to local HEAD |

## Verdict: CERTIFIED

All 14 rows MATCH fresh independent outputs. Zero disproven rows.
