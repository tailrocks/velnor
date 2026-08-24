# C005 fixture integration proof — 2026-08-24

Leaf: plans/velnorctl-migration/commands/C005-man.md
Fixture pin: `bd4be09375154e891052b5159801f613fa0b4f09` (tailrocks/velnor-actions-fixture main)
Campaign commits: implementation `b98801e`, review-fix `0630a98`.

## Live dispatch (hygiene followed)

- Pre-dispatch sweep: zero pending/in-progress runs found; single fresh
  dispatch `32744695926` (control-plane @ pinned SHA, created
  2026-08-24T15:24:49Z); monitored only that ID at ≤60s intervals; completed
  `success` (~2 min) — no stuck state ever observed.
- Jobs: validate-inputs success; scenario-hold (Velnor, velnor-trusted,
  velnor-target-mvp) success on the Velnor lane; aggregator success; other
  scenarios skipped by input design.
- Post-run sweep: zero non-completed runs remain (`gh run list` filter count 0).
- Full sanitized run JSON: `run-32744695926.json` in this directory.

## Local man-page proofs (binary target/debug/velnorctl)

- `man --directory <tmpA>` exit 0 → `velnorctl.1` + `man.1`, modes `644`
  verified via stat.
- Determinism: independent second tmpdir byte-identical
  (`diff -r` clean; matching shasums recorded below).
  - man.1 sha256 `886380c3ffab1fdad37029e0d876363e6103da42` (both dirs)
  - velnorctl.1 sha256 `8fc865f08508ff8e67a9669842c3995d619e4755` (both dirs)
- Documented refusals observed live:
  - existing member without `--force` → exit 6 (Conflict)
  - destination symlink → exit 2 (Usage)
  - symlinked member refused even WITH `--force` → exit 2 (Usage)
  - `--directory --force` flag-like value → exit 2 (Usage)
- Stdout mode: `man` exit 0, stderr 0 bytes (warnings-only stderr contract).
- Render/parse: `mandoc -Tascii` renders combined page and members without
  error (`RENDER_OK` / `MEMBER_RENDER_OK`); `-Tlint` reports one cosmetic
  WARNING — `.TH` date slot carries the version string (`TH 0.1.0`) and is
  used verbatim; recorded as future polish, does not affect parsing or
  documented examples.
- Combined page copy preserved sanitized: `combined-page.roff` (no tokens;
  metadata-derived content only).

## Documentation truth check

docs/reference/velnorctl-commands.md exit table matches observed codes above
(6 Conflict member-exists; 2 Usage destination/member/flag-value cases);
live fixture hold scenario executed successfully under the same control-plane
semantics the reference documents; no contradiction observed.
