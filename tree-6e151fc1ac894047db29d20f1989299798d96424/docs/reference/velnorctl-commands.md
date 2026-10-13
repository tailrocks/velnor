# velnorctl man

Reference for the `velnorctl man` leaf command (command task C005). Pages are
rendered from the same `CommandMetadata`/`FlagMetadata` structs in
`velnor-model` that power help text; there is no second source of the CLI
surface.

## Syntax

```
velnorctl man [--directory <PATH>] [--force]
```

Global flags (`--context`, `-o/--output`, `--instance`, `--repo`,
`--selector`, `--field-selector`, `--since`, `--timeout`, `--no-color`,
`-v/--verbose`) are accepted before or after the subcommand and are never
re-parsed by `man`. Unknown flags, unknown positionals, or a valueless
`--directory` exit with the `USAGE` class.

| Flag | Meaning |
|---|---|
| `--directory <PATH>` | Write the complete page set into this exact directory instead of stdout. |
| `--force` | Overwrite existing page members inside `--directory`; never bypasses destination checks. |

## Behavior

- Without `--directory`, one combined `velnorctl.1` roff page is written to
  stdout: binary NAME/SYNOPSIS, GLOBAL OPTIONS from
  `metadata::global_flags()`, OUTPUT / EXIT STATUS / SAFETY convention
  sections, then one section block per registered leaf command in stable
  name order. Every registered leaf appears exactly once.
- With `--directory <PATH>`, the complete deterministic page set is written:
  `velnorctl.1` plus one `<command>.1` per registered leaf. Each member is
  written atomically (temp file inside the destination directory, then
  rename) with mode `0644`.
- Rendering is byte-deterministic for a given build; reruns produce
  identical output.

## Exit statuses

Exactly one class applies per invocation (`velnor-model::ExitClass`;
commands refine reasons, never the numeric mapping):

| Code | Class | Raised when |
|---:|---|---|
| 0 | SUCCESS | Pages rendered/written. |
| 2 | USAGE | Invalid flag, positional, or missing value; destination is a symlink or not a directory (`man.destination_symlink`, `man.destination_not_directory`). |
| 6 | CONFLICT | An existing member would be overwritten without `--force` (`man.member_exists`). |
| 8 | OPERATION | Local I/O failure while rendering or writing pages (`man.io_failed`). |

## Output and source rules

Resource data goes to stdout; warnings and diagnostics go to stderr.
Machine output modes (`--output json|yaml|jsonl|name`) render versioned
resources stamped with a schema version; human table/wide views render the
same types and are never the source of truth. Rendered pages carry no
credential material by construction.

## Safety

`--directory` writes only into the exact destination given: system man paths
are never installed, updated, or removed implicitly. A symbolic-link or
non-directory destination is rejected outright regardless of `--force`, and
an existing member is overwritten only under explicit `--force`.

## Stable slots versus ephemeral runners

The Slot resource carries a typed `slotKind` field with exactly two values,
preserved as types so downstream consumers never infer it from labels or
names:

- `stable` — a persistent named slot reused across jobs;
- `ephemeral` — a single-job runner created for one job and discarded
  afterwards.
