# KCL Research

Status: historical brainstorming. The current implementation scope is GitHub
Actions runner compatibility with existing YAML unchanged. Do not implement KCL
or any Velnor-native workflow language before live target repository proof.

KCL was researched as a candidate for a future workflow definition language.

## Summary

KCL fits Velnor better than YAML and may fit Rust better than Pkl:

- schema-centric configuration language
- static typing, constraints, rules, defaults, modules
- designed for config/policy, not general app scripting
- core implementation is Rust-heavy
- CNCF Sandbox project
- existing package/module tooling
- multi-language SDK story, including Rust-facing internals and bindings

Primary sources:

- KCL website: https://www.kcl-lang.io/
- KCL repository: https://github.com/kcl-lang/kcl
- KCL bindings repository: https://github.com/kcl-lang/lib
- KCL organization: https://github.com/kcl-lang

## Why KCL Fits Velnor

### Typed Workflow Model

Velnor needs a typed model for:

- workflows
- triggers
- jobs
- steps
- matrix axes
- runners
- services
- artifacts
- caches
- secrets
- permissions
- environments

KCL's schema model maps naturally to this. Velnor can publish a KCL module containing workflow schemas and validation rules.

### Controlled Flexibility

GitHub Actions gets flexibility through YAML expressions, shell, JavaScript actions, Docker actions, and marketplace actions.

Velnor should put flexibility in typed KCL modules and keep arbitrary execution at step boundaries.

That means users can compose workflow definitions without turning the orchestrator into a general JavaScript runtime.

### Rust Runtime Alignment

KCL's main implementation is Rust-heavy. That matters for Velnor because the engine is planned in Rust:

- easier local embedding path than Pkl CLI-only integration
- better chance to inspect AST/types/plans in-process
- possible future direct API integration
- less impedance mismatch than Go-native CUE or JVM-heavy config tooling

## Name Conflict Warning

There is another KCL in the Rust ecosystem: KittyCAD Language. For example, `kcl_lib` on docs.rs refers to KittyCAD/Zoo CAD tooling, not Kusion Configuration Language.

For Velnor, "KCL" means Kusion Configuration Language from `kcl-lang/kcl`.

## GitHub Actions Shape to Preserve

Velnor should preserve this user model:

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

Velnor KCL should feel close:

```python
import velnor.workflow as wf

workflow = wf.Workflow {
    name = "CI"

    on = {
        pull_request = {}
        push = {
            branches = ["main"]
        }
    }

    jobs = {
        test = wf.Job {
            matrix = {
                os = ["ubuntu-latest", "macos-latest"]
                rust = ["stable", "beta"]
            }

            runs_on = "${{ matrix.os }}"

            steps = [
                wf.Checkout { version = "v4" }
                wf.Run { command = "rustup default ${{ matrix.rust }}" }
                wf.Run { command = "cargo test" }
            ]
        }
    }
}
```

## Possible KCL Schema Direction

```python
schema Workflow:
    name: str
    on: Triggers
    jobs: {str: Job}

schema Triggers:
    push?: PushTrigger
    pull_request?: PullRequestTrigger
    schedule?: [ScheduleTrigger]
    manual?: ManualTrigger

schema PushTrigger:
    branches?: [str]
    tags?: [str]
    paths?: [str]

schema Job:
    runs_on: str
    needs?: [str]
    matrix?: {str: [str]}
    steps: [Step]
    timeout_minutes?: int
    retry?: Retry

    check:
        timeout_minutes == Undefined or timeout_minutes > 0

schema Step:
    name?: str
    run?: str
    uses?: str
    with?: {str: any}
    env?: {str: str}

    check:
        run != Undefined or uses != Undefined
```

## Integration Plan

1. Define minimal Velnor KCL schema package.
2. Load/evaluate KCL from Rust CLI.
3. Convert KCL output into Rust structs.
4. Validate graph-level constraints in Rust:
   - missing `needs`
   - DAG cycles
   - invalid matrix references
   - unknown runner labels
   - invalid artifact/cache usage
5. Produce normalized execution plan JSON.
6. Execute plan locally.

## Risks

- Rust SDK surface may be less stable than CLI and other bindings.
- KCL ecosystem is smaller than GitHub Actions, CUE, or Jsonnet.
- KCL is strongly cloud-native/Kubernetes-adjacent; Velnor must keep CI/CD-first UX.
- Syntax is familiar but not identical to GitHub Actions, so migration docs matter.
- Need clarify whether Velnor stores expression syntax like `${{ matrix.os }}` or replaces it with native KCL references.

## Recommendation

Historical research conclusion only: KCL was a plausible workflow authoring
candidate during brainstorming. Do not implement it now.

Keep first milestone narrow:

- one workflow file
- `push` and `pull_request`
- jobs
- steps
- `needs`
- matrix
- local runner
- normalized execution plan

Do not build marketplace/plugin system until the KCL schema and Rust plan format feel correct.
