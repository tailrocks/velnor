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

- `actions/` — reusable building blocks (real content lands in plans 005/006).
- `templates/` — one normalized workflow template per repository class (real
  content lands in plans 005/006).
- `crates/velnor-actions-generator/` — the Rust generator seam and its skeleton
  self-check.

## Gates

Every check runs through repository-owned, locked mise tasks. Reproduce CI locally
with:

```bash
mise install --locked
mise run ci
```

`mise run ci` runs `fmt`, `lint` (clippy `-D warnings`), `test` (cargo-nextest),
`actionlint`, `deny` (advisory audit), and `generator-check`.
