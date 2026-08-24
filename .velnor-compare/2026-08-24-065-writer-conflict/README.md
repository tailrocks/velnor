# Plan 065 exclusive-writer conflict (2026-08-24, session velnor-tui @ b211326)

## What happened

Two executors wrote `crates/velnor-model` concurrently during this leaf:

- This session wrote its design: `source.rs`(Source), `condition.rs`,
  `phase.rs`, `sanitized.rs`, `since.rs`, `time.rs`, `cli_meta.rs`,
  `error_envelope.rs`, `resources.rs` (flat resources + ResourceMeta),
  tests `golden_schema.rs`, `redaction_corpus.rs`, `durations.rs`.
- A second writer interleaved and replaced the design with:
  `meta.rs` (ObjectMeta), `resource.rs` (VersionedResource envelope,
  SCHEMA_VERSION = "velnor.io/v1"), `timestamp.rs`, new `source.rs`
  (ResourceSource), then rewrote `lib.rs` and `resources.rs`
  (Spec-payload structs via versioned_resource! macro).

## Interleaved mtime timeline (epoch)

See `mtimes.txt`. Key overlap: this session's `durations.rs` landed at
...522; the other writer's `timestamp.rs` ...547, `source.rs` overwrite
...511, `meta.rs` ...557, `resource.rs` ...579; final rewrite of
`lib.rs`/`resources.rs` at ...642/...672. All within one ~5-minute window
while both sessions were live.

## Concurrent processes observed

Multiple `opencode --auto` instances (PIDs 17862, 17765, 19004) plus
`codex --enable goals --dangerously-bypass-approvals-and-sandbox`
(PID 28854). The campaign contract names exactly one parallel actor
(039, fleet/release-refs.toml); a second actor inside Plan 065 scope is
uncoordinated duplicate execution.

## Why this blocks the leaf

Neither design can be verified while files flip between two incompatible
wire contracts mid-gate (schema_version u32 vs apiVersion string; Source
vs ResourceSource; flat meta vs ObjectMeta+Spec envelope). Any gate result
would be evidence about a tree that no longer exists. Continuing would
guarantee mutual clobbering.

## State left behind

- No commits made by this session.
- This session's files remain on disk where not overwritten:
  condition.rs, phase.rs, sanitized.rs, since.rs, time.rs, cli_meta.rs,
  error_envelope.rs, tests/{golden_schema,redaction_corpus,durations}.rs.
  They are unreferenced by the current lib.rs and do not compile against it.
- fleet/release-refs.toml and .velnor-compare/* untouched (per brief).

## Post-evidence observation (session end)

Conflict remained LIVE through session end: after the first evidence
snapshot the other writer rewrote `resources.rs`, `lib.rs`, `timestamp.rs`,
`meta.rs` again and touched `Cargo.lock` (see second block of mtimes.txt).
Their lib currently compiles; this session's model tests do not. The leaf
was abandoned unmerged rather than clobbering an active peer writer.
Operator must kill or fence one of the duplicate 065 executors before any
re-execution; a fresh run should first reconcile whichever model design
survives on disk.
