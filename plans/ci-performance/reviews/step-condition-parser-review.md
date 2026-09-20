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

## Parent integration validation

The architectural defect was incremental text flushing at metadata boundaries:
it could preserve an old condition while inserting another, or compose an OR
expression without grouping the membership guard. Both renderer paths now use
one complete-step composer; nested shell/YAML text remains unchanged and
unsupported or duplicate condition metadata fails closed.

Parent validation of the integrated source: 2,003 nextest tests passed, zero
skipped; strict all-target Clippy passed; formatting and full actionlint with
ShellCheck passed. Twelve standalone parent probes passed. Initial full-suite
failures were retained: assertions expected the prior unparenthesized spelling.
Only those expected strings changed; required candidate, MBX, sccache and
policy membership gates remain asserted in both rendering paths.

Generator schema versions advance to 67 (legacy) and 72 (S2); regenerated Rust
workflow conditions group each input guard. This is a correctness prerequisite
for collapsed jobs and cache changes, not a measured performance improvement.
Real CI, all-consumer integration and final campaign gates remain outstanding.
No optimization iteration or plateau credit is assigned here.
