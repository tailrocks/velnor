# G3 independent exact review: Skills adapter

Reviewed: 2026-09-19T18:11:12Z  
Candidate: `4479ab7130f68e4a6cf06eadef1506f947b49b62`  
Base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`  
Worktree: `dual-lane-skills-adapter` (clean at review)

## Verdict

The schema2 adapter is materially functional, but not acceptance-ready. Four bounded gaps remain before it can advertise a complete, reproducible Skills contract: typed frontmatter validation, semantic helper/template ownership, an exact Bun toolchain, and stale generated docs in three of eight consumers. These are implementation-contract gaps, not a request for a legacy schema1 Skills engine.

## Verified behavior

- Scope is schema2 only. No legacy-path absence was scored as a defect, per the current migration scope.
- `cargo check -p velnor-workflow --no-default-features`, focused Skills tests (14/14), clippy with `-D warnings`, and workspace formatting checks passed.
- Full-library comparison: candidate `1689 passed, 2 failed`; base `1675 passed, 2 failed`. Both revisions fail the same two pre-existing closure feature-stamp tests (`closure::tests::stamped_features_match_dev_features` and `s2::closure::tests::stamped_features_match_dev_features`, empty `build.rs` features versus canonical `tui`). The candidate adds 14 passing tests and introduces no new full-suite failure.
- Temporary schema2 wrappers were used only for read-only scans of the eight pinned repositories. Every scan produced exactly one `Skills / Plugins` unit and no nested Rust, Bun, Swift, or other language unit:

  | consumer | pinned source SHA | catalog count | result |
  | --- | --- | ---: | --- |
  | code-quality | `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189` | 12 | 1 Skills unit |
  | macos | `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` | 15 | 1 Skills unit |
  | open-source | `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a` | 5 | 1 Skills unit |
  | pull-request | `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612` | 6 | 1 Skills unit |
  | roadmap | `66d9c79f6472ddace0e265335dc8dd36cdeb8a86` | 15 | 1 Skills unit |
  | rust | `bcc31b1d935dac4de191a6b71b3091f628d204c0` | 15 | 1 Skills unit |
  | skill-authoring | `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6` | 4 | 1 Skills unit |
  | typescript | `f434715f7c664af431f0be62982aa379400102de` | 12 | 1 Skills unit |

- Malformed plugin markers fail closed; strict recursive JSON parsing rejects duplicate keys; catalog order, provider names/versions, docs index order, missing references, and invalid declared skill templates are checked.
- The fixture preserves a real helper manifest (`skills/example/helpers/package.json`) as a Bun unit while hiding `skills/example/templates/**`. Ordinary standalone Rust and Bun controls retain their language units.
- No consumer repository name or `0.28.0` literal was found in the candidate Skills detector. No Velnor packaging requirement was introduced for these unrelated consumers.

## Findings

### F-01 — blocker: frontmatter is not typed

`parse_frontmatter` stores every scalar as a string, strips quotes manually, then only asks `serde_yaml` whether the document is a mapping (`skills.rs:541-616`). Required-field checks compare those strings (`skills.rs:263-305`). A temporary copy changing a valid `argument-hint` scalar to a YAML sequence still exited zero and produced the Skills unit. This violates strict typed frontmatter validation; wrong scalar types, and currently arbitrary unknown keys, are not rejected by the contract.

Bounded fix: deserialize a typed frontmatter mapping with duplicate-key detection and an explicit unknown-key policy; reject sequence/map values for scalar fields and validate booleans/required fields from their actual YAML types. Add negative fixtures for every wrong type and unknown-key decision.

### F-02 — blocker: helper/template boundary is incomplete

The hidden set covers only `skills/<catalog-name>/templates/**` (`skills.rs:494-506`). The generated helper command compiles every `scripts/**/*.ts` (`skills.rs:29-30`), including helper-owned template trees such as `scripts/macos-visual-qa/templates/run.ts` and `scripts/web-visual-qa/templates/**`. There is no typed executable-helper inventory or ownership classifier. The real helper package is retained, but embedded helper templates are not excluded from helper validation/claiming.

Bounded fix: classify repository roots and executable helper manifests first; exclude template/support trees from generated helper checks and generic detector claims; retain real helper package units. Add fixtures for both sides of the boundary.

### F-03 — blocker: Bun is mutable/unpinned

Generated setup uses an immutable `oven-sh/setup-bun` action SHA but supplies no `bun-version`; both generated commands invoke ambient `bun`. None of the eight consumer roots proves a repository-wide Bun version. The adapter records this limitation (`skills.rs:68-74`) instead of silently claiming reproducibility, which is honest but does not satisfy a deterministic generation/test contract.

Bounded fix: require one centrally owned exact Bun version/toolchain identity, pass it to setup and local validation, and test the generated unit with that exact toolchain. Do not infer a version from nested template examples.

### F-04 — blocker: generated docs are stale in 3/8 consumers

The generated docs-drift command catches this, but current source snapshots are not all green. Reproduced results: code-quality, open-source, pull-request, roadmap, and skill-authoring pass; macos (2 definition diffs), rust (1), and typescript (4) fail. Regenerate/fix those consumer docs or keep them explicitly outside the acceptance set; do not report all-eight generated meaning as clean while the gate fails.

### F-05 — high: Markdown reference parsing is incomplete

The resolver takes the first `)` and removes only a `#` suffix (`skills.rs:367-447`). It does not parse Markdown titles/parenthesized destinations or decode percent escapes and query components. It correctly rejects observed missing/root-escaping references and uses an explicit placeholder marker, but valid links with query/encoded paths can be false-rejected and malformed destinations can be mis-tokenized. Use a real Markdown destination parser plus safe URI/path normalization; add query, fragment, encoded, title, and nested-parenthesis fixtures.

### F-06 — medium: provider version validation is equality-only

Provider and marketplace versions must be nonempty and equal, but no version syntax/relational policy is enforced (`skills.rs:174-258`). A common invalid string could therefore be accepted consistently across all manifests. Define the supported version grammar/relationship and test malformed versions. Decide and document whether unknown JSON keys are forward-compatible or rejected; current parsing permits them.

### F-07 — medium: executable checks are shell strings, not a typed helper contract

The two generated commands are unstructured string constants (`skills.rs:29-30`, `95-99`) and operate on broad globs. This is not a second legacy generator, but it leaves ownership, exclusions, and toolchain identity outside a typed central primitive. Fold the bounded fixes above into one central Skills validation model so generated config and scanner semantics cannot drift.

## Required bounded follow-up

1. Replace manual frontmatter string parsing with typed validation and negative tests.
2. Add semantic helper/template ownership and executable-helper inventory; preserve real helper packages.
3. Pin one exact Bun toolchain centrally and emit/use it in setup and checks.
4. Resolve the three stale consumer docs snapshots.
5. Harden Markdown destinations and provider-version validation, with focused negative fixtures.

The existing base comparison supports preserved behavior and the two closure failures are baseline, not candidate regressions. No source, consumer, remote, install, or dispatch mutation was made during this review. Prior baseline artifacts remain preserved in `report.md` and `review.json`.
