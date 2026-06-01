# Pkl vs KCL For Velnor

Status: historical brainstorming. The current implementation scope is GitHub
Actions runner compatibility with existing YAML unchanged. Do not implement
Pkl, KCL, or any Velnor-native workflow language before live target repository
proof.

This comparison focuses only on language suitability for AI-authored CI/CD workflows.

Constraint:

- ignore Rust integration
- ignore popularity
- prioritize strong typing, readability, concision, and AI-agent safety

## Short Answer

KCL is stronger if we judge only by schema-first language design.

Pkl is stronger if we judge by workflow authoring experience, readability, and existing GitHub Actions prior art.

For Velnor, Pkl is still the better product language, but only if Velnor ships a strict Pkl schema package and keeps workflow primitives typed. Without that strict package, KCL would be safer for AI agents by default.

## Sources

- Pkl user manual: https://pkl-lang.org/main/current/index.html
- Pkl introduction and validation: https://pkl-lang.org/blog/introducing-pkl.html
- Pkl GitHub Actions package: https://pkl-lang.org/package-docs/pkg.pkl-lang.org/pkl-pantry/com.github.actions/1.7.0/Workflow/index.html
- KCL website: https://www.kcl-lang.io/
- KCL repository: https://github.com/kcl-lang/kcl
- KCL schema definition guide: https://peefy.github.io/docs/user_docs/guides/schema-definition/

## Type System And Validation

### Pkl

Pkl supports type annotations and constraints. Pkl's docs position it as an embeddable configuration language with rich templating and validation. Pkl constraints can be attached to type annotations, for example constrained strings, integers, regex matches, and arbitrary predicate-like checks.

Pkl is strong enough to model workflows safely:

```pkl
class RunStep {
  name: String?
  command: String(!isEmpty)
  shell: "bash"|"sh"|"pwsh" = "bash"
}

class Job {
  `runs-on`: String(!isEmpty)
  needs: Listing<String> = new {}
  steps: Listing<RunStep>(!isEmpty)
}

jobs: Mapping<String(matches(Regex("[a-zA-Z_][a-zA-Z0-9_-]*"))), Job>(!isEmpty)
```

Strength:

- good type annotations
- good constraints
- excellent templating and object amendment model
- existing typed GitHub Actions package proves workflows can be modeled

Weakness:

- the language feels more flexible than KCL
- strictness depends heavily on the schema package we provide
- without strict templates, users and agents can drift into loose object shapes

### KCL

KCL is schema-centric. Its own positioning is configuration and policy with schemas, constraints, rules, defaults, and validation. The `schema` keyword is the central modeling primitive.

KCL naturally pushes users toward explicit structures:

```python
schema RunStep:
    name?: str
    command: str
    shell: "bash" | "sh" | "pwsh" = "bash"

    check:
        command != ""

schema Job:
    runs_on: str
    needs?: [str] = []
    steps: [RunStep]

    check:
        runs_on != ""
        len(steps) > 0

schema Workflow:
    name: str
    jobs: {str: Job}

    check:
        len(jobs) > 0
```

Strength:

- schema-first
- constraints/rules feel central, not optional
- better raw-language fit for "AI must not invent wrong structure"
- fewer ways to accidentally write loose dynamic config

Weakness:

- less ergonomic for GitHub Actions-like workflow authorship
- less existing CI/CD prior art than Pkl's GitHub Actions package
- syntax is readable, but feels more like infra schema/policy than workflow authoring

## Concision

For simple workflow declarations, Pkl is usually more concise and closer to GitHub Actions:

```pkl
jobs {
  ["test"] {
    `runs-on` = "ubuntu-latest"
    steps {
      new { uses = "actions/checkout@v6" }
      new { run = "cargo test" }
    }
  }
}
```

Equivalent KCL tends to be explicit:

```python
jobs = {
    test = Job {
        runs_on = "ubuntu-latest"
        steps = [
            UseStep { uses = "actions/checkout@v6" }
            RunStep { command = "cargo test" }
        ]
    }
}
```

Verdict:

- Pkl wins for concise workflow authoring.
- KCL wins for explicit data modeling.

## Readability

Pkl reads like configuration with a schema behind it. The amendment model is good for "base workflow plus local changes", which is common in CI/CD.

KCL reads like typed infrastructure configuration. It is very understandable, but less close to GitHub Actions. It may be easier for agents to reason about schema definitions, but less pleasant for humans writing workflows.

Verdict:

- Pkl is more readable for workflow users.
- KCL is more readable for schema authors.

## AI-Agent Safety

AI agents benefit from:

- small valid surface area
- strong schemas
- good error messages
- deterministic normalized output
- typed reusable building blocks
- clear examples
- names that mirror existing ecosystem concepts

KCL provides stricter defaults because schemas are central.

Pkl can reach similar safety if Velnor provides:

- a strict `velnor.workflow` package
- sealed workflow classes where possible
- constrained unions for step kinds
- typed action catalog
- clear compile step from Pkl to normalized plan
- lints that reject unknown keys and loose `Dynamic`
- examples copied from real GitHub Actions migration patterns

Important point:

```text
Raw language safety: KCL > Pkl
Product safety with good Velnor schema: Pkl ~= KCL
Human workflow UX: Pkl > KCL
```

## Existing GitHub Actions Reference

Pkl has a major advantage: `pkl-pantry/com.github.actions`.

That package already models:

- workflows
- triggers
- jobs
- steps
- environment variables
- permissions
- concurrency
- GitHub contexts
- typed external actions
- typed action catalog

This matters because Velnor wants to feel close to GitHub Actions. The Pkl ecosystem already has a tested vocabulary for GitHub Actions-like workflow structure.

KCL does not appear to have an equivalent GitHub Actions typed package with the same relevance.

Verdict:

- Pkl wins strongly for Velnor-specific prior art.

## Example: Monorepo App Tests

Pkl:

```pkl
class App {
  name: String
  package: String
  paths: Listing<String>
  tools: Listing<String> = new { "rust"; "cargo:cargo-nextest" }
}

apps = new Listing<App> {
  new {
    name = "bitcoin-processor"
    package = "bitcoin-processor-app"
    paths {
      "backend-rust/bitcoin-processor-app/**"
      "backend/bitcoin-model/**"
    }
  }
}

jobs {
  ["changes"] = PathFilterJob { filters = appFilters(apps) }

  for (app in apps) {
    ["test-\(app.name)"] = RustNextestJob {
      package = app.package
      needs { "changes" }
      when = changedOrSelected(app)
    }
  }
}
```

KCL:

```python
schema App:
    name: str
    package: str
    paths: [str]
    tools?: [str] = ["rust", "cargo:cargo-nextest"]

apps = [
    App {
        name = "bitcoin-processor"
        package = "bitcoin-processor-app"
        paths = [
            "backend-rust/bitcoin-processor-app/**"
            "backend/bitcoin-model/**"
        ]
    }
]

jobs = {
    changes = PathFilterJob { filters = app_filters(apps) }
} | {
    "test-${app.name}" = RustNextestJob {
        package = app.package
        needs = ["changes"]
        when = changed_or_selected(app)
    } for app in apps
}
```

Pkl version feels closer to workflow authoring. KCL version feels clearer around schema/data contracts.

## Decision Matrix

| Criterion | Winner | Reason |
| --- | --- | --- |
| Strong typing by default | KCL | Schema-first design makes strict modeling central |
| Constraint/policy feel | KCL | Rules/checks are core to the language identity |
| Concision for workflows | Pkl | Less ceremony, better templating feel |
| Readability for workflow authors | Pkl | Closer to "typed GitHub Actions" |
| Readability for schema authors | KCL | Schemas are explicit and direct |
| AI-agent guardrails without extra framework | KCL | Harder to drift into loose shapes |
| AI-agent guardrails with Velnor schema package | Tie/slight Pkl | Pkl can be made strict and has better workflow vocabulary |
| GitHub Actions migration prior art | Pkl | `pkl-pantry/com.github.actions` already exists |
| Product adoption UX | Pkl | Looks friendlier and more modern to users |

## Historical Recommendation

This recommendation is deferred. Do not implement a typed DSL package now.

If typed authoring is revisited after live target GitHub Actions compatibility,
Pkl was the preferred product-language candidate in this comparison, but only
with a strict typed DSL package.

Do not expose "free-form Pkl" as the workflow model. Expose:

```pkl
amends "package://velnor.dev/workflow@1.0.0#/Workflow.pkl"
```

Then make `Workflow.pkl` strict:

- constrained job IDs
- typed triggers
- typed runner labels
- typed step union
- typed reusable workflow calls
- typed permissions
- typed secrets
- typed caches/artifacts
- typed Docker primitives
- validation for `needs`
- validation for required gates
- no unknown dynamic fields in core structures

Historical call:

```text
Best pure type-safety language: KCL
Best Velnor product language:   Pkl
```

Pkl gives better human and migration UX. KCL gives stronger raw schema posture. For AI agents, Pkl is acceptable only if Velnor's package is strict and examples/lints are first-class.
