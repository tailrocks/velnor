# G0 remaining-six workload contract

Captured `2026-09-19T23:41:44Z`; closing reread `2026-09-19T23:38:18Z`. The machine-readable contract is the authority: `workload-contract-remaining-six-20260919T233818Z.json`.

## Scope and limits

- Repositories: `tailrocks/parallax-telemetry-playground`; `jackin-project/jackin`; `jackin-project/jackin-agent-smith`; `jackin-project/jackin-the-architect`; `jackin-project/jackin-sentinel`; `jackin-project/jackin-role-action`.
- Read-only GitHub REST git-object, Actions-workflow, ruleset, branch-protection, PR, and `git ls-remote` observations.
- All six initial default-branch SHAs matched closing GitHub refs and `git ls-remote`; recursive trees were not truncated.
- No source checkout/edit, helper execution, workflow dispatch, Velnor host, install, release, artifact, job result, provider transcript, or publication.
- `gate_status` is incomplete/not evaluated. This is a source-derived expectation supplement, not a pass claim.

## Exact source heads

| Repository | `main` SHA | Workflows in exact tree | Open PRs | Required contexts from active `protect-main` ruleset |
|---|---|---:|---:|---|
| `tailrocks/parallax-telemetry-playground` | `54d09bf71181dcfc72d6829fabec3c53f55aacf9` | 10 | 0 | `DCO`, `ci-required` |
| `jackin-project/jackin` | `3b1e1fc0a20a7d861454746c9ebb50a335c9b412` | 15 | 2 | `DCO`, `Policy`, `ci-required` |
| `jackin-project/jackin-agent-smith` | `08cb1c2f82519bab1aa0c164879955a84d35463b` | 6 | 9 | `DCO`, `Policy`, `ci-required` |
| `jackin-project/jackin-the-architect` | `d0956f0192d2605cbd26d84bf8911211a727b6c0` | 6 | 21 | `DCO`, `Policy`, `ci-required` |
| `jackin-project/jackin-sentinel` | `a668b869b9a31622f3088c1d891c45716e94cff2` | 6 | 9 | `DCO`, `Policy`, `ci-required` |
| `jackin-project/jackin-role-action` | `8882236041e149153491ada7091382e76be1c313` | 6 | 10 | `DCO`, `Policy`, `ci-required` |

Required-check producer App IDs are unknown: ruleset detail exposed contexts only. The branch-protection endpoint returned HTTP 404 (`Branch not protected`) for every repository; rulesets are recorded separately.

## Workload findings

- Playground: portable Bun/Docker/Gradle/Rust workflow family is source-present; Swift/native source is Apple-bound, while the reused native-routing evidence records an Ubuntu route. No native execution proof.
- Jackin: current exact tree contains portable Bun/Docker/Rust, Swift/Xcode, desktop cadence, and release paths. `.github-gen/velnor-workflow.toml` pins automatic lanes to GitHub, `ubuntu-26.04`, and `macos-26`; all are source-only. Actions API exposes 20 rows while the exact tree has 15 workflow blobs; mismatch is preserved unknown.
- Smith, Architect, Sentinel: role manifest/Dockerfile and plain Docker workflow are source-present only. Role validation and runtime smoke/multiarch publisher workloads are explicitly `missing_workflow`.
- Role-action: composite action and docs workflow are source-present only. `.github/workflows/publish.yml` reusable publisher and immutable consumer fixture are explicitly `missing_workflow`.

Every repository row includes nonempty logical workloads, platform/architecture/provider data, exclusions, dependency edge kinds, immutable path/blob inventories, complete open-PR trust rows, access observations, and unknowns. Source presence is never execution success; missing workflows are never empty-success.

No evidence-branch publication was performed. JSON validity, six-repository count, stable-head checks, required nonempty contract fields, and secret scan were checked for this report; any later checkpoint ingestion remains separate.
