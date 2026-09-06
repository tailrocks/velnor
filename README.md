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
- **Mise-only tools.** Languages and executables are installed through the
  locked `mise.toml`/`mise.lock` graph. Do not add Homebrew, rustup, `cargo
  install`, or another package manager as an installation path.
- **Full-SHA pins.** Every external Action reference resolves to an immutable full
  40-hex commit SHA. Mutable refs (tags or branches) are never used.

## Layout

- `fleet/` — declared data: `repositories.toml` (the exhaustive 28-member map to
  five classes), `classes.toml` (the five class contracts), `caches.toml`
  (trusted class cache IDs, paths, lock inputs, phases, and compatible restore
  prefixes), and `block-sha` (the
  immutable commit that pins the internal composite-action closure used by the
  callable workflows — not the consumer release pin).
- `actions/` — reusable composite building blocks: `run-gate` (runs one named gate
  command identically on either lane), `aggregate` (emits the lane contract), and
  `cache-contract` (fails closed on missing cache authority, quota, attribution,
  cleanup, or materialization evidence).
- `.github/workflows/ci-<class>.yml` — the five owner-local callable (`workflow_call`)
  workflows, one per class. Generated; each pins its composite closure to `block-sha`.
- `templates/<class>/ci.yml` — the five normalized consumer templates, one per class,
  byte-identical within a class. Each has three owner-local reusable-workflow calls
  (jackin-project / tailrocks / ChainArgos) selected by `github.repository_owner`, a
  owner-local `@<sha> # <CalVer>` release pins, and a fail-closed `ci-required`
  aggregator.
- `crates/velnor-actions-generator/` — the legacy fleet model and audit library.
  It no longer exposes a workflow-writing CLI; Velnor owns every workflow output.

## Generator CLI

- `mise run generate` — invoke the Velnor workflow generator. The task uses the
  sibling `../velnor` checkout by default. In CI or another standalone checkout,
  it runs the pinned Velnor revision; set `VELNOR_WORKFLOW_SOURCE_DIR` to use a
  different local checkout.
- `tool-registry --root . --fleet fleet/repositories.toml` — validate the
  generator-owned tool registry, root mise graph, lockfile, and 28-repository
  manifest shape.
- The legacy `render-consumer` APIs remain only for compatibility tests. They are
  not workflow-generation entry points; all workflow files are produced by
  `velnor-workflow`.
- `audit --root .` — the full fleet audit (prints
  `fleet valid: 28 repositories, 5 classes, 5 templates`).

## Gates

Every check runs through repository-owned, locked mise tasks. Reproduce CI locally
with:

```bash
mise install --locked
mise run generate       # Velnor CLI; should be a no-op on a clean tree
mise run generator-check # Velnor CLI ownership and byte check
mise run ci
mise run ci:contract    # registry + fleet contract gate
bash tools/check-tool-registry.sh --fixtures tests/fixtures/tools/
```

`mise run ci` runs `fmt`, `lint` (clippy `-D warnings`), `test` (cargo-nextest),
`actionlint`, `deny` (advisory audit), `generator-check` (the fleet audit),
and the tool-registry contract. The registry lives at
`fleet/fleet-tools.toml`; `templates/tools/mise.toml` is its deterministic
full-policy projection. Consumer rendering normalizes only the tools already
used by that consumer, preserving the other `mise.toml` sections.
