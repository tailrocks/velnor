# E1 inventory — Velnor 17-unit confirmation

Verdict: **CONFIRMED** — no adds, no drops vs `evidence.md` §2.

## Refs resolved (via `gh api`, live)

| Ref | SHA |
| --- | --- |
| `refs/heads/main` (CURRENT main `33688938`) | `33688938297eee3933997dbbf368ee20fb1779d4` |
| `refs/heads/docs/bastion-final-plan` (campaign) | `96ccc0f1e7168ed16694225d284f9a1ae053e737` |

Fetch commands:

```text
gh api repos/tailrocks/velnor/contents/.github/ci/project.toml --method GET -f ref=33688938297eee3933997dbbf368ee20fb1779d4 -q '.content'
gh api repos/tailrocks/velnor/contents/.github/ci/project.toml --method GET -f ref=docs/bastion-final-plan -q '.content'
gh api repos/tailrocks/velnor/git/ref/heads/main
gh api repos/tailrocks/velnor/git/ref/heads/docs/bastion-final-plan
```

## File identity

| Ref | Blob SHA | Size | SHA-256 (decoded) |
| --- | --- | --- | --- |
| main | `9f5ba3326bad11189b034357479e8a5f3d4db1b5` | 35453 | `aa55170d47764a428f7d7383187fe66ae30db8d29183162134b47ce7d3baf7c5` |
| campaign | `9f5ba3326bad11189b034357479e8a5f3d4db1b5` | 35453 | `aa55170d47764a428f7d7383187fe66ae30db8d29183162134b47ce7d3baf7c5` |

`diff -u` main vs campaign: **IDENTICAL** (278 lines each).

## Unit IDs (both refs, file order)

```text
bun-velnor
docker
docs
opentofu
rust-policy
rust-unit-collector
rust-velnor-bench
rust-velnor-client
rust-velnor-control
rust-velnor-model
rust-velnor-render
rust-velnor-runner
rust-velnor-tools
rust-velnor-workflow
rust-velnor-workflow-contract
rust-velnorctl
rust-production-topology
```

Comparison vs `plans/bastion-three-provider-ci/evidence.md` §2: **17/17 match, same order**.
Adds: none. Drops: none. No coverage-equivalent correction needed.

## Providers (hosted/velnor)

Top-level: `runners = "both"`; `github_runner = "ubuntu-24.04"`;
`velnor_labels = ["self-hosted", "velnor-target-mvp"]`.
No per-unit runner override exists in `project.toml`; every unit defines both
`github_*_commands` and `velnor_*_commands`, so each unit is **hosted + velnor (both)**.

| Unit | Kind | Root | Provider |
| --- | --- | --- | --- |
| bun-velnor | bun | `.` | hosted + velnor |
| docker | docker | `.` | hosted + velnor |
| docs | docs | `.` | hosted + velnor |
| opentofu | opentofu | `.` | hosted + velnor |
| rust-policy | rust | `.` | hosted + velnor |
| rust-unit-collector | rust | `tools/unit-collector` | hosted + velnor |
| rust-velnor-bench | rust | `crates/velnor-bench` | hosted + velnor |
| rust-velnor-client | rust | `crates/velnor-client` | hosted + velnor |
| rust-velnor-control | rust | `crates/velnor-control` | hosted + velnor |
| rust-velnor-model | rust | `crates/velnor-model` | hosted + velnor |
| rust-velnor-render | rust | `crates/velnor-render` | hosted + velnor |
| rust-velnor-runner | rust | `crates/velnor-runner` | hosted + velnor |
| rust-velnor-tools | rust | `crates/velnor-tools` | hosted + velnor |
| rust-velnor-workflow | rust | `crates/velnor-workflow` | hosted + velnor |
| rust-velnor-workflow-contract | rust | `crates/velnor-workflow-contract` | hosted + velnor |
| rust-velnorctl | rust | `crates/velnorctl` | hosted + velnor |
| rust-production-topology | rust | `.` | hosted + velnor |

Note: `evidence.md` §1 audited Velnor SHA was `3353310c…`; CURRENT main is
`33688938…`. Inventory is unchanged across that move and across the campaign branch.
