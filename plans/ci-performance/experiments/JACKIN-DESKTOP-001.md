# JACKIN-DESKTOP-001: bound the desktop stage tool closure

Status: diagnosis plus local check-profile boundary validation. The Jackin
source now removes one redundant reserved Mise env entry; a dirty local
candidate renderer regenerated temporary output successfully. No generated
consumer file was written, no tool was installed, and no CI timing or speed
claim is made.

## Question

The Jackin desktop merge job took 2,635 s. The stage must keep the binding
drift gate, Rust and Swift tests, app build and verification, all native UI
checks, and scheduled dead-code coverage. The first question is whether its
tool setup is a bounded stage closure or an accidental materialization of the
whole project tool configuration.

## Evidence

Jackin's current source is on `95b437e735aafea5fe9b2e638c122345c5d141c3`; the
historical main run below used `41796158b1e45535ae4e74d5ff048cb5bb4e0488`.
The selected desktop task graph is unchanged in the source files compared with
the older main run at `3b1e1fc0a20a7d861454746c9ebb50a335c9b412`.

The authoritative raw job timestamps are:

| run / job | job wall | mise setup | task step |
| --- | ---: | ---: | ---: |
| `35475235030` / `105983237683` | 2,040 s | 16 s | 2,015 s |
| `35478203836` / `105991074920` | 2,635 s | 259 s | 2,363 s |

The current job's setup was `00:14:26Z`–`00:18:45Z`; its opaque
`Run desktop-merge` step was `00:18:46Z`–`00:58:09Z`. The older setup was
`23:07:54Z`–`23:08:10Z`; its task step was `23:08:11Z`–`23:41:46Z`. These are
raw `started_at`/`completed_at` values, not `updated_at`. The job API exposes no
inner task timestamps, so these numbers do not attribute the extra task time to
one command.

The generated workflows pin `jdx/mise-action` at
`c2a87611a18de5b3828c5652fe268e992400cb5c` (`v4.3.0`). `desktop-merge` passes:

```text
cargo-binstall rust aqua:nextest-rs/nextest/cargo-nextest cargo:sccache
cargo:boltffi_cli xcodegen swiftlint xcbeautify ripgrep
```

`desktop-scheduled` adds `periphery`. The adjacent `mise.lock` has exact locked
entries for all of those tokens (`rust` 1.97.1, `cargo:boltffi_cli` 0.30.1,
XcodeGen 2.46.0, SwiftLint 0.65.1, xcbeautify 3.2.1, Periphery 3.8.0,
ripgrep 15.2.0, and nextest 0.9.140). `mise.toml` sets `lockfile = true`.

The mise task graph is shell-opaque:

```text
desktop-ci       bindings-check → generate → format-check → lint → test
                 → build → test-swift → verify
desktop-merge    desktop-ci → desktop-test-ui
desktop-scheduled desktop-merge → desktop-deadcode
```

Every one of those desktop tasks reports `tools = {}` in `mise tasks --all
--json`; `mise tasks deps desktop-ci`, `desktop-merge`, and
`desktop-scheduled` each print only the root task because their nested `mise
run` calls are inside `run` shell text. This is the enabling condition for a
generator that cannot see the transitive tool closure.

The expensive product edges are also explicit:

* `desktop-test` runs `cargo nextest` and five Swift harnesses. If the
  XCFramework is absent, `cargo xtask desktop test` calls `build_xcframework`.
* `desktop-build` unconditionally calls `build_xcframework` again, then runs
  XcodeGen and `xcodebuild build`. Thus a clean runner can pack the same
  XCFramework twice in one `desktop-ci` invocation. The pack operation also
  regenerates and normalizes Swift bindings; that side effect cannot replace
  the earlier nonmutating `bindings-check` gate.
* `desktop-merge` invokes one full `desktop-ci`, then
  `native/Scripts/run-ui-tests.sh`. The UI driver finds 19 test methods, all
  19 names are unique, and runs each as a separate serial
  `xcodebuild test` (`-parallel-testing-enabled NO`, one `-only-testing`
  selector, one result bundle and JUnit report per test). They are distinct UI
  checks, not a duplicate selector list. The script has no
  `build-for-testing`/`test-without-building` handoff, so any build reuse must
  be demonstrated from raw logs before claiming it.
* `desktop-scheduled` intentionally reuses the complete merge graph and then
  runs Periphery. It therefore repeats the full PR-shaped graph on every
  scheduled invocation. This is cross-cadence duplication, not a second
  `desktop-ci` call inside one merge run.

The generated `ci-pr.yml` contains no `desktop-ci`, `desktop-merge`, or UI
task. Its native Swift unit invokes `swift-package-native-ci`, which runs
`desktop-xcframework`, `swift build`, and `swift test`; that overlaps the
XCFramework product but is not a duplicate full desktop PR graph and does not
cover the 19 UI checks.

## Local bounded diagnostic

No tool install, network download, cargo build, Xcode build, binding
generation, or UI run was performed. A dirty local candidate renderer did run
and wrote only temporary output for the check-profile boundary.

Using the local mise `2026.9.11` binary:

* `mise tasks --all --name-only` took 0.023 s and listed 40 tasks.
* `mise tasks info desktop-ci` took 0.019 s.
* `mise tasks validate desktop-ci desktop-merge desktop-scheduled` passed.
* `mise tasks graph --json` was refused with the exact error:
  `mise ERROR workspace project graph is experimental. Enable it with mise settings experimental=true`.
  No setting was changed.
* `mise install --include-task-tools --dry-run` proposed all 31 top-level
  tools. Since no desktop task declares task-local tools, this is a broad
  project install and is rejected for this stage.

With `MISE_TASK_RUN_AUTO_INSTALL=false MISE_AUTO_INSTALL=false`, the local
fail-closed probes were:

* `mise run desktop-generate`: `sh: xcodegen: command not found`.
* `mise run desktop-lint`: `sh: swiftlint: command not found`.
* `mise run desktop-deadcode`: `sh: periphery: command not found`.
* `mise run desktop-bindings-check`: `mise ERROR Tool not installed for shim: cargo` / `Missing tool version: core:rust@1.97.1`.
* `mise run desktop-format-check` passed through the host's
  `/Library/Developer/CommandLineTools/usr/bin/swift-format` via `xcrun`.

Available local commands include mise, a cargo wrapper, cargo-nextest's mise
shim, ripgrep, Swift, xcrun, and base macOS utilities (`plutil`, `codesign`,
`ditto`, `lipo`, `rustup`). Direct mise tools cargo-binstall, boltffi, sccache,
XcodeGen, SwiftLint, xcbeautify, and Periphery are absent. `swift-format` is
not a direct command but is discoverable through `xcrun`. The local
`DEVELOPER_DIR` is unset and `xcode-select` points at CommandLineTools, so this
host cannot stand in for the pinned GitHub Xcode 26.6 runner.

The only current repository use of `MISE_TASK_RUN_AUTO_INSTALL=false` is the
Renovate upstream-source workflow. The desktop workflows do not disable task
auto-install or not-found auto-install. Mise's task docs state that
`task.run_auto_install` defaults to true, that task-local `tools` install and
activate only for that task, and that `mise tasks deps` visualizes declared
dependencies rather than task references embedded in `run` shell text:

* [Task-local tools](https://mise.jdx.dev/tasks/task-configuration.html#tools)
* [Task dependencies](https://mise.jdx.dev/tasks/task-configuration.html#depends)
* [Tool management](https://mise.jdx.dev/dev-tools/)

## Alternatives

1. **Typed stage closure in the Velnor IR (preferred).** Add a generic stage
   declaration with named mise tasks, explicit tool keys, and runner
   capabilities. Resolve the named task DAG and product prerequisites in the
   generator, validate every mise key against the adjacent lock, and emit one
   deduplicated `install_args` set per generated job. Emit fail-closed
   `MISE_AUTO_INSTALL=false`, `MISE_TASK_RUN_AUTO_INSTALL=false`,
   `MISE_EXEC_AUTO_INSTALL=false`, and `MISE_NOT_FOUND_AUTO_INSTALL=false`
   around the check. For Jackin the closures are: PR desktop checks use Rust,
   nextest, boltffi, XcodeGen, and SwiftLint; merge adds xcbeautify and
   ripgrep for UI; scheduled adds Periphery. Keep `xcrun swift-format`,
   `xcodebuild`, and the SDK/Xcode version as typed runner capabilities. Do
   not include `cargo:sccache` until a real `RUSTC_WRAPPER`/cache contract is
   present; current Jackin source has no such use.

2. **Task-local mise tools with a generated stage installer.** Declare tools
   on the owning mise tasks and have the generic generator materialize the
   transitive stage closure before execution, then run with auto-install off.
   This uses mise's native task boundary, but `mise install
   --include-task-tools` is too broad in the current repository and must not be
   used as the CI stage installer without an isolated config or generated
   explicit key list. Tests must prove that nested task tools are available,
   unrelated top-level tools are not installed, and lock membership is exact.

3. **Verified stage tool cache.** Generate the same typed closure and cache
   only its exact tool directories, keyed by the `mise.lock` digest, stage
   manifest digest, runner OS/architecture, and Xcode/SDK capability. Restore,
   verify executable versions/checksums, install exact missing keys in locked
   mode, and run with every auto-install path disabled. This can reduce repeat
   setup on warm runners, but cache restoration is never evidence of tool
   presence until the manifest preflight passes.

The rejected option is a repo-specific shell escape such as adding an opaque
`mise_tools` list by hand for this desktop workflow. It hides the same bug for
the next task and cannot establish closure or product prerequisites. The
check-profile unit added no separate `mise_tools` field: it uses the existing
typed `CheckProfileSpec.tools` list and requires the consumer to declare that
explicit list. Unit-level `mise_tools` remains outside this bounded renderer.

## First bounded implementation unit

The first generic fix reuses the existing `CheckProfileSpec` boundary. The
resolver already validates named tasks and the lock-shaped `profile.tools`
list; it does not expose native task metadata, so the renderer does not parse
shell text or invent a competing stage schema. `profile.tools` remains the
explicit list supplied by consumer configuration. A task-local `tools` or
`depends` entry omitted from that list reaches the task with auto-install
disabled and fails at the real Mise boundary instead of silently broadening
installation. No separate `mise_tools` field was added or used for this
check-profile boundary.

Each hosted profile owns one pinned `jdx/mise-action`: a non-empty closure
passes only its declared `install_args`, and an empty closure uses
`install: false` for runtime setup. Velnor keeps its preinstalled mise and
installs only the declared list. The current Velnor command omits `--locked`;
an isolated Mise control showed that `settings.lockfile = true` alone still
attempts installation for a missing lock record, while explicit `--locked`
fails before installation. Add that flag or equivalent runner proof before
accepting the Velnor path. The profile job owns
`MISE_AUTO_INSTALL=false`, `MISE_TASK_RUN_AUTO_INSTALL=false`,
`MISE_EXEC_AUTO_INSTALL=false`, and `MISE_NOT_FOUND_AUTO_INSTALL=false`; a
profile env entry for any of those keys is rejected so YAML cannot override the
generated policy. Local mise `2026.9.11` reports all four effective settings as
`false` when set through those environment variables. This unit changes
provisioning correctness only; it carries no timing claim and does not remove
or reorder desktop checks.

## Required stage shape and acceptance

The generic stage model must keep two separate barriers:

* Run Swift format and lint as early static checks. Formatting already excludes
  generated bindings; lint must retain its nested policy configuration.
* Keep `desktop-bindings-check` as a mandatory, nonmutating prerequisite for
  every XCFramework, SwiftPM, app, and UI product in the optimized desktop
  stage. Any separate SwiftPM consumer must invoke the same check or consume
  an explicitly attested checked product. The check must regenerate into
  staging and compare bytes; a build-generated binding side effect is not a
  substitute. No generated Swift file may be edited to make the gate pass.

The stage still must execute and prove all five pure Swift harnesses, counted
XCTest and Swift Testing totals, app verification, all 19 UI methods with
nonzero results and no runtime warnings, and scheduled Periphery. Any future
build reuse must preserve those checks and make its artifact identity explicit.

Generator fixtures are required before implementation acceptance: nested typed
task closure and deduplication; unknown task/tool and lock mismatch rejection;
system capability mismatch rejection; separate format/lint and binding
barriers; clean-run XCFramework producer/consumer reuse without a second pack;
UI selector count/identity preservation; and scheduled dead-code retention.
The generator must consume typed constructors/metadata. It must not classify
arbitrary shell text as a normal stage or infer missing tools from regexes.

No timing candidate is valid until the generated workflow has a real pinned
runtime, exact tool/capability preflight, and the unchanged coverage above.
Then collect at least five paired baseline/candidate attempts per important
cohort, retain failures, and report raw stage timestamps separately from
parallel aggregate execution. This document records diagnosis only.
