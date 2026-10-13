# Decision: Use Pkl For Workflow Authoring

Status: accepted for typed workflow authoring after Phase 0

## Decision

Velnor will use Pkl as its typed workflow authoring language, but not in Phase 0.

Phase 0 is GitHub runner compatibility: existing repositories keep `.github/workflows/*.yml`, and Velnor registers as a self-hosted GitHub runner replacement.

After that is stable, Pkl will be evaluated into a typed workflow model, then Rust will validate and execute the normalized plan or generate GitHub-compatible YAML.

## Rationale

Pkl is the best fit for Velnor's user-facing workflow language:

- more readable and concise than YAML for complex workflows
- more flexible than KCL for configuration templating
- strong enough typing and validation when used through a strict package
- excellent fit for AI agents when the valid surface area is constrained
- direct prior art exists in `pkl-pantry/com.github.actions`
- typed action catalogs in Pkl map well to Velnor's future plugin/action model
- workflow authoring stays close to GitHub Actions while avoiding YAML's weak typing

KCL remains useful comparison material, but it is not the chosen authoring language.

## Binding Roadmap

### Phase 1: Unofficial Rust Bindings

Prototype with existing unofficial Rust bindings:

- `pklrust`
- `rpkl`

The initial goal is to evaluate `.pkl` workflow files into Rust structs through serde.

Target flow:

```text
workflow.pkl
  -> pklrust/rpkl
  -> RawWorkflow
  -> normalized Workflow
  -> ExecutionPlan
```

### Phase 2: CLI Fallback

Keep a conservative fallback through the official Pkl CLI:

```text
pkl eval --format json workflow.pkl
  -> serde_json
  -> RawWorkflow
```

This gives us a reliable escape hatch if unofficial bindings are unstable.

### Phase 3: Pkl Server Client

If needed, implement a small Rust client for the official `pkl server` message-passing API.

This avoids depending permanently on a third-party binding while still using official Pkl evaluation semantics.

### Phase 4: Native Rust Parser/Evaluator Investigation

Long term, investigate writing or adopting a Rust-native parser/evaluator.

This should only happen after the Velnor workflow model is stable. The first versions should not own Pkl language semantics unless there is a clear product reason.

## Strict Package Requirement

Velnor will not expose arbitrary free-form Pkl as the workflow model.

Users should write:

```pkl
amends "package://velnor.dev/workflow@1.0.0#/Workflow.pkl"
```

The package must provide:

- typed workflow structure
- typed triggers
- typed jobs
- typed step union
- typed permissions
- typed secrets
- typed cache/artifact primitives
- typed Docker primitives
- typed required gates
- typed reusable workflow calls
- validation for job IDs and `needs`
- validation for matrix references and outputs

## AI Agent Safety

Pkl's flexibility is useful, but AI agents need guardrails.

Required guardrails:

- strict examples
- typed catalogs
- clear compile/check command
- errors before execution
- normalized execution plan output
- lints for loose/dynamic structures
- no unknown keys in core workflow objects

## Implication

The Velnor product language is Pkl.

KCL is kept as a benchmark for strict schema design. Velnor's Pkl package should aim for KCL-level discipline while keeping Pkl's better workflow authoring experience.
