# Step-condition parser review

Verdict: PASS for the current uncommitted `step_block.rs` delta. No blocker found.

Scope: checkout `970a6dd5`, `crates/velnor-workflow/src/step_block.rs`, and both
`compose_step_block` callers in legacy and S2 `primitives/ir.rs`. The callers
share this parser and map its errors to `GeneratorError`; they cannot silently
diverge. The delta adds fail-closed rejection of an indented plain-scalar
continuation, moves unsupported-scalar validation before YAML scalar decoding,
and covers quoted `!cancelled()`.

Checks:

- An isolated copy of the current parser passed `CARGO_TARGET_DIR=/tmp/velnor-step-probe-target rtk cargo test --offline`: 11/11 tests passed.
- The probe covered quoted single/double YAML scalars, inline `#` handling,
  quoted `}}`, nested `if:` text in a `run: |` body, malformed and unterminated
  conditions, duplicate top-level conditions, and plain-scalar continuation.
- The added probe case with `if: ready` immediately followed by `run: |` passed;
  the continuation check stops at the next top-level mapping key and does not
  mistake the shell body’s deeper indentation for YAML condition text.
- The composed output contains `${{ (guard) && (ready) }}`. This preserves the
  guard and existing condition as separate parenthesized expressions in both
  legacy and S2 render paths.

The previous checkpoint implementation lacked continuation rejection and
rejected decoded quoted expressions beginning with `!`; the current ordering
fixes both without changing nested body text or the generated step boundary.
