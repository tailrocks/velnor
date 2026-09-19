# G3 distribution-consumer audit

Observed `2026-09-19T16:28:59Z` UTC. Read-only audit of the six in-scope
distribution consumers. Research clones are under `clones/`; no source
repository, remote, release, dispatch, install, or merge was mutated. The
Homebrew audits downloaded already-published assets into the existing local
Homebrew cache; they did not install packages.

## Effective runtime

The local state row for this worker is
`01a0ba7d-e457-7723-81ba-1f7ed038212c`: model `gpt-5.6-luna`, reasoning
`max`, CLI `0.155.0`, agent path `/root/g3_distribution_consumers`, and cwd
`velnor3`. This is independently recorded in the SQLite state database at
`/Users/donbeave/.codex-chainargos/state_5.sqlite`.

## Exact default-branch snapshot

All six default branches are `main`.

| repository | main SHA | generator pin | generated/workflow shape |
|---|---|---|---|
| `tailrocks/homebrew-tablerock` | `7d376cad41088420e92e49bf4030d761180a3512` | `b9c3156cdb88e63c11b9e595a3e694b02238c09a` | 6 workflows; generic `homebrew` unit |
| `tailrocks/homebrew-ruxel` | `ce29c817d29c54563bc479bce3966e4ea32845e6` | `b9c3156cdb88e63c11b9e595a3e694b02238c09a` | 6 workflows; generic `homebrew` unit |
| `tailrocks/homebrew-parallax` | `c9c59277f35cb1e81988c2bef0be2c688dadd608` | `b9c3156cdb88e63c11b9e595a3e694b02238c09a` | 6 workflows; generic `homebrew` unit |
| `tailrocks/homebrew-holla` | `cc25db8fd911cdf965652d3b094c10d006d056ee` | `b9c3156cdb88e63c11b9e595a3e694b02238c09a` | 6 workflows; generic `homebrew` unit |
| `tailrocks/holla-apt` | `0636074d4a16be4771685bd95bf2cec739cf359c` | `b9c3156cdb88e63c11b9e595a3e694b02238c09a` | only generated `ci-unit-docs.yml`; no aggregate/main/publish workflow |
| `jackin-project/homebrew-tap` | `f1669391582f92c95da1aa5958de4397dfedca09` | `b9c3156cdb88e63c11b9e595a3e694b02238c09a` | 6 workflows; explicit Homebrew unit, GitHub-only |

Every generic tap's generated `project.toml` reports static filesystem
analysis with `detected = []`; release, signing, update, and runner
capabilities remain manual inputs. The generic units watch formula/cask trees,
but do not infer native cask capability or package publication ownership.

## Consumer behavior map

### Homebrew TableRock

- `Formula/tablerock.rb`: stable `0.1.0`, source `e18cc91b…`, four targets;
  release-manifest SHA values match the live `v0.1.0` release.
- `Formula/tablerock-preview.rb`: preview `0.1.0-preview.867+a37344a`,
  source `a37344a…`, four targets; live preview asset digests match.
- `Casks/tablerock-app.rb` and `Casks/tablerock-app@preview.rb`: native app,
  arm64 only, macOS Tahoe (`:tahoe`); stable and preview app ZIP digests match
  release assets. The source native package requires macOS 26 (`native/Package.swift`),
  so the declared minimum is directionally correct.
- `scripts/package-update.sh` validates a typed five-asset stable manifest and
  rewrites one formula plus the stable cask. Its fixture test passes, but no
  cask install test exists.
- Native Homebrew audit on the current arm64 Mac fails both casks with
  `Use sha256 :no_check when URL is unversioned`; preview also fails
  `preview is a GitHub pre-release`. Do not weaken to `:no_check`: publish/use
  immutable versioned app URLs or add an explicit typed preview URL contract,
  then rerun native audit and clean cask installation.
- Main dispatch `35430875676` fails `generated-tree` and `required-checks`:
  the pinned render differs in state/`ci-main.yml`/`ci-policy.yml`, and the
  live ruleset lacks required `Policy`.

Evidence: `clones/homebrew-tablerock/.github-gen/velnor-workflow.toml:7-17`,
`.github/ci/project.toml:9-32`, `Casks/tablerock-app.rb:1-16`,
`Casks/tablerock-app@preview.rb:1-22`, `scripts/package-update.sh:8-44,86-100`.

### Homebrew Ruxel

- Stable `Formula/ruxel.rb` is disabled and points at mutable
  `archive/refs/heads/main.tar.gz` without a checksum; it exits with an
  instruction to install preview.
- Preview is `0.1.0-preview.85+cb5d1e9` from `cb5d1e9…`. The live `preview`
  release is `0.1.0-preview.89+110ed62…`; all four current artifact digests
  differ, so the tap is stale.
- No package-update script or source identity check exists; `mise` only runs
  Ruby syntax checks. Native Homebrew audit also reports that stable must
  declare a conflict with preview.
- Main dispatch `35431073357` fails `generated-tree` and `required-checks`;
  live ruleset lacks `Policy`.

Evidence: `clones/homebrew-ruxel/Formula/ruxel.rb:1-12`,
`Formula/ruxel-preview.rb:1-34`, `mise.toml:4-17`.

### Homebrew Parallax

- Stable `0.1.0` source `da3ce716…` and preview
  `0.1.0-preview.2526+1fa9666` source `1fa9666…`; four-target SHA values
  match live release manifests/assets.
- `scripts/package-update.sh` and its test cover stable only; preview remains
  static. Formula tests only assert `--version`; the `serve`/GreptimeDB
  behavior is not exercised by tap CI.
- Native Homebrew audit reports the stable formula's explicit `version` is
  redundant with its URL. Main dispatch `35431119313` fails
  `generated-tree` and `required-checks`; live ruleset lacks `Policy`.

Evidence: `clones/homebrew-parallax/Formula/parallax.rb:1-48`,
`Formula/parallax-preview.rb:1-48`, `.github/ci/project.toml:9-32`.

### Homebrew Holla

- Stable `1.0.3` and preview `1.0.3-preview.285+fca7d0c` have four-target
  SHA values matching live Holla release assets. Stable formula lacks a
  `source-sha` identity comment.
- Stable updater/test covers only stable; preview remains static. Stable and
  preview conflict declaration is one-sided; native Homebrew audit reports
  `holla` must also conflict with `holla-preview`, plus a redundant stable
  `version`.
- Main dispatch `35431027006` fails `generated-tree` (unit jobs skipped before
  policy passes); live ruleset currently includes `Policy`.

Evidence: `clones/homebrew-holla/Formula/holla.rb:1-35`,
`Formula/holla-preview.rb:1-38`, `mise.toml:1-24`.

### Holla APT (critical omission)

- Current tree has only `ci-unit-docs.yml`; GitHub Actions API lists no
  `ci-main`, `ci-pr`, `ci-policy`, `publish`, Pages, or feed workflow.
- `.github-gen/NO_WORKFLOWS_REQUIRED.md` explicitly admits that the
  `apt-repository` profile is descriptive only and omits signed `reprepro`/
  Pages publish, package-update, updater, and aggregate workflows.
- Config pins the generator but declares only a docs unit with `watch = []`
  and all four command lists empty. This is not meaningful APT verification.
- The repository nevertheless has a real package contract: `package-update.sh`
  validates `velnor.package-release.v1`, exact Holla source identity, and two
  `.deb` assets; it writes `package-state.json`. `conf/distributions` declares
  `amd64 arm64` and active `SignWith: E5BC87724E0F3E0A`; `holla.gpg` is
  committed. The local fixture test fails with exit 2 because the referenced
  `.github/workflows/publish.yml` is absent.
- README claims `publish.yml`, `workflow_dispatch`, `repository_dispatch`,
  cross-repo uploads, `reprepro`, and Pages deployment, but those files/triggers
  are absent from the current revision. The live endpoint currently serves a
  signed `stable` InRelease dated `2026-08-13` with Holla `1.0.3` for amd64 and
  arm64; this proves retained feed state, not current automation.
- Open PRs #75, #78, #80, #82 are Renovate/dependency updates; none restores
  the missing feed workflow. The latest relevant policy run `35082144161`
  failed `entrypoint-pin`, `pull-request-target`, and `required-checks` on the
  earlier clean-room omission branch.

Evidence: `clones/holla-apt/.github-gen/NO_WORKFLOWS_REQUIRED.md:1-18`,
`.github-gen/velnor-workflow.toml:11-24`, `.github/ci/project.toml:14-32`,
`scripts/package-update.sh:8-42`, `scripts/test-package-update.sh:40-50`,
`conf/distributions:1-10`, `README.md:41-106`.

### Jackin Homebrew tap

- Preview formula `0.6.4-preview.1181+a506eee` matches live preview assets,
  including both Linux capsule resources and macOS/Linux CLI archives.
- Stable `jackin` is intentionally disabled and source-builds from `HEAD`; no
  stable release exists. `jackin-dev` is pinned to `0.1.39`, while the latest
  live `jackin-dev` release is `0.1.52`; consumer is stale.
- No current `Casks/` tree exists. The updater's stable branch can generate
  `Casks/jackin-desktop.rb` only when a complete stable manifest includes a
  desktop asset, so that path is unexercised.
- Config excludes `Formula/**` and `Casks/**` from scanning and manually
  declares one Homebrew unit. Generated CI uses `mise run check`, which is
  actionlint plus package-updater fixtures—not `brew audit`, formula install,
  cask audit, or clean native installation.
- Main dispatch `35431020697` and current PR #494 checks pass, but this is not
  native cask/package proof. Native audit still reports invalid HEAD Git URL
  for stable and redundant `jackin-dev` version. PR #494 fixes preview formula
  audit ordering/description; PR #492 advances the generator pin; both remain
  open, so main remains at the older contract.

Evidence: `clones/homebrew-tap/.github-gen/velnor-workflow.toml:1-23`,
`.github/ci/project.toml:14-32`, `mise.toml:14-24`,
`Formula/jackin-preview.rb:1-55`, `Formula/jackin.rb:4-23`,
`Formula/jackin-dev.rb:1-34`, `scripts/package-update.sh`.

## Recent hosted evidence

| repository | run | result | material failure |
|---|---:|---|---|
| homebrew-tablerock | `35430875676` | failure | generated-tree; live ruleset lacks `Policy` |
| homebrew-ruxel | `35431073357` | failure | generated-tree; live ruleset lacks `Policy` |
| homebrew-parallax | `35431119313` | failure | generated-tree; live ruleset lacks `Policy` |
| homebrew-holla | `35431027006` | failure | generated-tree |
| holla-apt | `35082144161` | failure | omission branch: entrypoint-pin, pull-request-target, required-checks |
| jackin-project/homebrew-tap | `35431020697` | success | hosted unit pass does not cover native cask/install |

## Bounded central tasks

1. Advance to one reviewed generator epoch, regenerate all five generic taps
   and the Jackin tap, and repair live required-check rulesets. Never hand-edit
   generated state/workflows. Re-run each affected PR and resulting main SHA.
2. Add a typed Homebrew primitive that inventories formulas, casks, updater
   scripts, release identities, checksums, channel conflicts, and required
   architectures. Generate separate formula and native-cask verification:
   Homebrew audit plus clean artifact/version tests on hosted macOS for each
   advertised architecture; do not claim cask coverage from Ubuntu or Velnor
   Linux jobs. Keep product-specific package/update responsibilities in each
   tap; do not impose Velnor packaging on unrelated products.
3. Repair the cask release contract around immutable versioned URLs and
   manifest-bound SHA values (especially TableRock rolling preview). Preserve
   the explicit arm64-only boundary until an Intel app artifact exists.
   Add a native app-bundle smoke test and rerun `brew audit --strict --online`.
4. Add typed formula updater/source-identity checks. Refresh Ruxel preview to
   the current `0.1.0-preview.89+110ed62…` release, resolve stable/preview
   conflict declarations, and refresh Jackin-dev to `0.1.52`; test idempotent
   update, checksum, source-commit, and clean install/version behavior.
5. Replace Holla APT's docs-only omission with a central `apt-repository`
   primitive and generated workflows: package-state verification, signed
   `reprepro` indexes for both arches, one trusted publisher, Pages deployment,
   dispatch/event contracts, monotonic/idempotent updates, clean APT install/
   upgrade, and feed signature checks. Keep holla-apt's existing source
   identity/key/package-state contract; do not substitute Velnor's product
   packaging.
6. Add hostile fixtures for missing publish workflow, absent/extra package,
   wrong source commit, checksum mismatch, stale formula/cask, wrong
   architecture, preview/stable conflict, failed updater retry, unsigned feed,
   and stale generated tree. Then independently review raw evidence.

## Gate status

G3 distribution-consumer work is **blocked/incomplete**: Holla APT has no
meaningful delivery workflow; all five Homebrew consumers lack native cask/
clean-install proof; four generic tap main revisions fail generated-tree policy;
Ruxel and Jackin-dev are stale; and TableRock casks fail Homebrew audit.

## Jackin post-PR494 refresh

Observed `2026-09-19T17:30:57Z` UTC. This is a read-only refresh of the Jackin
consumer only; the original snapshot above remains unchanged. Fleet recorded
`2281ae9c95dfcb5bf82dfd025fdf58a50658b1d1` for `main`, and the GitHub API
confirmed the same revision. It is PR #494's merge commit, parent
`f1669391582f92c95da1aa5958de4397dfedca09`, merged at `2026-09-19T16:36:35Z`.
The exact delta is two files and six insertions/six deletions:

- `Formula/jackin-preview.rb`: shorter audit-safe description and conflict
  declaration moved before resources.
- `scripts/package-update.sh`: the preview formula template receives the same
  description/order fix; manifest, asset, checksum, extraction, and version
  checks are unchanged.

### Current responsibility map

| surface | current contract | verification state |
|---|---|---|
| Preview formula | `0.6.4-preview.1181+a506eee`; six immutable release assets (four CLI targets plus two Linux capsule resources), source comment `a506eee…`, symmetric preview/stable conflict, `--version` formula test | `brew audit --strict --online` passes for `jackin-preview`; fixture updater passes |
| Stable formula | Disabled until a stable release; mutable `HEAD` source build with Rust and optional Docker; conflicts with preview | Native audit still reports invalid HEAD Git URL; no stable artifact install proof |
| Dev formula | Static `0.1.39` four-target formula with no source identity or updater | Latest live `jackin-dev-v0.1.52`; native audit reports redundant URL-scanned version; stale |
| Cask | No `Casks/` tree or cask token. Stable updater branch would generate arm64-only `Casks/jackin-desktop.rb` from a complete seven-asset manifest and versioned desktop ZIP | No current cask to audit/install; clean native install not implemented |
| Updater | `scripts/package-update.sh` consumes verified identity/release manifests. Preview requires exact source identity, schema, six assets, SHA-256s, and extracted Linux x86_64 version; stable requires seven assets and rewrites formula plus desktop cask | `scripts/test-package-update.sh` passes stable idempotence, preview generation, and hostile mismatch fixtures |
| Generated CI | `.github-gen/velnor-workflow.toml` still pins generator `b9c3156cdb88e63c11b9e595a3e694b02238c09a`; six workflows; one manually declared `homebrew` unit, GitHub/Ubuntu 24.04, `mise run check` (`actionlint` plus updater fixture). Formula/Cask trees are excluded from static scan; release remains disabled | Hosted unit passes, but no generated native macOS audit, cask audit, install, or runtime version check |

### Hosted and native proof

- PR #494 head `17b5cfa00b04486c52d043885576cfeb40c55471` had successful PR CI
  run `35455135058`, policy run `35455133359`, and DCO check
  `105928955669`; the PR is closed/merged. The only live open PR is #492,
  generator sync, head `9255223b26d4a7340d92ec20fef7f95474aa4a58`.
- Resulting main push run `35455507231` for `2281ae9…` concluded success.
  Generated `Homebrew · / GitHub` job `105930230847`, `ci-required`
  `105930256962`, Policy `105929945743`, and planning all passed. An optional
  SonarCloud check failed (`105929985761`), but did not fail the run.
- Isolated native Homebrew audit against the exact detached revision passes for
  `jackin-preview`; all-formula audit still fails only the disabled stable HEAD
  URL and stale `jackin-dev` redundant-version findings. The tap reports zero
  cask tokens. No package was installed and no clean formula/cask runtime test
  was claimed.

### Gap status and bounded follow-up

PR #494 fixed the prior preview formula audit defect and is now merged. It did
not fix the remaining consumer gaps: stale Jackin-dev, the disabled stable
formula's native audit complaint, absent current cask coverage, no native
macOS generated job, no clean install/version proof, and release automation
remaining fail-closed. Central work should add a typed Homebrew primitive that
keeps these product-owned formula/updater responsibilities, conditionally
generates native cask checks only when a cask exists, and adds manifest-bound
source/checksum/conflict/architecture/install contracts. It must not impose a
Velnor packaging or release requirement on Jackin or other unrelated products.
