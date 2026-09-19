# G0 fleet inventory

- Observed: `2026-09-19T16:34:19Z` UTC (GitHub GraphQL branch/PR snapshot; contents API presence scan completed immediately before it).
- Source: read-only `gh api graphql` plus `gh api repos/{repo}/contents/...`; no clone, fetch, dispatch, merge, or remote mutation.
- Manifest check: 32 specified entries, 32 unique entries, 0 duplicate entries.
- Effective execution model from this agent's `turn_context`: `gpt-5.6-luna`, effort `max`.
- Open PR total: 78 (75 ready, 3 drafts) across 12 repositories. PR head/base revisions are in `open-prs.tsv`; config blob and generator revisions are in `configs.tsv`.

| repository | default branch / SHA | open PRs | `.github-gen` presence | workflow presence | inventory gap / category note |
| --- | --- | ---: | --- | --- | --- |
| tailrocks/velnor | `main` / `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9` | 4 (#948 D, #952, #953, #954) | `sources`, `velnor-workflow.toml` | 15: `ci-main`, `ci-policy`, `ci-pr`, `ci-release-package-signer`, `ci-runtime-products`, `ci-unit-{bun,docker,docs,opentofu,rust}`, `maintenance`, `nightly`, `preview`, `release` | G1 recovery/pin/bootstrap and hosted-first policy audit. |
| tailrocks/velnor-apt | `main` / `b24d7d4370001119cd5ddcb6f9e07aa9007051e7` | 3 (#224, #225, #227) | `FEED_COVERAGE.md`, `velnor-workflow.toml` | 9: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-docs`, `maintenance`, `nightly`, `release`, `renovate-validate`, `renovate` | APT feed/signature/publish behavior needs category audit. |
| tailrocks/parallax | `main` / `6a12bf47a816b63e848b563aaa45ef9694159c79` | 3 (#109 D, #110 D, #111) | `velnor-workflow.toml` | 8: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-{bun,docker,rust}`, `maintenance`, `nightly` | Polyglot coverage present; verify all discovered responsibilities. |
| tailrocks/tracing-request-level | `main` / `f234158eb3d5caddda83b19e527ffb7d0f675a23` | 0 | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-rust`, `maintenance`, `nightly` | Follow wrapper/child run chain; no open PR head to reconcile. |
| tailrocks/termrock | `main` / `936982e60bce6d19cf33ae09b53545a955f1073d` | 0 | `NO_WORKFLOWS_REQUIRED.md`, `velnor-workflow.toml` | 0 | Explicit no-op marker conflicts with meaningful-workload requirement; scanner omission investigation. |
| tailrocks/termpane | `main` / `7602430f18852350cc3155e58bb96799234cbfe5` | 0 | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-rust`, `maintenance`, `nightly` | Rust coverage present. |
| tailrocks/tablerock | `main` / `e2fe040c9d9cbf0eb3994d8b6cdffd0a772de6fc` | 0 | `velnor-workflow.toml` | 7: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-{rust,swift}`, `maintenance`, `nightly` | Native Apple routing/toolchain must be validated. |
| tailrocks/schemalane | `main` / `ab49e2dd910ee41e96daea97ad6998f7961e5cfc` | 0 | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-rust`, `maintenance`, `nightly` | Rust coverage present. |
| tailrocks/ruxel | `main` / `3d34049820a6d4ce3707e574f407cbf4047ff932` | 0 | `velnor-workflow.toml` | 7: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-{docker,rust}`, `maintenance`, `nightly` | Rust + Docker coverage present. |
| tailrocks/pg-bigdecimal | `main` / `7dc5267d855801dbffb54aa12fc87791aa000a93` | 0 | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-rust`, `maintenance`, `nightly` | Rust coverage present. |
| tailrocks/parallax-telemetry-playground | `main` / `54d09bf71181dcfc72d6829fabec3c53f55aacf9` | 0 | `velnor-workflow.toml` | 10: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-{bun,docker,gradle,rust,swift}`, `maintenance`, `nightly` | Polyglot + native Apple routing audit. |
| tailrocks/homebrew-tablerock | `main` / `7d376cad41088420e92e49bf4030d761180a3512` | 0 | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-homebrew`, `maintenance`, `nightly` | Homebrew artifact/update behavior audit. |
| tailrocks/homebrew-ruxel | `main` / `ce29c817d29c54563bc479bce3966e4ea32845e6` | 0 | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-homebrew`, `maintenance`, `nightly` | Homebrew artifact/update behavior audit. |
| tailrocks/homebrew-parallax | `main` / `c9c59277f35cb1e81988c2bef0be2c688dadd608` | 0 | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-homebrew`, `maintenance`, `nightly` | Homebrew artifact/update behavior audit. |
| tailrocks/homebrew-holla | `main` / `cc25db8fd911cdf965652d3b094c10d006d056ee` | 2 (#139, #142) | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-homebrew`, `maintenance`, `nightly` | Homebrew artifact/update behavior audit. |
| tailrocks/holla-apt | `main` / `0636074d4a16be4771685bd95bf2cec739cf359c` | 4 (#75, #78, #80, #82) | `NO_WORKFLOWS_REQUIRED.md`, `velnor-workflow.toml` | 1: `ci-unit-docs` | No feed delivery workflow despite APT role; restore meaningful publisher coverage. |
| tailrocks/holla | `main` / `027dac6be2070b7e836c1ca123861fcbade72003` | 9 (#197–#205) | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-rust`, `maintenance`, `nightly` | Rust coverage present; nine open heads require reconciliation. |
| tailrocks/homebrew-velnor | `main` / `7af1249f3d69c9f2e548583cdc9f3e737da41b81` | 0 | none | 0 | Source formula has no generator config/CI; packaged product, channels, and CI gap. |
| tailrocks/tailrocks-typescript-skills | `main` / `f434715f7c664af431f0be62982aa379400102de` | 0 | none | 0 | Skills category has no generated config/CI; inspect manifests/references/templates. |
| tailrocks/tailrocks-skill-authoring-skills | `main` / `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6` | 0 | none | 0 | Skills category has no generated config/CI; inspect manifests/references/templates. |
| tailrocks/tailrocks-rust-skills | `main` / `bcc31b1d935dac4de191a6b71b3091f628d204c0` | 0 | none | 0 | Skills category has no generated config/CI; inspect manifests/references/templates. |
| tailrocks/tailrocks-roadmap-skills | `main` / `66d9c79f6472ddace0e265335dc8dd36cdeb8a86` | 0 | none | 0 | Skills category has no generated config/CI; inspect manifests/references/templates. |
| tailrocks/tailrocks-pull-request-skills | `main` / `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612` | 0 | none | 0 | Skills category has no generated config/CI; inspect manifests/references/templates. |
| tailrocks/tailrocks-open-source-skills | `main` / `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a` | 0 | none | 0 | Skills category has no generated config/CI; inspect manifests/references/templates. |
| tailrocks/tailrocks-macos-skills | `main` / `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` | 0 | none | 0 | Skills category has no generated config/CI; inspect manifests/references/templates. |
| tailrocks/tailrocks-code-quality-skills | `main` / `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189` | 0 | none | 0 | Skills category has no generated config/CI; inspect manifests/references/templates. |
| jackin-project/jackin | `main` / `665f7e3735c1f76ce5ee8a9e27c676381a474cfb` | 2 (#975, #1002) | `velnor-workflow.toml` | 15: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-{bun,docker,rust,swift}`, `desktop-{merge,scheduled}`, `maintenance`, `nightly`, `release`, `renovate-{upstream-sources,validate}`, `renovate` | Polyglot/native product; preserve Apple and release responsibilities. |
| jackin-project/jackin-agent-smith | `main` / `08cb1c2f82519bab1aa0c164879955a84d35463b` | 9 (#181–#184, #188–#191, #206) | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-docker`, `maintenance`, `nightly` | Role/image workload requires Docker build/arch/runtime audit. |
| jackin-project/homebrew-tap | `main` / `f1669391582f92c95da1aa5958de4397dfedca09` | 2 (#492, #494) | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-homebrew`, `maintenance`, `nightly` | Homebrew artifact/update behavior audit. |
| jackin-project/jackin-the-architect | `main` / `d0956f0192d2605cbd26d84bf8911211a727b6c0` | 21 (#418–#421, #425–#434, #436–#438, #440–#441, #455–#456) | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-docker`, `maintenance`, `nightly` | Role/image workload requires Docker build/arch/runtime audit; 21 open heads. |
| jackin-project/jackin-sentinel | `main` / `a668b869b9a31622f3088c1d891c45716e94cff2` | 9 (#117–#120, #124–#126, #143–#144) | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-docker`, `maintenance`, `nightly` | Role/image workload requires Docker build/arch/runtime audit. |
| jackin-project/jackin-role-action | `main` / `8882236041e149153491ada7091382e76be1c313` | 10 (#157–#160, #164–#167, #181–#182) | `velnor-workflow.toml` | 6: `ci-main`, `ci-policy`, `ci-pr`, `ci-unit-docs`, `maintenance`, `nightly` | Action/role repo is docs-only generated; action metadata/consumer fixture gap. |

## Access and interpretation

- All 32 repository metadata, default branch refs, open-PR lists, `.github-gen` paths, and `.github/workflows` paths were readable with the authenticated `gh` account. No repository access blocker occurred.
- `main` was the live default branch for all 32 rows at the snapshot. The table is an observation, not a pass: it does not assert generated semantics, required-check success, workload completeness, or package delivery.
- `.github-gen` absence and `NO_WORKFLOWS_REQUIRED.md` are recorded as gaps because the specification requires meaningful category coverage; they are not accepted as completed migrations.
- PRs with multiple base SHAs and PR heads are intentionally preserved in `open-prs.tsv`; do not assume one base revision per repository.
