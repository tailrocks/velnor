# G3 skills/plugin adapter — implementation evidence

Source branch: `codex/github-first-skills-adapter`\
Source base: `abe9ad82a2d4d01b706bbc6122ab6ccb150faad9`\
Implementation commit: `2ec3502026a2461aa520f6da6ac0a3cef3f1a93d`

## Scope

- S2 only; no legacy scanner or fleet repository files changed.
- Added `UnitKind::Skills`, `skills-plugin-pipeline`, typed Bun provisioning,
  canonical watch globs, and `crates/velnor-workflow/src/s2/scan/skills.rs`.
- Detector validates provider manifests, strict duplicate-key JSON, catalog
  names/order, typed live/template frontmatter, generated-doc index, CommonMark
  local Markdown links, template JSON/TOML/YAML/frontmatter, and helper/doc
  verification commands.
- Only embedded template trees are hidden from generic manifest detectors:
  `skills/<catalog-name>/templates/**` requires skill template context, and
  `scripts/<helper>/templates/**` requires a helper source declaration.
  Helper packages outside those metadata-backed boundaries remain executable
  units.

## Exact repository scans

Command:
`target/debug/velnor-workflow --providers github-hosted --dry-run --plain <isolated-research-copy>`

All scans used the G0-pinned HEAD SHAs and an isolated schema-2 config. Each
exited 0 with exactly one `Skills / Plugins` unit, no nested language unit,
and `tool=1.4.0`. Dry-run output was 10 generated files per repository.

| Repository | HEAD | Skills detected |
| --- | --- | ---: |
| `tailrocks-code-quality-skills` | `0b9a1eaa83ca2ad9b2c895741648e7cde10f6189` | 12 |
| `tailrocks-macos-skills` | `1fb177a9a4dc16120b4bc7ca9c0eb4e68f9119d2` | 15 |
| `tailrocks-open-source-skills` | `6e77a448f9776e837bfc9ab18b833bd5fdc26d3a` | 5 |
| `tailrocks-pull-request-skills` | `2b4f71f49fd27061e64d16b2b7f83d9bd2df5612` | 6 |
| `tailrocks-roadmap-skills` | `66d9c79f6472ddace0e265335dc8dd36cdeb8a86` | 15 |
| `tailrocks-rust-skills` | `bcc31b1d935dac4de191a6b71b3091f628d204c0` | 15 |
| `tailrocks-skill-authoring-skills` | `9e25890fd63f7ca6c587490ba7cc432f5fdd98d6` | 4 |
| `tailrocks-typescript-skills` | `f434715f7c664af431f0be62982aa379400102de` | 12 |

## Verification at `2ec3502026a2461aa520f6da6ac0a3cef3f1a93d`

- `cargo check -p velnor-workflow --no-default-features`: pass.
- Focused detector tests:
  `cargo test -p velnor-workflow --no-default-features s2::scan::skills::tests -- --nocapture`: 24 pass.
- `cargo clippy -p velnor-workflow --no-default-features --lib -- -D warnings`: pass.
- Full library:
  `cargo test -p velnor-workflow --no-default-features --lib`: 1,699 pass; 2 pre-existing closure tests fail because the
  checkout's `build.rs` spells default features as `""` while canonical closure
  expects `"tui"` (`closure::tests::stamped_features_match_dev_features` and
  `s2::closure::tests::stamped_features_match_dev_features`).
- Helper validator:
  `bun --version && find scripts -type f -name '*.ts' -not -path '*/templates/*' -print0 | xargs -0 bun build --target=bun --no-bundle --outdir <temporary-dir>`: 8/8 pass under Bun `1.4.0`; no consumer tree was modified.
- Generated-doc validator ran from temporary `git archive` copies under Bun
  `1.4.0`: 5/8 byte-stable. Drift remains in
  `tailrocks-macos-skills` (`docs/skills/tailrocks-macos-design/definition.md`,
  `docs/skills/tailrocks-macos-visual-baseline/definition.md`),
  `tailrocks-rust-skills` (`docs/skills/tailrocks-tui-design/definition.md`),
  and `tailrocks-typescript-skills`
  (`docs/skills/tailrocks-tanstack-project-audit/definition.md`,
  `docs/skills/tailrocks-tanstack-project-migrate/definition.md`,
  `docs/skills/tailrocks-tanstack-project-remediate/definition.md`,
  `docs/skills/tailrocks-web-design/definition.md`); no consumer docs
  were modified.

## Explicit remaining gate

The Skills unit uses the central Velnor Bun `1.4.0` pin and asserts
`bun --version == 1.4.0` before both generated-doc and helper checks. The
three consumer docs drift items remain a later consumer-owner task; no fleet
rollout or consumer generation was performed.
