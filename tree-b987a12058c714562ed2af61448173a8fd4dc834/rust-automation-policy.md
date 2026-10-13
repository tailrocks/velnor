# Rust Automation Policy

Velnor repository automation should be Rust-first.

Rules:

- Do not add new committed shell or Python scripts for repository automation.
- Add new automation as `velnor-tools` subcommands or Rust code in another
  workspace crate.
- Existing shell/Python scripts are migration backlog, not the preferred
  interface.
- When committing repository changes, use:

```sh
cargo run -q -p velnor-tools -- commit -m "<message>" [--push]
```

Current Rust replacements:

- `cargo run -q -p velnor-tools -- check-runner-reference`
- `cargo run -q -p velnor-tools -- fixture-audit`
- `cargo run -q -p velnor-tools -- commit -m "..."`

Remaining migration backlog:

- `scripts/target_audit.py`
- `scripts/target_verify.sh`
- fixture readiness/report/status/smoke scripts
- target smoke and live proof scripts
- shell self-tests for the above

Migration order should prioritize scripts that are part of the normal local
gate first, then live proof scripts.
