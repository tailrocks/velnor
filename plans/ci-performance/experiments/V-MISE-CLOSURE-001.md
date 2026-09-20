# V-MISE-CLOSURE-001: static check-profile tool closure

Status: design proposal; no closure resolver is implemented by this record.
Date: 2026-09-20.

## Root cause

`CheckProfileSection` has typed `tasks` and `tools` fields, but the generator
currently treats them as unrelated strings. Source1's task parser reads only
`[tasks.<name>]` header lines. Source2 parses top-level task names with TOML,
but neither path follows `depends` or reads task-local `tools`. `apply_check_profiles`
copies `row.tools()` directly into `CheckProfileSpec.tools`; the renderer then
installs that explicit list. The current root lock helper returns only top-level
tool keys, and it intentionally ignores per-unit locks and tool version records.

This structure lets a profile name a task whose effective Mise tool closure is
larger than its explicit list. Generated jobs disable all four Mise auto-install
paths, so a missing task-local tool fails only when the task executes. A shell
command such as `run = "mise run hidden"` is opaque to a TOML task graph and
cannot be safely inferred by scanning text. Executing tasks during generation
would run repository code and would make generation depend on host tools,
secrets, network, and mutable state.

The immediate renderer unit now emits `mise --yes --locked install` for a
non-empty Velnor profile in both schema implementations. That enforces the
lock boundary at runtime, but it does not discover the correct profile list.

## Preferred design: one static Mise model for both schemas

Add an internal, typed static model built from the repository's checked-in
Mise files. Keep `CheckProfileSection` and `CheckProfileSpec` as the profile
contract; do not add a second legacy tool-list field or a shell parser.

The model should contain, at minimum:

```text
MiseStaticModel {
  root_tools: Map<ToolKey, ToolSelector>
  tasks: Map<TaskName, MiseTask {
    depends: Vec<TaskName>
    tools: Map<ToolKey, ToolSelector>
  }>
  lock: Map<ToolKey, LockedTool { version, ...platform records }>
}
```

Both source1 and source2 must call the same TOML parser and resolver. The
resolver must read only an explicitly selected repository config and its
adjacent lock. It must not read the user's global Mise config, invoke `mise`,
run a task, source a shell file, or inspect arbitrary project code.

For each profile, resolve the transitive `depends` graph from every named
task. Detect a missing task and a cycle with the profile and dependency path in
the error. Collect task-local `tools` only from that selected transitive graph;
the root `[tools]` map supplies selectors for selected requirements and does
not mean every root tool is installed. A task-local selector overrides the
root selector for that task. If two selected tasks require different selectors
for one key, fail generation rather than choose an order-dependent value.

The first resolver should accept the static Mise shapes already used by the
consumers: a string selector or a table with a string `version`; it should
reject unsupported dynamic/table shapes with the file path and key. Lock
validation must compare the effective selector with the lock record's version,
not merely its key. A selector that cannot be compared without a network
lookup (for example an unresolved channel or alias) is a generation error.
Platform records may prove artifact availability, but they do not change the
tool version identity. Missing lock, missing key, malformed lock, and version
mismatch all fail before output is written.

`check_profile.tools` remains the typed list for opaque consumer extras. The
resolver materializes discoverable task-local requirements automatically and
passes a typed, versioned install plan to the renderer; consumers do not repeat
those keys merely because they are discoverable. Every explicit extra is still
validated against the lock. Effective selectors come from the Mise files and
are checked against lock versions. A profile with no explicit `tools`
declaration may pass when the discovered closure is non-empty only if the
resolver can supply that closure; it must fail when an opaque extra is needed.
This removes the current omission failure while retaining one profile contract.

Task-local version overrides must be installed and activated at that version.
A bare `mise install <key>` selects the root declaration, so it is invalid when
the local selector differs unless the typed install plan carries the exact
versioned input. Do not merely validate the key and then silently install the
root version. Selector comparison must follow Mise semantics: exact selectors
require exact lock evidence; ranges, channels, aliases, and backend/platform
selectors require an explicit lock proof, and are rejected when that proof
cannot be established without a network lookup.

The resolver must not infer `mise run` references embedded in `run` strings.
Consumers that invoke a hidden task from shell text must list that task's
effective tools explicitly in `check_profile.tools`, and the resolver must
still validate those tool keys against the lock. This is an explicit coverage
obligation, not evidence that the generic scanner understood the shell. A
future typed task declaration can make such calls visible, but adding a second
parallel profile field now would create two competing contracts.

Start with root `mise.toml` and root `mise.lock`, because those are the files
the current config contract and generated jobs use. If a task is selected from
a nested config, require an explicit config-plus-lock mapping in the existing
typed repository configuration and resolve the pair together. Never silently
combine a nested `mise.toml` with the root lock. Per-unit lock support is a
separate bounded extension and remains a hard error until that mapping exists.

## Alternatives considered

1. **Static graph resolver (preferred).** Parse TOML into the model above,
   expand declared dependencies, merge root/task tool selectors, and validate
   the explicit profile closure against the lock. This gives deterministic
   source1/source2 behavior and catches missing dependencies before generation.
   It cannot see shell-opaque calls, so those remain an explicit consumer
   declaration. No project code executes.

2. **Consumer-authored opaque extras only.** Keep `tools` as an explicit,
   lock-validated list for shell-opaque requirements while the static resolver
   derives selected task-local tools. This preserves an escape hatch without
   requiring consumers to duplicate discoverable keys. It still depends on
   consumers declaring every shell-nested requirement, so it is not a complete
   task-language model.

3. **Mise-driven installation or task introspection.** Generate a selected
   task invocation and ask Mise to resolve/install its tools at runtime, or
   use a broad `mise install`/task-tool flag. This follows Mise's behavior but
   makes CI depend on runtime resolution, can install tools outside the
   selected closure, and is incompatible with `MISE_*AUTO_INSTALL=false` and
   strict lock evidence. It also cannot safely prove shell-nested calls during
   generation. Reject this alternative for the generic renderer.

## Required tests before implementation is accepted

Use the same fixture corpus through source1 and source2, then compare the
resolved model and generated profile steps byte-for-byte where their schemas
are otherwise equivalent.

* Parse `[tools]`, inline `[tasks]`, quoted task names, nested
  `[tasks.<name>]`, task `depends`, and task-local `tools`.
* Resolve a two-level dependency chain and collect root plus task-local tools.
* Reject a missing dependency and a dependency cycle with a useful path.
* Prove a task-local selector overrides the root selector, and reject two
  conflicting selected selectors for one key.
* Accept a selector whose exact lock version matches; reject missing lock,
  missing key, malformed lock, platform-only records without a version, and a
  version mismatch. Cover quoted tool keys such as `cargo:sccache` and table
  selectors with `os`/`default-features` metadata.
* Reject an unsupported dynamic selector without attempting a network lookup.
* Omit a discoverable task-local key from `check_profile.tools` and prove the
  resolver materializes it. Validate explicit opaque extras as lock-backed
  inputs, and reject an invalid extra.
* Prove a task-local version override is installed at that version, or reject
  it with a precise capability error; never fall back to the root selector.
  Exercise exact versions and Mise range/channel selectors according to the
  lock's actual semantics rather than lexical string equality.
* Prove a shell string containing `mise run hidden` does not get parsed or
  executed; the fixture's sentinel script must remain untouched. The profile
  passes only when the hidden tool is explicitly listed and lock-validated.
* Prove source1's old line parser and source2's TOML parser are gone from the
  closure path: inline task keys and quoted names must work identically.
* Prove no repository task, script, shell, network request, or `mise` process
  runs during scanning. A test fixture should fail if its sentinel is touched.
* Keep renderer tests for one pinned hosted Mise action, `install: false` for
  empty profiles, exactly one Velnor locked install command, and all four
  generated auto-install flags set to false.
* Run the existing isolated Mise controls: with auto-install off an omitted
  task tool fails without an installer attempt; with auto-install on and
  offline mode Mise attempts installation. These runtime checks complement
  static tests; YAML substring checks alone are insufficient.

## Compatibility and rollout constraints

This is a breaking migration permitted by the project rules. Do not retain an
old parser, a second `tools` alias, a compatibility env override, or a broad
fallback installer. Existing explicit profile lists remain valid only after
their keys and selectors are proven against the committed lock. Jackin's
current profiles have no task-local tool metadata in Mise's task JSON, but
their nested shell calls still require the explicit list; that fact is not a
generic inference.

Do not alter `unit.mise_tools` semantics in this change. Do not claim that a
successful generated file proves task closure, provider correctness, or CI
performance. Regenerate consumer YAML only from the tested pinned generator;
never hand-edit generated files. Bump the generator revision only when the
resolver and its generated output are integrated. Keep source1/source2 parity
and the strict `--locked` Velnor command as acceptance gates.

The first bounded experiment is fixture-only: implement the static model in a
private test helper or isolated module, run the cases above for both schemas,
and compare the resulting closure diagnostics with the actual Jackin,
Parallax, and Velnor `mise.toml`/`mise.lock` files. Do not use that experiment
to claim a timing improvement or to accept generated consumer workflows.
