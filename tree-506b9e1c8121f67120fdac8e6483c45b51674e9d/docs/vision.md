# Velnor Vision

Velnor is a GitHub Actions-like workflow engine with Pkl workflow definitions and a Rust runtime.

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

Velnor should keep that model where it works. The main change is replacing YAML plus ad hoc expressions with a Pkl-based typed workflow definition.

## Why Not YAML

YAML is good as a serialization format, but weak as a workflow authoring language:

- structure is only validated after parsing
- reuse is awkward
- rich types are missing
- dynamic configuration usually becomes string interpolation
- complex workflows become pseudo-code embedded in YAML
- many mistakes are found only after pushing to the CI provider

Velnor should keep the readability of GitHub Actions while adding static validation, composition, and richer domain types.

## Proposed Stack

- Pkl: workflow authoring, schemas, defaults, validation, constraints, reusable modules
- Rust: parser integration, planner, scheduler, runner, CLI, server
- Containers/processes: execution isolation for arbitrary user commands
- Typed plugins: reusable building blocks with declared inputs, outputs, permissions, and runtime requirements

Pkl is preferred because it has the strongest authoring experience and visibility among the modern config-language candidates. Rust integration should start by invoking the official Pkl CLI, then move toward `pkl server` embedding when needed.

See [research/pkl-rust.md](research/pkl-rust.md).

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

## Example: Velnor Pkl Sketch

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

Velnor should be flexible because users compose typed workflow primitives and only escape into scripts at execution boundaries.

## Non-Goals

- Do not build a YAML transpiler as the primary product.
- Do not require JavaScript for custom workflow logic.
- Do not hide runtime behavior behind untyped string interpolation.
- Do not make Pkl users learn a completely different CI model from GitHub Actions.

## Open Questions

- Should Velnor support importing existing GitHub Actions directly?
- Should `Use` support OCI-based actions only, or also GitHub-style repositories?
- Should Pkl evaluate on the control plane only, or can runners evaluate local modules?
- How much of GitHub Actions expression syntax should be preserved?
- Should the first release target local execution, self-hosted server, or GitHub app integration?
