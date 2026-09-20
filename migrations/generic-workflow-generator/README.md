# Generic GitHub-default workflow generator migration

Execution records for the fleet migration. Not generator input. Not user documentation.

- `manifest.toml` — targets, deletion candidates, generator revision
- `task-graph.md` — work order
- `capability-matrix.md` — A/B/C/D mapping per discovered responsibility
- `decision-log.md` — verified decisions
- `evidence/` — SHAs, PR URLs, run URLs
- `config/fleet/` — release ledger and desired policy snapshots
- `retirement/` — deletion audit

Verified state only. No optimistic checkboxes.

## Repository migration script

`migrate-repo.sh owner/repo` requires Python 3.11+ (`tomllib`). Its config
converter parses TOML, maps the supported schema-1 provider/runner fields, and
validates the strict schema-2 field tree before atomically replacing the
config. Unknown or malformed input stops with the original config intact.
Comments are removed when a config is rewritten; multiline strings and arrays
retain their parsed values.

Before cloning a target, the script requires a clean generator checkout, an
executable whose `--revision` equals that checkout's full commit SHA, and a
valid `--closure` report. It generates in a staging copy and runs
`--verify-pinned` with `VELNOR_WORKFLOW_PINNED_BINARY` and
`VELNOR_WORKFLOW_PINNED_CLOSURE` set to that exact executable before syncing
the verified result into the migration worktree.

Run the deterministic converter and shell checks with:

```sh
rtk python3 -m unittest discover -s migrations/generic-workflow-generator/tests -p 'test_*.py' -v
rtk bash migrations/generic-workflow-generator/tests/schema2-config-shell.sh
rtk cargo test -p velnor-workflow --test schema2_migration
```
