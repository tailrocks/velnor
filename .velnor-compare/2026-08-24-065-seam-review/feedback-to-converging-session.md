# Feedback to converging session — Plan 065 seam review (ce98a27)

From: Session C validator (independent; did not implement). Per
`plans/goal-execution/COORDINATION.md` protocol 3: implementer cannot be sole
reviewer. Review target: commit `ce98a27` (+ folded module work), at HEAD
`1abb12b`, branch `velnor-estate-standard`.

Incident note (rule 4): this session's one-line registry row edit was clobbered
by the concurrent registry-restore write (~19:05Z+07); loss was the row only,
no leaf scope affected. Recorded here as required evidence.

## Verdict: BLOCK — flip 065 only after MAJOR fixes below land and gates rerun

## Independent gate evidence (this session's verifier, read-only)

- Env proven: rtk 0.45.0, mise 2026.8.10, branch + HEAD match, clean tree.
- `rtk mise run test-focused -- -p velnor-model -p velnor-render -p velnorctl`
  → 81/81 passed, exit 0.
- `rtk mise run check` → exit 0 (fmt, actionlint, clippy -D warnings, deny ok;
  nextest **944/944**, 19 binaries). Implementer claim said 943/943; actual
  count at HEAD is 944 — record corrected number in execution evidence.
- Landing diff stat clean (32 files, no artifact/secret-named files).
- Secret scan of diff: clean (2 hits are redaction-corpus prefix strings in a
  test constant, false positives).

## Findings (verified by compiled probes, not assertion)

1. MAJOR `crates/velnorctl/src/globals.rs:133`: leftover
   `eprintln!("DBG top index=…")` emits for every argv token on every real
   invocation (doubled via main.rs re-parse), polluting stderr against the
   warnings-only stderr contract. Fix: delete the line.
2. MAJOR `crates/velnor-model/src/sanitized.rs:46`: derived `Deserialize`
   accepts any string; probe round-tripped
   `"https://user:ghp_secrettoken@github.example.com/x"` through
   `SanitizedUrl` verbatim. Redaction-by-construction is bypassed whenever a
   resource deserializes (files/API round-trips). Fix: custom `Deserialize`
   that re-runs `project()` or stores parsed parts only.
3. MAJOR `crates/velnor-model/src/since.rs:40`: `Since::resolve` panics
   (`out of range`) for parseable `--since 18446744073709551615ms`;
   `unwrap_or(i64::MAX)` saturation then unchecked subtraction violates the
   typed-overflow duration contract (`*_ms` overflow must be a typed
   serialization/validation error, never wrap/panic). Fix: `checked_sub` +
   typed error surfaced through ExitClass mapping.
4. MINOR `crates/velnor-model/src/resources.rs:19`: `#[serde(flatten)]`
   neutralizes `ResourceMeta`'s `deny_unknown_fields` for embedded resources
   (probe accepted unknown field on Slot JSON). Post-deserialize validate or
   document the limit in the leaf evidence.
5. MINOR `crates/velnorctl/src/globals.rs:117`: help/version pre-scan ignores
   the `--` terminator, so `velnorctl status -- --help` short-circuits instead
   of passing through as promised by module docs.
6. NIT `crates/velnorctl/src/main.rs:7`: argv parsed twice (run() +
   parse_invocation()); derive machine_output from the single parse.
7. NIT `crates/velnorctl/src/globals.rs:147`: inline values on switches
   silently ignored (`--no-color=false` still disables color).

Clean dimensions: schema versioning + RFC 3339 types; phase list exactly the
leaf's 11 phases with fail-closed serde (no `#[serde(other)]`); renderer matrix
complete with stdout/stderr separation; ExitClass exhaustiveness; workspace-norm
dependencies only; no network/fs/capability expansion; strong negative tests.

## Required before status flip

- Fixes for findings 1–3 (blocking) landed as leaf-scoped commits on this
  branch by the leaf owner; 4–7 recommended same pass.
- Rerun focused + `rtk mise run check`; update execution-evidence test counts.
- Flip stays atomic (item file + TASKS.md + category index) per protocol 4.
