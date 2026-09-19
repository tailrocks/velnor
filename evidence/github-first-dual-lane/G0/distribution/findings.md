# G0 distribution evidence

Task: `G0-distribution`  
Observed: `2026-09-19T16:26:35Z`  
Source workspace: `/Users/donbeave/Projects/tailrocks/velnor-project/velnor3`  
Source revision: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`

## Boundaries and runtime proof

- Read-only investigation. No source edits, fetch, publish, dispatch, or Mac-runtime operation.
- `rtk` shell prefix used; `gh` access was read-only.
- Effective child session metadata: `gpt-5.6-luna`, reasoning `max`; rollout evidence is `/Users/donbeave/.codex-chainargos/sessions/2026/09/19/rollout-2026-09-19T23-14-25-01a0ba72-8fe6-7733-b003-39a11d1ef165.jsonl` (`session_meta` and `turn_context`). Root session is Astra/low as authorized.

## Exact revisions and live channel state

| surface | revision/state | evidence |
|---|---|---|
| `tailrocks/velnor-apt` `main` | `b24d7d4370001119cd5ddcb6f9e07aa9007051e7`; title `chore: sync velnor-workflow to fdeed261 (B4 pin-sync)` | [commit](https://github.com/tailrocks/velnor-apt/commit/b24d7d4370001119cd5ddcb6f9e07aa9007051e7) |
| `tailrocks/homebrew-velnor` `main` | `7af1249f3d69c9f2e548583cdc9f3e737da41b81`; title `feat(homebrew): add velnorctl source formula (#1)` | [commit](https://github.com/tailrocks/homebrew-velnor/commit/7af1249f3d69c9f2e548583cdc9f3e737da41b81) |
| APT generator pin | `fdeed261bd2247a38db6922a7726cd45d3d6f31e` in `.github-gen/velnor-workflow.toml` | [config at APT main](https://github.com/tailrocks/velnor-apt/blob/b24d7d4370001119cd5ddcb6f9e07aa9007051e7/.github-gen/velnor-workflow.toml) |
| Velnor stable application | `v0.1.274` -> source commit `120f223655587ab0bcf2530cd4b203e0375a9dca` | [release](https://github.com/tailrocks/velnor/releases/tag/v0.1.274) |
| newest Velnor release | `velnor-workflow-runtime-v1-63cea86d9b7bf2d5`, target `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`; `isPrerelease=false` | [release](https://github.com/tailrocks/velnor/releases/tag/velnor-workflow-runtime-v1-63cea86d9b7bf2d5) |
| Velnor preview | Git tag `preview` -> `701fbdd1a66939540310349074eab11af2517e54`; no GitHub release or `release-manifest.json` asset | [tag](https://github.com/tailrocks/velnor/tree/preview), [release list](https://github.com/tailrocks/velnor/releases) |
| live APT stable | `/last-publish = v0.1.274`; stable index has `0.1.273`, `0.1.274` | [last-publish](https://velnor-apt.tailrocks.com/last-publish), [InRelease](https://velnor-apt.tailrocks.com/dists/stable/InRelease) |
| live APT preview | `/last-publish-preview = 0.1.274~preview.145+d3e441f`; preview index retains `.121+f913637` rollback and `.145+d3e441f` candidate | [last-publish-preview](https://velnor-apt.tailrocks.com/last-publish-preview), [InRelease](https://velnor-apt.tailrocks.com/dists/preview/InRelease) |

## Contract evidence

APT main is generated from an `apt-repository` contract for package `velnor-runner`, source `tailrocks/velnor`, signer `7E66E3A53F9B3B5CA61D0F53261EDAC957DEB801`, and feed `https://velnor-apt.tailrocks.com`. The stable README documents signed install/upgrade and says the package includes `velnor-runner`, `velnorctl`, and `velnor-workflow`, but has no preview-channel install/switch instructions. [README](https://github.com/tailrocks/velnor-apt/blob/b24d7d4370001119cd5ddcb6f9e07aa9007051e7/README.md)

Homebrew main contains only `Formula/velnorctl.rb`: it source-builds `velnorctl` from `v0.1.274`, has no `.github` workflow, no preview formula/channel, and no packaged `velnor-runner`/`velnor-workflow` sibling contract. README explicitly limits the formula to the native operator CLI and Docker-backed Linux execution. [formula](https://github.com/tailrocks/homebrew-velnor/blob/7af1249f3d69c9f2e548583cdc9f3e737da41b81/Formula/velnorctl.rb), [README](https://github.com/tailrocks/homebrew-velnor/blob/7af1249f3d69c9f2e548583cdc9f3e737da41b81/README.md)

## Failures and root causes

1. **Stable discovery selects runtime namespace.** APT scheduled run `35433047049` fails in verify: `stable version must be a vX.Y.Z tag, found velnor-workflow-runtime-v1-63cea86d9b7bf2d5`. The generated source uses unfiltered `gh release list --exclude-pre-releases --limit 1`; runtime releases are non-prerelease and newest. [run](https://github.com/tailrocks/velnor-apt/actions/runs/35433047049), [generated workflow](https://github.com/tailrocks/velnor-apt/blob/b24d7d4370001119cd5ddcb6f9e07aa9007051e7/.github/workflows/release.yml)

2. **Verify→publish hidden sentinel is dropped.** Explicit stable run `35334663704` verifies, then publish says `publish: refusing — verify has not armed the reprepro sentinel`. At APT `b24d7d4`, `Upload verified feed inputs` lacks `include-hidden-files: true`; `.reprepro-ok` is written by verify and removed by `actions/upload-artifact` default filtering. Current Velnor source already encodes the structural fix and regression test at `crates/velnor-workflow/src/primitives/release.rs:4283-4286,8330-8353`, but the consumer pins older `fdeed261` and was not regenerated with it. [run](https://github.com/tailrocks/velnor-apt/actions/runs/35334663704), [workflow lines](https://github.com/tailrocks/velnor-apt/blob/b24d7d4370001119cd5ddcb6f9e07aa9007051e7/.github/workflows/release.yml), [source](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/primitives/release.rs#L4283-L4286), [test](https://github.com/tailrocks/velnor/blob/abe9ad82a2d4d01b706bbc6122ab6ccb150faad9/crates/velnor-workflow/src/primitives/release.rs#L8330-L8353)

3. **Preview source endpoint is absent.** APT generated preview verify calls `gh release download preview --pattern release-manifest.json` and `gh release view preview`; Velnor has no `preview` GitHub release (only a tag), so a clean preview run cannot discover/fetch a candidate. Existing Pages preview is stale state, not a valid source endpoint. [workflow](https://github.com/tailrocks/velnor-apt/blob/b24d7d4370001119cd5ddcb6f9e07aa9007051e7/.github/workflows/release.yml), [releases](https://github.com/tailrocks/velnor/releases)

4. **G1 hosted recovery is not complete.** APT source contract still declares `runners = "both"`, `automatic = "both"`, and `pull_request_on_velnor = true`; dispatch `35430878317` left Velnor-dependent Documentation queued while hosted jobs completed. This must be corrected in generator/config and regenerated before publication. [config](https://github.com/tailrocks/velnor-apt/blob/b24d7d4370001119cd5ddcb6f9e07aa9007051e7/.github-gen/velnor-workflow.toml), [run](https://github.com/tailrocks/velnor-apt/actions/runs/35430878317)

5. **Homebrew has no delivery automation or complete product.** There are no Homebrew workflows or open PRs. Formula #1 only installs source-built `velnorctl`; it cannot satisfy the G2 installed-product/sibling-discovery contract without an explicit macOS asset/package design. [PR #1](https://github.com/tailrocks/homebrew-velnor/pull/1)

Historical APT run `35280645007` also failed because the verified artifact was downloaded to `.` while consumers read `incoming/…`; merged APT PR #241 corrected paths and added key-secret wiring, but a hosted rerun is still required. [run](https://github.com/tailrocks/velnor-apt/actions/runs/35280645007), [PR #241](https://github.com/tailrocks/velnor-apt/pull/241)

Corroborating failures: scheduled APT run `35327386749` repeats the runtime-tag discovery error; PR #241's merged revision is `b24d7d4370001119cd5ddcb6f9e07aa9007051e7`; PR #239 merged as `0ecde07bffa9a127fe99e02e4e6f6526ee386961` and introduced the typed feed workflow and coverage notes. [run 35327386749](https://github.com/tailrocks/velnor-apt/actions/runs/35327386749), [PR #239](https://github.com/tailrocks/velnor-apt/pull/239)

## Reusable work already present

- `crates/velnor-workflow/src/apt.rs` resolves stable tag commits with `git ls-remote` and has a regression test (`resolve_commit_invokes_git_ls_remote`); this addresses the older FEED_COVERAGE defect, but only becomes live after the pinned generator is updated and consumer workflows are regenerated.
- Current generator source contains the hidden-file handoff fix and tests; use it via generated-config regeneration, never hand-edit `.github`.
- APT `b24d7d4` already has corrected `incoming`/`public` artifact directories and `APT_GPG_PRIVATE_KEY` wiring from #241. `FEED_COVERAGE.md` is stale and still describes prior gaps.
- Live APT feed is signed by the expected key and has one retained stable/preview rollback pair; preserve these channel invariants during recovery.

## Bounded G2 implementation tasks

1. **Product release identity/discovery (source prerequisite, can start during G1):** add a typed application-product release identity distinct from `velnor-workflow-runtime-*`; discovery must paginate, filter/validate application release tags/manifests, reject runtime releases, invalid/missing assets, API errors, and preserve explicit channel/rollback state. Add mixed-release and pagination tests. Regenerate consumers only after source tests pass.
2. **Preview source contract:** choose and implement one authoritative rolling application-preview release endpoint with complete `release-manifest.json`, explicit prerelease semantics, commit binding, and immutable assets; update APT/Homebrew consumers to that contract. Do not publish merely to revive stale Pages state.
3. **APT handoff/recovery:** bump the generator pin and regenerate APT so `include-hidden-files: true` reaches `b24d`-successor; prove verify→artifact→publish on hosted runner, then stable/preview channel switching and rollback. Keep publication gated on G1 hosted-only recovery.
4. **Homebrew package contract:** define macOS arm64/Intel product assets and installed binary set (`velnorctl`, `velnor-workflow`, and any required runner/host control binary); generate stable and preview formula/update workflows with checksums, source identity/version tests, upgrade/channel-switch/rollback behavior, and no runtime namespace collision. Decide/document Linux runner vs native macOS boundary explicitly.
5. **End-to-end acceptance:** test signed APT install/upgrade `stable→stable`, `preview→preview`, `preview→stable`, generated Homebrew PR/main CI for both channels, clean hosted macOS install, command usefulness/version/source identity/sibling discovery. No package publication before G1 gate passes.

## Open PRs / disposition

- APT #227 is open but stale Renovate action-only work (`61f97d506637d8d517d487c73b6a3d188feb0a5b`; base `12669a...`), with failed policy/required checks. Rebase/regenerate after G1; not a G2 package fix. [PR](https://github.com/tailrocks/velnor-apt/pull/227)
- Homebrew has no open PRs.
