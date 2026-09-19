# G3 action and role-image audit

Status: early read-only audit. No source, workflow, fleet, or consumer tree was changed.

Observed: `2026-09-19T16:36:45Z`  
Velnor base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`  
Runtime: the session ledger records worker model `gpt-5.6-luna`, max reasoning; `rtk 0.49.0`; concurrency 256.  
Isolation: four fresh shallow research clones under `/tmp/velnor-g3-action-roles.ueumsS/`; the existing Velnor and consumer trees were preserved.

## Exact upstream revisions

These are the exact `main` tips used for this audit, obtained from `git ls-remote` and cloned without mutation.

| Repository | `main` revision | Tree shape relevant to G3 |
|---|---|---|
| [`jackin-role-action`](https://github.com/jackin-project/jackin-role-action/tree/8882236041e149153491ada7091382e76be1c313) | `8882236041e149153491ada7091382e76be1c313` | Composite action, downloader, generated docs CI; no current `publish.yml` |
| [`jackin-agent-smith`](https://github.com/jackin-project/jackin-agent-smith/tree/08cb1c2f82519bab1aa0c164879955a84d35463b) | `08cb1c2f82519bab1aa0c164879955a84d35463b` | `Dockerfile`, `jackin.role.toml`, generated Docker unit |
| [`jackin-the-architect`](https://github.com/jackin-project/jackin-the-architect/tree/d0956f0192d2605cbd26d84bf8911211a727b6c0) | `d0956f0192d2605cbd26d84bf8911211a727b6c0` | Toolchain-heavy `Dockerfile`, role manifest, generated Docker unit |
| [`jackin-sentinel`](https://github.com/jackin-project/jackin-sentinel/tree/a668b869b9a31622f3088c1d891c45716e94cff2) | `a668b869b9a31622f3088c1d891c45716e94cff2` | Hooks/env-heavy role, `Dockerfile`, generated Docker unit |
| [`jackin` fixture source](https://github.com/jackin-project/jackin/tree/665f7e3735c1f76ce5ee8a9e27c676381a474cfb) | `665f7e3735c1f76ce5ee8a9e27c676381a474cfb` | Sentinel direct-action consumer fixture |

Current open sync PR heads were also recorded: role-action `8ea816d2cfd33645ecce61b20bdf9655e4781307` (#182, green); agent-smith `b9db5b149cc46baba9c49549432307c29e3972b0` (#206, Policy failure); architect `eaf9c16399b70fc9c325394e683145007a19d710` (#456, green); sentinel `cdf18c2b19f7b95a85868c824b6bdffb88156b3d` (#144, green). Old sync attempts repeatedly failed only the generated-tree policy because the pinned Velnor revision was stale. This is a generator synchronization issue, not evidence that the role contracts are covered.

## Executive findings

1. `jackin-role-action` still contains the composite action contract, but the migrated current tree no longer contains the reusable `.github/workflows/publish.yml` product. The old workflow was removed by [migration commit `a184e6c1`](https://github.com/jackin-project/jackin-role-action/commit/a184e6c1a352ff79b55d79649edc53ab8d11a881). Existing callers still pin the old workflow revision (`3683b3450c2d052ef26cce02922a6678b5fd20f5`), while current README text points at another historical revision. A current-`main` caller of `.../publish.yml` receives no file. This is a hard consumer-compatibility gap.

2. The three role-image repositories retain Dockerfiles and `jackin.role.toml`, but their generated Docker units only build the Dockerfile. They do not run `jackin-role validate`, role-runtime smoke, multi-architecture Buildx, image merge/publish, or signing. The generated project metadata has release disabled in all three repos.

3. Current generated configs are hosted-only: `runner = "github"`, automatic lanes `github`, `ubuntu-24.04`; `velnor_labels = ["self-hosted", "velnor-target-mvp"]` is retained as metadata but does not create a Velnor lane. G3 dual-lane evidence therefore cannot be inferred from these workflows.

4. Velnor's generic scanner has Docker support but no GitHub Action or Jackin role semantic unit. It does not parse `action.yml`, `jackin.role.toml`, the role image identity, action `uses` dependencies, or runtime smoke contracts. The file walker excludes `.github`, so action workflows are invisible to the generic scan. Buildx capability probes are generic infrastructure probes, not role/action verification.

5. The role-action README documents Dockerfile lint, role validation, amd64 build, and a reusable publish workflow; only the first three exist in `action.yml`, and none are exercised by the generated docs unit. README inputs omit the implemented `registry-cache-image` input. Documentation and current product surface are therefore already inconsistent.

## Current generated configuration

All four repositories use generator schema 1 / state generator 49. Role-action declares a docs/REUSE unit; the other three declare one Docker unit. The generated Docker command is:

```text
docker build --file Dockerfile --tag local-ci:dockerfile .
```

The generated Docker workflow installs `docker/setup-buildx-action` and exposes the GitHub Actions runtime, but the unit command remains ordinary `docker build`; it does not invoke the role action's Buildx pipeline or the old publisher's Docker-container builder, matrix, digest artifact, `imagetools`, or cosign stages. The generated `ci/project.toml` analysis sees only the Renovate configuration signal, reports static build limitations, and has `release.enabled = false`.

Role-action's generated docs unit runs `mise run ci` (REUSE compliance). That preserves the old reuse check. It does not run `actionlint` over the action contract, execute the downloader, invoke the composite action, or call a consumer.

## Responsibility preservation matrix

Classification uses: **preserved** = current generated path proves it; **partial** = source remains but generated verification is absent; **missing** = migration removed the behavior or no replacement exists.

| Responsibility | Old/current source evidence | Current generated behavior | Classification / required proof |
|---|---|---|---|
| Composite action metadata (`action.yml`) | Current [`action.yml`](https://github.com/jackin-project/jackin-role-action/blob/8882236041e149153491ada7091382e76be1c313/action.yml) has `path`, `jackin-version`, `skip-build`, `registry-cache-image`; composite `runs` | No generated unit parses or invokes it | Partial; add action metadata and consumer fixtures |
| Jackin binary download/version selection | `scripts/download-jackin-role.sh` supports release assets and `latest-build`; checksum verification and target selection | No generated test or fake-artifact fixture | Partial; test explicit version, latest-build, checksum, target, missing artifact |
| Role validation | Action runs `jackin-role validate "$AGENT_PATH"` after download | None of the four generated units calls it | Partial source / missing CI proof; failure must stop downstream build |
| Hadolint | Action pins hadolint and runs it unless `skip-build` | No action fixture; Docker units are not role-action invocation | Partial; prove lint failure propagation and skip semantics |
| Single-arch action build | Action pins setup-buildx/build-push, `linux/amd64`, `push=false`, `load=true`, scoped GHA cache | Generic role Docker unit uses plain `docker build` | Partial; prove build/no-build and exact platform/cache behavior |
| `skip-build=true` | Conditional hadolint, setup-buildx, and build-push steps in `action.yml` | No fixture | Unverified; add no-build assertion while validation still runs |
| Registry cache input | Implemented in metadata/build args; README omits it | No generated path | Partial and docs drift; add cache fixture |
| Reusable role-image publisher | Old [`publish.yml`](https://github.com/jackin-project/jackin-role-action/blob/52ee2604aa660d8840fc5644113e0918615f90e2/.github/workflows/publish.yml) accepted `workflow_call`, validated, built amd64/arm64, optionally pushed, merged, and signed | Current `main` has no `.github/workflows/publish.yml` | **Missing / hard blocker**; restore exact contract or provide a typed replacement before consumer pin migration |
| Reusable publisher validation | Old publisher downloaded role binary, ran hadolint and `jackin-role validate`, emitted image and label outputs | No replacement | Missing; fixture caller must fail before build on bad role |
| Multi-arch Buildx/QEMU | Old publisher had platform matrix, Buildx Docker-container driver, QEMU when needed, digest artifacts | Generic unit has only generic Buildx setup; no matrix or QEMU contract | Missing; prove amd64 + arm64 and one-arch failure behavior |
| Image tags/labels/digest merge | Old publisher emitted OCI labels, pushed per-platform digests, used `docker buildx imagetools create` | Role manifests only state `published_image = ...:latest`; no publisher | Missing; preserve image identity and deterministic tag/SHA contract |
| Signing | Old publisher signed merged image with cosign | No current generated or role path | Missing; keep publish/sign separate from untrusted PR validation |
| Role Dockerfile build | Three current role trees retain Dockerfiles; all use pinned `projectjackin/construct:0.36-trixie` digest | Generated Docker unit builds one host image | Partial; add manifest, Dockerfile, and architecture fixtures |
| Role manifest semantics | `jackin.role.toml` carries version, image, agents, plugins, env, hooks; sentinel has dependency-derived prompts | Scanner does not parse it; no generated validation | Partial; add typed role-image unit and schema failure fixtures |
| Role runtime hooks/smoke | Sentinel README specifies launch cockpit, env dialogs, hooks, report, and dirty-worktree cleanup E2E; architect has hooks/toolchain; Smith has Node setup | No runtime launch/smoke command in generated CI | **Missing**; add non-publishing runtime smoke for each role shape |
| Docker architecture availability | Old publisher promised amd64/arm64; current manifests do not declare supported platforms | Current generated run is hosted amd64 only | Missing; explicitly test platform matrix and unsupported-arch failure |
| REUSE/docs compliance | Old reuse workflows were removed; role-action generated docs unit runs `mise run ci` | Current docs unit exists and is generated | Preserved, subject to required-check wiring |
| Renovate | Old Renovate workflow was deleted; generator detects Renovate config but emits no Renovate job | No current Renovate workflow | Missing unless deliberately external; classify and test ownership |
| Policy/DCO/Sonar/required gate | Generated policy and required checks remain; current PR checks show them | Does not imply role/action semantic coverage | Preserved as generic policy only; keep separate from G3 acceptance |
| Consumer caller compatibility | [`ChainArgos/jackin-agent-brown` workflow](https://github.com/ChainArgos/jackin-agent-brown/blob/197f0a2d916b7a620bea3df942856001606c1e3a/.github/workflows/publish-image.yml) (file blob `77682dea0d432829c07779588ad1f06b85fd2551`) pins reusable workflow `3683b345...`; `AndreiSotnikov/jackin-typescriper` and the `jackin` sentinel fixture call action `@main` | Current role-action `main` lacks the reusable path; direct consumers are mutable refs | Missing compatibility proof; provide pinned success/failure fixtures and immutable consumer pins |

The exact brown workflow blob was retrieved during the audit and calls `jackin-project/jackin-role-action/.github/workflows/publish.yml@3683b3450c2d052ef26cce02922a6678b5fd20f5`. Direct-action consumers observed were `AndreiSotnikov/jackin-typescriper/.github/workflows/validate.yml` and `jackin`'s `crates/jackin/tests/fixtures/roles/jackin-sentinel/.github/workflows/validate.yml`; both currently use `@main`, which is not an immutable compatibility boundary.

## Product-specific contract details

### `jackin-role-action`

At revision `8882236`, `action.yml` is a composite action. Its contract is:

1. Download `/tmp/jackin-role` using `scripts/download-jackin-role.sh` with `JACKIN_VERSION` and optional `GH_TOKEN`.
2. Run pinned Hadolint unless `skip-build=true`.
3. Run `jackin-role validate` against the supplied path.
4. Unless skipped, set up pinned Buildx and build `linux/amd64` with `push=false`, `load=true`, GHA cache, and optional registry cache.

The downloader supports versioned release assets and `latest-build`, with SHA-256 checks. It resolves only `x86_64` and `aarch64` targets. The script uses fixed `/tmp/jackin-role` paths and latest-build artifact selection from the newest successful `ci.yml` artifact; this needs deterministic fixture coverage and provenance checks. The version regex accepts a semver prefix with trailing text, so the central test should define whether that is intentional.

The old publisher had more responsibilities than the composite action: reusable `workflow_call` inputs, caller credentials, validation, amd64/arm64 Buildx matrix, cache, digest artifact handoff, merge/tag, and cosign. Deleting its file without a replacement breaks reusable callers even though direct composite consumers can still resolve `action.yml`.

### Role image repositories

- **agent-smith**: pinned Construct base, `USER agent`, trusted MISE path, Node 24.20.0 install with BuildKit cache/secret. Manifest publishes `docker.io/projectjackin/jackin-agent-smith:latest`, uses Claude/Sonnet, and installs code-review and feature-dev plugins. No generated runtime smoke.
- **the-architect**: same pinned Construct base; installs build-essential/OpenSSL/CMake, Rust/Tofu/Caveman/CTX7/skills/headroom/UV/RTK toolchain with cache/secret handling and hooks/config. Manifest targets Claude/Codex/AMP/OpenCode/Kimi/Grok and multiple marketplaces. No generated runtime smoke or multiarch publisher.
- **sentinel**: same pinned Construct base with a minimal image layer; manifest has all agents, setup/source/preflight hooks, static and interactive env prompts, selects, and dependency interpolation. Its README describes a rich E2E/report contract, but the repository contains no corresponding generated smoke test.

All three use `published_image` tags ending in `:latest`; none of the generated configs declares an image release contract or platform set. The static Docker build therefore cannot prove that an image is runnable by Jackin or consumable by the publisher.

## Velnor generic support boundary

At Velnor base `abe9ad82` ([scanner dispatch](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/s2/scan/mod.rs), [Docker scanner](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/s2/scan/docker.rs), [unit kinds](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/s2/mod.rs)):

- `crates/velnor-workflow/src/s2/scan/mod.rs` dispatches Docker, docs, Gradle, Homebrew, Node, OpenTofu, Rust, signals, and Swift scanners. There is no action or role scanner.
- `UnitKind` has Rust, Gradle, Node, Bun, Swift, OpenTofu, Docker, Homebrew, and Docs; no GitHub Action or Jackin Role kind.
- `s2/scan/docker.rs` finds Dockerfiles and emits a plain `docker build`; it does not parse `action.yml`, `jackin.role.toml`, `.dockerignore` semantics, image identity, `uses` dependencies, or smoke commands.
- `s2/scan/signals.rs` detects deny/nextest/Renovate only. The tracked-file walker excludes `.github`, so workflows and action metadata under that directory are not part of generic shape detection.
- Capability tests probe Docker, privileged nested Docker, Buildx/Compose, testcontainers, services, browser binaries, and native macOS arm64. These prove runner routing primitives, not the action/role contract.
- Generic Docker/buildx primitives can be reused, but they are insufficient to preserve role validation, consumer invocation, publisher matrix, image merge, signing, or runtime smoke. Those semantics must be typed declarations/primitives rather than inferred from arbitrary scripts.

## Required fixture matrix

Fixtures should be network-free and use fake `gh`, artifact archives, checksums, registry, and cosign endpoints. Every fixture must report a machine-readable result into the normal required gate.

| Fixture | Success assertion | Failure assertion |
|---|---|---|
| Action metadata | Valid composite metadata, all four inputs, shell and `uses` steps lint; direct consumer resolves an immutable action SHA | Missing `runs`, unsupported `using`, malformed input/default, missing shell, mutable `@main` rejected by policy |
| Download: versioned release | Explicit `1.2.3` target asset, checksum matches, binary `--version` matches | Unsupported architecture, missing asset, malformed/nonmatching checksum, checksum mismatch, binary version mismatch |
| Download: latest-build | Newest unexpired expected artifact selected; fallback successful `ci.yml` run selected; archive extracted | Expired/missing artifact, wrong artifact name, missing role archive, duplicate ambiguous archive, source-SHA/provenance mismatch |
| Validation success | Fake `jackin-role validate` returns zero; action completes | Validator nonzero; action exits nonzero and no build/publish step runs |
| Validation/build failure | Hadolint, validator, and Buildx stages run in contract order | Hadolint failure stops validator/build where contract requires; Buildx failure propagates and no image is claimed |
| No-build | `skip-build=true` still downloads and validates; no Hadolint/Buildx/build-push process occurs | Any build process starts under no-build; validator skipped unexpectedly |
| Build | `skip-build=false` runs pinned Hadolint, setup-buildx, `linux/amd64`, push false/load true, expected cache scopes | Wrong platform, push enabled in validation, missing cache isolation, build runs after validation failure |
| Registry cache | Optional `registry-cache-image` maps to cache-from/cache-to only, with no image push | Empty/malformed cache input changes output image or causes an unbounded cache target |
| Role manifest | Smith, Architect, and Sentinel manifests parse; image identity and hooks/env schema are recorded | Invalid version, missing Dockerfile/image, malformed hooks or dependency interpolation fails before Docker build |
| Docker build | Each current Dockerfile builds with pinned base and expected context; no credentials leak | Missing Dockerfile, invalid Dockerfile/Hadolint, unpinned base where policy forbids it, secret/cache misuse |
| Buildx architecture | Buildx produces amd64 and arm64 (native or controlled QEMU), records platform/image metadata | One platform fails and merge/publish is blocked; unsupported host architecture fails clearly; plain single-platform build cannot satisfy a multiarch fixture |
| Runtime smoke | Launch each role with minimal fake env; hooks/report/toolchain markers and resolved env are observable; Sentinel report succeeds | Missing hook, unresolved dependency, missing report/runtime marker, dirty-worktree cleanup failure, nonzero process |
| Reusable publisher no-publish | Old `workflow_call` caller contract validates and performs read-only Buildx builds, digest artifacts, no registry write | Missing input/output, accidental push, artifact collision, `.dockerbuild` artifact corrupts download |
| Reusable publisher publish | Controlled fake registry receives platform manifests, `imagetools create`, deterministic tags, and cosign call after both platforms pass | One platform, merge, registry, or signing failure prevents success and required gate |
| Consumer compatibility | Old brown-style caller and current direct-action fixtures succeed at pinned role-action revision | Current `main` lacks reusable path; mutable consumer ref rejected; action output/error not propagated |

## Bounded central tasks

1. **Restore the product boundary first.** Either restore `.github/workflows/publish.yml` with its old `workflow_call` contract at a new immutable revision, or model the exact contract as a typed Velnor publisher primitive. Do not update external consumer pins until a caller fixture proves inputs, outputs, no-publish, publish, multiarch, merge, and signing behavior. Keep credentials and registry writes out of pull-request validation.

2. **Add typed action/role detection.** Introduce a narrow scanner/declaration for root `action.yml`/`action.yaml` and `jackin.role.toml`. Record action inputs, composite `uses`/shell steps, downloader, validator, conditional build, and consumer fixtures. Record role Dockerfile, `published_image`, agent/runtime metadata, hook/smoke command, and supported platforms. Do not infer security-sensitive publish behavior from arbitrary shell text.

3. **Add two central verification primitives.** An action primitive must run metadata/lint, download/version fixtures, validator success/failure, no-build, Buildx build, cache, and direct consumer. A role-image primitive must run manifest validation, Dockerfile/Hadolint, role validation, Buildx platform matrix, image metadata, and runtime smoke. Generic Docker build remains a lower-level primitive.

4. **Add the fixture harness before changing the four repos.** Use fake artifact/registry/cosign services and deterministic binaries. Assert process order and failure propagation. Include single Dockerfile, paired platform Dockerfiles, amd64/arm64 routing, and explicit unsupported-architecture cases. Feed all results into `ci-required`.

5. **Declare the four repositories narrowly.** Role-action declares its composite and reusable-publisher surfaces plus one direct consumer fixture. Smith, Architect, and Sentinel declare their role manifest, image identity, platform set, Buildx build, and runtime smoke command. Keep release/publish as an explicitly separate writer path; current `release.enabled=false` must not silently erase the contract.

6. **Close generated ownership gaps.** Classify Renovate ownership (restore a generated job or document external ownership), preserve REUSE, and retain policy/DCO/Sonar checks. A generator update must not delete an unowned reusable workflow, consumer contract, or role smoke obligation. Regenerate from the exact Velnor revision and prove generated-tree policy.

7. **Pin consumers after proof.** Replace direct `@main` references with immutable action SHAs after the new contract is green. Update brown-style reusable callers only after the reusable path is restored/proven. Record each producer revision and consumer revision in G0 evidence.

## G3 stop condition

G3 action/role verification is not complete while current `jackin-role-action@main` lacks the reusable publisher path, role-image CI is only a plain hosted Docker build, and no fixture exercises download/version, validation failure, no-build, Buildx architecture, Docker runtime smoke, or consumer compatibility. The generic scanner's Docker unit is useful as a baseline but does not preserve these product responsibilities.
