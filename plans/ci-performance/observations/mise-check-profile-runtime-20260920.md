# Mise check-profile boundary probes

Date: 2026-09-20. This is runtime and local-render evidence, not a timing
claim and not proof of an attested release.

## Runtime probes

The probes used the installed host binary `/Users/donbeave/.local/bin/mise`
(`2026.9.11 macos-arm64 (2026-09-18)`) with a temporary fixture and isolated
MISE/XDG-specific data, cache, state, config, install, and download
directories. The omission control also supplied explicit empty
`MISE_GLOBAL_CONFIG_FILE` and project `MISE_CONFIG_FILE` paths. The relevant
environment values were:

```text
MISE_AUTO_INSTALL=false
MISE_EXEC_AUTO_INSTALL=false
MISE_NOT_FOUND_AUTO_INSTALL=false
MISE_TASK_RUN_AUTO_INSTALL=false
MISE_YES=0
```

The temporary `nested/mise.toml` declared `child:leaf` and `parent`, with
`parent.depends = ["child:leaf"]`. `mise -C nested run parent` exited 0,
printed `childparent`, and logged both task commands in dependency order.
The same command for an undeclared `absent` task exited 1 with `mise ERROR no
task absent found`.

The temporary `missing/mise.toml` declared `parent` with a missing
`child:leaf` dependency. `mise -C missing run parent` exited 1 with
`mise ERROR task not found: child:leaf`; the parent command did not run.

The temporary `tool/mise.toml` declared `[tools] python = "3.14.7"`, while its
lockfile had no `python` entry. `mise -C tool --locked run probe` exited 1,
reported `python@3.14.7 is not in the lockfile`, and then failed the Python
shim. It did not update the lockfile or silently install Python.

A second fixture copied the exact locked Python record from Jackin's
`mise.lock`, declared it only as the `probe` task's `tools = { python =
"3.14.7" }`, and left the isolated Mise install/download directories empty.
With `MISE_*AUTO_INSTALL=false`, the generated-command-shaped invocation
`mise run probe` exited 1 at the shim and created no install or download
directory. The same invocation with all four auto-install settings true and
`MISE_OFFLINE=1` exited 1 before the task, reporting `installing 1 tool` and
`Failed to install core:python@3.14.7: offline mode is enabled`. This is a
direct omission control: auto-off does not invoke the installer, while the
baseline auto-on behavior does. No network or tool installation occurred.

These probes prove the task boundary fails closed at the actual Mise
invocation; rendered YAML string assertions alone would not prove this.

The lock requirement is a separate control. In a fixture with
`settings.lockfile = true` and a missing Python lock record,
`mise install python` (the current Velnor renderer shape) still attempted the
install and only failed because offline mode was enabled. The same fixture
with `mise --locked install python` failed immediately with
`python@3.14.7 is not in the lockfile`. Therefore hosted action behavior does
not establish the Velnor lane's lock behavior: its explicit installer must
carry `--locked`, or an equivalent runner policy must be demonstrated.

The source2 task parser validates only explicitly named top-level task keys.
Actual Mise resolves the declared dependency graph at runtime, as shown above.
No transitive tool closure is inferred: `CheckProfileSpec.tools` remains the
explicit list supplied by consumer configuration, and any omitted task tool
must fail at the same auto-install-disabled runtime boundary.

## Jackin tool and renderer probe

Jackin source checkout:

```text
/Users/donbeave/Projects/tailrocks/jackin-project/jackin
HEAD 95b437e735aafea5fe9b2e638c122345c5d141c3
```

A direct comparison of every `check_profile.tools` token with the top-level
`[[tools.<key>]]` entries in `mise.lock` found zero missing keys:

| profile | explicit tools | missing lock keys |
| --- | ---: | --- |
| `desktop-merge` | 9 | none |
| `desktop-scheduled` | 10 | none |

Using the installed Mise binary against the actual Jackin checkout,
`mise tasks --all --json` reported 40 tasks, zero tasks with a non-empty
`tools` map, and zero declared dependencies on the 23 `desktop-*` task rows.
The desktop task scripts can still invoke nested `mise run` commands inside
shell text; the JSON task graph does not expose those references. Thus the
current Jackin profile lists are complete for the explicit tool contract
checked here, while a generic renderer cannot infer a hidden shell closure.

The local candidate renderer was
`/tmp/velnor-ci-integration/target/debug/velnor-workflow`, reporting source
revision `17142395609c29de76d7b99b3f661d55aa95edc5` and closure
`1e7d319be3752d36196041a46d9dbce6e39c8f0a5fe20f01efc33dd9088e0295`.
Its checkout had only the two `check_profiles` source files modified. This
binary is a dirty local validation artifact, not an immutable candidate.

Before the source edit, this exact command failed closed:

```text
velnor-workflow --plain --force --default-branch main --runners both \
  --output /tmp/jackin-candidate-before \
  /Users/donbeave/Projects/tailrocks/jackin-project/jackin
```

Observed error:

```text
check profile `renovate-upstream-sources` env `MISE_TASK_RUN_AUTO_INSTALL`
conflicts with generated Mise auto-install policy; omit it
```

The only Jackin source change was removal of that redundant env assignment,
its explanatory comment, and the now-empty env table from
`.github-gen/velnor-workflow.toml`. The candidate renderer then succeeded into
`/tmp/jackin-candidate-after2`, creating 19 generated files. `desktop-merge` and `desktop-scheduled` each received the
exact explicit lock-key list as `install_args`; every generated profile job
emits `MISE_AUTO_INSTALL`, `MISE_EXEC_AUTO_INSTALL`,
`MISE_NOT_FOUND_AUTO_INSTALL`, and `MISE_TASK_RUN_AUTO_INSTALL` as `"false"`.
The empty-tool Renovate profile emits `install: false` and still gets the
same fail-closed environment.

The pinned action's primary README at
`https://raw.githubusercontent.com/jdx/mise-action/c2a87611a18de5b3828c5652fe268e992400cb5c/README.md`
defines `install` as the `mise install` switch and `install_args` as its
additional tool arguments. It says a repository `mise.lock` causes the action
to add `--locked` automatically, and that `install: false` skips tool
installation. The generated non-empty profiles therefore have one
`jdx/mise-action@c2a87611a18de5b3828c5652fe268e992400cb5c` setup step carrying
the explicit tool list and cache settings; they do not add a second runtime
Mise setup action. Empty profiles use the same pinned action with
`install: false`.

The Jackin source edit remains an uncommitted migration input. It must ship
with the matching renderer/generated pin, because the old generated renderer
would otherwise rely on the removed env override to avoid broad task-tool
installation.

## Three local regeneration checks

Using the same local binary and temporary output roots, generation completed
for all three campaign checkouts:

| checkout | command mode | result |
| --- | --- | --- |
| Jackin | schema 1, `--runners both` | 19 files |
| Parallax | schema 2, config provider universe | 12 files |
| Velnor | schema 2, config provider universe | 21 files |

For schema 2, `--runners` is intentionally not accepted by the source2
dispatch; the config's typed `providers`, `automatic_providers`, and
`default_dispatch_providers` are used. Passing the legacy flag produced the
exact parser error `unexpected argument '--runners' found`; no output was
written. The successful schema2 commands omitted that flag and preserved the
consumer config's provider declarations.

Parallax output still contains the known watch-root defect:

```text
id = "bun-ui"
root = "ui"
watch = [..., "scripts/**", "src/**"]
```

The matcher sees repository-root paths, so `ui/src/...` is not covered by
those unprefixed patterns and can force conservative full selection. The
successful render therefore does not establish correct affected-unit
selection or accept the source2 migration. No generated consumer files were
written; all outputs were temporary.

## Strict lock renderer unit

The working source now emits `mise --yes --locked install "${tools[@]}"` for
non-empty Velnor check profiles in both the source1 and source2 renderers. The
focused source1/source2 check-profile tests passed (`60` library tests and `7`
scheduled-check integration tests); scoped rustfmt checks for both files also
passed. A full crate format check was not a clean signal because unrelated
parent policy edits in `src/policy.rs` and `src/s2/policy.rs` were already
unformatted. The renderer change is uncommitted and does not establish
consumer regeneration or CI performance.

The config validator now fails generation when a non-empty check-profile tool
list has no pinned lock keys, while still checking malformed and duplicate
ids before lock membership. The focused source1/source2 config suite passed
12/12, including valid lock membership, missing lock, malformed id, and
duplicate id cases. Unit `mise_tools` validation remains separate; this strict
lock requirement applies to the generated scheduled-profile install boundary.
