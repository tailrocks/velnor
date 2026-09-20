# PR963 independent exact source review

Date: 2026-09-20

## Revision identity

- PR head reviewed: `b1563dadb022bbf8eff6b699c09f4e4267c53d81`.
- PR API base at review: `325719f1e05d3d46322c9fd3eeb9ad545e175638`.
- Current main used for applicability: `89f82dd8b287f46a3cf4c0920f341f6ca6c736db`.
- Review checkout: detached exact head `b1563dad`; clean.
- Hosted tested merge: `83c31ffe6c9ac20824883f3b9b8cff0c8244e0e2`, with parents current main `89f82dd8` and PR head `b1563dad`. Its tree is `a6cf426295f2c473e57b3bfaa0838992d4c8a715`, equal to `git merge-tree --write-tree 89f82dd8 b1563dad`.
- The PR API base is historical relative to current main; hosted CI nevertheless tested the explicit merge above. This review is bound only to `b1563dad` and that merge.

## Verdict

**REJECT source for this revision. Do not merge.**

The Rust scanner changes are materially correct and the exact source has green focused checks. However, the Skills frontmatter validator still violates the authoritative G0 remediation contract: it accepts provider fields outside the exact live schema and treats required policy fields as optional/invalid values as accepted. This is a fail-closed schema blocker, not a generated-output pin issue.

## Blocking finding: Skills frontmatter contract is too permissive

In `crates/velnor-workflow/src/s2/scan/skills.rs`:

- Lines 1112-1122 allow nine keys, including `compatibility`, `metadata`, and `allowed-tools`. The authoritative contract at `G0/skills-review/remediation-contract.md:55-61` allows exactly `name`, `description`, `argument-hint`, `license`, `user-invocable`, and optional `disable-model-invocation`; unknown top-level keys must fail.
- Lines 1169-1185 require only `name` and `description`. The contract at `G0/skills-review/remediation-contract.md:64-66` requires nonempty `argument-hint`, `license == "Apache-2.0"`, and `user-invocable == true`.
- Lines 1695-1712 test and accept `license: MIT` and `user-invocable: false`.
- Lines 1715-1736 test and accept a frontmatter block with `argument-hint`, `license`, and `user-invocable` removed.

The implementation does correctly reject unknown fields outside its nine-key allowlist and checks YAML types, but that does not satisfy the narrower project contract. The bot thread requesting standard optional fields is not authority to broaden the live schema; provider-specific JSON fields must remain provider-owned, while the G0 contract explicitly fixes the SKILL.md live keys. Required remediation: use the exact six-key live schema (plus optional disable flag), enforce the required values, and retain negative fixtures for wrong types, wrong license, false user-invocable, missing required fields, and unknown keys. No approval transfers from older PR963 revisions.

## Correctly fixed and independently verified source behavior

- `crates/velnor-workflow/src/rust_include.rs:145-293`: aliases are collected per lexical scope before use; standard/core qualified calls are recognized while arbitrary qualified user macros are not; dynamic/nonliteral includes remain fail-closed. Tests at lines 991-1062 cover aliasing, literal wrappers, and declaration-after-use lexical behavior.
- `rust_include.rs:380-488`: one-literal-argument macro wrappers are parsed and substituted recursively; nonliteral wrapper calls are ignored rather than over-approximated.
- `rust_include.rs:27-57`: `BuildOutput` include parts are excluded from the source watch set, preserving build-script/source watches.
- `crates/velnor-workflow/src/scan/rust.rs:382-386,559-565,759-818` and `src/s2/scan/rust.rs:432-436,559-565,760-820`: both scanners resolve the nearest/longest owning package before reporting Rust source ownership, preventing cross-package overfetch.
- Private HTML artifact retrieval uses the GitHub token as a Bearer header; Velnor-only steps remain informational under the current provider policy.

## Checks

Exact detached `b1563dad` local checks:

- `cargo test --locked -p velnor-workflow --lib`: 1840 passed.
- `cargo test --locked -p velnor-tools --bin velnor-tools`: 248 passed.
- `cargo test --locked -p velnor-workflow --test package_release`: 3 passed.
- `cargo fmt --all -- --check`, both relevant clippy invocations with `-D warnings`, actionlint, and `git diff --check 325719f1..b1563dad`: pass.
- Exact binary reports revision `b1563dad` and closure `ef5e27b15ab43780ae56bdb615a32a718769b9f267f2c18eaa51b0f9f7eeb179`.
- Full workspace: 2337 passed, 1 timing benchmark failed (`workflow_command_parse_benchmark`, 18.471s versus 10s); the isolated 26-test `workflow_command` rerun passed. This is an environment-sensitive benchmark result, but it does not cure the Skills contract finding.

Hosted run `35486133346` is revision-accurate through tested merge `83c31ffe`: workflow, tools, and generator checks passed; the workflow job reported 1973/1973 and tools reported 250/250. Required DCO, Policy (`35486132167`), and `ci-required` checks passed. The planning log had a nonempty 17-unit hosted workload with `excluded=[]`; 20 jobs succeeded and 48 were intentionally skipped: 17 Velnor execution lanes, 17 Velnor shadows, 13 package cargo-prep lanes, and `Control / Prepare Cargo`. Thus green hosted status is not an empty-workload artifact, but it is not approval of the unresolved schema contract.

## Generated pin follow-up

The exact source check exits successfully while reporting that generated output matches the candidate render, not the declared historical generator revision `0dc79895ff1c5e88be7c3822c437e1c5b5282e12`. After an approved source merge, the declared generator pin must be bumped and generated outputs regenerated authoritatively. This expected follow-up is separate from the blocking source rejection above.

## Feedback census

Fresh API/GraphQL reads found 9 submitted bot review records (all `COMMENTED`, none approval), 28 inline comments/threads, all 28 unresolved, with no pagination remainder. Seven inline comments target `b1563dad`; the Skills comment is the current schema discussion. Issue comments contain no newer submitted b156 verdict. This report is independent and revision-bound; later heads require a new review.
