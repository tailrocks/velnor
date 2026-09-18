# Consumer Recon — 2026-09-17T09:18Z (READ-ONLY, no mutations)

## 1. Live SHAs vs retained baselines

| Repo | Retained | Live (`gh api repos/<r>/git/ref/heads/main --jq .object.sha`) | ahead/behind (`compare/<old>...<new>`) | Units retained → live proxy* |
|---|---|---|---|---|
| tailrocks/velnor | 3353310c | `c04aec98e590e0a20c3316abbfa7880fae32b32a` | ahead **83**, behind 0 | 17 → **37** |
| jackin-project/jackin | 92f347ac | `e4300b431f3f06b2f810a54881a2b2332daf2af1` | ahead **2**, behind 0 | 40 → 40 |
| ChainArgos/java-monorepo | 235e479b | `38a8fb5777fd02fec8b3904d86387aba05940e9a` | ahead **1**, behind 0 | 71 → 71 |

\* Live proxy = `grep -c "uses: \./"` in each repo's `.github/workflows/ci-main.yml`
(fetched via `gh api repos/<r>/contents/.github/workflows/ci-main.yml --jq .content | base64 -d`).
Jackin 40 and ChainArgos 71 match retained counts exactly, so proxy is credible; Velnor drifted 17→37.

Velnor main head commits (`gh api repos/tailrocks/velnor/commits`, first 5):
- c04aec98 2026-09-17T09:16:17Z Merge PR #924 (fix/d2-bridge-r2)
- 73356c2c regen generator state for R2 bridge sources
- eb0303a3 feat(workflow): bridge provider-schema pipeline alongside schema-1 (R2)
- a2840748 fix(workflow): gate renovate token via job env (#923)
- ca832734 same, pre-merge

Drift as old→new→consequence:
- Velnor 3353310c→c04aec98 (+83, units 17→37) → campaign baselines/pins stale; regen state + consumer re-sync required.
- Jackin 92f347ac→e4300b43 (+2, units 40=40) → stable; no unit drift.
- ChainArgos 235e479b→38a8fb57 (+1, units 71=71) → stable; no unit drift.

## 2. tailrocks/velnor PRs + releases

`gh pr view <n> --repo tailrocks/velnor --json state,headRefOid,title,baseRefName`:
- #901 `fix(dogfood): ruleset 403 fallback + D19 pin bump` — state **CLOSED** (unmerged), head 6926760d27f616be2ac0d9f161182d02e8482910
- #904 `feat(ci): consume immutable runtime products instead of building velnor-workflow in CI` — state **MERGED**, head facf18cce3236030d771a5ec67df69f55ff162ca
- #912 `docs(plan): bastion final plan with gates A-G and execution goal` — state **OPEN**, head 8b7b4ac1450051dda5d39ca124dc8b011a0d102d

`gh release list --repo tailrocks/velnor --limit 20`: 20/20 are `velnor-workflow-runtime-v1-<sha>` immutable runtime products, no semver releases in window. Latest: `velnor-workflow-runtime-v1-304405643c571048` (2026-09-17T08:24:02Z, tagged Latest). Oldest in window: `...-d39cbf21e351b410` (2026-09-16T20:01:33Z).

## 3. tailrocks/velnor-apt

- main SHA: `d62820d47a3814e98c4e25512151b4af14e3c3e1` (`chore: sync velnor-workflow to b9c3156c (#238)`).
- Signed-feed omission notice: **STILL EXISTS** — `.github-gen/NO_WORKFLOWS_REQUIRED.md`, blob `3172bb883ec343a676d82c2594cb1a399191bf07`, 809 bytes. Only file in repo matching code-search `omitted`; `unsigned`/`omission` match nothing. Text: "APT workflows are intentionally omitted until the generator gains apt-repository primitives (signed reprepro + Pages publish, package channel updater)."
- BUT feed is live and signed: `curl -sI https://velnor-apt.tailrocks.com/dists/stable/InRelease` → **HTTP/2 200**, last-modified Mon 14 Sep 2026 14:53:13 GMT; body starts `-----BEGIN PGP SIGNED MESSAGE-----` / `Hash: SHA512`. `/velnor.gpg` → 200, same timestamp. Root `/` → **404** (GitHub Pages stock 404; expected — apt repos serve no index).
- Pages (`gh api repos/tailrocks/velnor-apt/pages`): build_type `workflow`, cname `velnor-apt.tailrocks.com` verified, HTTPS enforced, cert approved (expires 2026-11-06).
- Contradiction to note: repo tree has **no publish.yml** — only YAML under main is `.github/workflows/ci-unit-docs.yml` (+ actionlint.yaml), confirmed via recursive tree. README's `publish.yml` reference is aspirational; something outside apt main published the signed Sep-14 feed. Notice premise is stale.

## 4. Scale Set (actions/runner)

`git ls-remote https://github.com/actions/runner.git HEAD` and `refs/heads/main` → both `80bb1fb827fa44d489263061e71ef4adba7ad8cd`. Retained fb563005 → live 80bb1fb8: **drifted**, re-pin/re-validate runner source before protocol work.

## 5. Jackin + ChainArgos

Generator pins (`.github-gen/velnor-workflow.toml` `[generator]`):
- Jackin: repository `jackin-project/jackin`, **revision `b6f53c094e9f021f0a2503fdc3d58300b6408a1b`** (pinned).
- ChainArgos: repository `ChainArgos/java-monorepo`, **NO revision key** (floating generator!).
- (velnor-apt for reference: revision `b9c3156cdb88e63c11b9e595a3e694b02238c09a`.)

Default-branch CI health (`gh run list --repo <r> --limit 3`):
- Jackin: Maintenance/schedule success (10s); **CI/Main workflow_dispatch FAILURE** (9m15s, run 35201266199, 08:43Z); Nightly success (10s). → main-lane CI red.
- ChainArgos: **all 3 runs QUEUED 5–12h** (Maintenance 5h34m, Nightly 5h48m, CI/Main push 12h46m). → velnor-only lane (`runners="velnor"`) with no fleet pickup; nothing executing.

Local actions consumers use that Velnor doesn't own: **NONE** — but G1 gap is the inverse:
- ChainArgos all 6 `ci-unit-*.yml` (bun/docker/docs/gradle/node/rust) contain `uses: ./.github/actions/report-velnor-ci-outcomes`, yet ChainArgos has **no `.github/actions/` dir** (contents API 404). Velnor **owns** `.github/actions/report-velnor-ci-outcomes` (plus `setup-velnor-workflow`). G1 work = vendor/sync the Velnor-owned action into ChainArgos, not new ownership.
- Jackin: zero `uses: ./` outside reusable-workflow calls (`ci-unit-bun/docker/rust.yml`, all present locally; code-search confirms only ci-pr/ci-main match); no `.github/actions` dir and no references to it. Clean.
- Reusable-workflow refs (`./.github/workflows/ci-unit-*.yml`) in both consumers resolve to files present in their own trees (Jackin also ships unused-here `ci-unit-swift.yml`, `ci-policy.yml`, desktop/renovate workflows; ChainArgos ships `ci-unit-gradle/node/docs.yml` which Velnor lacks as files but that is generated-per-repo surface, not a missing action).

## 6. Permissions (no values printed)

- `gh auth status`: logged in as donbeave (keyring); token scopes: **admin:org, delete_repo, gist, repo, workflow**. No value printed (only scope names + masked `gho_***`).
- Administration:write for velnor-d1-canary app: **NO**.
  How determined (all read-only): `gh api apps/velnor-d1-canary` → HTTP 404 (slug unresolvable); `gh api orgs/tailrocks/installations` lists 5 app_slugs (renovate, dco-2, tailrocks-package-updater, chatgpt-codex-connector, jackin-daemon) — no canary; `orgs/jackin-project/installations` (claude, dco-2, sonarqubecloud, jackin-package-updater, chatgpt-codex-connector, jackin-daemon) and `orgs/ChainArgos/installations` (slack, gitbook-com, claude, amp-for-github, chatgpt-codex-connector, linear-code, …) — no canary. No installation in any campaign org ⇒ no Administration grant in effect.

## Commands run (all read-only: gh api GET, gh pr view, gh release list, gh run list, gh auth status, git ls-remote, curl -sI/-s GET)
1. `gh api repos/tailrocks/velnor/git/ref/heads/main --jq .object.sha` (+ jackin, + java-monorepo)
2. `gh pr view {901,904,912} --repo tailrocks/velnor --json state,headRefOid,title,baseRefName`
3. `gh release list --repo tailrocks/velnor --limit 20`
4. `gh api repos/tailrocks/velnor-apt/git/ref/heads/main --jq .object.sha`
5. `curl -sI https://velnor-apt.tailrocks.com/` + `curl -s … | head -50`
6. `git ls-remote https://github.com/actions/runner.git HEAD` + `refs/heads/main`
7. `gh auth status`
8. `gh api repos/tailrocks/velnor-apt/git/trees/main`, `…/contents/`
9. `gh run list --repo jackin-project/jackin --limit 3` (+ ChainArgos)
10. `gh api repos/<r>/contents/.github-gen[/velnor-workflow.toml] --jq .content | base64 -d` (jackin, chainargos, apt)
11. `gh api repos/tailrocks/velnor-apt/readme --jq .content | base64 -d`; code-searches `repo:tailrocks/velnor-apt {signed,omission,unsigned,omitted}`
12. `gh api apps/velnor-d1-canary`; `gh api orgs/{tailrocks,jackin-project,ChainArgos}/installations`
13. `gh api repos/tailrocks/velnor-apt/pages`; `…/commits` (5); `…/contents/{.github-gen,conf,config}`
14. code-search `uses: ./ path:.github/workflows` in jackin (2 files) + chainargos (8 files); decoded ci-pr/ci-main/ci-unit-*.yml and grepped `uses: \./`
15. `gh api repos/<r>/compare/<old>...<new> --jq {ahead,behind}` ×3
16. `curl -sI …/dists/stable/InRelease`, `…/velnor.gpg`; `curl -s …/InRelease | head -6`
17. `gh api repos/<r>/contents/.github/actions` ×3 (velnor: 2 entries; jackin+chainargos: 404)
18. `gh api repos/<r>/contents/.github/workflows` ×3; `gh api repos/tailrocks/velnor/commits` (5); grep -c unit proxies
19. `gh api repos/tailrocks/velnor-apt/contents/.github-gen/NO_WORKFLOWS_REQUIRED.md --jq {path,sha,size}`
20. `gh api repos/tailrocks/velnor-apt/git/trees/main?recursive=1` (yml filter); `gh run list --repo tailrocks/velnor-apt --limit 5`
