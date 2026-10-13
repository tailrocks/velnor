# Review: single hosted Mise bootstrap

Date: 2026-09-20
Scope: isolated integration worktree `/tmp/velnor-ci-integration`, base `17142395609c29de76d7b99b3f661d55aa95edc5`
Verdict: **PASS for this bounded unit**

The change is limited to `render_tool_steps` and its focused tests in the
legacy and S2 scheduled-check renderers, plus generator revisions 57 and 58
and the generator-state input. It does not add the separate auto-install
environment policy change.

## Root cause and behavior

For a hosted check profile with declared tools, the renderer emitted two
`jdx/mise-action` invocations: one with `install_args` and one with
`install: false`. The pinned action enters runtime setup on every invocation, with an existing-
binary check that can avoid another download. Cache lookup also runs when
enabled. The second invocation therefore repeats setup checks, cache lookup
and environment activation; it does not prove a second runtime download.

The candidate emits exactly one action for a hosted profile with tools. It
keeps the existing `install_args`, `cache: true`, and trusted `cache_save`
expression. A hosted profile with no declared tools still emits one
runtime-only action (`install: false`). Velnor profiles retain their existing
preinstalled-Mise shell path. No task-local Mise tool expansion or automatic
install flags are changed here.

## Pinned action verification

The repository pins `jdx/mise-action@c2a87611a18de5b3828c5652fe268e992400cb5c`
(v4.3.0). The exact pinned `action.yml` documents `install` as defaulting to
true and `install_args` as arguments to `mise install`; it documents
`install: false` as disabling install/bootstrap. The exact pinned
`src/index.ts` calls `setupMise` for every invocation (download conditional on
binary absence/version policy), restores the cache when
`cache` is enabled, and calls `miseInstall` when `install` is true. Its
`miseInstall` adds `--locked` when a repository lock file is present. Sources:

- <https://raw.githubusercontent.com/jdx/mise-action/c2a87611a18de5b3828c5652fe268e992400cb5c/action.yml>
- <https://raw.githubusercontent.com/jdx/mise-action/c2a87611a18de5b3828c5652fe268e992400cb5c/src/index.ts>

This confirms that the duplicate action was a real repeated bootstrap/cache
boundary, not merely duplicate YAML labels.

## Independent checks

From `/tmp/velnor-ci-integration`:

- `cargo test -p velnor-workflow primitives::check_profiles::tests --lib`: 58 passed.
- `cargo test -p velnor-workflow s2::primitives::check_profiles::tests --lib`: 26 passed.
- Focused `tools_render_per_lane` tests passed in both renderers.
- `cargo fmt --all -- --check` passed.
- `git diff --check` passed.
- Diff review found only the five intended files: both renderer modules,
  both generator revision constants, and generator state.

The focused test asserts one pinned action for a tool-bearing hosted profile
while retaining Velnor installation assertions. A future follow-up may add an
explicit empty-hosted-profile count assertion, but the renderer branch is
direct and the complete focused suites cover the current behavior.
