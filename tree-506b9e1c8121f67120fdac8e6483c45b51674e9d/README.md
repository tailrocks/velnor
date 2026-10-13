# Velnor

Codename for a unified workflow engine for CI/CD and general-purpose pipelines.

## Idea

Velnor aims to combine CI/CD orchestration with broader workflow automation.

The user experience should feel close to GitHub Actions: workflows, triggers, jobs, steps, matrices, environments, secrets, artifacts, caches, reusable workflow modules, and runner labels should all feel familiar.

The difference is the implementation model:

- workflow definitions are written in Pkl instead of YAML
- workflow primitives are strongly typed
- validation happens before execution
- the engine and runner are implemented in Rust
- custom building blocks expose typed interfaces instead of unconstrained JavaScript actions

## Direction

See [docs/vision.md](docs/vision.md).

KCL research: [docs/research/kcl.md](docs/research/kcl.md).

Language comparison and target repo workflow analysis: [docs/research/config-language-comparison.md](docs/research/config-language-comparison.md).

Pkl/Rust integration research: [docs/research/pkl-rust.md](docs/research/pkl-rust.md).

Pkl vs KCL language comparison: [docs/research/pkl-vs-kcl.md](docs/research/pkl-vs-kcl.md).
