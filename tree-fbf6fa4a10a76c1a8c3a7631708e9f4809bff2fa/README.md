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

- `fleet/` — declared data: `repositories.toml` (the exhaustive 28-member map to
  four classes), `classes.toml` (the four class contracts), `caches.toml`
  (trusted class cache IDs, paths, lock inputs, phases, and compatible restore
  prefixes), and `block-sha` (the
  immutable commit that pins the internal composite-action closure used by the
  callable workflows — not the consumer release pin).
- `actions/` — reusable composite building blocks: `run-gate` (runs one named gate
  command identically on either lane), `aggregate` (emits the lane contract), and
  `cache-contract` (fails closed on missing cache authority, quota, attribution,
  cleanup, or materialization evidence).
- `.github/workflows/ci-<class>.yml` — the four owner-local callable (`workflow_call`)
  workflows, one per class. Generated; each pins its composite closure to `block-sha`.
- `templates/<class>/ci.yml` — the four normalized consumer templates, one per class,
  byte-identical within a class. Each has three owner-local reusable-workflow calls
  (jackin-project / tailrocks / ChainArgos) selected by `github.repository_owner`, a
  owner-local `@<sha> # <CalVer>` release pins, and a fail-closed `ci-required`
  aggregator.
- `crates/velnor-actions-generator/` — the Rust generator: `model` (data + validation),
  `render` (deterministic rendering), `cache` (trusted cache declaration/key
  validation), `audit` (regeneration, byte, closure, and fail-closed aggregation
  checks), and the CLI.

## Generator CLI

- `generate --root .` — render the four templates (and, once `block-sha` is bound,
  the four callable workflows).
- `render-consumer --root . --repository OWNER/REPO --jackin-release-sha <40-hex>
  --tailrocks-release-sha <40-hex> --chainargos-release-sha <40-hex> --calver
  <CalVer> --output DIR` — materialize one consumer's
  `DIR/.github/workflows/ci.yml`, atomically replacing the three owner-local SHA
  placeholders and their shared CalVer. Each SHA is the target of that owner's
  immutable mirror tag; mirror histories need not share commit identities.
- `audit --root .` — the full fleet audit (prints
  `fleet valid: 28 repositories, 4 classes, 4 templates`).

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
