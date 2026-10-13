# clap-cli-migration writer conflict (2026-08-24 ~16:05Z)

## Timeline

1. Operator directive (all sessions): rewrite `velnorctl` CLI on idiomatic
   stable clap; PR #286 surface.
2. This session registered claim row `clap-cli-migration` in
   COORDINATION.md and pushed to `velnor-estate-standard` (`f79de0b`, mirrored
   `4668bba`) at ~15:45Z, then dispatched an implementation subagent on local
   branch `velnorctl-clap-migration`.
3. The subagent observed continuous writes across the entire claimed scope by
   another live agent (three concurrent `opencode --auto` processes visible)
   with NO registry row: `globals.rs`+`metadata.rs` deleted, `completion.rs`
   added, `lib.rs`/`main.rs`/`man.rs` + all three test files rewritten
   mid-read. It made zero edits and stood down (correct rule 4 behavior).

## Root cause

The coordination registry lives on `velnor-estate-standard`; the clap task
runs on a separate local branch off merged main where a peer session executing
the same operator directive never reads it. Claim-before-write cannot prevent
this class of collision across branches.

## Disposition

- This session STOPS writing clap-migration scope (rule 4).
- On-disk design is coherent and converges toward the same operator spec;
  per "plan wins" + reconcile-forward, the active writer's tree governs.
- This session re-reads every migrated file only after writer quiescence,
  then contributes verification/review only if ownership resolves here;
  otherwise it yields the leaf entirely and returns to pool monitoring.
- Observed mid-state defects to check at reconciliation time: typed `--since`
  (String vs velnor_model::Since), typed Duration timeout, `-vvv` → typed
  verbosity enum, missing `OutputArg::is_machine()`, clap_mangen `.TH` date
  determinism.

## Snapshot at yield

Dirty files on `velnorctl-clap-migration`: Cargo.toml, Cargo.lock,
crates/velnorctl/Cargo.toml, deleted src/globals.rs + src/metadata.rs,
modified src/lib.rs src/main.rs src/man.rs, new src/completion.rs, modified
tests/cli_smoke.rs tests/man.rs tests/parser_matrix.rs.
Diff stat at snapshot: 11 files changed, 876 insertions(+), 1752 deletions(-).
