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
- `cargo run -q -p velnor-tools -- fixture-readiness`
- `cargo run -q -p velnor-tools -- fixture-report`
- `cargo run -q -p velnor-tools -- fixture-smoke-plan`
- `cargo run -q -p velnor-tools -- fixture-status`
- `cargo run -q -p velnor-tools -- live-host-doctor-plan`
- `cargo run -q -p velnor-tools -- target-audit`
- `cargo run -q -p velnor-tools -- target-smoke-plan`
- `cargo run -q -p velnor-tools -- target-verify`
- `cargo run -q -p velnor-tools -- commit -m "..."`

Remaining migration backlog:

- fixture smoke scripts
- target smoke and live proof scripts
- remaining shell self-tests for the above
- `live_sequence_common.sh` runtime helper remains Bash until live smoke/proof
  orchestration moves fully into Rust; its validation contract is now covered
  by `velnor-tools` Rust tests. Fixture smoke, host doctor, and target smoke
  planning/default validation are covered by `fixture-smoke-plan`,
  `live-host-doctor-plan`, and `target-smoke-plan`.

Migration order should prioritize scripts that are part of the normal local
gate first, then live proof scripts.
