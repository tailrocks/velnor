# Target Action Registry (source-of-truth pins)

> **What this is:** the authoritative list of action families Velnor's native
> adapters replace, each pinned to the **version we focus on**, with a direct
> link to that version's source so we can read the original TypeScript /
> composite / Docker behavior before implementing or changing an adapter.
>
> **Rule:** *the upstream source at the pinned tag is the contract.* When you
> implement or modify an adapter, open the **Source @ version** link, read the
> real inputs/outputs/behavior, and match it — do not guess from docs or memory.
>
> **Always track latest.** Each row records both the **Focus** version (what our
> adapter targets today) and where to check the **latest** release. Re-verify on
> every coverage pass; when upstream ships a newer major/minor that the targets
> adopt, bump Focus, update the source link, re-read the diff, and re-audit the
> adapter. Stale pins are a bug.
>
> **Scope of analysis is consumer-driven.** We do **not** reimplement every
> upstream feature — only the features the two consumer repos actually use:
>
> - **Jackin** — `jackin-project/jackin`
> - **ChainArgos** — `ChainArgos/java-monorepo`
>
> Those workflows define which inputs/outputs matter. A feature absent from both
> consumers is out of focus until one adopts it. The **Features in focus** column
> is the intersection of "upstream supports it" and "a consumer uses it."

## How to re-verify a row

```sh
# Latest release for a repo (compare against the Focus pin below):
gh api repos/<owner>/<repo>/releases/latest --jq '.tag_name'

# Read the source at the pinned version (TypeScript = real behavior):
#   open the "Source @ version" URL, inspect action.yml + src/
```

The fixture's pinned `@vN` references mirror this table; keep them in sync with
`crates/velnor-tools/src/main.rs` (`fixture_required_snippets`) and the audit
list (`target-audit --check-target-mvp`).

## Core `actions/*`

| Action | Repo | Kind | Focus | Source @ version | Latest | Features in focus |
|--------|------|------|-------|------------------|--------|-------------------|
| `actions/checkout` | [actions/checkout](https://github.com/actions/checkout) | TS | `v5` ⚠️ | <https://github.com/actions/checkout/tree/v5> | [releases](https://github.com/actions/checkout/releases) | `path`, `ref`, `token` + token masking, `fetch-depth`, external/selected-repo checkout. **Out of focus** (contract §132): `submodules`, sparse checkout, LFS. ⚠️ pin unverified — fixture does not pin checkout; confirm the version the consumers use and adjust. |
| `actions/cache` | [actions/cache](https://github.com/actions/cache) | TS | `v5` | <https://github.com/actions/cache/tree/v5> | [releases](https://github.com/actions/cache/releases) | key, `restore-keys` prefix, exact/partial hit, `fail-on-cache-miss`, `lookup-only`, outputs (`cache-hit`, `cache-primary-key`, `cache-matched-key`) |
| `actions/upload-artifact` | [actions/upload-artifact](https://github.com/actions/upload-artifact) | TS | `v7` | <https://github.com/actions/upload-artifact/tree/v7> | [releases](https://github.com/actions/upload-artifact/releases) | glob, `if-no-files-found`, `include-hidden-files`, `retention-days`, outputs (`artifact-id`, `artifact-url`, `artifact-digest`) |
| `actions/download-artifact` | [actions/download-artifact](https://github.com/actions/download-artifact) | TS | `v8` | <https://github.com/actions/download-artifact/tree/v8> | [releases](https://github.com/actions/download-artifact/releases) | pattern/name, `merge-multiple`, `download-path` |
| `actions/upload-pages-artifact` | [actions/upload-pages-artifact](https://github.com/actions/upload-pages-artifact) | composite | `v5` | <https://github.com/actions/upload-pages-artifact/tree/v5> | [releases](https://github.com/actions/upload-pages-artifact/releases) | path, single-file tar contract for Pages |
| `actions/deploy-pages` | [actions/deploy-pages](https://github.com/actions/deploy-pages) | TS | `v5` | <https://github.com/actions/deploy-pages/tree/v5> | [releases](https://github.com/actions/deploy-pages/releases) | `page_url` output (synthetic for now — see checklist §3) |

## Rust / setup / tooling

| Action | Repo | Kind | Focus | Source @ version | Latest | Features in focus |
|--------|------|------|-------|------------------|--------|-------------------|
| `dorny/paths-filter` | [dorny/paths-filter](https://github.com/dorny/paths-filter) | TS | `v4` | <https://github.com/dorny/paths-filter/tree/v4> | [releases](https://github.com/dorny/paths-filter/releases) | YAML rules, glob, per-rule boolean + `_count` + `_files` + `changes`, git-diff source |
| `jdx/mise-action` | [jdx/mise-action](https://github.com/jdx/mise-action) | TS | `v4` | <https://github.com/jdx/mise-action/tree/v4> | [releases](https://github.com/jdx/mise-action/releases) | install, `install_args`, working-directory, `MISE_*` env, PATH injection (incl. Python-via-mise) |
| `extractions/setup-just` | [extractions/setup-just](https://github.com/extractions/setup-just) | TS | `v4` | <https://github.com/extractions/setup-just/tree/v4> | [releases](https://github.com/extractions/setup-just/releases) | just version resolution + PATH |
| `Swatinem/rust-cache` | [Swatinem/rust-cache](https://github.com/Swatinem/rust-cache) | TS | `v2` | <https://github.com/Swatinem/rust-cache/tree/v2> | [releases](https://github.com/Swatinem/rust-cache/releases) | `shared-key`, `cache-directories`, `cache-on-failure`, outputs |
| `mozilla-actions/sccache-action` | [mozilla-actions/sccache-action](https://github.com/mozilla-actions/sccache-action) | TS | `v0.0.10` | <https://github.com/mozilla-actions/sccache-action/tree/v0.0.10> | [releases](https://github.com/mozilla-actions/sccache-action/releases) | server start + env injection, soft-fail gates, `--show-stats` |
| `rui314/setup-mold` | [rui314/setup-mold](https://github.com/rui314/setup-mold) | composite | `v1` | <https://github.com/rui314/setup-mold/tree/v1> | [releases](https://github.com/rui314/setup-mold/releases) | mold install + linker wiring |
| `crazy-max/ghaction-github-runtime` | [crazy-max/ghaction-github-runtime](https://github.com/crazy-max/ghaction-github-runtime) | TS | `v4` | <https://github.com/crazy-max/ghaction-github-runtime/tree/v4> | [releases](https://github.com/crazy-max/ghaction-github-runtime/releases) | exports `ACTIONS_*` runtime env |
| `renovatebot/github-action` | [renovatebot/github-action](https://github.com/renovatebot/github-action) | TS+Docker | `v46` | <https://github.com/renovatebot/github-action/tree/v46> | [releases](https://github.com/renovatebot/github-action/releases) | runs renovate image, token masking, `RENOVATE_*` env |

## Docker family

| Action | Repo | Kind | Focus | Source @ version | Latest | Features in focus |
|--------|------|------|-------|------------------|--------|-------------------|
| `docker/login-action` | [docker/login-action](https://github.com/docker/login-action) | TS | `v4` | <https://github.com/docker/login-action/tree/v4> | [releases](https://github.com/docker/login-action/releases) | stdin password, default registry |
| `docker/setup-buildx-action` | [docker/setup-buildx-action](https://github.com/docker/setup-buildx-action) | TS | `v4` | <https://github.com/docker/setup-buildx-action/tree/v4> | [releases](https://github.com/docker/setup-buildx-action/releases) | buildx builder bootstrap |
| `docker/metadata-action` | [docker/metadata-action](https://github.com/docker/metadata-action) | TS | `v6` | <https://github.com/docker/metadata-action/tree/v6> | [releases](https://github.com/docker/metadata-action/releases) | tag templates (`type=semver`/`ref`/`sha`/pep440/custom), `tags`/`labels`/`json` outputs |
| `docker/build-push-action` | [docker/build-push-action](https://github.com/docker/build-push-action) | TS | `v7` | <https://github.com/docker/build-push-action/tree/v7> | [releases](https://github.com/docker/build-push-action/releases) | file, platforms, tags, labels, `cache-from`/`cache-to`, push/load |
| `docker/bake-action` | [docker/bake-action](https://github.com/docker/bake-action) | TS | `v7` | <https://github.com/docker/bake-action/tree/v7> | [releases](https://github.com/docker/bake-action/releases) | files, `set`, push |

## Local composites & reusable workflows (in-repo, not versioned upstream)

| Item | Where | Features in focus |
|------|-------|-------------------|
| `aggregate-needs` | `.github/actions/aggregate-needs` | `needs.*.result` + `toJSON(needs)` for required aggregate jobs |
| `check-deployed-docs` | `.github/actions/check-deployed-docs` | Pages deploy verification |
| `check-fixture-output` | `.github/actions/check-fixture-output` | fixture output assertions |
| GitHub-expanded reusable workflows | consumer repos | `workflow_call`, inputs, `secrets: inherit`, `toJSON(needs)` |

## Runtime features (behavior truth: `actions/runner`)

Not versioned per-action; pinned to the runner release tracked in
[`latest-runner-v2-refresh-2026-06-01.md`](latest-runner-v2-refresh-2026-06-01.md)
(`actions/runner` `v2.334.0`). Re-verify with
`cargo run -q -p velnor-tools -- check-runner-reference`.

- command files (`GITHUB_ENV`/`OUTPUT`/`PATH`/`STATE`/`STEP_SUMMARY`, heredoc)
- job/step outputs, `needs.*.result`
- expression eval (`lit`/`expr`/`format`; contexts; `contains`/`fromJSON`/`toJSON`)
- `defaults.run`, per-step `working-directory`, matrix expansion
- runtime/cache/OIDC env injection, secret masking
