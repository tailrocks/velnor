# templates/

Normalized per-class workflow templates for the Velnor Actions fleet.

This directory is **canonical**, **headless**, and **generated**: exactly one template
per repository class, and generated files within a class are byte-identical. Do not
hand-edit these templates — change the shared class model or the declared repository
data and regenerate. Every external Action reference is pinned to a full 40-hex commit
SHA; mutable tags or branches are never used.

## The five templates

`<class>/ci.yml` (code, native, tap, apt, fixture) is the consumer workflow every
repository of that class ships as `.github/workflows/ci.yml`. Each template:

- declares three static owner-local reusable-workflow calls — one per owner
  (jackin-project, tailrocks, ChainArgos) — selected only by exact
  `github.repository_owner`; exactly one runs and the other two skip;
- gives each static call an owner-local SHA placeholder and shares one anchored
  `@CALVER@` across all three calls (the only non-executable placeholders), all
  replaced together by the Velnor workflow generator; this binds the same CalVer release while
  allowing the three mirror tags to target their independent Git histories;
- exposes the sole `lane` selector (`github`, `velnor`, `both`) on
  `workflow_dispatch`, and triggers on `pull_request`, `push`, `merge_group`, and
  `workflow_dispatch`; omitted lane defaults to GitHub for `jackin-project` and
  Velnor for `tailrocks` and `ChainArgos`;
- ends with a fail-closed `ci-required` aggregator that uses `if: always()` and a
  positive truth table: it accepts only a recognized owner whose selected call and
  explicit contract output are both `success` while the other two calls are
  `skipped` with empty outputs.

The checked-in `.github/workflows` surface is materialized only by
`velnor-workflow`; use `mise run generate` in the canonical repository and
`velnor-workflow TARGET --adopt --runners both --plain` for a reviewed consumer
migration.
