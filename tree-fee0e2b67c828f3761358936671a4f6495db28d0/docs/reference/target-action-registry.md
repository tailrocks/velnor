# Target Action Registry (source-of-truth)

> **What this is:** the authoritative list of action families Velnor's native
> adapters replace, with a direct link to each action's **latest** source so we
> can read the original TypeScript / composite / Docker behavior before
> implementing or changing an adapter.
>
> **One version: latest.** Velnor tracks the **latest** behavior of each action
> only — it does **not** support historical versions. Velnor routes by action
> *family* and ignores the pinned `@ref` (contract §66), so the exact tag in any
> workflow does not matter. Only the **major** is recorded below as a coarse
> marker. When upstream changes behavior in a way a consumer relies on, update
> the Velnor adapter to match — that is the maintenance model.
>
> **Rule:** *the latest upstream source is the contract.* When you implement or
> modify an adapter, open the **Source** link, read the real
> inputs/outputs/behavior, and match it — do not guess from docs or memory.
>
> **Scope of analysis is consumer-driven.** We do **not** reimplement every
> upstream feature — only the features the two consumer repos actually use:
>
> - **Jackin** — `jackin-project/jackin`
> - **ChainArgos** — `ChainArgos/java-monorepo`
>
> A feature absent from both consumers is out of focus until one adopts it. The
> **Features in focus** column is the intersection of "upstream supports it" and
> "a consumer uses it."

## How to re-verify a row

```sh
# Latest release for a repo (informational — Velnor always targets latest):
gh api repos/<owner>/<repo>/releases/latest --jq '.tag_name'

# Read the latest source (TypeScript = real behavior):
#   open the "Source" link below (default branch / latest major), inspect action.yml + src/
```

The fixture-snippet assertions in `crates/velnor-tools/src/main.rs`
(`fixture_required_snippets`) track the fixture's *actual* tags; they are
routing markers, not behavior pins.

## Core `actions/*`

| Action | Major | Kind | Source (latest) | Features in focus |
|--------|-------|------|-----------------|-------------------|
| `actions/checkout` | v6 | TS | [main](https://github.com/actions/checkout) | `path`, `ref`, `token` + token masking, `fetch-depth`, external/selected-repo checkout. **Out of focus** (contract §132): `submodules`, sparse checkout, LFS. |
| `actions/cache` | v5 | TS | [main](https://github.com/actions/cache) | key, `restore-keys` prefix, exact/partial hit, `fail-on-cache-miss`, `lookup-only`, outputs (`cache-hit`, `cache-primary-key`, `cache-matched-key`) |
| `actions/upload-artifact` | v7 | TS | [main](https://github.com/actions/upload-artifact) | glob, `if-no-files-found`, `include-hidden-files`, `retention-days`, outputs (`artifact-id`, `artifact-url`, `artifact-digest`) |
| `actions/download-artifact` | v8 | TS | [main](https://github.com/actions/download-artifact) | pattern/name, `merge-multiple`, `download-path` |
| `actions/upload-pages-artifact` | v5 | composite | [main](https://github.com/actions/upload-pages-artifact) | path, single-file tar contract for Pages |
| `actions/deploy-pages` | v5 | TS | [main](https://github.com/actions/deploy-pages) | `page_url` output (synthetic for now — see checklist §3) |

## Rust / setup / tooling

| Action | Major | Kind | Source (latest) | Features in focus |
|--------|-------|------|-----------------|-------------------|
| `dorny/paths-filter` | v4 | TS | [master](https://github.com/dorny/paths-filter) | YAML rules, glob, per-rule boolean + `_count` + `_files` + `changes`, git-diff source |
| `jdx/mise-action` | v4 | TS | [main](https://github.com/jdx/mise-action) | install, `install_args`, working-directory, `MISE_*` env, PATH injection (incl. Python-via-mise) |
| `extractions/setup-just` | v4 | TS | [main](https://github.com/extractions/setup-just) | just version resolution + PATH |
| `Swatinem/rust-cache` | v2 | TS | [master](https://github.com/Swatinem/rust-cache) | `shared-key`, `cache-directories`, `cache-on-failure`, outputs |
| `mozilla-actions/sccache-action` | v0 | TS | [master](https://github.com/mozilla-actions/sccache-action) | server start + env injection, soft-fail gates, `--show-stats` |
| `rui314/setup-mold` | v1 | composite | [main](https://github.com/rui314/setup-mold) | mold install + linker wiring |
| `crazy-max/ghaction-github-runtime` | v4 | TS | [master](https://github.com/crazy-max/ghaction-github-runtime) | exports `ACTIONS_*` runtime env |
| `renovatebot/github-action` | v46 | TS+Docker | [main](https://github.com/renovatebot/github-action) | runs renovate image, token masking, `RENOVATE_*` env |
| `docker/setup-qemu-action` | v3 | TS | [master](https://github.com/docker/setup-qemu-action) | binfmt install via the QEMU static image (`docker run --rm --privileged <image> --install <platforms>`), reset support, platforms output; cache-image is GHA-specific and ignored |
| `sigstore/cosign-installer` | v4 | composite (pwsh) | [main](https://github.com/sigstore/cosign-installer) | native install: preinstalled pinned cosign in the job image, version-mismatch download from the official release, install-dir on PATH |
| `hadolint/hadolint-action` | v3 | Docker | [master](https://github.com/hadolint/hadolint-action) | hadolint binary preinstalled in the job image; `HADOLINT_*` env mapping, `-c` config, recursive find, `results` output + `HADOLINT_RESULTS` env (agent-role repos via jackin-role-action) |

## Docker family

| Action | Major | Kind | Source (latest) | Features in focus |
|--------|-------|------|-----------------|-------------------|
| `docker/login-action` | v4 | TS | [master](https://github.com/docker/login-action) | stdin password, default registry |
| `docker/setup-buildx-action` | v4 | TS | [master](https://github.com/docker/setup-buildx-action) | buildx builder bootstrap |
| `docker/metadata-action` | v6 | TS | [master](https://github.com/docker/metadata-action) | tag templates (`type=semver`/`ref`/`sha`/pep440/custom), `tags`/`labels`/`json` outputs |
| `docker/build-push-action` | v7 | TS | [master](https://github.com/docker/build-push-action) | file, platforms, tags, labels, `cache-from`/`cache-to`, push/load |
| `docker/bake-action` | v7 | TS | [master](https://github.com/docker/bake-action) | files, `set`, push |

## Local composites & reusable workflows (in-repo, not versioned upstream)

| Item | Where | Features in focus |
|------|-------|-------------------|
| `aggregate-needs` | `.github/actions/aggregate-needs` | `needs.*.result` + `toJSON(needs)` for required aggregate jobs |
| `check-deployed-docs` | `.github/actions/check-deployed-docs` | Pages deploy verification |
| `check-fixture-output` | `.github/actions/check-fixture-output` | fixture output assertions |
| GitHub-expanded reusable workflows | consumer repos | `workflow_call`, inputs, `secrets: inherit`, `toJSON(needs)` |

## Runtime features (behavior truth: `actions/runner`)

Not versioned per-action; tracked against the latest runner release recorded in
[`latest-runner-v2-refresh-2026-06-01.md`](latest-runner-v2-refresh-2026-06-01.md)
(`actions/runner` `v2.334.0`). Re-verify with
`cargo run -q -p velnor-tools -- check-runner-reference`.

- command files (`GITHUB_ENV`/`OUTPUT`/`PATH`/`STATE`/`STEP_SUMMARY`, heredoc)
- job/step outputs, `needs.*.result`
- expression eval (`lit`/`expr`/`format`; contexts; `contains`/`fromJSON`/`toJSON`)
- `defaults.run`, per-step `working-directory`, matrix expansion
- runtime/cache/OIDC env injection, secret masking
