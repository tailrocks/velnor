# Plan 060: Estate-wide nextest-only Rust testing

## Status

- **Priority**: P0
- **Risk**: MED — removing `cargo test --doc` without replacement loses coverage
- **Depends on**: 041 fixture contract; 046 estate audit
- **Current**: IN PROGRESS — Velnor, Termrock, Parallax, TableRock, and the
  telemetry playground are delivered; fixture PR #4, Jackin PR #810, and the
  remaining repository program branches are pushed. Playground PR #8 was
  superseded by policy-compliant direct-main delivery. Local nextest and
  actionlint evidence is green. Velnor `1bd00cf` extends the mechanical rule
  beyond workflow `run:` blocks to live scripts, configuration, instructions,
  and Rust documentation; the current 13-repository audit has zero errors and
  zero test-runner findings. Jackin's host-attach environment leak into
  injected-fake tests was structurally repaired and its affected 2,182-test
  nextest suite passes. Jackin `67b60bea` replaces its indirect rustdoc runner
  with a parser-backed gate requiring nextest mirrors for runnable examples;
  all 27 library packages and 263 xtask tests pass. Java `6f7ee98b` and Ruxel
  `e96ad0c` remove the last live source/script instructions found by the wider
  audit. V-A/V-B/V-C remain fleet-blocked.

## Requirement

Use `cargo nextest run` as the sole Rust test runner in local verification,
CI, scripts, documentation, and agent instructions. Never invoke
`cargo test`.

Stable nextest does not run rustdoc doctests. Therefore each executable
documentation example currently covered only by `cargo test --doc` must gain
an equivalent nextest-discoverable unit or integration regression before the
doctest command is removed. Coverage removal, ignored examples, and a hidden
secondary test runner are forbidden.

## Scope

- Velnor, `tailrocks/velnor-actions-fixture`, and all 13 estate repositories.
- `velnor-tools audit-ci` enforcement for executable/instructional
  `cargo test` commands.
- Workflow, script, Justfile, instruction, and current direction-doc commands.
- Regression tests replacing doctest-only execution.

Historical incident prose and parser fixtures that intentionally model an
arbitrary GitHub `run:` string may retain the words only when they do not
execute or instruct the command.

## Execution

1. Inventory executable and instructional occurrences across delivered default
   branches and active program branches.
2. Migrate ordinary unit/integration invocations to locked nextest commands,
   preserving filters, ignored-test selection, features, packages, and output
   capture semantics.
3. For every doctest command, identify the covered examples, add equivalent
   nextest-discoverable regressions, prove them failing under the old defect
   where applicable, then remove the doctest invocation.
4. Strengthen the fixture and `audit-ci` so future executable or instructional
   `cargo test` drift fails mechanically.
5. Run repository static gates and nextest suites, deliver through each
   repository's current policy, then obtain V-A/V-B/V-C evidence for changed
   workflow surfaces.

## Verification

- `rg` finds no executable or instructional `cargo test` across current
  delivered sources; reviewed historical/parser-only matches are classified.
- `cargo nextest run --workspace --locked` or the repository-layout equivalent
  passes in every changed repository.
- `actionlint` passes for every changed workflow.
- `velnor-tools audit-ci` rejects a fixture containing an executable
  `cargo test` command and the delivered estate reports zero unexplained
  findings.

## Done criteria

- [ ] Velnor and fixture delivered (Velnor complete; fixture PR #4 open)
- [ ] All 13 repositories delivered
- [ ] Doctest coverage preserved as nextest-discoverable regressions
- [ ] Mechanical audit prevention green
- [ ] V-A/V-B/V-C evidence recorded
