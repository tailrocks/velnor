# velnorctl commands

Reference for the `velnorctl` CLI. The command surface is a native
[clap](https://docs.rs/clap) application: one typed command tree is the single
source of truth for parsing, help, version, usage errors, shell completions
(`clap_complete`), and man pages (`clap_mangen`). There is no second source of
the CLI surface; the machine-readable `SchemaDocument` served to metadata
consumers is derived from the live clap command tree.

## Syntax

```
velnorctl [GLOBAL FLAGS] <COMMAND> [ARGS]...
```

Global flags (`--context`, `-o/--output`, `--instance`, `--repo`,
`--selector`, `--field-selector`, `--since`, `--timeout`, `--no-color`,
`-v/--verbose`) use clap's native global arguments and are accepted before or
after the subcommand. Unknown flags, unknown values, and missing values are
rejected by clap with exit code 2 (the `USAGE` class); unknown subcommands get
clap's normal diagnostics including did-you-mean suggestions.

## Commands

### `velnorctl man`

Generate man pages for the current command tree (command task C005).

```
velnorctl man [--directory <PATH>] [--force]
```

| Flag | Meaning |
|---|---|
| `--directory <PATH>` | Write the complete page set into this exact directory instead of stdout. |
| `--force` | Overwrite existing page members inside `--directory`; never bypasses destination checks. |

Without `--directory`, one combined `velnorctl.1` roff page is written to
stdout: the clap-rendered root manual, Velnor OUTPUT / EXIT STATUS / SAFETY
convention sections, then one section block per registered leaf command in
stable name order. With `--directory`, the complete deterministic page set is
written (`velnorctl.1` plus one `<command>.1` per leaf), each member atomically
(temp file inside the destination, then rename) with mode `0644`.

Exit statuses for `man`:

| Code | Class | Raised when |
|---:|---|---|
| 0 | SUCCESS | Pages rendered/written. |
| 2 | USAGE | Destination is a symlink or not a directory (`man.destination_symlink`, `man.destination_not_directory`); a symlinked member is refused even under `--force` (`man.member_symlink`). |
| 6 | CONFLICT | An existing member would be overwritten without `--force` (`man.member_exists`). |
| 8 | OPERATION | Local I/O failure while rendering or writing pages (`man.io_failed`). |

### `velnorctl completion <SHELL>`

Generate shell completion scripts from the same clap command tree users
execute (`clap_complete`); `<SHELL>` accepts every `clap_complete::Shell`
variant (`bash`, `zsh`, `fish`, `elvish`, `powershell`). Output goes to
stdout.

## Output and source rules

Resource data goes to stdout; warnings and diagnostics go to stderr.
Machine output modes (`--output json|yaml|jsonl|name`) render versioned
resources stamped with a schema version; human table/wide views render the
same types and are never the source of truth. Rendered pages carry no
credential material by construction.

Runtime failures report either a human `error:` line or, under a machine
`--output` mode, the versioned machine error envelope on stderr; both map to
the same documented exit class.

## Safety

`man --directory` writes only into the exact destination given: system man
paths are never installed, updated, or removed implicitly. A symbolic-link or
non-directory destination is rejected outright regardless of `--force`, and
an existing member is overwritten only under explicit `--force`.

## Stable slots versus ephemeral runners

The Slot resource carries a typed `slotKind` field with exactly two values,
preserved as types so downstream consumers never infer it from labels or
names:

- `stable` — a persistent named slot reused across jobs;
- `ephemeral` — a single-job runner created for one job and discarded
  afterwards.
