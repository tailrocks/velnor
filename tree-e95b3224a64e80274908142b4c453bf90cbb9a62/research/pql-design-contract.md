# Deferred PQL Design Contract

Status: research only. Do not implement PQL, Pkl, KCL, or any native Velnor
workflow parser before the GitHub Actions runner replacement passes live target
repository proof.

This document captures the future typed authoring direction from the current
research. Phase 0 still consumes existing GitHub Actions YAML through GitHub
itself. PQL only becomes relevant after Velnor has proved it can run the target
repositories as a self-hosted runner.

## Product Goal

PQL should feel like strongly typed GitHub Actions, not like a new CI model.
The user should recognize:

- workflows
- triggers
- jobs
- `needs`
- matrices
- runner labels
- environment variables
- permissions
- caches
- artifacts
- Docker/build primitives
- reusable workflow-like modules
- typed action/plugin calls

The difference is that PQL rejects bad structure before a workflow is queued.
YAML accepts many mistakes until runtime. PQL should make common CI/CD mistakes
type errors or validation errors.

## Boundary With Phase 0

Phase 0 path:

```text
GitHub YAML -> GitHub scheduler/matrix/workflow_call expansion
            -> AgentJobRequestMessage
            -> Velnor GitHub adapter
            -> NormalizedJobPlan
            -> Docker executor
            -> run-service reporter
```

Future PQL path:

```text
PQL source -> PQL evaluator/type checker
           -> typed workflow model
           -> Velnor planner
           -> NormalizedJobPlan
           -> Docker executor
           -> reporter
```

PQL must lower into the same execution model as GitHub job messages. It should
not create a second executor or a second action-adapter system.

## Design Rules

- Keep GitHub Actions vocabulary where it is already good.
- Replace stringly typed action inputs with typed helpers where Velnor owns the
  primitive.
- Keep raw shell steps explicit.
- Keep raw marketplace-style `Use` explicit.
- Make runner platform constraints typed; a Linux-only runner must not silently
  accept macOS labels.
- Make Docker, cache, artifact, and Pages behavior domain primitives instead of
  unstructured strings.
- Keep expressions as a visible escape hatch, not as the default way to model
  data flow.
- Prefer typed references for step outputs, job outputs, matrix values, and
  inputs.
- Unknown fields should be errors.

## Core Types

The first package should expose these concepts:

```text
Workflow
Trigger
WorkflowInput
Permissions
Job
Runner
Matrix<T>
Step
RunStep
CheckoutStep
CacheStep
UploadArtifactStep
DownloadArtifactStep
DockerLoginStep
DockerBakeStep
DockerBuildStep
RequiredGateStep
UseStep
CompositeModule
```

The important part is the `Step` union. CI behavior should be typed at the
boundary:

```text
Step =
  Run(command: NonEmptyString, shell: Shell = Bash)
| Checkout(repository?, ref?, path?, fetchDepth?)
| Cache(paths: List<Path>, key: CacheKey, restoreKeys?)
| UploadArtifact(name: ArtifactName, paths: NonEmptyList<Path>)
| DownloadArtifact(name?, path?)
| DockerLogin(registry, username, passwordSecret)
| DockerBake(files, targets, push, cache?)
| DockerBuild(context, file?, tags, push, labels?)
| RequiredGate(needs: NeedsRef)
| Use(action: ActionRef, inputs: TypedMap)
```

Raw `Use` exists for migration, but target-adapter families should get typed
wrappers first.

## GitHub YAML To PQL Examples

### Rust CI

GitHub Actions:

```yaml
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - run: cargo fmt --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo test
```

PQL shape:

```text
job "check" {
  runner = Runner.linux("ubuntu-latest")

  steps = [
    Checkout(),
    Run("cargo fmt --check"),
    Run("cargo clippy --all-targets --all-features -- -D warnings"),
    Run("cargo test"),
  ]
}
```

### Matrix

GitHub Actions:

```yaml
strategy:
  matrix:
    include:
      - package: bitcoin-processor-app
      - package: ethereum-processor-app
steps:
  - run: cargo test -p ${{ matrix.package }}
```

PQL shape:

```text
type RustPackage = enum {
  bitcoinProcessorApp = "bitcoin-processor-app"
  ethereumProcessorApp = "ethereum-processor-app"
}

job "test" {
  matrix = Matrix.include<RustPackage>([
    RustPackage.bitcoinProcessorApp,
    RustPackage.ethereumProcessorApp,
  ])

  steps = [
    Run("cargo test -p ${matrix.value}"),
  ]
}
```

The key improvement is that matrix entries can be constrained to known package
values instead of arbitrary strings.

### Cache

GitHub Actions:

```yaml
- uses: actions/cache@v5
  with:
    path: |
      ~/.cargo/registry
      target
    key: rust-${{ runner.os }}-${{ hashFiles('**/Cargo.lock') }}
```

PQL shape:

```text
Cache {
  paths = [Home(".cargo/registry"), Workspace("target")]
  key = CacheKey.parts(["rust", runner.os, hashFiles("**/Cargo.lock")])
}
```

The cache key is structured data until rendering. The path type also separates
home-relative and workspace-relative paths.

### Docker Bake

GitHub Actions:

```yaml
- uses: docker/login-action@v4
  with:
    username: ${{ secrets.DOCKERHUB_USERNAME }}
    password: ${{ secrets.DOCKERHUB_TOKEN }}
- uses: docker/bake-action@v7
  with:
    files: docker-bake.hcl
    targets: bitcoin-processor-app
    push: false
```

PQL shape:

```text
DockerLogin {
  registry = DockerHub
  username = Secret("DOCKERHUB_USERNAME")
  password = Secret("DOCKERHUB_TOKEN")
}

DockerBake {
  files = [Workspace("docker-bake.hcl")]
  targets = [DockerTarget("bitcoin-processor-app")]
  push = false
}
```

The Docker target can be validated against a declared target set before the job
starts.

### Required Gate

GitHub Actions commonly implements required gates with shell or expressions over
`needs`. PQL should make that a primitive:

```text
RequiredGate {
  needs = allPrevious()
  failOn = [Failure, Cancelled]
}
```

The generated plan can still lower this to a normal script or native gate step,
but users and AI agents should not hand-write the fan-in logic.

## Target Repository Coverage First

The first PQL package should model only primitives already needed by the target
repositories:

- checkout
- Rust setup/mise/tool setup
- cache
- artifacts
- paths filter
- sccache soft-fail handling
- Docker login
- Buildx/Bake/build-push
- GitHub runtime export
- Renovate container execution
- Pages artifact/deploy
- local composite equivalents for `aggregate-needs` and `check-deployed-docs`

Do not design broad marketplace compatibility first. The same target-repository
boundary used for Phase 0 should guide the first typed package.

## AI-Agent Guardrails

PQL exists partly to make AI-generated workflows safer. The package should make
bad output hard to express:

- action names are enum/catalog values where possible
- runner labels are typed by platform and architecture
- secret references are not plain strings
- artifact/cache paths are explicit path types
- Docker targets/tags/labels have dedicated types
- job outputs reference typed producer outputs
- raw expressions require an `Expression(...)` wrapper
- raw shell requires a `Run(...)` wrapper with a non-empty command

The expected agent failure mode should be a type-check error, not a wrong CI
run.

## Open Decisions

- PQL syntax is not final. It may be a Pkl package, a Velnor-owned language, or
  a Rust-backed schema format.
- PQL should probably compile to both Velnor-native plans and GitHub Actions
  YAML during migration, but that is future work.
- Typed authoring should start with one Linux target workflow after live GitHub
  compatibility is proven.
- The package should be strict by default, with explicit escape hatches.

## Non-Goals

- no Phase 0 parser
- no Phase 0 PQL CLI
- no Phase 0 workflow scheduler
- no replacement for GitHub YAML before the target repositories pass live UI
  validation on Velnor
