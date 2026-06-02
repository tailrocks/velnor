# Archived Brainstorming: Pkl For Workflow Authoring

Status: archived brainstorming only; no current implementation

## Current Decision

Velnor will not implement Pkl support now. Pkl, PQL, KCL, and any
Velnor-native workflow language are not required, not selected, and not part of
the current implementation plan.

Phase 0 is GitHub runner compatibility: existing repositories keep `.github/workflows/*.yml`, and Velnor registers as a self-hosted GitHub runner replacement.

The current product target is one-to-one GitHub Actions runner compatibility for the two target repositories. GitHub continues to own YAML parsing, workflow scheduling, matrices, reusable workflows, and the Actions UI.

Pkl remains only a future authoring idea from earlier research. There is no Pkl CLI command, no Pkl package, no Pkl adapter, and no Pkl runtime support in the current runner.

## Rationale

If typed workflow authoring is revisited after target GitHub Actions compatibility is proven, Pkl was previously considered attractive because:

- more readable and concise than YAML for complex workflows
- more flexible than KCL for configuration templating
- strong enough typing and validation when used through a strict package
- excellent fit for AI agents when the valid surface area is constrained
- direct prior art exists in `pkl-pantry/com.github.actions`
- typed action catalogs in Pkl map well to Velnor's future plugin/action model
- workflow authoring stays close to GitHub Actions while avoiding YAML's weak typing

KCL remains useful comparison material. Neither KCL nor Pkl is part of the current implementation scope.

## Historical Binding Notes

Do not treat this as a roadmap. These are old research notes only.

### Future Option 1: Unofficial Rust Bindings

Prototype with existing unofficial Rust bindings:

- `pklrust`
- `rpkl`

One old idea was to evaluate `.pkl` workflow files into Rust structs through serde.

Target flow:

```text
workflow.pkl
  -> pklrust/rpkl
  -> RawWorkflow
  -> normalized Workflow
  -> ExecutionPlan
```

### Future Option 2: CLI Fallback

Keep a conservative fallback through the official Pkl CLI:

```text
pkl eval --format json workflow.pkl
  -> serde_json
  -> RawWorkflow
```

This gives us a reliable escape hatch if unofficial bindings are unstable.

### Future Option 3: Pkl Server Client

If needed, implement a small Rust client for the official `pkl server` message-passing API.

This avoids depending permanently on a third-party binding while still using official Pkl evaluation semantics.

### Future Option 4: Native Rust Parser/Evaluator Investigation

Long term, investigate writing or adopting a Rust-native parser/evaluator.

This should only happen after the Velnor workflow model is stable. The first versions should not own Pkl language semantics unless there is a clear product reason.

## Future Strict Package Requirement

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

## Future AI Agent Safety

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

No product language is active now.

The current implementation must keep moving toward GitHub Actions compatibility
for `jackin-project/jackin` and `ChainArgos/java-monorepo`, using their existing
YAML unchanged.
