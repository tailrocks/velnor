# Scheduled tool boundary review

Verdict: **HOLD pending a typed task-tool closure or an explicit generated
installation step**.

The check-profile patch correctly centralizes four Mise auto-install controls
in both legacy and S2 renderers, rejects profile overrides, and makes hosted
profiles install only `profile.tools`. The pinned `jdx/mise-action` contract
accepts `install: false` and explicit `install_args`; generated output has one
Mise action for a hosted profile. This removes hidden root-manifest setup from
empty profiles.

The boundary is incomplete. `CheckProfileSpec.tools` is copied directly from
`[[check_profile]].tools` in `apply_check_profiles`; there is no resolver that
expands the selected task graph or task-local tools. `apply_check_profiles`
only parses task names and verifies that `mise.toml` declares them. Mise's
current task contract says `[tasks.<name>].tools` installs and activates tools
for that task, and `MISE_TASK_RUN_AUTO_INSTALL=false` disables that automatic
installation. A profile that selects a task with `tools.ripgrep = ...` but
omits it from `check_profile.tools` therefore reaches `mise run` with a missing
tool and fails. A profile that lists only a parent task also needs the closure
of its `depends` tasks and their task-local tools.

This is a correctness gap exposed by the architectural change, not a reason to
re-enable hidden installation. The generator should either statically resolve
selected Mise task/dependency metadata into a typed tool closure and install
that exact lock-compatible set, or require a typed `task_tools`/closure field
and validate it against the task graph and lock. `mise install
--include-task-tools` is a documented fallback but installs every task tool in
the scope, so it violates the stage-minimality goal unless the profile declares
that broad scope. Add a fixture with a task-local tool and a dependency-local
tool; prove missing closure fails generation or the generated job installs it.

Secondary finding: the Velnor profile path emits `mise --yes install` without
`--locked`, while `profile.tools` are validated against `mise.lock`. Hosted
`jdx/mise-action` adds locked installation when a lock exists at its pinned
version, but the local path should carry the same lock requirement or document
why its runner image provides the exact immutable tools. This is an existing
reproducibility weakness in the shared renderer, separate from the new env
policy.

Primary references verified against installed Mise 2026.9.11:

- [Mise task `tools` and `--include-task-tools`](https://mise.jdx.dev/tasks/task-configuration.html#tools)
- [Mise `task.run_auto_install`](https://mise.jdx.dev/tasks/task-configuration.html#task-run-auto-install)
- [Mise settings auto-install controls](https://mise.jdx.dev/configuration/settings.html)
- [Pinned mise-action inputs](https://github.com/jdx/mise-action/blob/c2a87611a18de5b3828c5652fe268e992400cb5c/action.yml)
