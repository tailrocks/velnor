# Velnor

Velnor is a Rust self-hosted GitHub Actions runner and node-local control
plane. GitHub remains the scheduler and job source of truth; Velnor validates
jobs before side effects, executes admitted work through an explicitly selected
Docker or Firecracker backend, and keeps bounded operational evidence.

Research project. Capability and proof status are documented in the
[documentation](docs/README.md).

Licensed under the [Apache License 2.0](LICENSE).
