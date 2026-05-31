# Velnor

Codename for a unified workflow engine for CI/CD and general-purpose pipelines.

## Idea

Velnor aims to combine CI/CD orchestration with broader workflow automation.

The user experience should feel close to GitHub Actions: workflows, triggers, jobs, steps, matrices, environments, secrets, artifacts, caches, reusable workflow modules, and runner labels should all feel familiar.

The difference is the implementation model:

- workflow definitions are written in KCL instead of YAML
- workflow primitives are strongly typed
- validation happens before execution
- the engine and runner are implemented in Rust
- custom building blocks expose typed interfaces instead of unconstrained JavaScript actions

## Direction

See [docs/vision.md](docs/vision.md).

KCL research: [docs/research/kcl.md](docs/research/kcl.md).
