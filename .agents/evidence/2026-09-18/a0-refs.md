# A0 git-refs author — findings (bastion campaign, tailrocks/velnor @ docs/bastion-final-plan)

Date (UTC): 2026-09-16 ~23:41. Read-only. Local HEAD `38ffbfd73bc67002ddce7e38fdaf15ff6497317d` = PR #912 head (OPEN, consistent).
`git fetch origin`: clean, no new output (up to date). Local `origin/main` = `33688938297eee3933997dbbf368ee20fb1779d4` (matches API).

## 1. Full command outputs

### Main SHAs (gh api .../git/ref/heads/main)
```text
=== velnor main ===
{"sha":"33688938297eee3933997dbbf368ee20fb1779d4","url":"https://api.github.com/repos/tailrocks/velnor/git/commits/33688938297eee3933997dbbf368ee20fb1779d4"}
=== jackin main ===
{"sha":"0be3fcf95cd33c14fd3fe47af08026f17135a157","url":"https://api.github.com/repos/jackin-project/jackin/git/commits/0be3fcf95cd33c14fd3fe47af08026f17135a157"}
=== java-monorepo main ===
{"sha":"38a8fb5777fd02fec8b3904d86387aba05940e9a","url":"https://api.github.com/repos/ChainArgos/java-monorepo/git/commits/38a8fb5777fd02fec8b3904d86387aba05940e9a"}
=== scaleset main ===
{"sha":"e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5","url":"https://api.github.com/repos/actions/scaleset/git/commits/e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5"}
```
Default branch verified `main` for all three (tailrocks/velnor, jackin-project/jackin, ChainArgos/java-monorepo).

### Local refs
```text
origin/main: 33688938 Merge pull request #914 from tailrocks/chore/proof-candidate-path-3
             5703db61 chore(ci): bump D19 pin to 7341ef4b
             7341ef4b chore(ci): record why the artifact check pipes through jq
branch HEAD: 38ffbfd7 docs(bastion): fix markdownlint errors in plan files
```

### PR states
```text
PR 904: {"baseRefName":"main","headRefName":"feat/ci-immutable-runtime-products",
  "headRefOid":"facf18cce3236030d771a5ec67df69f55ff162ca","state":"MERGED",
  "title":"feat(ci): consume immutable runtime products instead of building velnor-workflow in CI",
  "mergedAt":"2026-09-16T20:29:27Z","mergeCommit":{"oid":"4dc8da58e2e8cbb6e30e792716075a2130ccffae"}}
PR 901: {"baseRefName":"main","headRefName":"fix/dogfood-pin-ruleset-403-fallback",
  "headRefOid":"6926760d27f616be2ac0d9f161182d02e8482910","state":"CLOSED",
  "title":"fix(dogfood): ruleset 403 fallback + D19 pin bump",
  "mergedAt":null,"closedAt":"2026-09-16T22:14:13Z"}   # closed UNMERGED
PR 912: {"baseRefName":"main","headRefName":"docs/bastion-final-plan",
  "headRefOid":"38ffbfd73bc67002ddce7e38fdaf15ff6497317d","state":"OPEN",
  "title":"docs(plan): bastion final plan with gates A-G and execution goal"}
```

### Releases (gh release list --limit 20)
```text
velnor-workflow-runtime-v1-9f236b40635970a4  Latest   2026-09-16T22:37:32Z
velnor-workflow-runtime-v1-93c8a35808d1210b          2026-09-16T22:33:12Z
velnor-workflow-runtime-v1-fd45efac853b6570          2026-09-16T22:16:15Z
velnor-workflow-runtime-v1-ff27a1c010cb4faf          2026-09-16T22:13:09Z
velnor-workflow-runtime-v1-1ff514a2852c82d3          2026-09-16T22:00:25Z
velnor-workflow-runtime-v1-78673ee16c42581e          2026-09-16T21:40:43Z
velnor-workflow-runtime-v1-c0ce56f09b27859c          2026-09-16T21:28:52Z
velnor-workflow-runtime-v1-6ae4ca31ef453567          2026-09-16T21:09:02Z
velnor-workflow-runtime-v1-ad493401f003a130          2026-09-16T20:48:02Z
velnor-workflow-runtime-v1-d39cbf21e351b410          2026-09-16T20:01:33Z
velnor-workflow-runtime-v1-e5ad6f4f923cd154          2026-09-16T19:35:49Z
velnor-workflow-runtime-v1-2bd48976b59bf084          2026-09-16T19:14:02Z
velnor-workflow-runtime-v1-f1f88c200e5b3b82          2026-09-16T18:46:02Z
velnor-workflow-runtime-v1-203eb9b79141e5a2          2026-09-16T18:33:48Z
v0.1.274                                             2026-09-06T22:29:26Z
v0.1.273                                             2026-09-06T20:56:02Z
v0.1.272                                             2026-09-06T19:29:54Z
v0.1.270                                             2026-09-06T17:34:26Z
v0.1.269                                             2026-09-06T15:36:42Z
v0.1.268                                             2026-09-06T14:24:48Z
```
Identity split (do not conflate):
- Crate version: `crates/velnor-runner/Cargo.toml @ origin/main` = `0.1.275`.
- v-tag release: latest `v0.1.274` (2026-09-06); `v0.1.275` → `release not found` (gap persists).
- Runtime-product release: closure-addressed `velnor-workflow-runtime-v1-<sha16>`, 14 live, Latest = `...-9f236b40635970a4`; race closure `f1f88c200e5b3b82` present (created 2026-09-16T18:43:34Z).
- Daemon package / official runner release: not queried in this step (runner baseline `v2.337.0` per evidence §6, re-resolve at conformance step).

### Scale Set refs
```text
heads/main: {"sha":"e6daac702355cdb5b880b4fbdcf6d85dcd9e48e5"}
tags/v0.4.0: {"sha":"6ce025902cd964747a078c2aabe7340ebc667eca","type":"commit"}  # unchanged
```

### APT omission blob (tailrocks/velnor-apt)
```text
contents/.github-gen/NO_WORKFLOWS_REQUIRED.md:
  {"name":"NO_WORKFLOWS_REQUIRED.md","path":".github-gen/NO_WORKFLOWS_REQUIRED.md",
   "sha":"3172bb883ec343a676d82c2594cb1a399191bf07","size":809}
velnor-apt heads/main: {"sha":"d62820d47a3814e98c4e25512151b4af14e3c3e1"}
content (head): declares apt-repository a descriptive label only; velnor-workflow has
no Class-A/B primitive for APT feed CI; typed config produces ci-unit-docs.yml only;
APT workflows intentionally omitted until generator gains apt-repository primitives.
=> APT primitives STILL ABSENT. Do not remove notice.
```

### Permissions (KEYS / SHAPE ONLY, no secret values)
```text
actions/permissions keys: ["allowed_actions","enabled","sha_pinning_required"]
  {allowed_actions: "all", enabled: true}
repo runner-groups: HTTP 500 Internal Server Error, empty body (GitHub-side; retry later)
org runner-groups: [{"id":1,"name":"Default","visibility":"all","allows_public_repositories":false},
                    {"id":3,"name":"velnor-trusted","visibility":"selected","allows_public_repositories":true}]
velnor-trusted repos (18): tailrocks/holla, holla-apt, homebrew-holla, homebrew-parallax,
  homebrew-ruxel, homebrew-tablerock, parallax, parallax-telemetry-playground, pg-bigdecimal,
  ruxel, schemalane, tablerock, tailrocks-skills, termrock, tracing-request-level,
  velnor, velnor-actions-fixture, velnor-apt
velnor-trusted runners: {total_count: 7}
environments: [{"name":"github-pages"}]
github-pages protection: {protection_rule_types: ["branch_policy"],
  branch_policy_keys: ["custom_branch_policies","protected_branches"]}
pages: {build_type: "workflow", status: null,
  keys: [build_type, cname, custom_404, html_url, https_certificate, https_enforced,
         pending_domain_unverified_at, protected_domain_state, public, source, status, url]}
```

## 2. Drift table vs plans/bastion-three-provider-ci/evidence.md

| # | Fact (evidence §) | Old observation | New evidence (2026-09-16) | Drift? | Consequence |
|---|---|---|---|---|---|
| 1 | Velnor main (§1) | `3353310c…` | `33688938…` (API = local origin/main; PR #914 merged) | YES | Audited SHA stale; re-resolve generator pin + 17-unit inventory at execution SHA before any gate |
| 2 | Jackin main (§1) | `92f347ac…` | `0be3fcf9…` | YES | Re-read unit manifest at new SHA; keep 40-unit/36R+1B+1D+2S counts, record coverage change |
| 3 | Java-monorepo main (§1) | `235e479b…` | `38a8fb57…` | YES | Re-read 71-unit table at new SHA; record coverage change |
| 4 | Generator pins (§1) | `b9c3156c…` (velnor/jackin), `1279c4f9…` (chainargos) | not re-resolved in A0 (refs-only step) | PENDING | Resolve at unit-manifest step; do not assume pins followed mains |
| 5 | PR #904 (§5, §7) | OPEN at `62a74bf5…`, race: closure `f1f88c20…` unavailable | MERGED 20:29Z at `facf18cc…` (merge `4dc8da58…`); product `...-f1f88c200e5b3b82` exists (18:43Z) | YES | Race closed by merge; reuse verified work; rerun evidence must come from post-merge runs, not PR description claims |
| 6 | PR #901 (§5) | ruleset-403 fallback, historical issue | CLOSED UNMERGED 22:14Z (`mergedAt: null`), head `6926760d…` | YES (status now known) | Keep as historical issue only; verify fallback logic lives elsewhere before relying on it |
| 7 | Crate vs release (§5) | runner `0.1.275`, no `v0.1.275` release | crate still `0.1.275`; `v0.1.275` still not found; latest v-tag `v0.1.274` | NO (gap persists) | Keep four-way distinction (crate / v-tag / runtime-product / daemon pkg); Policy `generated-tree` inputs must pin the runtime product, not the crate version |
| 8 | Runtime products (§5) | single race closure `f1f88c20…` | 14 `velnor-workflow-runtime-v1-*` releases, Latest `9f236b40…` (22:37Z) | YES (expanded) | Pin exact closure + conformance-test; `Latest` floats and is not a pin |
| 9 | Scale Set main (§6, §7) | `fb563005…` (2026-09-15) | `e6daac70…` | YES | Re-pin scaler revision + conformance-test listener API; `v0.4.0` tag stable at `6ce02590…`, never mix interfaces |
| 10 | APT omission (§5 APT, §7) | blob `3172bb88…`, primitives absent | blob `3172bb88…` (size 809), same content; velnor-apt main `d62820d4…` | NO | Generator must still implement apt-repository primitives; notice stays |
| 11 | Runner permissions (§7 "refresh") | no prior values recorded | org groups Default + velnor-trusted (18 repos incl. velnor, velnor-apt; 7 runners); repo endpoint HTTP 500 | NEW | Retry repo endpoint; trust/policy steps must enumerate the 7 runners + group membership at execution time |
| 12 | Actions perms (§7 "refresh") | no prior values recorded | keys [allowed_actions, enabled, sha_pinning_required]; all/enabled | NEW | Record as baseline; policy steps decide whether `all` is acceptable |
| 13 | Environments/Pages (§7 "refresh") | no prior values recorded | only env `github-pages` (branch_policy); Pages build_type `workflow`, status null | NEW | APT Pages-publish design must target `workflow` build type + github-pages branch policy |
| 14 | Branch PR #912 (header) | branch `docs/bastion-final-plan` | OPEN at `38ffbfd7…` = local HEAD | NO | Working baseline consistent |

## 3. A0 consequence summary
- 3/3 repo mains moved + Scale Set main moved + PR #904 merged: every §7 "still" claim except APT is now stale. Execution must use the new SHAs above, not the audited ones.
- No architectural change: drifts are ref movements and a merge, all within the revalidation loop the plan already requires.
- Open items for later steps: generator-pin re-resolution (4), repo runner-groups HTTP 500 retry (11), runner `v2.337.0` / digest conformance (out of A0 scope).
