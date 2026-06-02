# GitHub Actions vs Pkl vs KCL

This compares three authoring models for Velnor:

- current GitHub Actions YAML
- Velnor with Pkl
- Velnor with KCL

Status: archived brainstorming only. The current implementation scope is GitHub
Actions runner compatibility with existing YAML unchanged. Do not implement
Pkl, PQL, KCL, or any Velnor-native workflow language. This document is not a
roadmap and creates no current requirement.

The examples focus on the workflows used in:

- `jackin-project/jackin`
- `ChainArgos/java-monorepo`

## Executive Summary

| Criterion | GitHub Actions YAML | Pkl | KCL |
| --- | --- | --- | --- |
| Current ecosystem | Best | Medium | Smaller |
| Strong typing | Weak | Strong with strict package | Strong by design |
| Concision | Good for small workflows, poor for large ones | Best | Medium |
| Readability | Familiar, but noisy at scale | Best for workflow authors | Best for schema authors |
| AI-agent safety | Weak unless generated from schema | Strong if package is strict | Strongest raw language |
| Reuse | Composite actions and reusable workflows, split model | Natural modules/classes/functions | Natural schemas/modules/functions |
| GitHub Actions migration | Native | Best migration target | Good, but less visually close |
| Existing GitHub Actions typed reference | Native YAML docs | `pkl-pantry/com.github.actions` | No equally relevant package found |

Historical research note:

```text
If typed authoring is revisited later, Pkl was the strongest product-language
candidate in this comparison. Do not implement it now.
```

KCL is safer if we judge only raw language typing. Pkl is better if we judge the whole Velnor product: migration, readability, concision, and GitHub Actions similarity.

## Example 1: Simple Rust CI

### GitHub Actions YAML

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]
  workflow_dispatch:

permissions:
  contents: read

jobs:
  check:
    runs-on: ubuntu-latest
    env:
      CARGO_INCREMENTAL: "0"
      CARGO_TERM_COLOR: always
    steps:
      - uses: actions/checkout@v6
      - run: rustup component add rustfmt clippy
      - run: cargo fmt --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo test
```

### Pkl

```pkl
amends "package://velnor.dev/workflow@1.0.0#/Workflow.pkl"

name = "CI"

on {
  push { branches { "main" } }
  pullRequest { branches { "main" } }
  workflowDispatch {}
}

permissions {
  contents = "read"
}

jobs {
  ["check"] = RustJob {
    runsOn = "ubuntu-latest"

    env {
      ["CARGO_INCREMENTAL"] = "0"
      ["CARGO_TERM_COLOR"] = "always"
    }

    steps {
      Checkout { version = "v6" }
      Run { command = "rustup component add rustfmt clippy" }
      Run { command = "cargo fmt --check" }
      Run { command = "cargo clippy --all-targets --all-features -- -D warnings" }
      Run { command = "cargo test" }
    }
  }
}
```

### KCL

```python
import velnor.workflow as wf

workflow = wf.Workflow {
    name = "CI"

    on = wf.Triggers {
        push = wf.Push { branches = ["main"] }
        pull_request = wf.PullRequest { branches = ["main"] }
        workflow_dispatch = wf.WorkflowDispatch {}
    }

    permissions = wf.Permissions {
        contents = "read"
    }

    jobs = {
        check = wf.RustJob {
            runs_on = "ubuntu-latest"

            env = {
                CARGO_INCREMENTAL = "0"
                CARGO_TERM_COLOR = "always"
            }

            steps = [
                wf.Checkout { version = "v6" }
                wf.Run { command = "rustup component add rustfmt clippy" }
                wf.Run { command = "cargo fmt --check" }
                wf.Run { command = "cargo clippy --all-targets --all-features -- -D warnings" }
                wf.Run { command = "cargo test" }
            ]
        }
    }
}
```

### Read

GitHub Actions is shortest here because YAML is fine for simple workflows. Pkl remains close. KCL is explicit but heavier.

For simple workflows:

```text
YAML ~= Pkl > KCL
```

## Example 2: Matrix Build

Based on `jackin` release/preview builds.

### GitHub Actions YAML

```yaml
jobs:
  build:
    needs: [check-version, test]
    strategy:
      matrix:
        include:
          - target: x86_64-apple-darwin
            os: macos-latest
          - target: aarch64-apple-darwin
            os: macos-latest
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
            zigbuild: true
          - target: aarch64-unknown-linux-gnu
            os: ubuntu-latest
            zigbuild: true
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v6
      - run: rustup target add ${{ matrix.target }}
      - name: Build
        run: |
          if [ "${{ matrix.zigbuild }}" = "true" ]; then
            cargo zigbuild --release --locked --target ${{ matrix.target }}.2.17
          else
            cargo build --release --locked --target ${{ matrix.target }}
          fi
      - uses: actions/upload-artifact@v7
        with:
          name: jackin-${{ matrix.target }}
          path: |
            jackin-*.tar.gz
            jackin-*.sha256
```

### Pkl

```pkl
class BuildTarget {
  target: String(!isEmpty)
  os: "ubuntu-latest"|"macos-latest"
  zigbuild: Boolean = false
}

buildTargets = new Listing<BuildTarget> {
  new { target = "x86_64-apple-darwin"; os = "macos-latest" }
  new { target = "aarch64-apple-darwin"; os = "macos-latest" }
  new { target = "x86_64-unknown-linux-gnu"; os = "ubuntu-latest"; zigbuild = true }
  new { target = "aarch64-unknown-linux-gnu"; os = "ubuntu-latest"; zigbuild = true }
}

jobs {
  ["build"] = RustBuildJob {
    needs { "check-version"; "test" }

    matrix {
      include = buildTargets
    }

    runsOn = matrix.os

    steps {
      Checkout { version = "v6" }
      Run { command = "rustup target add \(matrix.target)" }

      Run {
        name = "Build"
        command = if (matrix.zigbuild)
          "cargo zigbuild --release --locked --target \(matrix.target).2.17"
        else
          "cargo build --release --locked --target \(matrix.target)"
      }

      UploadArtifact {
        name = "jackin-\(matrix.target)"
        paths {
          "jackin-*.tar.gz"
          "jackin-*.sha256"
        }
      }
    }
  }
}
```

### KCL

```python
schema BuildTarget:
    target: str
    os: "ubuntu-latest" | "macos-latest"
    zigbuild?: bool = False

    check:
        target != ""

build_targets = [
    BuildTarget { target = "x86_64-apple-darwin", os = "macos-latest" }
    BuildTarget { target = "aarch64-apple-darwin", os = "macos-latest" }
    BuildTarget { target = "x86_64-unknown-linux-gnu", os = "ubuntu-latest", zigbuild = True }
    BuildTarget { target = "aarch64-unknown-linux-gnu", os = "ubuntu-latest", zigbuild = True }
]

jobs = {
    build = wf.RustBuildJob {
        needs = ["check-version", "test"]

        matrix = wf.Matrix {
            include = build_targets
        }

        runs_on = "${{ matrix.os }}"

        steps = [
            wf.Checkout { version = "v6" }
            wf.Run { command = "rustup target add ${{ matrix.target }}" }
            wf.Run {
                name = "Build"
                command = if matrix.zigbuild:
                    "cargo zigbuild --release --locked --target ${{ matrix.target }}.2.17"
                else:
                    "cargo build --release --locked --target ${{ matrix.target }}"
            }
            wf.UploadArtifact {
                name = "jackin-${{ matrix.target }}"
                paths = ["jackin-*.tar.gz", "jackin-*.sha256"]
            }
        ]
    }
}
```

### Read

Pkl and KCL both improve over YAML because `BuildTarget` becomes a real type. Pkl is more compact and workflow-like. KCL is more obviously typed.

For matrix workflows:

```text
Typing: KCL >= Pkl > YAML
Readability: Pkl > KCL > YAML
```

## Example 3: Path-Aware Monorepo Testing

Based on `ChainArgos/java-monorepo`.

### GitHub Actions YAML

```yaml
jobs:
  changes:
    runs-on: hetzner-sentry-ci
    outputs:
      bitcoin-processor: ${{ steps.filter.outputs.bitcoin-processor }}
      eth-processor: ${{ steps.filter.outputs.eth-processor }}
    steps:
      - uses: actions/checkout@v6
      - uses: dorny/paths-filter@v4
        id: filter
        with:
          filters: |
            bitcoin-processor:
              - "backend-rust/bitcoin-processor-app/**"
              - "backend-rust/bitcoin-migration/**"
              - "backend/bitcoin-model/**"
              - "backend/tailrocks-type/**"
              - "Cargo.toml"
              - "Cargo.lock"
            eth-processor:
              - "backend-rust/eth-processor-app/**"
              - "backend-rust/eth-migration/**"
              - "backend/eth-model/**"
              - "backend/tailrocks-type/**"
              - "Cargo.toml"
              - "Cargo.lock"

  test-bitcoin-processor:
    needs: changes
    if: needs.changes.outputs.bitcoin-processor == 'true'
    runs-on: hetzner-sentry-ci
    steps:
      - uses: actions/checkout@v6
      - run: cargo nextest run -p bitcoin-processor-app --profile ci

  test-eth-processor:
    needs: changes
    if: needs.changes.outputs.eth-processor == 'true'
    runs-on: hetzner-sentry-ci
    steps:
      - uses: actions/checkout@v6
      - run: cargo nextest run -p eth-processor-app --profile ci
```

### Pkl

```pkl
class RustApp {
  name: String(matches(Regex("[a-z][a-z0-9-]*")))
  package: String(!isEmpty)
  paths: Listing<String>(!isEmpty)
  tools: Listing<String> = new { "rust"; "cargo:cargo-nextest" }
}

commonRustPaths = new Listing<String> {
  "Cargo.toml"
  "Cargo.lock"
}

apps = new Listing<RustApp> {
  new {
    name = "bitcoin-processor"
    package = "bitcoin-processor-app"
    paths {
      "backend-rust/bitcoin-processor-app/**"
      "backend-rust/bitcoin-migration/**"
      "backend/bitcoin-model/**"
      "backend/tailrocks-type/**"
    }
  }
  new {
    name = "eth-processor"
    package = "eth-processor-app"
    paths {
      "backend-rust/eth-processor-app/**"
      "backend-rust/eth-migration/**"
      "backend/eth-model/**"
      "backend/tailrocks-type/**"
    }
  }
}

jobs {
  ["changes"] = PathFilterJob {
    runsOn = "hetzner-sentry-ci"
    filters = apps.toMap(
      (app) -> app.name,
      (app) -> app.paths + commonRustPaths,
    )
  }

  for (app in apps) {
    ["test-\(app.name)"] = RustNextestJob {
      needs { "changes" }
      runsOn = "hetzner-sentry-ci"
      package = app.package
      when = changed(app.name)
    }
  }
}
```

### KCL

```python
schema RustApp:
    name: str
    package: str
    paths: [str]
    tools?: [str] = ["rust", "cargo:cargo-nextest"]

    check:
        regex.match(name, r"[a-z][a-z0-9-]*")
        package != ""
        len(paths) > 0

common_rust_paths = [
    "Cargo.toml"
    "Cargo.lock"
]

apps = [
    RustApp {
        name = "bitcoin-processor"
        package = "bitcoin-processor-app"
        paths = [
            "backend-rust/bitcoin-processor-app/**"
            "backend-rust/bitcoin-migration/**"
            "backend/bitcoin-model/**"
            "backend/tailrocks-type/**"
        ]
    }
    RustApp {
        name = "eth-processor"
        package = "eth-processor-app"
        paths = [
            "backend-rust/eth-processor-app/**"
            "backend-rust/eth-migration/**"
            "backend/eth-model/**"
            "backend/tailrocks-type/**"
        ]
    }
]

jobs = {
    changes = wf.PathFilterJob {
        runs_on = "hetzner-sentry-ci"
        filters = {
            app.name = app.paths + common_rust_paths for app in apps
        }
    }
} | {
    "test-${app.name}" = wf.RustNextestJob {
        needs = ["changes"]
        runs_on = "hetzner-sentry-ci"
        package = app.package
        when = wf.changed(app.name)
    } for app in apps
}
```

### Read

This is where YAML breaks down. It repeats app definitions across path filters, jobs, conditions, required gates, Docker targets, and manual inputs.

Pkl and KCL both make apps first-class data. KCL expresses schema rules more directly. Pkl is more concise and easier to scan as workflow logic.

For monorepos:

```text
Structure: Pkl ~= KCL > YAML
AI safety: KCL > Pkl > YAML
Human workflow UX: Pkl > KCL > YAML
```

## Example 4: Required Check Aggregator

Both target repos use "required" jobs because conditional jobs may skip, but branch protection still wants one stable check.

### GitHub Actions YAML

```yaml
jobs:
  rust-required:
    name: Rust required
    needs:
      - check
      - test-bitcoin-processor
      - test-eth-processor
      - test-tron-processor
    if: always()
    runs-on: hetzner-sentry-ci
    steps:
      - name: Fail if check failed
        if: needs.check.result == 'failure' || needs.check.result == 'cancelled'
        run: exit 1

      - name: Fail if bitcoin tests failed
        if: needs.test-bitcoin-processor.result == 'failure' || needs.test-bitcoin-processor.result == 'cancelled'
        run: exit 1

      - name: Fail if eth tests failed
        if: needs.test-eth-processor.result == 'failure' || needs.test-eth-processor.result == 'cancelled'
        run: exit 1

      - name: OK
        run: echo OK
```

### Pkl

```pkl
jobs {
  ["rust-required"] = RequiredGate {
    name = "Rust required"
    runsOn = "hetzner-sentry-ci"
    needs {
      "check"
      for (app in apps) { "test-\(app.name)" }
    }
    failOn { "failure"; "cancelled" }
    allow { "success"; "skipped" }
  }
}
```

### KCL

```python
jobs = {
    rust_required = wf.RequiredGate {
        name = "Rust required"
        runs_on = "hetzner-sentry-ci"
        needs = ["check"] + ["test-${app.name}" for app in apps]
        fail_on = ["failure", "cancelled"]
        allow = ["success", "skipped"]
    }
}
```

### Read

Both Pkl and KCL crush YAML here because `RequiredGate` should be a primitive.

For required aggregators:

```text
Pkl ~= KCL >>> YAML
```

## Example 5: Reusable Workflow Call

Based on `ChainArgos/java-monorepo` Kestra Docker image flow.

### GitHub Actions YAML

```yaml
jobs:
  docker-jvm-base:
    uses: ./.github/workflows/kestra-build-image.yml
    secrets: inherit
    with:
      package: jvm-base

  docker-kestra-backup:
    needs: docker-jvm-base
    uses: ./.github/workflows/kestra-build-image.yml
    secrets: inherit
    with:
      package: kestra-backup

  docker-kestra-playwright:
    needs: docker-jvm-base
    uses: ./.github/workflows/kestra-build-image.yml
    secrets: inherit
    with:
      package: kestra-playwright
```

### Pkl

```pkl
kestraImage = ReusableWorkflow {
  uses = "./.github/workflows/kestra-build-image.yml"
  secrets = "inherit"
}

jobs {
  ["docker-jvm-base"] = kestraImage {
    with { ["package"] = "jvm-base" }
  }

  ["docker-kestra-backup"] = kestraImage {
    needs { "docker-jvm-base" }
    with { ["package"] = "kestra-backup" }
  }

  ["docker-kestra-playwright"] = kestraImage {
    needs { "docker-jvm-base" }
    with { ["package"] = "kestra-playwright" }
  }
}
```

### KCL

```python
kestra_image = wf.ReusableWorkflow {
    uses = "./.github/workflows/kestra-build-image.yml"
    secrets = "inherit"
}

jobs = {
    docker_jvm_base = kestra_image {
        with = { package = "jvm-base" }
    }

    docker_kestra_backup = kestra_image {
        needs = ["docker-jvm-base"]
        with = { package = "kestra-backup" }
    }

    docker_kestra_playwright = kestra_image {
        needs = ["docker-jvm-base"]
        with = { package = "kestra-playwright" }
    }
}
```

### Read

YAML is already okay here. Pkl's amendment model is elegant. KCL is explicit.

For reusable workflow calls:

```text
Pkl > YAML > KCL
```

## Example 6: Docker Buildx/Bake

Based on `java-monorepo` Rust Docker workflow.

### GitHub Actions YAML

```yaml
jobs:
  docker-bake:
    needs: changes
    if: needs.changes.outputs.bake-targets != ''
    runs-on: hetzner-sentry-ci
    steps:
      - uses: actions/checkout@v6
      - uses: docker/setup-buildx-action@v4
        with:
          name: chainargos-rust-workspace-builder
          driver: docker-container
          cleanup: false
      - uses: docker/login-action@v4
        with:
          username: ${{ secrets.DOCKERHUB_USERNAME }}
          password: ${{ secrets.DOCKERHUB_TOKEN }}
      - uses: docker/bake-action@v7
        with:
          files: backend-rust/docker-bake.hcl
          targets: ${{ needs.changes.outputs.bake-targets }}
          set: |
            *.cache-from=type=gha,scope=rust-workspace
            *.cache-to=type=gha,scope=rust-workspace,mode=max
            bitcoin-processor-app.push=${{ github.event_name == 'push' && needs.changes.outputs.bitcoin-processor == 'true' }}
            eth-processor-app.push=${{ github.event_name == 'push' && needs.changes.outputs.eth-processor == 'true' }}
```

### Pkl

```pkl
jobs {
  ["docker-bake"] = DockerBakeJob {
    needs { "changes" }
    when = hasBakeTargets()
    runsOn = "hetzner-sentry-ci"

    builder {
      name = "chainargos-rust-workspace-builder"
      driver = "docker-container"
      cleanup = false
    }

    login = DockerLogin {
      registry = "docker.io"
      username = secret("DOCKERHUB_USERNAME")
      password = secret("DOCKERHUB_TOKEN")
    }

    bake {
      files { "backend-rust/docker-bake.hcl" }
      targets = output("changes", "bake-targets")

      cache {
        from = "type=gha,scope=rust-workspace"
        to = "type=gha,scope=rust-workspace,mode=max"
      }

      for (app in apps) {
        push[app.package] = isPushToMain() && changed(app.name)
      }
    }
  }
}
```

### KCL

```python
jobs = {
    docker_bake = wf.DockerBakeJob {
        needs = ["changes"]
        when = wf.has_bake_targets()
        runs_on = "hetzner-sentry-ci"

        builder = wf.DockerBuilder {
            name = "chainargos-rust-workspace-builder"
            driver = "docker-container"
            cleanup = False
        }

        login = wf.DockerLogin {
            registry = "docker.io"
            username = wf.secret("DOCKERHUB_USERNAME")
            password = wf.secret("DOCKERHUB_TOKEN")
        }

        bake = wf.DockerBake {
            files = ["backend-rust/docker-bake.hcl"]
            targets = wf.output("changes", "bake-targets")
            cache = wf.BuildCache {
                from = "type=gha,scope=rust-workspace"
                to = "type=gha,scope=rust-workspace,mode=max"
            }
            push = {
                app.package = wf.is_push_to_main() and wf.changed(app.name)
                for app in apps
            }
        }
    }
}
```

### Read

YAML exposes Docker implementation details as action calls and stringly typed `set` lines. Pkl/KCL can model Docker Bake as a domain primitive.

For Docker workflows:

```text
Safety: KCL >= Pkl >>> YAML
Readability: Pkl >= KCL >>> YAML
```

## Example 7: Workflow Inputs

### GitHub Actions YAML

```yaml
on:
  workflow_dispatch:
    inputs:
      packages:
        description: "Space-separated list of packages to test. Empty for all affected."
        type: string
        default: ""
      push:
        description: "Push built images to Docker Hub"
        type: boolean
        default: false
```

### Pkl

```pkl
on {
  workflowDispatch {
    inputs {
      ["packages"] = StringInput {
        description = "Space-separated list of packages to test. Empty for all affected."
        default = ""
      }

      ["push"] = BooleanInput {
        description = "Push built images to Docker Hub"
        default = false
      }
    }
  }
}
```

### KCL

```python
on = wf.Triggers {
    workflow_dispatch = wf.WorkflowDispatch {
        inputs = {
            packages = wf.StringInput {
                description = "Space-separated list of packages to test. Empty for all affected."
                default = ""
            }
            push = wf.BooleanInput {
                description = "Push built images to Docker Hub"
                default = False
            }
        }
    }
}
```

### Read

GitHub Actions supports only limited input types. Pkl/KCL can expose richer typed inputs for Velnor.

For inputs:

```text
Pkl ~= KCL > YAML
```

## Example 8: Release Artifact Fan-In

Based on `jackin` release flow.

### GitHub Actions YAML

```yaml
jobs:
  release:
    needs: [check-version, build, build-jackin-capsule]
    runs-on: ubuntu-latest
    permissions:
      contents: write
    outputs:
      arm64_macos: ${{ steps.shas.outputs.arm64_macos }}
      x86_macos: ${{ steps.shas.outputs.x86_macos }}
    steps:
      - uses: actions/checkout@v6
      - uses: actions/download-artifact@v8
        with:
          path: artifacts
          merge-multiple: true
      - name: Read platform SHA256s
        id: shas
        run: |
          echo "arm64_macos=$(awk '{print $1}' artifacts/jackin-aarch64-apple-darwin.tar.gz.sha256)" >> "$GITHUB_OUTPUT"
          echo "x86_macos=$(awk '{print $1}' artifacts/jackin-x86_64-apple-darwin.tar.gz.sha256)" >> "$GITHUB_OUTPUT"
      - name: Create GitHub Release
        env:
          GH_TOKEN: ${{ github.token }}
        run: gh release create "v${VERSION}" artifacts/*.tar.gz --generate-notes
```

### Pkl

```pkl
jobs {
  ["release"] = GitHubReleaseJob {
    needs { "check-version"; "build"; "build-jackin-capsule" }
    runsOn = "ubuntu-latest"

    permissions {
      contents = "write"
    }

    artifacts = DownloadArtifacts {
      path = "artifacts"
      mergeMultiple = true
    }

    checksums {
      ["arm64_macos"] = Sha256File { path = "artifacts/jackin-aarch64-apple-darwin.tar.gz.sha256" }
      ["x86_macos"] = Sha256File { path = "artifacts/jackin-x86_64-apple-darwin.tar.gz.sha256" }
    }

    release {
      tag = "v\(env.VERSION)"
      files { "artifacts/*.tar.gz" }
      generateNotes = true
    }
  }
}
```

### KCL

```python
jobs = {
    release = wf.GitHubReleaseJob {
        needs = ["check-version", "build", "build-jackin-capsule"]
        runs_on = "ubuntu-latest"

        permissions = wf.Permissions {
            contents = "write"
        }

        artifacts = wf.DownloadArtifacts {
            path = "artifacts"
            merge_multiple = True
        }

        checksums = {
            arm64_macos = wf.Sha256File {
                path = "artifacts/jackin-aarch64-apple-darwin.tar.gz.sha256"
            }
            x86_macos = wf.Sha256File {
                path = "artifacts/jackin-x86_64-apple-darwin.tar.gz.sha256"
            }
        }

        release = wf.GitHubRelease {
            tag = "v${env.VERSION}"
            files = ["artifacts/*.tar.gz"]
            generate_notes = True
        }
    }
}
```

### Read

Velnor should not force users to write shell for release plumbing. Pkl/KCL can type the fan-in.

For release flows:

```text
Pkl ~= KCL >>> YAML
```

## Language-Level Comparison

### GitHub Actions YAML

Best parts:

- everyone knows it
- native GitHub integration
- huge marketplace
- good for small workflows

Problems:

- not really strongly typed
- expressions are stringly typed
- dynamic logic becomes shell
- reuse model is split across actions, composite actions, reusable workflows
- large monorepo workflows repeat data
- AI agents easily write valid YAML with invalid semantics

### Pkl

Best parts:

- concise config-as-code
- strong validation via types and constraints
- object amendment is natural for templates
- readable for workflow authors
- `pkl-pantry/com.github.actions` is direct prior art
- typed action catalogs map well to Velnor plugins

Problems:

- strictness depends on Velnor package design
- flexible enough that bad schema design can become loose
- less schema-obvious than KCL

### KCL

Best parts:

- schema-first
- constraints and checks are central
- good for AI guardrails
- explicit data modeling
- reads like typed infra config

Problems:

- less concise than Pkl for workflow authoring
- less visually close to GitHub Actions
- less CI/CD-specific prior art
- may feel more like policy/IaC than workflow engine UX

## Archived Finding

This finding is deferred and non-binding. Do not implement Pkl, PQL, KCL, or any
Velnor-native workflow language now.

If typed authoring is revisited after live target GitHub Actions compatibility,
Pkl was the preferred user-facing candidate in this comparison, with KCL's
discipline as a schema benchmark.

If this research is reopened later, any typed package should be strict enough
that AI agents get immediate failures when they invent wrong fields, wrong step
kinds, wrong output references, or wrong runner labels.

Possible historical authoring sketch, not accepted design:

```pkl
amends "package://velnor.dev/workflow@1.0.0#/Workflow.pkl"
```

Possible historical runtime sketch, not accepted design:

```text
future typed source
  -> strict package validation
  -> normalized JSON or equivalent value
  -> Rust structs
  -> Rust DAG/runtime semantic validation
  -> execution plan
```

Historical decision:

```text
GitHub Actions YAML = compatibility baseline
Pkl                 = researched future user-facing candidate
KCL                 = researched strictness benchmark
```
