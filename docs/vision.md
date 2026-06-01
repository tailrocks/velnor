# Velnor Vision

Velnor is a GitHub Actions-like workflow engine and runner with a Rust runtime. Phase 0 focuses only on GitHub self-hosted runner compatibility for existing YAML workflows.

## Product Shape

The goal is not to invent a completely new CI/CD mental model. GitHub Actions already has a useful shape:

- workflows
- triggers
- jobs
- steps
- `needs` dependencies
- matrix expansion
- reusable workflow units
- secrets and variables
- artifacts and caches
- environments and approvals
- hosted or self-hosted runners

Velnor should keep that model where it works. Phase 0 keeps GitHub YAML exactly as-is and replaces the self-hosted runner implementation.

## Future Language Brainstorming

Typed workflow authoring was researched as a future idea, but it is not current
implementation work. Phase 0 does not replace YAML, parse workflows, introduce
Pkl/PQL/KCL, or define a Velnor-native workflow language. Existing research
notes are historical brainstorming only, not an implementation plan. None of
those languages are required now, selected now, or planned for Phase 0.

YAML is weak as an authoring language:

- structure is only validated after parsing
- reuse is awkward
- rich types are missing
- dynamic configuration usually becomes string interpolation
- complex workflows become pseudo-code embedded in YAML
- many mistakes are found only after pushing to the CI provider

Those points may matter later. They do not change the current goal: run existing GitHub Actions workflows unchanged on Velnor.

## Proposed Stack

- Rust: parser integration, planner, scheduler, runner, CLI, server
- Containers/processes: execution isolation for arbitrary user commands
- Typed plugins: reusable building blocks with declared inputs, outputs, permissions, and runtime requirements

Phase 0 is the accepted first implementation target: existing GitHub Actions YAML should run on a Velnor self-hosted runner.

See [phase-0-github-runner-compat.md](phase-0-github-runner-compat.md).

## Example: GitHub Actions

```yaml
name: CI

on:
  pull_request:
  push:
    branches: [main]

jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest]
        rust: [stable, beta]

    runs-on: ${{ matrix.os }}

    steps:
      - uses: actions/checkout@v4
      - run: rustup default ${{ matrix.rust }}
      - run: cargo test
```

## Phase 0 Concepts

### Workflow

A workflow is still a GitHub Actions YAML workflow owned by GitHub. Velnor does
not parse or schedule it in Phase 0.

### Trigger

Triggers are GitHub Actions triggers. GitHub evaluates them before Velnor sees
a runner job:

- `push`
- `pull_request`
- `schedule`
- `workflow_dispatch`
- `workflow_call`
- `repository_dispatch`
- `tag`
- `release`

### Job

A job is the already-expanded unit GitHub sends to a self-hosted runner. Velnor
executes that job in a Linux Docker container.

- `runsOn`
- `needs`
- `steps`
- `matrix`
- `timeout`
- `retry`
- `services`
- `container`
- `environment`
- `permissions`
- `outputs`

### Step

A step is either a shell command or one of the supported GitHub Actions-style
`uses:` entries required by the target repositories:

- `Run`
- `Checkout`
- `Use`
- `UploadArtifact`
- `DownloadArtifact`
- `RestoreCache`
- `SaveCache`

### Rust-Native Action Adapter

A Rust-native action adapter is Velnor's internal replacement for a target
marketplace action. It implements only the behavior needed by the two target
repositories:

- inputs
- outputs
- required permissions
- supported platforms
- runtime type

This keeps the marketplace idea, but avoids making JavaScript the default extension language.

## Design Principle

GitHub Actions is flexible because it lets users escape into YAML expressions, shell, JavaScript, Docker, and marketplace actions.

Configuration-language ideas remain archived brainstorming. The active design
principle is simpler: keep GitHub Actions YAML unchanged and move execution
into the Rust runner.

## Non-Goals

- Do not build a YAML transpiler in Phase 0.
- Do not require JavaScript for custom workflow logic.
- Do not hide runtime behavior behind untyped string interpolation.
- Do not introduce a Velnor-native workflow language in Phase 0.

## Open Questions

- Should Velnor support importing existing GitHub Actions directly?
- Should `Use` support OCI-based actions only, or also GitHub-style repositories?
- Should configuration-language research ever be reopened after live target
  compatibility is proven?
- How much of GitHub Actions expression syntax should be preserved?
- Should the first release target local execution, self-hosted server, or GitHub app integration?
