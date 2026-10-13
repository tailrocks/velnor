# Contributing documentation

Audience: contributors changing Velnor code, interfaces, tests, packages, or
operational behavior. This is the maintenance contract for the docs linked
from [the index](../index.md).

> Navigation: [← Evidence record](../verification/evidence-record-2026-09-01.md) · [Index](../index.md) · [Next: Start again →](../index.md)

## Choose the document type first

Use one primary user need per document. The current set is pragmatic rather
than a claim that every page is a pure Diátaxis quadrant: architecture/system
pages explain, operator/development pages combine how-to steps with the
reference needed to execute them, and interface/protocol pages are reference.

| Need | Velnor form | Example |
| --- | --- | --- |
| Learn the project | Tutorial/onboarding | [documentation index](../index.md) |
| Understand the system | Explanation | [architecture](../concepts/architecture.md), [system](../concepts/system.md), [execution](execution.md), [security](../operations/security-and-data.md) |
| Complete a task | How-to | [operator guide](operator.md), [development](development.md) |
| Look up a contract | Reference | [interface reference](../reference/interface.md), [runner protocol](../reference/runner-protocol.md) |

This separation follows [Diátaxis](https://diataxis.fr/). Do not turn a
reference table into a tutorial or bury an operational command inside a design
essay.

## Evidence rules

1. Read the implementation and relevant tests before writing a claim.
2. Cite the source as `path:line` near the claim. Link files when useful.
3. Label each claim `CURRENT`, `FUTURE`, `DESIGN-ONLY`, `HISTORICAL`, or
   `OPEN/UNPROVEN` when its status is not obvious from the document title.
4. Separate repository proof from live-host proof. A unit test does not prove
   a deployed GitHub integration, kernel isolation, package publication, or
   production readiness.
5. State limits. Say what the code does not establish, especially for secrets,
   logs, storage confidentiality, cleanup, and failure recovery.
6. Remove or rewrite stale claims. Do not leave two competing descriptions of
   one interface.

The evidence vocabulary and document ownership table are in
[index.md](../index.md). Source dependency law is enforced by
`crates/velnor-client/tests/dependency_boundaries.rs`; runtime proof boundaries
are summarized in [runtime-checking](../verification/runtime-checking.md).

## Procedure and API checklist

Every how-to should include:

- audience and prerequisites;
- exact command or configuration;
- expected result and exit behavior;
- failure meaning and next diagnostic;
- whether it changes local or external state;
- rollback or cleanup, when the action is mutating.

Every API reference should include:

- endpoint/command and authorization boundary;
- request fields, defaults, bounds, and validation;
- response shape and version negotiation;
- errors, exit codes, retry/deadline semantics, and idempotency;
- persistence and cursor behavior;
- one minimal example.

The local API reference in [interface](../reference/interface.md) is the
model for this format.

## Change workflow

Keep docs in the same change as the code. Before submitting:

```sh
mise run fmt
mise run test
git diff --check
```

Then review rendered Markdown, verify every internal link, and check examples
against the current parser/source. Do not use live credentials or mutate a
fleet merely to make a documentation example look successful.

Use plain international English: define abbreviations at first use, prefer
short sentences and imperative instructions, avoid idioms, and write for the
named audience. This follows [Google's audience guidance](https://developers.google.com/tech-writing/one/audience)
and its [documentation best practices](https://google.github.io/styleguide/docguide/best_practices.html).
