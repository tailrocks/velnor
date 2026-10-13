# velnor-actions

Canonical source of the Velnor Actions fleet.

## Policy

- **Canonical.** `tailrocks/velnor-actions` is the single public canonical source.
  `jackin-project/velnor-actions` and `ChainArgos/velnor-actions` are generated,
  byte-identical mirrors — never edit them by hand.
- **Headless.** The delivered surface is repository files, GitHub Actions
  checks/logs, and CLI output only. There is no service or UI.
- **No hand edits.** Workflows and templates are generated from the shared class
  model and declared repository data. Per-repository workflow forks are not a
  baseline; change the class model or the declared data instead.
- **Full-SHA pins.** Every external Action reference resolves to an immutable full
  40-hex commit SHA. Mutable refs (tags or branches) are never used.

## Layout

- `fleet/` — declared data: `repositories.toml` (the exhaustive 24-member map to
  five classes), `classes.toml` (the five class contracts), and `block-sha` (the
  immutable commit that pins the internal composite-action closure used by the
  callable workflows — not the consumer release pin).
- `actions/` — reusable composite building blocks: `run-gate` (runs one named gate
  command identically on either lane) and `aggregate` (emits the lane contract).
- `.github/workflows/ci-<class>.yml` — the five owner-local callable (`workflow_call`)
  workflows, one per class. Generated; each pins its composite closure to `block-sha`.
- `templates/<class>/ci.yml` — the five normalized consumer templates, one per class,
  byte-identical within a class. Each has three owner-local reusable-workflow calls
  (jackin-project / tailrocks / ChainArgos) selected by `github.repository_owner`, a
  shared `@<sha> # <CalVer>` release pin, and a fail-closed `ci-required` aggregator.
- `crates/velnor-actions-generator/` — the Rust generator: `model` (data + validation),
  `render` (deterministic rendering), `audit` (regeneration, byte, closure, and
  fail-closed aggregation checks), and the CLI.

## Generator CLI

- `generate --root .` — render the five templates (and, once `block-sha` is bound,
  the five callable workflows).
- `render-consumer --root . --repository OWNER/REPO --release-sha <40-hex> --calver
  <CalVer> --output DIR` — materialize one consumer's `DIR/.github/workflows/ci.yml`,
  replacing `@FLEET_SHA@`/`@CALVER@` atomically.
- `audit --root .` — the full fleet audit (prints
  `fleet valid: 24 repositories, 5 classes, 5 templates`).

## Gates

Every check runs through repository-owned, locked mise tasks. Reproduce CI locally
with:

```bash
mise install --locked
mise run generate       # re-render from data (should be a no-op on a clean tree)
mise run ci
```

`mise run ci` runs `fmt`, `lint` (clippy `-D warnings`), `test` (cargo-nextest),
`actionlint`, `deny` (advisory audit), and `generator-check` (the fleet audit).
