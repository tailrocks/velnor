# Capability / coverage matrix

Status: partial. `static-workflow` for velnor `release.yml` is still C.

| Responsibility | Class | Notes |
|---|---|---|
| Rust fmt, clippy, test from Cargo metadata | A | Default features; no `--all-features` |
| Workspace `cargo check` | B | `workspace_check` |
| Named mise tasks | B | `ci_tasks` must exist in `mise.toml` |
| Docker image validation | A | Detected Dockerfiles |
| Docker mutable-mount seed | B | Typed cache; commands materialized |
| Bun / Node / Gradle / Swift / OpenTofu / docs | A | Detected |
| GitHub default runner | A | Automatic events |
| Optional Velnor | B | `runners` + `velnor_labels`; dispatch-only unless `runners = "velnor"` (trusted default-branch events) |
| Required check `CI / Required` | A | Job id `ci-required` |
| Native binary + GHCR + guest image + deb release | C | Still static `release.yml` |
| Homebrew tap update | C | Needed by holla/ruxel/tablerock/jackin-dev |
| APT feed update | C | Needed by holla-apt / velnor-apt |
| Signed archives / attest | C | Needed by several product repos |
| Role-action product | keep | jackin-role-action product, not CI wrapper |
| velnor-actions class consumers | D after C | Replace with generated CI |

Outside-scope operational `uses:` of retired repos (deletion blockers): `ChainArgos/blockchain-nodes`, `ChainArgos/jackin-agent-brown`, `jackin-project/homebrew-tap`, `jackin-project/jackin-dev`, `tailrocks/homebrew-parallax`.
