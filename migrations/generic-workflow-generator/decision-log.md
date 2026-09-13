# Decision log

Verified decisions only.

- 2026-09-13: GitHub is the automatic and omitted-dispatch default. Velnor jobs exist only when `runners` is `velnor` or `both`, and run only on trusted default-branch `workflow_dispatch` with `runner=velnor|both`.
- 2026-09-13: Required check display name is `Required` under workflow `CI` (`CI / Required`). Job id stays `ci-required`.
- 2026-09-13: `pull_request` and `merge_group` both publish the required check. No workflow-level path filters.
- 2026-09-13: `--adopt` removed. Foreign workflow bodies are never imported. `--force` replaces unowned workflow files with generated output.
- 2026-09-13: `[workflow] templates` and `pull_request_on_velnor = true` fail closed.
- 2026-09-13: Generation-config command arrays fail closed. Runtime command lists are materialized from typed capabilities (scan + `workspace_check` + `ci_tasks` + docker seed cache).
- 2026-09-13: Rust clippy/test no longer pass `--all-features` by default.
- 2026-09-13: `ci_tasks` names must exist in `mise.toml`. Not a shell-command array.
- 2026-09-13: static-workflow still present for this repo's `release.yml` until a generic native-release capability replaces it. That remains an open removal.
