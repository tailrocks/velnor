# Target Action Registry (source-of-truth pins)

> **What this is:** the authoritative list of action families Velnor's native
> adapters replace, each pinned to an **exact release tag + frozen commit SHA**,
> with a direct link to that commit's source so we can read the original
> TypeScript / composite / Docker behavior before implementing or changing an
> adapter.
>
> **Rule:** *the upstream source at the pinned SHA is the contract.* When you
> implement or modify an adapter, open the **Source @ SHA** link, read the
> real inputs/outputs/behavior, and match it — do not guess from docs or memory.
>
> **Always track latest.** Pins are the **latest stable release** as of the
> verify date below. Each row records the exact **Focus tag**, its frozen SHA,
> and a **Latest** link. Re-verify on every coverage pass; when upstream ships a
> newer release, bump the tag + SHA, re-read the diff, and re-audit the adapter.
> Stale pins are a bug.
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
# Latest release tag for a repo (compare against the Focus tag below):
gh api repos/<owner>/<repo>/releases/latest --jq '.tag_name'

# Resolve a tag to its frozen commit SHA (the immutable Source @ SHA link):
gh api repos/<owner>/<repo>/commits/<tag> --jq '.sha'

# Read the source at the pinned SHA (TypeScript = real behavior):
#   open the "Source @ SHA" link, inspect action.yml + src/
```

**Registry vs fixture pins.** Velnor routes native action families by *family*
and **ignores the pinned `@ref`** (contract §66), so the fixture YAML may pin
loose major tags (`@v4`) while this registry pins the exact tag + commit SHA.
The exact pin here is the **comparison anchor** — the source we read to verify
adapter behavior. When the fixture repo bumps a tag, re-run the verify command
below and update the affected row. The fixture-snippet assertions in
`crates/velnor-tools/src/main.rs` (`fixture_required_snippets`) track the
fixture's *actual* tags, not these exact pins.

> **Pin format.** Each row pins an **exact release tag** and its **frozen commit
> SHA**. The *Source @ SHA* link points at the immutable commit tree — that is
> the stable thing we read and compare against; tags can be re-pointed, a SHA
> cannot. *Latest verified:* 2026-06-03 via
> `gh api repos/<owner>/<repo>/releases/latest`.

## Core `actions/*`

| Action | Repo | Kind | Focus tag | Source @ SHA (frozen) | Latest | Features in focus |
|--------|------|------|-----------|-----------------------|--------|-------------------|
| `actions/checkout` | [actions/checkout](https://github.com/actions/checkout) | TS | `v6.0.3` | [`df4cb1c`](https://github.com/actions/checkout/tree/df4cb1c069e1874edd31b4311f1884172cec0e10) | [releases](https://github.com/actions/checkout/releases) | `path`, `ref`, `token` + token masking, `fetch-depth`, external/selected-repo checkout. **Out of focus** (contract §132): `submodules`, sparse checkout, LFS. ⚠️ fixture does not pin checkout — confirm the version the consumers use. |
| `actions/cache` | [actions/cache](https://github.com/actions/cache) | TS | `v5.0.5` | [`27d5ce7`](https://github.com/actions/cache/tree/27d5ce7f107fe9357f9df03efb73ab90386fccae) | [releases](https://github.com/actions/cache/releases) | key, `restore-keys` prefix, exact/partial hit, `fail-on-cache-miss`, `lookup-only`, outputs (`cache-hit`, `cache-primary-key`, `cache-matched-key`) |
| `actions/upload-artifact` | [actions/upload-artifact](https://github.com/actions/upload-artifact) | TS | `v7.0.1` | [`043fb46`](https://github.com/actions/upload-artifact/tree/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a) | [releases](https://github.com/actions/upload-artifact/releases) | glob, `if-no-files-found`, `include-hidden-files`, `retention-days`, outputs (`artifact-id`, `artifact-url`, `artifact-digest`) |
| `actions/download-artifact` | [actions/download-artifact](https://github.com/actions/download-artifact) | TS | `v8.0.1` | [`3e5f45b`](https://github.com/actions/download-artifact/tree/3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c) | [releases](https://github.com/actions/download-artifact/releases) | pattern/name, `merge-multiple`, `download-path` |
| `actions/upload-pages-artifact` | [actions/upload-pages-artifact](https://github.com/actions/upload-pages-artifact) | composite | `v5.0.0` | [`fc324d3`](https://github.com/actions/upload-pages-artifact/tree/fc324d3547104276b827a68afc52ff2a11cc49c9) | [releases](https://github.com/actions/upload-pages-artifact/releases) | path, single-file tar contract for Pages |
| `actions/deploy-pages` | [actions/deploy-pages](https://github.com/actions/deploy-pages) | TS | `v5.0.0` | [`cd2ce8f`](https://github.com/actions/deploy-pages/tree/cd2ce8fcbc39b97be8ca5fce6e763baed58fa128) | [releases](https://github.com/actions/deploy-pages/releases) | `page_url` output (synthetic for now — see checklist §3) |

## Rust / setup / tooling

| Action | Repo | Kind | Focus tag | Source @ SHA (frozen) | Latest | Features in focus |
|--------|------|------|-----------|-----------------------|--------|-------------------|
| `dorny/paths-filter` | [dorny/paths-filter](https://github.com/dorny/paths-filter) | TS | `v4.0.1` | [`fbd0ab8`](https://github.com/dorny/paths-filter/tree/fbd0ab8f3e69293af611ebaee6363fc25e6d187d) | [releases](https://github.com/dorny/paths-filter/releases) | YAML rules, glob, per-rule boolean + `_count` + `_files` + `changes`, git-diff source |
| `jdx/mise-action` | [jdx/mise-action](https://github.com/jdx/mise-action) | TS | `v4.0.1` | [`1648a78`](https://github.com/jdx/mise-action/tree/1648a7812b9aeae629881980618f079932869151) | [releases](https://github.com/jdx/mise-action/releases) | install, `install_args`, working-directory, `MISE_*` env, PATH injection (incl. Python-via-mise) |
| `extractions/setup-just` | [extractions/setup-just](https://github.com/extractions/setup-just) | TS | `v4.0.0` | [`53165ef`](https://github.com/extractions/setup-just/tree/53165ef7e734c5c07cb06b3c8e7b647c5aa16db3) | [releases](https://github.com/extractions/setup-just/releases) | just version resolution + PATH |
| `Swatinem/rust-cache` | [Swatinem/rust-cache](https://github.com/Swatinem/rust-cache) | TS | `v2.9.1` | [`c193711`](https://github.com/Swatinem/rust-cache/tree/c19371144df3bb44fab255c43d04cbc2ab54d1c4) | [releases](https://github.com/Swatinem/rust-cache/releases) | `shared-key`, `cache-directories`, `cache-on-failure`, outputs |
| `mozilla-actions/sccache-action` | [mozilla-actions/sccache-action](https://github.com/mozilla-actions/sccache-action) | TS | `v0.0.10` | [`9e7fa8a`](https://github.com/mozilla-actions/sccache-action/tree/9e7fa8a12102821edf02ca5dbea1acd0f89a2696) | [releases](https://github.com/mozilla-actions/sccache-action/releases) | server start + env injection, soft-fail gates, `--show-stats` |
| `rui314/setup-mold` | [rui314/setup-mold](https://github.com/rui314/setup-mold) | composite | `v1` | [`9c9c13b`](https://github.com/rui314/setup-mold/tree/9c9c13bf4c3f1adef0cc596abc155580bcb04444) | [tags](https://github.com/rui314/setup-mold/tags) | mold install + linker wiring. (No semver releases; `v1` is the only stable major.) |
| `crazy-max/ghaction-github-runtime` | [crazy-max/ghaction-github-runtime](https://github.com/crazy-max/ghaction-github-runtime) | TS | `v4.0.0` | [`04d248b`](https://github.com/crazy-max/ghaction-github-runtime/tree/04d248b84655b509d8c44dc1d6f990c879747487) | [releases](https://github.com/crazy-max/ghaction-github-runtime/releases) | exports `ACTIONS_*` runtime env |
| `renovatebot/github-action` | [renovatebot/github-action](https://github.com/renovatebot/github-action) | TS+Docker | `v46.1.14` | [`693b9ef`](https://github.com/renovatebot/github-action/tree/693b9ef15eec82123529a37c782242f091365961) | [releases](https://github.com/renovatebot/github-action/releases) | runs renovate image, token masking, `RENOVATE_*` env |

## Docker family

| Action | Repo | Kind | Focus tag | Source @ SHA (frozen) | Latest | Features in focus |
|--------|------|------|-----------|-----------------------|--------|-------------------|
| `docker/login-action` | [docker/login-action](https://github.com/docker/login-action) | TS | `v4.2.0` | [`650006c`](https://github.com/docker/login-action/tree/650006c6eb7dba73a995cc03b0b2d7f5ca915bee) | [releases](https://github.com/docker/login-action/releases) | stdin password, default registry |
| `docker/setup-buildx-action` | [docker/setup-buildx-action](https://github.com/docker/setup-buildx-action) | TS | `v4.1.0` | [`d7f5e7f`](https://github.com/docker/setup-buildx-action/tree/d7f5e7f509e45cec5c76c4d5afdd7de93d0b3df5) | [releases](https://github.com/docker/setup-buildx-action/releases) | buildx builder bootstrap |
| `docker/metadata-action` | [docker/metadata-action](https://github.com/docker/metadata-action) | TS | `v6.1.0` | [`80c7e94`](https://github.com/docker/metadata-action/tree/80c7e94dd9b9319bd5eb7a0e0fe9291e23a2a2e9) | [releases](https://github.com/docker/metadata-action/releases) | tag templates (`type=semver`/`ref`/`sha`/pep440/custom), `tags`/`labels`/`json` outputs |
| `docker/build-push-action` | [docker/build-push-action](https://github.com/docker/build-push-action) | TS | `v7.2.0` | [`f9f3042`](https://github.com/docker/build-push-action/tree/f9f3042f7e2789586610d6e8b85c8f03e5195baf) | [releases](https://github.com/docker/build-push-action/releases) | file, platforms, tags, labels, `cache-from`/`cache-to`, push/load |
| `docker/bake-action` | [docker/bake-action](https://github.com/docker/bake-action) | TS | `v7.2.0` | [`6614cfa`](https://github.com/docker/bake-action/tree/6614cfa25eff9a0b2b2697efb0b6159e7680d584) | [releases](https://github.com/docker/bake-action/releases) | files, `set`, push |

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
