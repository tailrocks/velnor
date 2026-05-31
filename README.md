# Velnor

Codename for a unified workflow engine for CI/CD and general-purpose pipelines.

## Idea

Velnor aims to combine CI/CD orchestration with broader workflow automation.

The user experience should feel close to GitHub Actions: workflows, triggers, jobs, steps, matrices, environments, secrets, artifacts, caches, reusable workflow modules, and runner labels should all feel familiar.

The first implementation target:

- existing `.github/workflows/*.yml` files keep working
- Velnor registers as a GitHub self-hosted runner replacement
- jobs run inside Docker-isolated environments
- the initial scope is the workflows used by `jackin-project/jackin` and `ChainArgos/java-monorepo`

The later typed workflow model:

- workflow definitions are written in Pkl instead of YAML
- workflow primitives are strongly typed
- validation happens before execution
- the engine and runner are implemented in Rust
- custom building blocks expose typed interfaces instead of unconstrained JavaScript actions

## Direction

See [docs/vision.md](docs/vision.md).

## Current Runner Scaffold

The first Rust crate is `velnor-runner`. It currently provides the local CLI/config shell for Milestone 0:

```sh
cargo run --bin velnor-runner -- configure \
  --url https://github.com/OWNER/REPO \
  --token "$GITHUB_RUNNER_REGISTRATION_TOKEN" \
  --labels velnor,hetzner-sentry-ci \
  --replace

cargo run --bin velnor-runner -- configure \
  --url https://github.com/OWNER/REPO \
  --token fake \
  --labels velnor \
  --dry-run

cargo run --bin velnor-runner -- status
cargo run --bin velnor-runner -- run
```

`configure` now validates runner scope URLs, exchanges a GitHub runner registration token for tenant credentials, and can add/replace a runner agent in the selected pool. `run --once` can exchange stored OAuth runner credentials, create a classic runner session, and poll one message.

Phase 0 runner compatibility: [docs/phase-0-github-runner-compat.md](docs/phase-0-github-runner-compat.md).

Implementation roadmap: [docs/implementation-roadmap.md](docs/implementation-roadmap.md).

Language decision: [docs/decision-pkl.md](docs/decision-pkl.md).

KCL research: [docs/research/kcl.md](docs/research/kcl.md).

Language comparison and target repo workflow analysis: [docs/research/config-language-comparison.md](docs/research/config-language-comparison.md).

Pkl/Rust integration research: [docs/research/pkl-rust.md](docs/research/pkl-rust.md).

Pkl vs KCL language comparison: [docs/research/pkl-vs-kcl.md](docs/research/pkl-vs-kcl.md).

Side-by-side GitHub Actions vs Pkl vs KCL examples: [docs/research/github-actions-vs-pkl-vs-kcl.md](docs/research/github-actions-vs-pkl-vs-kcl.md).

GitHub runner protocol contract: [docs/research/github-runner-protocol-contract.md](docs/research/github-runner-protocol-contract.md).
