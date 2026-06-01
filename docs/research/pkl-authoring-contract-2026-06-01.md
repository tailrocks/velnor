# Pkl Authoring Contract

Date: 2026-06-01

This document defines the future typed authoring layer for Velnor. It is not
Phase 0 runtime work. Phase 0 remains GitHub self-hosted runner compatibility
with existing `.github/workflows/*.yml`.

The Pkl layer must stay close enough to GitHub Actions that migration feels
natural, but strict enough that humans and AI agents get type errors before a
pipeline starts.

## Upstream Reference

Primary reference: `pkl-pantry/com.github.actions`.

The upstream package is useful because it already models GitHub Actions concepts
in Pkl:

- workflow templates
- triggers
- jobs
- steps
- env and permissions
- containers
- typed action catalog entries
- generated typed action definitions through `com.github.actions.contrib`

Important design lesson: upstream Pkl intentionally keeps GitHub Actions
familiar while adding tooling, documentation, and typechecking. Velnor should
reuse that mental model, but not copy every GitHub/YAML escape hatch into the
product surface.

## Product Rule

Velnor Pkl compiles to the same normalized plan as GitHub runner jobs:

```text
GitHub YAML -> GitHub scheduler -> AgentJobRequestMessage -> GitHub adapter
Pkl workflow -> Pkl evaluator    -> TypedWorkflow          -> Pkl adapter

GitHub adapter \
                -> NormalizedJobPlan -> Docker executor -> reporter
Pkl adapter    /
```

There must not be a separate Pkl executor.

## Compatibility Rule

Pkl should preserve GitHub Actions vocabulary where the concept is good:

- `workflow`
- `on`
- `permissions`
- `concurrency`
- `jobs`
- `runsOn`
- `needs`
- `strategy.matrix`
- `env`
- `defaults.run`
- `steps`
- `run`
- `uses`
- `with`
- `if`
- `outputs`
- artifacts
- cache
- services
- containers

Pkl should improve the weak parts:

- replace untyped action inputs with typed helper classes
- replace multiline YAML strings for structured inputs with typed lists/maps
- replace required-check boilerplate with a `RequiredGate`
- replace repeated monorepo package/path definitions with typed package data
- make raw expressions explicit through an `Expression` wrapper
- make raw `uses:` explicit through `Use`, while common actions use typed
  wrappers

## Strict Package Shape

Users should write:

```pkl
amends "package://velnor.dev/workflow@1.0.0#/Workflow.pkl"
```

Core modules:

```text
Workflow.pkl
Trigger.pkl
Job.pkl
Step.pkl
Expression.pkl
Runner.pkl
Permissions.pkl
Matrix.pkl
Docker.pkl
Artifact.pkl
Cache.pkl
Pages.pkl
RequiredGate.pkl
Catalog.pkl
```

Package invariants:

- unknown fields fail evaluation or Rust-side validation
- job ids match GitHub-compatible stable ids
- every `needs` entry points at an existing job
- every job output references an existing step output or typed helper output
- every step id is unique inside a job when it is referenced
- `runsOn` must be a typed runner label set, not a free-form accidental string
- `permissions` uses an enum of known GitHub permissions and values
- raw expressions are allowed only through `Expression`
- raw action references are allowed only through `Use`
- typed helpers own structured inputs and render GitHub-compatible `with` only
  at compile time

## Core Types

Sketch:

```pkl
class Workflow {
  name: String(!isEmpty)
  on: Triggers
  permissions: Permissions = new { contents = "read" }
  env: Mapping<String(!isEmpty), String> = new {}
  jobs: Mapping<JobId, Job>
}

typealias JobId = String(matches(Regex("[A-Za-z_][A-Za-z0-9_-]*")))

abstract class Job {
  name: String? = null
  runsOn: Runner
  needs: Listing<JobId> = new {}
  if: Expression? = null
  permissions: Permissions? = null
  env: Mapping<String(!isEmpty), String> = new {}
  defaults: RunDefaults = new {}
  steps: Listing<Step>
  outputs: Mapping<String(!isEmpty), Expression> = new {}
}

abstract class Step {
  name: String? = null
  id: String? = null
  if: Expression? = null
  env: Mapping<String(!isEmpty), String> = new {}
  continueOnError: Boolean = false
}

class Run extends Step {
  command: String(length > 0 && length < 21_000)
  shell: Shell? = null
  workingDirectory: String? = null
}

class Use extends Step {
  action: ActionFamily
  ref: String? = null
  with: Mapping<String(!isEmpty), String|Number|Boolean> = new {}
}

class Expression {
  text: String(!isEmpty)
}
```

`Use.ref` is retained for GitHub YAML generation and optional lockfile output.
For Velnor-native execution of supported action families, implementation
selection still ignores refs/SHAs and uses the internal Rust adapter for the
action family.

## Typed Helper Surface

Initial helpers should match the target repositories first:

```pkl
class Checkout extends Step {
  repository: String? = null
  ref: String|Expression? = null
  path: String? = null
  token: String|Expression? = null
  fetchDepth: UInt? = 1
  fetchTags: Boolean = false
  persistCredentials: Boolean = true
  clean: Boolean = true
}

class Cache extends Step {
  paths: Listing<String(!isEmpty)>
  key: String|Expression
  restoreKeys: Listing<String> = new {}
  failOnCacheMiss: Boolean = false
  lookupOnly: Boolean = false
}

class UploadArtifact extends Step {
  artifactName: String(!isEmpty)
  paths: Listing<String(!isEmpty)>
  ifNoFilesFound: "warn"|"error"|"ignore" = "warn"
  overwrite: Boolean = false
  includeHiddenFiles: Boolean = false
}

class DownloadArtifact extends Step {
  artifactName: String? = null
  path: String? = null
  pattern: String? = null
  mergeMultiple: Boolean = false
}

class DockerBake extends Step {
  files: Listing<String(!isEmpty)> = new {}
  targets: Listing<String(!isEmpty)> = new {}
  push: Boolean|Expression = false
  load: Boolean = false
  set: Listing<String> = new {}
}

class RequiredGate extends Job {
  failOn: Listing<NeedResult> = new { "failure"; "cancelled" }
  allow: Listing<NeedResult> = new { "success"; "skipped" }
}
```

These helpers lower to `ExecutableStep::Native` or generated run steps. They do
not execute marketplace JavaScript.

## Target Workflow Example

This is a Pkl-style version of the `java-monorepo` path-filter/test pattern.

```pkl
class RustPackage {
  id: String(matches(Regex("[a-z][a-z0-9-]*")))
  crate: String(!isEmpty)
  paths: Listing<String(!isEmpty)>
}

packages = new Listing<RustPackage> {
  new {
    id = "bitcoin-processor"
    crate = "bitcoin-processor-app"
    paths {
      "backend-rust/bitcoin-processor-app/**"
      "backend-rust/bitcoin-migration/**"
      "backend/bitcoin-model/**"
      "backend/tailrocks-type/**"
    }
  }
  new {
    id = "eth-processor"
    crate = "eth-processor-app"
    paths {
      "backend-rust/eth-processor-app/**"
      "backend-rust/eth-migration/**"
      "backend/eth-model/**"
      "backend/tailrocks-type/**"
    }
  }
}

commonRustPaths = new Listing<String> { "Cargo.toml"; "Cargo.lock" }

jobs {
  ["changes"] = PathFilterJob {
    runsOn = Runner.selfHosted("hetzner-sentry-ci")
    filters = packages.toMap((pkg) -> pkg.id, (pkg) -> pkg.paths + commonRustPaths)
  }

  for (pkg in packages) {
    ["test-\(pkg.id)"] = RustNextestJob {
      runsOn = Runner.selfHosted("hetzner-sentry-ci")
      needs { "changes" }
      if = Expression("needs.changes.outputs['\(pkg.id)'] == 'true'")
      crate = pkg.crate
    }
  }

  ["rust-required"] = RequiredGate {
    runsOn = Runner.selfHosted("hetzner-sentry-ci")
    needs {
      "check"
      for (pkg in packages) { "test-\(pkg.id)" }
    }
  }
}
```

Lowering should produce GitHub-like jobs:

- one path-filter job using native `dorny/paths-filter` behavior
- one Rust test job per package
- one required gate equivalent to the current target YAML boilerplate

## Compiler Outputs

The first Pkl compiler should support two outputs:

1. GitHub YAML:
   - useful for gradual migration
   - keeps GitHub scheduler/UI as the source of truth
   - can use Pkl lockfile behavior for action SHA pinning
2. Velnor plan JSON:
   - useful for future non-GitHub execution
   - must deserialize into the same Rust normalized plan model

CLI shape:

```text
velnor workflow check workflow.pkl
velnor workflow render-github workflow.pkl --out .github/workflows/ci.yml
velnor workflow plan workflow.pkl --out target/velnor-plan.json
```

## Rust Integration Rule

Start with the official Pkl evaluator boundary:

```text
pkl eval --format json workflow.pkl
  -> serde_json::from_slice::<RawWorkflow>
  -> Rust semantic validation
  -> TypedWorkflow
  -> GitHub YAML or NormalizedJobPlan
```

`pklrust` or `rpkl` can replace the CLI bridge after the schema is stable, but
Rust must still perform semantic validation that depends on the whole workflow
graph, such as `needs` references and output references.

## AI Agent Rules

The package should be optimized for generated edits:

- prefer helpers such as `RustNextestJob`, `DockerBake`, `Cache`, and
  `RequiredGate`
- keep raw `Run` and `Use` available but visually obvious
- require `velnor workflow check` before rendering/executing
- print errors with object paths such as `jobs["test-api"].steps[3].with.key`
- include examples for every target pattern
- keep one canonical way to express common patterns

This is the main advantage over raw YAML: AI agents can generate smaller,
typed, repeatable workflow definitions and get structural errors before the
runner starts.
