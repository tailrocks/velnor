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
notes are historical brainstorming only, not an implementation plan.

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

## Deferred Example: Typed Workflow Sketch

```pkl
amends "package://velnor.dev/workflow@1.0.0#/Workflow.pkl"

name = "CI"

on {
  pullRequest {}
  push {
    branches = List("main")
  }
}

jobs {
  ["test"] = new Job {
    matrix {
      ["os"] = List("ubuntu-latest", "macos-latest")
      ["rust"] = List("stable", "beta")
    }

    runsOn = "${{ matrix.os }}"

    steps = List(
      Checkout { version = "v4" },
      Run { command = "rustup default ${{ matrix.rust }}" },
      Run { command = "cargo test" },
    )
  }
}
```

The shape remains close to GitHub Actions, but the config is typed and can be evaluated into an execution plan before any runner starts.

## Core Concepts

### Workflow

A workflow is the top-level unit. It defines triggers, permissions, defaults, environment variables, jobs, and metadata.

### Trigger

Triggers should mirror common GitHub Actions events where possible:

- `push`
- `pullRequest`
- `schedule`
- `manual`
- `workflowCall`
- `repositoryEvent`
- `tag`
- `release`

### Job

A job is a node in the execution DAG. It has:

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

A step is a command or typed action. Initial step kinds:

- `Run`
- `Checkout`
- `Use`
- `UploadArtifact`
- `DownloadArtifact`
- `RestoreCache`
- `SaveCache`

### Typed Action

A typed action is a reusable building block with explicit schema:

- inputs
- outputs
- required permissions
- supported platforms
- runtime type
- version

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
