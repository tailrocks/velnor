# Command Task C005: Implement `velnorctl man`

> **Executor instructions**: Implement only `velnorctl man`. Do not fold
> sibling commands into this task. Run every verification gate. Update this
> task and command index status when complete.
>
> **Drift check**: `rtk git diff --stat 35d5bb7..HEAD -- crates/velnor-runner/src/main.rs crates/velnor-runner/src/cli.rs crates/velnorctl crates/velnor-model crates/velnor-render`
> Compare live command/service shapes before editing; stop on incompatible drift.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: Plans 064–065
- **Category**: command migration
- **Planned at**: commit `35d5bb7`, 2026-08-24

## Why this matters

Generate man pages for current command tree and global conventions.

## Current state

Old binary owns Clap parsing and direct dispatch. New workspace/parser seam comes from Plans 064–065; no stable command exists yet.

## Scope

Implement parser, typed arguments, handler, rendering, errors, tests, help, and completion metadata for only `velnorctl man` in `crates/velnorctl/src/commands/man.rs` and `crates/velnorctl/tests/man.rs`.
Use shared model/client/control services from dependency plans. Never parse another command's human output or spawn the old binary.
Apply global inspection conventions: versioned table/wide/JSON/YAML/JSONL/name output where meaningful, warnings on stderr, resource data on stdout, useful non-zero exits.

## Required behavior

- Generate from same Clap metadata as executable help; include every leaf command exactly once.
- With no path option, write one combined `velnorctl.1` page to stdout.
  `--directory <path>` atomically writes the complete deterministic page set
  with mode `0644`. Reject symlink/non-directory destinations and existing
  members unless explicit `--force`; never install system files implicitly.
- Document exit statuses, output/source rules, safety flags, and stable-slot versus ephemeral-runner distinction.

## Steps

1. Add exact typed Clap shape for `velnorctl man`; closed values use `ValueEnum`. Reject unknown or sibling-command arguments.
2. Call the shared typed service/client. Keep handler thin; map authorization, connectivity, invalid input, timeout, unavailable data, and domain failure to documented exits.
3. Render human and machine output from versioned resources. Redact credentials and authorization material by construction.
4. Add parser, service-mock, transport, golden-output, exit-code, and no-secret tests named with filter `command_c005`.
5. Update command reference, generated completion/man metadata, and migration matrix. Do not retain an old alias.

**Verify**: `rtk cargo nextest run -p velnorctl --locked command_c005` passes; then `rtk mise run check` exits 0.

## Mandatory fixture integration

Pin exact `tailrocks/velnor-actions-fixture` commit. Before dispatch, cancel every pending/in-progress old fixture run, delete only stale validation-owned runner registrations, and prove both sets clean.
Generate and render man pages from temporary path, follow documented commands to inspect fresh fixture hold run, and prove examples parse unchanged.
Monitor only newly returned run IDs at intervals no longer than 60 seconds. Diagnose queued or unchanged state before two minutes. Save sanitized `.json`, `.jsonl`, `.log`, or `.md` only; never rendered GitHub HTML.

## Done criteria

- [x] `velnorctl man --help` exits 0 and documents exact accepted syntax.
- [x] Focused tests and `rtk mise run check` pass.
- [x] Fixture validation proves live behavior and no secret leakage.
- [x] No sibling command, compatibility alias, or direct internal-file parser was added.

## Execution evidence (2026-08-24)

- Implementation `b98801e`, review-fix `0630a98`; drift ruling recorded: leaf's "same Clap metadata as help" satisfied via the Plan-064 hand-rolled CommandMetadata/SchemaDocument single source (clap is forbidden in velnorctl by dependency law); flat module src/man.rs per live layout.
- Gates via rtk mise tasks: fmt/lint clean; `test-focused -- -p velnorctl command_c005` 18/18; `-p velnorctl` 41/41; full `rtk mise run check` 997/997 exit 0. Independent verifier PASS twice; reviewer APPROVE-FLIP with MED+LOW findings all closed in 0630a98 (io-failure exit-8 test, force-vs-symlinked-member refusal, roff_escape, hyphen escaping, flag-like --directory value Usage).
- Fixture: pin bd4be09375154e891052b5159801f613fa0b4f09; pre-dispatch sweep zero active runs; single dispatch run 32744695926 control-plane, completed success (~2 min), scenario-hold green on Velnor lane; post sweep zero non-completed runs; evidence `.velnor-compare/2026-08-24-C005-fixture-proof/evidence.md`.
- Live behavior proofs: --help exit 0 stderr-empty; directory mode modes 644 byte-deterministic across tmpdirs; refusals observed 6/2/2/2 (member-exists, dest symlink, symlinked member under force, flag-like value); mandoc renders pages without error (cosmetic .TH date-slot WARNING noted).

## STOP conditions

- Shared service cannot provide required authoritative data or behavior.
- Implementation needs an unapproved capability, trust expansion, protocol guess, or destructive action outside exact command scope.
- Fixture would need weakening, or two-minute stasis cannot be diagnosed.
