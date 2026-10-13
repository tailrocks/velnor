# Archived Research: Pkl And Rust Integration

This is archived brainstorming. Pkl, PQL, KCL, and any Velnor-native workflow
language are not part of the current implementation scope. Do not implement Pkl
support now. This document is not a roadmap and creates no current requirement.

## Summary

There is no official Apple-maintained Rust binding listed in Pkl's official bindings. Official bindings currently focus on Java, Kotlin, Swift, and Go.

Old Rust integration ideas:

1. Old simple option: call the `pkl` CLI from Rust and evaluate workflow files to JSON.
2. Old embedding option: use Pkl's official `pkl server` protocol from Rust.
3. Old binding option: adopt or build a Rust binding around the message-passing API and Pkl binary encoding.

Primary sources:

- Pkl language bindings: https://pkl-lang.org/main/current/language-bindings.html
- Pkl CLI: https://pkl-lang.org/main/current/pkl-cli/index.html
- Pkl language binding specification: https://pkl-lang.org/main/current/bindings-specification/index.html
- Pkl message passing API: https://pkl-lang.org/main/current/bindings-specification/message-passing-api.html
- Pkl GitHub Actions package: https://pkl-lang.org/package-docs/pkg.pkl-lang.org/pkl-pantry/com.github.actions/1.7.0/Workflow/index.html
- `apple/pkl-pantry`: https://github.com/apple/pkl-pantry
- `rpkl`: https://docs.rs/rpkl/latest/rpkl/
- `pklrust`: https://docs.rs/pklrust/latest/pklrust/
- `vize` Pkl config loader: https://github.com/ubugeeei-prod/vize/blob/main/crates/vize_carton/src/config/loader.rs

## Official Position

Pkl's official docs list Java, Kotlin, Swift, and Go bindings. Rust is not listed.

The official language binding specification is still useful for Rust. It says Pkl can be embedded in any host application. Today that embedding model uses a child process running `pkl server` and a message-passing protocol. A future C library is planned.

Old research conclusion: Rust would not need to parse or evaluate Pkl itself.
Velnor could have treated Pkl as a compiler/evaluator process if this idea were
ever reopened.

## Archived Prototype Paths

If Pkl is ever revisited later, one old idea was to use it as a workflow
language through typed deserialization from evaluated Pkl.

Two old prototype paths were:

1. Direct CLI JSON bridge.
2. `pklrust` with `EvaluatorManager` and typed serde output.

The direct CLI bridge is the most conservative. `pklrust` is closer to how a real Rust application is already using Pkl.

### Option A: CLI JSON Bridge

```text
workflow.pkl
  -> pkl eval --format json workflow.pkl
  -> serde_json::from_slice::<Workflow>()
  -> Rust semantic validation
  -> ExecutionPlan
  -> runner
```

Advantages:

- simple to implement
- uses official Pkl CLI behavior
- no dependency on immature Rust crates
- easy to debug from terminal
- JSON boundary looked stable enough for a prototype

Rust sketch:

```rust
use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Deserialize)]
struct Workflow {
    name: String,
    jobs: std::collections::BTreeMap<String, Job>,
}

#[derive(Debug, Deserialize)]
struct Job {
    runs_on: String,
    #[serde(default)]
    needs: Vec<String>,
    steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
struct Step {
    name: Option<String>,
    run: Option<String>,
    uses: Option<String>,
}

fn load_workflow(path: &str) -> anyhow::Result<Workflow> {
    let output = Command::new("pkl")
        .args(["eval", "--format", "json", path])
        .output()?;

    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr));
    }

    let workflow = serde_json::from_slice(&output.stdout)?;
    Ok(workflow)
}
```

### Option B: pklrust Typed Evaluation

`pklrust` exposes an evaluator manager, evaluator options, module sources, and typed evaluation into serde structs.

Shape:

```rust
use pklrust::{EvaluatorManager, EvaluatorOptions, ModuleSource};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Workflow {
    name: String,
    jobs: std::collections::BTreeMap<String, Job>,
}

fn load_workflow(path: &std::path::Path) -> Result<Workflow, Box<dyn std::error::Error>> {
    let mut manager = EvaluatorManager::new()?;
    let options = EvaluatorOptions::preconfigured()
        .root_dir(path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_string_lossy());
    let evaluator = manager.new_evaluator(options)?;

    let workflow = manager.evaluate_module_typed::<Workflow>(
        &evaluator,
        ModuleSource::file(path),
    )?;

    manager.close_evaluator(&evaluator)?;
    Ok(workflow)
}
```

This is close to how `ubugeeei-prod/vize` loads `vize.config.pkl`: it searches for local `pkl` binaries, creates a `pklrust::EvaluatorManager`, sets evaluator root directory, evaluates the file into a Rust `RawVizeConfig`, and then applies Rust-side defaults/normalization.

That pattern is a good Velnor reference:

```text
find workflow.pkl
  -> find usable pkl binary
  -> pklrust EvaluatorManager
  -> evaluate_module_typed::<RawWorkflow>
  -> normalize into Workflow
  -> validate execution semantics
```

## Better Long-Term Path

Use `pkl server` and the language binding protocol:

```text
Rust process
  -> spawn pkl server
  -> CreateEvaluator
  -> EvaluateRequest
  -> Pkl binary result
  -> Rust deserializer
```

Advantages:

- no repeated process startup per workflow
- better embedding story
- custom module/resource readers become possible
- closer to official binding architecture
- would enable future Rust-native typed-authoring package manager behavior if
  this research is reopened

This is also the model used by official non-JVM bindings such as Go and Swift.

## Community Crates

### rpkl

`rpkl` evaluates a `.pkl` file and deserializes it into a Rust type. It exposes evaluator options and a `from_config` API.

Potential use:

```rust
let workflow: Workflow = rpkl::from_config("workflow.pkl")?;
```

Risk:

- community crate
- still depends on Pkl evaluator behavior
- must verify maintenance, error quality, version compatibility, and package support

### pklrust

`pklrust` exposes evaluator types, module sources, readers, value decoding, serde conversion, and a macro for inline Pkl.

Potential use:

```rust
let value = pklrust::evaluate_text("name = \"CI\"")?;
```

Risk:

- young ecosystem
- docs coverage is limited
- should be evaluated with real Velnor schemas before depending on it

Observed use:

- `ubugeeei-prod/vize` uses `pklrust` for production-style config loading.
- It supports local binary discovery such as `node_modules/.bin/pkl`, `@pkl-community/pkl`, and fallback `pkl` on `PATH`.
- It treats Pkl process/IO errors differently from schema/deserialization errors, which is exactly the distinction Velnor needs.

### pkl-rust / pklr

There are additional Rust experiments:

- `Sir-NoChill/pkl-rust`
- `jdx/pklr`

`jdx/pklr` describes itself as a pure Rust parser/evaluator. This is interesting long-term, but too early to base Velnor on. Velnor should avoid owning Pkl language semantics unless the project becomes mature enough and compatible with Apple Pkl.

## Pkl GitHub Actions Package As Reference

Pkl already has a typed GitHub Actions workflow package in `pkl-pantry`:

```text
package://pkg.pkl-lang.org/pkl-pantry/com.github.actions@1.7.0#/Workflow.pkl
```

This package is directly relevant to Velnor:

- it models GitHub Actions workflows as Pkl types
- it includes triggers, jobs, steps, env, permissions, concurrency, and contexts
- it validates job IDs and job references
- it provides typed steps for known external actions
- it has a typed action catalog under `actions/`
- it can generate more action types using `com.github.actions.contrib`

Example from the package shape:

```pkl
amends "@com.github.actions/Workflow.pkl"

import "@com.github.actions/catalog.pkl"

jobs {
  ["build"] {
    `runs-on` = "ubuntu-latest"
    steps {
      catalog.`actions/checkout@v6`

      (catalog.`actions/cache/restore@v4`) {
        with {
          key = "my-cache-key"
          pathList { "foo"; "bar" }
        }
      }
    }
  }
}
```

Velnor should use this as the primary reference model rather than designing the workflow schema from scratch.

Suggested approach:

1. Study `com.github.actions.Workflow`.
2. Copy the parts that map directly to Velnor: workflow, triggers, jobs, steps, env, permissions, concurrency, matrix.
3. Extend with Velnor-native primitives: `PathFilter`, `RequiredGate`, `DockerBake`, `Artifact`, `Cache`, `GitHubApi`, `Release`.
4. Keep GitHub Actions naming where it helps migration, but do not preserve YAML limitations.

Important distinction:

```text
pkl-pantry/com.github.actions = typed generator for GitHub Actions YAML
future typed package         = typed source of truth for Velnor execution plan
```

So Velnor should borrow schema and UX ideas, but compile to Velnor's Rust execution plan instead of GitHub YAML.

## Packaging Options

Old Pkl distribution options:

1. Require `pkl` on `PATH`.
2. Download/manage a pinned Pkl binary in `~/.velnor`.
3. Vendor platform-specific Pkl binaries with Velnor releases.

Archived distribution note:

- Old MVP idea: require `pkl` on `PATH`, with clear error message.
- Old early product idea: add `velnor pkl install` or auto-download pinned Pkl.
- Old production idea: pin supported Pkl version range and cache binary per platform.

## Validation Split

Pkl should validate shape-level and type-level config:

- required properties
- valid enum values
- step/action input types
- defaults
- reusable modules

Rust should validate execution semantics:

- DAG cycles
- missing `needs`
- unknown outputs
- invalid matrix references
- runner availability
- cache/artifact lifecycle
- secret permission policy
- plugin version resolution

This keeps Pkl as authoring/compiler layer and Rust as planner/runtime layer.

## Example Velnor Flow

```text
velnor check workflow.pkl
  1. pkl eval --format json workflow.pkl
  2. deserialize JSON into Rust structs
  3. validate DAG and runtime semantics
  4. print normalized plan

velnor run workflow.pkl
  1. same validation
  2. schedule jobs
  3. execute steps on runner
```

## Archived Finding

Pkl is not the active product language. The current implementation scope is
GitHub Actions runner compatibility with existing YAML unchanged.

If this research is ever reopened, old prototype options were:

- `pklrust` typed evaluation, if it passes a quick spike against real workflows, or
- `pkl eval --format json`, if we want the lowest-risk dependency path.

The old conservative advice was to avoid writing a Pkl evaluator in Rust and
treat the Pkl evaluator as a compiler process.

No schema is being built now. No `rpkl`, `pklrust`, `pkl server`, or Pkl CLI
path is in scope now.
