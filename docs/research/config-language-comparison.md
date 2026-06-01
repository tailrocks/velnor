# Workflow Language Comparison

This note compares candidate authoring languages for Velnor and tests them against two real GitHub Actions estates:

- https://github.com/jackin-project/jackin
- https://github.com/ChainArgos/java-monorepo

Status: archived brainstorming only. The current implementation scope is GitHub
Actions runner compatibility with existing YAML unchanged. Do not implement
Pkl, PQL, KCL, CUE, Dhall, Jsonnet, Starlark, Nickel, Nix, or any
Velnor-native workflow language. This document is not a roadmap and creates no
current requirement.

## Sources

- Pkl: https://pkl-lang.org/ and https://github.com/apple/pkl
- KCL: https://www.kcl-lang.io/ and https://github.com/kcl-lang/kcl
- CUE: https://cue.dev/ and https://github.com/cue-lang/cue
- Dhall: https://dhall-lang.org/
- Jsonnet: https://jsonnet.org/ and https://github.com/google/jsonnet
- Starlark: https://bazel.build/rules/language and https://github.com/bazelbuild/starlark
- Nickel: https://nickel-lang.org/
- Nix: https://nixos.org/ and https://nix.dev/

## Real Workflow Inventory

### jackin-project/jackin

Repository shape:

- Rust workspace
- docs site
- Docker image build/publish flow
- GitHub Pages deploy
- Homebrew preview/stable release publishing
- Renovate validation

GitHub Actions footprint:

- 7 workflow files
- 2 local composite actions
- about 1,981 workflow/action YAML lines

Important features used:

- `push`, `pull_request`, `workflow_dispatch`, `schedule`, `workflow_run`
- path-aware gating with `dorny/paths-filter`
- job outputs used by downstream `if` conditions
- required-check aggregator jobs using `if: always()` and `toJSON(needs)`
- matrix builds for CPU architecture and Rust targets
- multi-platform Docker Buildx
- GitHub Actions cache and registry cache
- artifacts for platform build digests and release archives
- GitHub Pages deploy with `pages: write` and `id-token: write`
- release job that creates GitHub Releases
- Homebrew tap checkout, PR creation, and auto-merge
- composite actions for repeated docs/check aggregation logic
- heavy shell scripts inside `run`
- all third-party actions pinned by commit SHA

### ChainArgos/java-monorepo

Repository shape:

- Java/Gradle backend
- Rust backend workspace
- frontend packages
- Docker/Kestra image build flows
- Ansible configs
- self-hosted runner

GitHub Actions footprint:

- 7 workflow files
- about 872 workflow YAML lines

Important features used:

- `push`, `pull_request`, `workflow_dispatch`, `workflow_call`, `schedule`, `merge_group`
- self-hosted runner label: `hetzner-sentry-ci`
- reusable workflows for Docker image builds
- workflow inputs with string and boolean types
- secrets inherited or explicitly passed
- path filters per Rust app
- conditional per-package test jobs
- manual target selection via workflow input
- Docker Buildx and Docker Bake
- Gradle/Rust/Ansible/Kestra workflows
- required-check aggregators for skipped/failed conditional jobs

## Replacement Capability Checklist

Velnor must support these before it can replace the two target repos:

- workflow triggers: `push`, `pull_request`, `workflow_dispatch`, `workflow_call`, `workflow_run`, `schedule`, `merge_group`
- branch/path filters
- workflow inputs with real types
- workflow/job/step environment variables
- defaults for shell and working directory
- runner labels, including self-hosted labels
- permissions
- secrets and secret inheritance
- concurrency groups and cancel policy
- job DAG via `needs`
- job outputs and step outputs
- conditionals over event/ref/input/output/needs/step state
- matrix expansion with `include`
- artifact upload/download
- cache restore/save
- reusable workflow modules
- local reusable actions/modules
- Docker Buildx/Bake primitives or plugin equivalents
- stable required-check aggregation
- first-class "skipped is OK, failed/cancelled is not OK" behavior
- GitHub API token handling for migration period
- pinned external actions or typed plugins with locked versions

## Candidate Summary

## Popularity And Age

GitHub metadata captured on 2026-05-31. `createdAt` is the GitHub repository creation date, not necessarily the original language birth date.

| Language | Main repo used for comparison | GitHub created | Stars | Notes |
| --- | --- | ---: | ---: | --- |
| Nix | https://github.com/NixOS/nix | 2012-02-08 | 16,986 | Oldest GitHub repo in this set; major ecosystem, but heavier UX than Velnor wants |
| Jsonnet | https://github.com/google/jsonnet | 2014-08-01 | 7,517 | Mature config language; dynamic typing |
| Dhall | https://github.com/dhall-lang/dhall-haskell | 2016-09-07 | 965 | Strong safety/typing, lower GitHub star count |
| Starlark | https://github.com/bazelbuild/starlark | 2018-08-16 | 3,009 | Language repo; popularity is bigger through Bazel/Buck usage than repo stars suggest |
| Nickel | https://github.com/nickel-lang/nickel | 2019-01-08 | 2,926 | Rust-native and config-first, but smaller ecosystem |
| CUE | https://github.com/cue-lang/cue | 2021-07-02 | 6,124 | Strong schema/config validation; Go-native |
| KCL | https://github.com/kcl-lang/kcl | 2022-05-05 | 2,363 | Rust-heavy core; CNCF Sandbox; smaller but aligned with Velnor |
| Pkl | https://github.com/apple/pkl | 2024-01-19 | 11,373 | Newest repo; very strong early popularity and best UX signal |

Popularity ranking by stars:

1. Nix
2. Pkl
3. Jsonnet
4. CUE
5. Starlark
6. Nickel
7. KCL
8. Dhall

Age ranking by GitHub repository creation:

1. Nix
2. Jsonnet
3. Dhall
4. Starlark
5. Nickel
6. CUE
7. KCL
8. Pkl

Interpretation:

- Pkl is the newest but already second by stars, which is a strong UX/popularity signal.
- KCL is less popular by stars, but more aligned with Rust implementation and typed config/policy.
- CUE is more popular than KCL and very strong technically, but brings a Go-native center of gravity.
- Jsonnet and Starlark are older and known, but weaker for Velnor's strong typing goal.
- Nix is by far the most popular/oldest here, but likely too much language/ecosystem weight for GitHub Actions-like CI UX.
- Dhall is technically safe but not popular enough to justify its ergonomics cost for this product.

| Language | Fit | Strength | Risk |
| --- | --- | --- | --- |
| KCL | Strong | Schemas, constraints, Rust-heavy core, CNCF Sandbox | Smaller ecosystem than Jsonnet/Starlark/Nix; cloud-native bias |
| Pkl | Strong | Best authoring feel, rich validation, Apple project | No official Rust-native path today; likely CLI/JVM bridge first |
| CUE | Strong | Excellent schema/config unification and validation | Go-native; syntax less friendly for workflow authors |
| Starlark | Medium | Proven in Bazel/Buck, deterministic scripting | No static typing; feels like scripting, not config schema |
| Jsonnet | Medium | Mature, famous, simple JSON generation story | Dynamic typing; validation is external |
| Dhall | Medium | Typed, safe, total, strong guarantees | Syntax is less mainstream; harder sell for CI users |
| Nickel | Medium | Rust-native, typed contracts, config-first | Less famous; smaller ecosystem |
| Nix | Low/Medium | Reproducible build environments | Too much ecosystem/language complexity for a GitHub Actions replacement UX |

## Archived Shortlist

Old shortlist:

1. KCL
2. Pkl
3. CUE

Old comparison note: Starlark fit a Bazel-like programmable extension language.
Jsonnet fit generated workflow JSON. Dhall fit totality and safety. None are
current choices.

## Complex Workflow Example

This example is based on the `ChainArgos/java-monorepo` Rust workflow:

- path filters detect affected Rust apps
- `workflow_dispatch` may run selected packages
- lint job runs once
- tests run per package
- required job fails only if a needed job failed or was cancelled

### GitHub Actions Shape

```yaml
name: Rust

on:
  push:
    branches: [main]
    paths:
      - "backend-rust/**"
      - "Cargo.toml"
      - "Cargo.lock"
  pull_request:
    paths:
      - "backend-rust/**"
      - "Cargo.toml"
      - "Cargo.lock"
  workflow_dispatch:
    inputs:
      packages:
        type: string
        default: ""

concurrency:
  group: rust-ci-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

jobs:
  changes:
    runs-on: hetzner-sentry-ci
    outputs:
      bitcoin-processor: ${{ steps.filter.outputs.bitcoin-processor }}
    steps:
      - uses: actions/checkout@v6
      - uses: dorny/paths-filter@v4
        id: filter
        with:
          filters: |
            bitcoin-processor:
              - "backend-rust/bitcoin-processor-app/**"
              - "Cargo.toml"
              - "Cargo.lock"

  check:
    needs: changes
    runs-on: hetzner-sentry-ci
    steps:
      - uses: actions/checkout@v6
      - run: cargo fmt --all -- --check
      - run: cargo clippy --workspace --all-targets -- -D warnings

  test-bitcoin-processor:
    needs: changes
    if: needs.changes.outputs.bitcoin-processor == 'true'
    runs-on: hetzner-sentry-ci
    steps:
      - uses: actions/checkout@v6
      - run: cargo nextest run -p bitcoin-processor-app --profile ci

  rust-required:
    needs: [check, test-bitcoin-processor]
    if: always()
    runs-on: hetzner-sentry-ci
    steps:
      - run: echo OK
```

## KCL Sketch

KCL gives Velnor typed schemas and constraints while staying close to Python-like config.

```python
import velnor.workflow as wf

apps = [
    {
        name = "bitcoin-processor"
        package = "bitcoin-processor-app"
        paths = [
            "backend-rust/bitcoin-processor-app/**"
            "backend-rust/bitcoin-migration/**"
            "backend/bitcoin-model/**"
            "backend/tailrocks-type/**"
        ]
        tools = ["rust", "cargo:cargo-nextest", "protoc"]
    }
    {
        name = "eth-processor"
        package = "eth-processor-app"
        paths = [
            "backend-rust/eth-processor-app/**"
            "backend-rust/eth-migration/**"
            "backend/eth-model/**"
            "backend/tailrocks-type/**"
        ]
        tools = ["rust", "cargo:cargo-nextest", "protoc"]
    }
]

workflow = wf.Workflow {
    name = "Rust"

    on = {
        push = {
            branches = ["main"]
            paths = wf.common_rust_paths + ["backend-rust/**", "scripts/**"]
        }
        pull_request = {
            paths = wf.common_rust_paths + ["backend-rust/**", "scripts/**"]
        }
        workflow_dispatch = {
            inputs = {
                packages = wf.StringInput {
                    default = ""
                    description = "Space-separated package list. Empty means all affected."
                }
            }
        }
    }

    concurrency = wf.Concurrency {
        group = "rust-ci-${{ github.ref }}"
        cancel_in_progress = "${{ github.event_name == 'pull_request' }}"
    }

    permissions = {
        contents = "read"
        pull_requests = "read"
    }

    env = {
        CARGO_TERM_COLOR = "always"
        CARGO_INCREMENTAL = "0"
        RUSTFLAGS = "-C link-arg=-fuse-ld=mold"
        CARGO_BUILD_JOBS = "4"
    }

    jobs = {
        changes = wf.PathFilterJob {
            runs_on = "hetzner-sentry-ci"
            filters = {
                for app in apps {
                    app.name = app.paths + wf.common_rust_paths
                }
            }
        }

        check = wf.Job {
            name = "Format and lint"
            needs = ["changes"]
            runs_on = "hetzner-sentry-ci"
            steps = [
                wf.Checkout { version = "v6" }
                wf.Mise { install = ["rust", "protoc"] }
                wf.Run { name = "Rustfmt", command = "cargo fmt --all -- --check" }
                wf.Run {
                    name = "Clippy"
                    command = wf.rust_clippy_for_changed_packages(apps)
                }
            ]
        }

        for app in apps {
            "test-${app.name}" = wf.RustNextestJob {
                package = app.package
                runs_on = "hetzner-sentry-ci"
                needs = ["changes"]
                when = wf.changed_or_selected(app.name, app.package)
                tools = app.tools
            }
        }

        rust_required = wf.RequiredJob {
            name = "Rust required"
            runs_on = "hetzner-sentry-ci"
            needs = ["check"] + [for app in apps { "test-${app.name}" }]
            fail_on = ["failure", "cancelled"]
            allow = ["success", "skipped"]
        }
    }
}
```

### KCL Notes

Pros:

- Best match for Velnor's typed schema package idea.
- `apps` list eliminates copy-paste in monorepo test jobs.
- Velnor can define `PathFilterJob`, `RustNextestJob`, and `RequiredJob` as typed reusable abstractions.
- Good Rust alignment.

Concern:

- Need decide whether `${{ ... }}` expressions remain for GitHub compatibility or become native Velnor expressions.

## Pkl Sketch

Pkl has excellent configuration authoring feel and validation, but Rust integration is weaker today.

```pkl
import "package://velnor.dev/workflow@1.0.0#/Workflow.pkl"

class RustApp {
  name: String
  package: String
  paths: Listing<String>
  tools: Listing<String> = new { "rust"; "cargo:cargo-nextest" }
}

apps = new Listing<RustApp> {
  new {
    name = "bitcoin-processor"
    package = "bitcoin-processor-app"
    paths = new {
      "backend-rust/bitcoin-processor-app/**"
      "backend-rust/bitcoin-migration/**"
      "backend/bitcoin-model/**"
      "backend/tailrocks-type/**"
    }
    tools = new { "rust"; "cargo:cargo-nextest"; "protoc" }
  }
  new {
    name = "eth-processor"
    package = "eth-processor-app"
    paths = new {
      "backend-rust/eth-processor-app/**"
      "backend-rust/eth-migration/**"
      "backend/eth-model/**"
      "backend/tailrocks-type/**"
    }
    tools = new { "rust"; "cargo:cargo-nextest"; "protoc" }
  }
}

workflow = new Workflow {
  name = "Rust"

  triggers = new {
    push = new {
      branches = new { "main" }
      paths = commonRustPaths + new { "backend-rust/**"; "scripts/**" }
    }
    pullRequest = new {
      paths = commonRustPaths + new { "backend-rust/**"; "scripts/**" }
    }
    manual = new {
      inputs = new {
        ["packages"] = new StringInput {
          default = ""
        }
      }
    }
  }

  concurrency = new Concurrency {
    group = "rust-ci-${{ github.ref }}"
    cancelInProgress = "\${{ github.event_name == 'pull_request' }}"
  }

  jobs = new Mapping<String, Job> {
    ["changes"] = new PathFilterJob {
      runsOn = "hetzner-sentry-ci"
      filters = apps.toMap((app) -> app.name, (app) -> app.paths + commonRustPaths)
    }

    ["check"] = new Job {
      needs = new { "changes" }
      runsOn = "hetzner-sentry-ci"
      steps = new {
        new Checkout { version = "v6" }
        new Mise { install = new { "rust"; "protoc" } }
        new Run { name = "Rustfmt"; command = "cargo fmt --all -- --check" }
        new Run { name = "Clippy"; command = rustClippyForChangedPackages(apps) }
      }
    }

    for (app in apps) {
      ["test-\(app.name)"] = new RustNextestJob {
        package = app.package
        needs = new { "changes" }
        runsOn = "hetzner-sentry-ci"
        when = changedOrSelected(app.name, app.package)
        tools = app.tools
      }
    }

    ["rust-required"] = new RequiredJob {
      needs = new { "check" } + apps.map((app) -> "test-\(app.name)")
      failOn = new { "failure"; "cancelled" }
      allow = new { "success"; "skipped" }
    }
  }
}
```

### Pkl Notes

Pros:

- Very readable config-as-code.
- Classes and functions fit workflow modules nicely.
- Strong candidate for UX.

Concern:

- Rust engine would likely start with CLI/JVM bridge or generated JSON, not direct native Rust embedding.

## CUE Sketch

CUE is excellent for constraints and validation, but less friendly for users coming from GitHub Actions.

```cue
package workflow

#App: {
  name:    string
  package: string
  paths: [...string]
  tools: [...string] | *["rust", "cargo:cargo-nextest"]
}

apps: [
  {
    name: "bitcoin-processor"
    package: "bitcoin-processor-app"
    paths: [
      "backend-rust/bitcoin-processor-app/**",
      "backend-rust/bitcoin-migration/**",
      "backend/bitcoin-model/**",
      "backend/tailrocks-type/**",
    ]
    tools: ["rust", "cargo:cargo-nextest", "protoc"]
  },
  {
    name: "eth-processor"
    package: "eth-processor-app"
    paths: [
      "backend-rust/eth-processor-app/**",
      "backend-rust/eth-migration/**",
      "backend/eth-model/**",
      "backend/tailrocks-type/**",
    ]
    tools: ["rust", "cargo:cargo-nextest", "protoc"]
  },
]

workflow: {
  name: "Rust"
  on: {
    push: {
      branches: ["main"]
      paths: commonRustPaths + ["backend-rust/**", "scripts/**"]
    }
    pull_request: {
      paths: commonRustPaths + ["backend-rust/**", "scripts/**"]
    }
    workflow_dispatch: inputs: packages: {
      type: "string"
      default: ""
    }
  }

  concurrency: {
    group: "rust-ci-${{ github.ref }}"
    cancel_in_progress: "${{ github.event_name == 'pull_request' }}"
  }

  jobs: {
    changes: {
      kind: "path_filter"
      runs_on: "hetzner-sentry-ci"
    }

    check: {
      needs: ["changes"]
      runs_on: "hetzner-sentry-ci"
      steps: [
        { uses: "checkout", version: "v6" },
        { run: "cargo fmt --all -- --check" },
        { run: "cargo clippy --workspace --all-targets -- -D warnings" },
      ]
    }

    for app in apps {
      "test-\(app.name)": {
        kind: "rust_nextest"
        package: app.package
        needs: ["changes"]
        runs_on: "hetzner-sentry-ci"
        when: "\(app.name) changed or \(app.package) selected"
      }
    }
  }
}
```

### CUE Notes

Pros:

- Very strong validation and unification model.
- Best if Velnor wants schema-first rigor.

Concern:

- Go-native ecosystem and CUE syntax may feel less approachable than KCL/Pkl.

## Starlark Sketch

Starlark is famous because Bazel uses it. It is deterministic scripting, not a strongly typed config language.

```python
load("@velnor//workflow:defs.bzl", "workflow", "rust_nextest_job", "required_job")

apps = [
    {
        "name": "bitcoin-processor",
        "package": "bitcoin-processor-app",
        "paths": [
            "backend-rust/bitcoin-processor-app/**",
            "backend-rust/bitcoin-migration/**",
            "backend/bitcoin-model/**",
            "backend/tailrocks-type/**",
        ],
    },
    {
        "name": "eth-processor",
        "package": "eth-processor-app",
        "paths": [
            "backend-rust/eth-processor-app/**",
            "backend-rust/eth-migration/**",
            "backend/eth-model/**",
            "backend/tailrocks-type/**",
        ],
    },
]

jobs = {
    "changes": path_filter_job(apps),
    "check": rust_check_job(needs = ["changes"]),
}

for app in apps:
    jobs["test-" + app["name"]] = rust_nextest_job(
        package = app["package"],
        needs = ["changes"],
        when = changed_or_selected(app["name"], app["package"]),
    )

jobs["rust-required"] = required_job(
    needs = ["check"] + ["test-" + app["name"] for app in apps],
)

workflow(
    name = "Rust",
    triggers = rust_triggers(),
    runner = "hetzner-sentry-ci",
    jobs = jobs,
)
```

### Starlark Notes

Pros:

- Good for custom logic and deterministic evaluation.
- Familiar to Bazel/Buck users.

Concern:

- Dynamic language. Velnor would need separate schema validation.

## Jsonnet Sketch

Jsonnet is mature and famous, but dynamic. It is strongest when generating JSON/YAML, not enforcing workflow semantics.

```jsonnet
local apps = [
  {
    name: 'bitcoin-processor',
    package: 'bitcoin-processor-app',
    paths: [
      'backend-rust/bitcoin-processor-app/**',
      'backend-rust/bitcoin-migration/**',
      'backend/bitcoin-model/**',
      'backend/tailrocks-type/**',
    ],
  },
  {
    name: 'eth-processor',
    package: 'eth-processor-app',
    paths: [
      'backend-rust/eth-processor-app/**',
      'backend-rust/eth-migration/**',
      'backend/eth-model/**',
      'backend/tailrocks-type/**',
    ],
  },
];

{
  name: 'Rust',
  on: rustTriggers(),
  jobs:
    {
      changes: pathFilterJob(apps),
      check: rustCheckJob(['changes']),
    }
    + {
      ['test-' + app.name]: rustNextestJob(app)
      for app in apps
    }
    + {
      'rust-required': requiredJob(
        ['check'] + ['test-' + app.name for app in apps]
      ),
    },
}
```

### Jsonnet Notes

Pros:

- Compact generation.
- Mature ecosystem.

Concern:

- Not strongly typed. Easy to recreate "YAML generator" problem.

## Dhall Sketch

Dhall is typed and safe, but verbose for workflow users.

```dhall
let App =
      { Type =
        { name : Text
        , package : Text
        , paths : List Text
        , tools : List Text
        }
      }

let apps =
      [ { name = "bitcoin-processor"
        , package = "bitcoin-processor-app"
        , paths =
          [ "backend-rust/bitcoin-processor-app/**"
          , "backend-rust/bitcoin-migration/**"
          , "backend/bitcoin-model/**"
          , "backend/tailrocks-type/**"
          ]
        , tools = [ "rust", "cargo:cargo-nextest", "protoc" ]
        }
      , { name = "eth-processor"
        , package = "eth-processor-app"
        , paths =
          [ "backend-rust/eth-processor-app/**"
          , "backend-rust/eth-migration/**"
          , "backend/eth-model/**"
          , "backend/tailrocks-type/**"
          ]
        , tools = [ "rust", "cargo:cargo-nextest", "protoc" ]
        }
      ]

in  Workflow::{
    , name = "Rust"
    , on = rustTriggers
    , jobs =
        buildRustJobs apps
    }
```

### Dhall Notes

Pros:

- Strong safety story and typed imports.
- Good for trusted, reproducible config.

Concern:

- Likely too alien for GitHub Actions migration.

## Design Implications for Velnor

The target repos show that the hard part is not running shell commands. The hard part is expressing these safely:

- path-aware monorepo selection
- conditional required checks
- reusable workflow/action modules
- dynamic Docker cache/publish policy
- release artifact fan-in
- typed workflow inputs
- GitHub event/ref semantics

Therefore Velnor should make these first-class typed primitives:

- `PathFilter`
- `RequiredGate`
- `WorkflowCall`
- `Matrix`
- `Artifact`
- `Cache`
- `DockerBuild`
- `DockerBake`
- `Release`
- `GitHubPages`
- `GitHubApi`
- `ChangedOrSelected`

If these are primitives, user workflow code becomes much smaller than GitHub Actions YAML while staying understandable.

## Archived Finding

If typed authoring is ever reopened later, this old research favored Pkl as a
candidate to re-evaluate:

- authoring experience is strongest
- visibility is strongest among the modern config-language candidates
- Pkl already has a typed GitHub Actions package in `pkl-pantry`
- typed action catalogs are direct prior art for Velnor plugins
- syntax is approachable for GitHub Actions users
- abstractions can remove real duplication in both target repos

KCL was the Rust/config-policy challenger in this archived comparison. CUE was
the schema/validation challenger. None are selected now.
