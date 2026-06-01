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

## GitHub Compatibility Mapping

PQL should be close enough to GitHub Actions that a workflow author can migrate
one job at a time. The names should remain familiar, but the weak YAML shapes
should become typed fields or typed unions.

| GitHub Actions concept | PQL shape | Type-safety improvement | Lowering target |
| --- | --- | --- | --- |
| `name` | `Workflow.name: NonEmptyString` | empty names rejected | workflow metadata |
| `on` | `Trigger` union | invalid trigger/input combinations rejected | GitHub YAML generator or Velnor scheduler |
| `permissions` | `Permissions` record with enum values | unknown scopes and misspelled access levels rejected | GitHub-compatible permissions model |
| `jobs.<id>` | `JobId -> Job` map | job id format and duplicate ids rejected | `NormalizedJobPlan.identity` |
| `needs` | `List<JobRef>` | references must point to declared jobs | scheduler or GitHub YAML generator |
| `runs-on` | `Runner` union | Linux/macOS/ARM labels cannot be mixed accidentally | runner labels in `JobExecutionPlan` |
| `strategy.matrix` | `Matrix<T>` | matrix values can be enums/records, not ad hoc strings | scheduler-expanded job plans |
| `env` | `Env` map | secret references and plain strings are distinct | job/step env maps |
| `defaults.run` | `RunDefaults` | shell and working directory are typed | script planning |
| `steps[*].run` | `Run(command: NonEmptyString)` | empty shell commands rejected | `ExecutableStep::Script` |
| `steps[*].uses: actions/checkout` | `Checkout(...)` | checkout inputs are typed paths/refs/depths | `CheckoutPlan` |
| `steps[*].uses: actions/cache` | `Cache(...)` | cache key and paths are structured | native cache adapter |
| `steps[*].uses: actions/upload-artifact` | `UploadArtifact(...)` | artifact name/path/no-files behavior typed | native artifact adapter |
| `steps[*].uses: dorny/paths-filter` | `PathFilter(...)` | filters are named typed outputs | native paths-filter adapter |
| `steps[*].uses: docker/*` | `DockerLogin`, `DockerBuild`, `DockerBake` | Docker inputs are domain values, not multiline strings | native Docker adapters |
| local composite actions | `ModuleCall(...)` or typed helper | module inputs/outputs are declared | composite expansion or native helper |
| `if` expressions | typed condition helpers plus `Expression(...)` escape hatch | common gates avoid string expressions | step condition evaluator |
| job outputs | `outputs { name = step.output }` | output producers must exist | job output expressions |

This mapping intentionally keeps the user-facing model close to GitHub Actions
while making the dangerous edges explicit. PQL should not hide CI/CD concepts
behind a totally new abstraction; it should make the GitHub-shaped workflow
strict.

## Target-Shaped PQL Sketches

These examples are not implementation work. They define the future authoring
surface that should eventually lower to the same normalized plans that Phase 0
creates from GitHub job messages.

### Path Filter Plus Required Gate

GitHub Actions shape used by the target repositories:

```yaml
jobs:
  changes:
    runs-on: ubuntu-latest
    outputs:
      docs: ${{ steps.filter.outputs.docs }}
    steps:
      - uses: actions/checkout@v6
      - uses: dorny/paths-filter@v4
        id: filter
        with:
          filters: |
            docs:
              - 'docs/**'

  docs-required:
    needs: [changes, docs-link-check, repo-link-check]
    runs-on: ubuntu-latest
    if: always()
    steps:
      - uses: ./.github/actions/aggregate-needs
        with:
          needs-json: ${{ toJSON(needs) }}
```

PQL direction:

```text
job "changes" {
  runner = Runner.linux("ubuntu-latest")

  outputs {
    docs = steps.filter.outputs["docs"]
  }

  steps = [
    Checkout(),
    PathFilter(
      id = "filter",
      filters = {
        docs = [WorkspaceGlob("docs/**")]
      },
    ),
  ]
}

job "docs-required" {
  runner = Runner.linux("ubuntu-latest")
  needs = [jobs.changes, jobs.docsLinkCheck, jobs.repoLinkCheck]
  condition = Always

  steps = [
    RequiredGate(needs = needs.all(), failOn = [Failure, Cancelled]),
  ]
}
```

The typed version removes the `toJSON(needs)` shell/composite plumbing from the
authoring surface. The lowerer can still emit a native gate step, a local
composite call, or GitHub-compatible YAML during migration.

### Rust Tooling And Cache

GitHub Actions shape:

```yaml
steps:
  - uses: actions/checkout@v6
  - uses: jdx/mise-action@v4
  - uses: actions/cache@v5
    with:
      path: |
        ~/.cargo/registry
        ~/.cargo/git
        target
      key: rust-${{ runner.os }}-${{ hashFiles('**/Cargo.lock') }}
  - run: cargo clippy --workspace --all-targets -- -D warnings
  - run: cargo test --workspace
```

PQL direction:

```text
job "rust-check" {
  runner = Runner.linux("hetzner-sentry-ci")

  env {
    CARGO_INCREMENTAL = "0"
    CARGO_TERM_COLOR = "always"
  }

  steps = [
    Checkout(fetchDepth = Full),
    Mise(packages = [Tool.rust]),
    Cache(
      paths = [
        Home(".cargo/registry"),
        Home(".cargo/git"),
        Workspace("target"),
      ],
      key = CacheKey.parts(["rust", runner.os, hashFiles("**/Cargo.lock")]),
    ),
    Run("cargo clippy --workspace --all-targets -- -D warnings"),
    Run("cargo test --workspace"),
  ]
}
```

The improvement is not shorter syntax. The improvement is that cache paths,
tool setup, runner platform, and non-empty commands are checked before a job is
queued.

### Docker Bake

GitHub Actions shape:

```yaml
steps:
  - uses: actions/checkout@v6
  - uses: docker/setup-buildx-action@v4
    with:
      driver: docker-container
  - uses: docker/login-action@v4
    with:
      username: ${{ secrets.DOCKERHUB_USERNAME }}
      password: ${{ secrets.DOCKERHUB_TOKEN }}
  - uses: docker/bake-action@v7
    with:
      files: backend-rust/docker-bake.hcl
      targets: bitcoin-processor-app
      push: false
```

PQL direction:

```text
type RustImageTarget = enum {
  bitcoinProcessorApp = "bitcoin-processor-app"
}

job "docker-bake" {
  runner = Runner.selfHosted(labels = ["hetzner-sentry-ci"], os = Linux)

  steps = [
    Checkout(),
    DockerBuildx(builder = "velnor-builder", driver = DockerContainer),
    DockerLogin(
      registry = DockerHub,
      username = Secret("DOCKERHUB_USERNAME"),
      password = Secret("DOCKERHUB_TOKEN"),
    ),
    DockerBake(
      files = [Workspace("backend-rust/docker-bake.hcl")],
      targets = [RustImageTarget.bitcoinProcessorApp],
      push = false,
    ),
  ]
}
```

The target name becomes an enum value that can be validated against a declared
image catalog. `push` is a boolean, not a string. Secrets are explicit secret
references, not normal string interpolation.

### Reusable Workflow-Like Module

GitHub Actions shape:

```yaml
jobs:
  docker-jvm-base:
    uses: ./.github/workflows/kestra-build-image.yml
    with:
      image: jvm-base
      dockerfile: docker/jvm-base/Dockerfile
```

PQL direction:

```text
module BuildImage {
  input image: DockerImageName
  input dockerfile: WorkspaceFile
  input push: Boolean = false

  job "build" {
    runner = Runner.selfHosted(labels = ["hetzner-sentry-ci"], os = Linux)
    steps = [
      Checkout(),
      DockerBuild(
        file = dockerfile,
        tags = [DockerTag("project/${image}:latest")],
        push = push,
      ),
    ]
  }
}

BuildImage(
  id = "docker-jvm-base",
  image = DockerImageName("jvm-base"),
  dockerfile = WorkspaceFile("docker/jvm-base/Dockerfile"),
)
```

The module keeps the reusable workflow concept, but its inputs are checked
before scheduling. A future GitHub migration mode could still render this back
to `workflow_call` YAML.

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
