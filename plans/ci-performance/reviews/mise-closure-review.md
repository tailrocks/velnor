# Mise closure review

Date: 2026-09-20. Reviewer: `/root/jackin_inventory`.

Verdict: **proceed with constraints**. The static resolver in
`V-MISE-CLOSURE-001` is the right boundary, but its model is too small for
Mise 2026.9.11. Add typed execution edges and per-task tool bindings before
implementation. Do not infer shell text or fall back to
`mise install --include-task-tools`.

## Actual Jackin shape

Read-only `mise -C jackin --locked tasks --json` reported 40 tasks. Every task
has `tools = {}`, no task has an alias, and the only declared dependency is
`ci -> build`. There are no `depends_post` or `wait_for` entries. The root
`[tools]` table has 34 entries; it is a repository-wide declaration map, not a
request to install all 34 tools for a selected desktop task. `rust` is selected
through `rust-toolchain.toml` because `idiomatic_version_file_enable_tools`
enables Rust discovery; it is also present in `mise.lock`.

The desktop graph is hidden in shell strings:

* `desktop-ci` runs `desktop-bindings-check`, `desktop-generate`,
  `desktop-format-check`, `desktop-lint`, `desktop-test`, `desktop-build`, a
  direct `cargo xtask desktop test-swift`, and `desktop-verify`, in that order.
* `desktop-merge` runs `desktop-ci` then `desktop-test-ui`.
* `desktop-scheduled` runs `desktop-merge` then `desktop-deadcode`.
* `swift-package-native-ci` runs `desktop-xcframework`, then direct SwiftPM
  commands.

The installed Mise task JSON correctly reports these parents with an empty
`depends` list and empty `tools`; `mise tasks deps` cannot recover the shell
calls. The current generated profiles therefore must retain an explicit typed
tool list until these calls become native task references.

Static source inspection found the following Mise-managed desktop tools:

| task | Mise requirements visible from source |
| --- | --- |
| `desktop-bindings`, `desktop-bindings-check`, `desktop-xcframework` | `rust`, `cargo:boltffi_cli` (`boltffi` invokes Cargo) |
| `desktop-test` | `rust`, `aqua:nextest-rs/nextest/cargo-nextest`; direct Swift is an Xcode capability |
| `desktop-test-swift` (new typed wrapper needed) | `rust`; Swift/XCTest is an Xcode capability |
| `desktop-build` | `rust`, `cargo:boltffi_cli`, `xcodegen`; `xcodebuild`, `ditto`, signing and Mach-O tools are host capabilities |
| `desktop-verify` | `rust`; signing, Gatekeeper, Mach-O and plist tools are host capabilities |
| `desktop-generate` | `xcodegen` |
| `desktop-lint` | `swiftlint` |
| `desktop-test-ui` | `xcbeautify`, `ripgrep`; `xcodebuild`, `xcrun`, and shell utilities are host capabilities |
| `desktop-deadcode` | `periphery` |

`cargo-binstall` is an installer/provider prerequisite for Cargo-backed Mise
tools, not a command used by these tasks. Keep it in the provisioning plan
until the pinned action/provider contract proves it can be removed. `sccache`
is explicitly provisioned today but no desktop command directly invokes it;
preserve it as an explicit provider/runtime requirement until the CI wrapper
contract is proven. Neither may be inferred from the task shell.

`pipx:reuse` is a useful root-map control: its declaration has
`depends = ["python", "uv"]`. Mise uses this for installation ordering and
does not add or configure those tools automatically. The resolver must model
configured tool dependencies and fail when a selected dependency is absent;
root-map membership alone is not an install request.

## Required model correction

Use a per-invocation binding rather than `Map<ToolKey, ToolSelector>`:

```text
ToolRequirement {
  key: ToolKey,
  selector: Selector,
  scope: TaskName or ProfileExtra,
  origin: TaskLocal | ProfileExtra | ToolDependency | IdiomaticFile,
  lock: LockedArtifact,
  install_dependencies: [ToolKey],
}

MiseTask {
  name, aliases,
  tools: [ToolRequirement],
  before: [TaskRef],       // depends
  after: [TaskRef],        // depends_post
  wait_for: [TaskRef],
  run: [OpaqueShell | TypedTaskRef | TypedParallelTaskRefs],
}

TaskClosure {
  ordered_invocations: [TaskInvocation],
  tool_requirements: [ToolRequirement],
  host_capabilities: [CapabilityRequirement],
  opaque_coverage: [ProfileExtra],
}
```

The same key with two task-local selectors is legal: task-local tools apply to
that task only, so retain `(task, key, selector)` identity instead of rejecting
all selector differences globally. Emit/install both exact locked versions
when selected. A profile extra without a task scope resolves against the root
selector; it must never silently inherit a task-local override.

Resolve these edges with their Mise semantics:

1. Expand `depends` before the task and `depends_post` after it. Optional
   missing references are the only omission allowed; ordinary missing names,
   cycles, alias collisions, and unsupported argument forms fail generation.
2. `wait_for` adds an ordering constraint only when the referenced invocation
   is already scheduled. It does not add the task or its tools. An unresolved
   non-optional wait target is an error; an unscheduled optional target is
   empty.
3. Expand structured `run` entries (`{ task = ... }` and `{ tasks = [...] }`)
   as typed ordered/parallel invocations. Mise intentionally excludes these
   from `tasks deps`, so a resolver that reads only `depends` is incomplete.
4. Treat strings such as `mise run hidden`, `cargo xtask`, and external script
   bodies as opaque. No shell parser, command-name guesser, or task execution
   is allowed. Their requirements remain explicit profile/task declarations.
5. Resolve aliases with Mise's name-precedence rule. Reject an ambiguous
   inherited/nested config rather than combining configs implicitly.
6. Follow `[tools].depends` as install-order edges only when the dependency is
   configured in the selected config scope. It is not a license to install all
   root tools.

Lock matching must bind key, exact effective version, backend/options, and the
current runner platform URL/checksum. A version-only match is insufficient:
Mise lockfiles can contain multiple entries for one tool/version with different
options. Load the enabled Rust idiomatic file as a declared input; otherwise a
profile's `rust` requirement cannot be proven from `mise.toml` alone. Keep each
`mise.toml` paired with its own adjacent lock (`docs/mise.toml` and
`docs/mise.lock` are a separate scope).

Primary Mise references: [task properties](https://mise.jdx.dev/tasks/task-configuration.html),
[task tools](https://mise.jdx.dev/tasks/task-configuration.html#tools),
[task dependency semantics](https://mise.jdx.dev/tasks/task-configuration.html#depends),
[tool dependencies](https://mise.jdx.dev/dev-tools/),
[locked installation](https://mise.jdx.dev/dev-tools/mise-lock.html#strict-lockfile-mode),
and [`task.run_auto_install`](https://mise.jdx.dev/configuration/settings.html#task-run-auto-install).

## Minimal Jackin declaration migration

The bounded migration adds exact task-local `tools` to the leaf tasks listed
above, plus a typed `desktop-test-swift` wrapper. It replaces only nested
`mise run` calls and the direct test-swift command in the three desktop parent
tasks with ordered native Mise task references. Native task references provide
fail-on-error ordering; leaf shell commands remain opaque and keep their
existing command bodies. No command was converted merely to make it parseable.

Until that migration and generated-output review land, keep the current
desktop profile extras (including provider prerequisites) as the explicit
closure for the opaque parent task. After migration, the resolver may
materialize the union from typed child invocations and remove only entries
proven redundant. Host Xcode capabilities remain separate typed requirements;
Mise closure must never pretend `xcrun swift-format`, `xcodebuild`, or signing
tools are Mise tools.

## Isolated runtime probes

No Jackin task or repository script was run. The final static probe used Mise
2026.9.11 with isolated `/tmp` configs and a sentinel shell command:

* `/tmp/jackin-mise-closure-fixture`: `tasks info --json local` exposed
  `tools.node = 22.0.0`, `depends = ["base"]`, `depends_post = ["post"]`,
  `wait_for = ["peer"]`, and a structured `{ task = "nested" }` run step.
  `tasks deps --dot local` showed `base` and post ordering, but not the
  unscheduled `peer` or the structured nested call. The sentinel stayed
  untouched.
* `/tmp/jackin-mise-closure-valid`: with root `node = 24.18.0` and a task-local
  `node = 22.0.0`, `mise --locked install --dry-run` proposed only
  `node@24.18.0`; adding `--include-task-tools` proposed both exact versions.
  Exit was 0 and the task sentinel stayed untouched. This proves local-version
  identity must survive closure resolution; it is not a performance result.
* The same first fixture with incomplete platform lock records failed closed:
  `No lockfile URL found for ripgrep@15.2.0 on platform macos-arm64`,
  `No lockfile URL found for node@22.0.0 on platform macos-arm64`, and
  `node@24.18.0 is not in the lockfile`. No task ran.

These probes validate the shape and failure boundary only. They do not prove
artifact availability, cache warmth, task correctness, or CI speed.

The checked-out Jackin task graph was then inspected with `mise tasks --json`:
41 tasks parsed, including the three structured desktop parents and the typed
leaf tools. A copied tree with only the stale profile auto-install env line
removed passed Velnor dry-run generation: 40 units and 146 dependency edges;
no generated files were written. The source profile still carries that stale
env override and must be removed by the auto-install configuration unit.

## Independent Parallax challenge

Reviewer: `/root/parallax_inventory`. Host probe: Mise `2026.9.11` on
`macos-arm64`; fixture and sentinel live under
`/tmp/velnor-mise-sentinel-phase1`. The probe used isolated Mise data/cache/
state/config paths and all four auto-install settings set to `false`.

The actual runner preserved the required execution semantics:

* `run = [{ task = "leaf" }, { tasks = ["second", "leaf"] }]` produced
  `leaf`, then `second`; the duplicate `leaf` was not run twice.
* A failing structured step stopped later steps (`pipeline-fail` produced only
  `fail`, exit `17`). A regular failing dependency also stopped the parent and
  its post-dependency. A task's own failure still ran `depends_post` (`own-fail`
  produced `own-fail`, then `leaf`, exit `19`).
* `wait_for` did not schedule an otherwise idle task (`waiter` produced only
  `waiter`), but did order it behind a task scheduled in the same parallel
  group (`wait-pipeline` produced `leaf`, then `waiter`).
* An optional wildcard dependency is scheduled when it matches: `depends =
  [{ task = "group:*", optional = true }]` ran both `group:a` and `group:b`
  before `optional-wild`.
* Mise task-name precedence wins over an alias collision: an alias `shared`
  on `alias-source` did not shadow the real `[tasks.shared]`; `mise run
  alias-parent` ran `shared` then `alias-parent`.

The last two probes expose a generator boundary defect. `Manifest::has_task`
only performs exact task/alias lookup (`mise_closure.rs:192-216`), so an
optional wildcard that matches real tasks is treated as absent and contributes
no tools. The generated profile can therefore pass static validation while the
runtime schedules unvisited tasks with missing task-local tools. Expand Mise
patterns against the loaded task set, or reject all wildcard references
including optional ones until expansion exists. The alias collision is the
opposite direction: the resolver rejects a valid Mise configuration at
`mise_closure.rs:178-185`, whereas Mise gives the concrete task name
precedence over an alias. Fix precedence or fail only genuinely ambiguous
cross-config cases.

The resolver also reads only root `mise.toml`/`mise.lock` and direct task
tables. Mise 2026.9.11 applies task-template `extends`, task-file and
`task_config.includes` overlays, and parent/environment config scopes. A task
that inherits `tools`, `depends`, or a command through those mechanisms is
accepted with an incomplete closure here. This must be an explicit unsupported
shape error or a complete, paired config/lock loader; silently ignoring it is
unsafe with auto-install disabled.

Lock matching at `mise_closure.rs:232-317` binds only key, selector text, and a
permissive backend match. It ignores lock `options` and `platforms`, and treats
an absent lock backend as matching an explicit backend. Mise documents options
as artifact identity and platform records as required by strict locked installs
for URL-backed tools. Either bind those fields to the requested target
platform/backend or reject such declarations before output. Runtime
`--locked` remains a useful second gate, not proof that generation resolved the
right artifact.

Several unsupported forms fail closed, which is safe but currently limits the
phase: dependency arrays with arguments/env, wildcard patterns, and structured
`run` task refs with `args`/`env` are valid Mise syntax but rejected. The
parser additionally accepts a standalone structured `depends` table and
silently ignores `optional` on a structured `run` item, although the Mise
schema does not define that run field. Reject malformed shapes consistently;
do not silently broaden or drop edges. `TaskClosure.tasks` records resolver
visit order, not actual `run` step order (`run` children are visited after the
parent is appended); keep it diagnostic only until an invocation graph models
serial/parallel phases.

Primary semantic references: [task configuration](https://mise.jdx.dev/tasks/task-configuration.html),
[running tasks](https://mise.jdx.dev/tasks/running-tasks.html),
[task templates](https://mise.jdx.dev/tasks/templates),
[Mise schema task entries](https://mise.jdx.dev/schema/mise.json), and
[lockfile fields](https://mise.jdx.dev/dev-tools/mise-lock.html).

## Independent phase1 patch review

Date: 2026-09-20. Scope: `/tmp/velnor-mise-closure-phase1/velnor-mise-closure-phase1.patch`, SHA-256
`602e01483b54a6694f06f11362c5f77f06cb9215760151fe458bd546b82d9a93`.

The patch applied cleanly to a `git archive HEAD` tree. In that isolated tree,
`cargo test --locked -p velnor-workflow --lib config::mise_closure::tests -- --nocapture`
passed 7/7. Additional reviewer-only probes for the real Velnor manifest, a
Jackin task-local tool graph, lock identity, aliases, valid structured Mise
references, and regular/post phases passed 14/14; the probes intentionally
encode the currently observed defects below. These results are parser and
closure checks, not CI timing or artifact availability evidence.

Verdict: **HOLD for generic closure acceptance**. The resolver is a useful
fail-closed foundation, but it can accept an incompatible lock identity and
its task set is not an execution graph. Committing it as a validation-only
foundation is possible only if consumers cannot use its result to install or
remove explicit tools; current wiring validates profiles while the runtime
still installs the explicit profile list, so it must not be advertised as
complete transitive tool closure or as a speedup.

### Blocking correctness findings

* **Lock identity is fail-open.** `Lockfile::resolve` accepts a lock row with
  no backend when the request names one (`mise_closure.rs:282-317`), and the
  parsed `LockEntry` drops lock `options`, platform records, URLs, and
  checksums. A clean fixture with an explicit backend and a backend-less lock
  row returned `Ok`; a platformless lock row also returned `Ok`. This can
  select an artifact that is not the requested provider or current runner
  platform. Match the effective backend and options, bind the target
  OS/architecture to the selected platform record and its URL/checksum, and
  reject missing or ambiguous identity fields. Mise `--locked` is a second
  runtime check, not proof that generation resolved the right artifact.

* **Regular and post phases are collapsed.** `task_state` and
  `TaskClosure.tasks` deduplicate a task referenced from both `depends` and
  `depends_post` (`mise_closure.rs:411-459`). A reviewer fixture proved one
  closure entry. Mise documents that the same task in both phases executes
  before and after the parent. Either model phase/invocation identity, or
  explicitly make this structure a tool-union set and keep execution ordering
  out of it. Do not use the current list as proof of runtime ordering.

* **Valid structured references are rejected, safely but incompletely.**
  `parse_task_ref_value` and `parse_run_tasks` reject `args`/`env`, although
  current Mise supports them for structured task references. A valid fixture
  returned an unsupported-key error. This is preferable to silently losing an
  edge, but the supported syntax must be documented and covered by an explicit
  unsupported-shape diagnostic until invocation semantics are implemented.

* **Alias behavior diverges from Mise.** The manifest loader rejects a task
  name colliding with an alias, while Mise resolves the concrete task name in
  that case. A fixture using `[tasks.shared]` and an alias named `shared`
  confirmed Mise's task-name precedence. Implement that precedence or reject
  only genuinely ambiguous inherited aliases; do not reject a valid direct
  task configuration.

The resolver does correctly recurse typed tool dependencies, retain task-local
tool identity, detect cycles, and keep opaque shell text out of the typed
graph. However, `apply_check_profiles` discards the resolved closure after
`validate_profile_tools`; inferred task-local tools are not yet an install
plan. Keep explicit profile tools until a versioned, platform-aware install
plan consumes the closure. Also add the plain `rust-toolchain` idiomatic-file
form before claiming complete Rust tool discovery; phase1 reads only
`rust-toolchain.toml`.

Required follow-up before acceptance: add lock identity/platform/options tests,
separate tool-union from phase-aware invocation modeling, define supported
structured-reference/alias semantics, and prove the generated install plan
uses exactly validated products. Until then, retain the existing explicit
tool contract and mark this patch a correctness-hardening prerequisite, not a
performance result.

## Current bounded implementation verdict

The implementation now carries the reviewed fail-closed boundaries into both
profile renderers. It resolves the target lock platform from the typed profile
declaration and, where the renderer owns a documented hosted label, checks that
label against an exact OS/architecture table. Unknown self-hosted labels still
require `platform`; label/platform conflicts and runner/OS conflicts stop
generation. The generator host is never used to select a lock artifact.

Lock resolution now binds the requested selector, effective backend, options,
and target platform record, including URL/checksum for artifact-backed
providers. Empty or malformed identity fields, missing platform records, and
ambiguous matches fail closed. Typed `depends`, `depends_post`, `wait_for`,
aliases, and structured task references remain represented in the closure;
opaque shell remains explicit and is never scanned for tools.

The current installer contract intentionally emits only bare root lock keys.
Task-local requirements with the exact same locked root identity deduplicate;
task-local selectors that differ are rejected with an explicit migration error.
This is a correctness boundary, not completion of exact task-local Mise
selection. The deferred completion dependency is a coordinated locked
selection format consumed by generator, runner, and action; no profile tool is
silently replaced by the root version.

Rust channel-only idiomatic discovery remains supported. Rustup components,
targets, and profile options are rejected until the typed `RustToolchain`
provisioning path is transported through `CheckProfileSpec` and its renderer;
stringifying those options as Mise tool options would be unsafe.

Independent focused evidence for this bounded unit: 21 closure tests, 7
scheduled-profile integration tests, and library Clippy with `-D warnings`
pass in an isolated copy. The parent 1,943-test suite exercised the separate
cancellation repair and excluded this Mise patch; it provides no full-suite
evidence for this unit. These focused checks establish bounded generation and
fail-closed correctness only. They prove
no CI speedup and do not validate unknown custom runner hardware.
